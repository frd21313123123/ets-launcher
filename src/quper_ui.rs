use crate::{
    mod_browser,
    ui::{self, Button, Icon, Kind, Palette, Tone, W},
};
use eframe::egui::{
    self, Align, CornerRadius, Layout, Margin, Rect, Sense, Stroke, Vec2, pos2, vec2,
};
use ets_launcher::{
    download::{self, SharedTransfer, Status, Transfer},
    format_bytes,
    game::Game,
    maps,
    quper::{
        self, Catalog, Combo, FileRow, Plan,
        library::{Library, Report},
    },
    settings::Settings,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

enum Event {
    Catalog(Result<Catalog, String>),
    Plan(String, Result<Plan, String>),
    Image(String, Result<egui::ColorImage, String>),
    Job(Result<Report, String>),
}

#[derive(Clone, Copy, PartialEq)]
enum CardFilter {
    All,
    Downloaded,
    Convoy,
}

#[derive(Clone, Copy, PartialEq)]
enum FileFilter {
    All,
    Ready,
    Pending,
    Import,
}

/// Library state of the opened build, refreshed periodically instead of every frame.
struct Detail {
    id: String,
    checked: Instant,
    missing: Vec<String>,
    ready: Option<PathBuf>,
}

pub struct QuperUi {
    catalog: Option<Catalog>,
    plan: Option<Plan>,
    selected: Option<String>,
    game: Game,
    version: String,
    search: String,
    filter: CardFilter,
    files: FileFilter,
    show_problems: bool,
    refreshing: bool,
    loading_plan: bool,
    next_refresh: Instant,
    job: bool,
    transfer: SharedTransfer,
    cancel: CancellationToken,
    metadata_cancel: CancellationToken,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    note: Option<(Tone, String)>,
    report: Option<Report>,
    images: BTreeMap<String, egui::TextureHandle>,
    image_requests: BTreeSet<String>,
    /// Card badges: `(ready mods, total, environment ready)` for builds already started.
    progress: BTreeMap<String, Option<(usize, usize, bool)>>,
    detail: Option<Detail>,
    browser: mod_browser::Browser,
    browser_sender: mpsc::Sender<mod_browser::Event>,
    browser_receiver: mpsc::Receiver<mod_browser::Event>,
    browser_queue: VecDeque<(String, String)>,
    browser_current: Option<String>,
    browser_request: Option<(String, String)>,
    imports: Vec<PathBuf>,
}

impl QuperUi {
    pub fn new(settings: &Settings) -> Self {
        let catalog = maps::config_path("quper-catalog.json")
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Catalog>(&b).ok())
            .filter(|c| c.validate().is_ok());
        let (sender, receiver) = mpsc::channel();
        let (browser_sender, browser_receiver) = mpsc::channel();
        let mut ui = Self {
            catalog,
            plan: None,
            selected: None,
            game: settings.selected_game,
            version: String::new(),
            search: String::new(),
            filter: CardFilter::All,
            files: FileFilter::All,
            show_problems: false,
            refreshing: false,
            loading_plan: false,
            next_refresh: Instant::now(),
            job: false,
            transfer: Arc::new(Mutex::new(Transfer::default())),
            cancel: CancellationToken::new(),
            metadata_cancel: CancellationToken::new(),
            sender,
            receiver,
            note: None,
            report: None,
            images: BTreeMap::new(),
            image_requests: BTreeSet::new(),
            progress: BTreeMap::new(),
            detail: None,
            browser: mod_browser::Browser::default(),
            browser_sender,
            browser_receiver,
            browser_queue: VecDeque::new(),
            browser_current: None,
            browser_request: None,
            imports: Vec::new(),
        };
        ui.ensure_version(settings.selected_game);
        ui
    }

    pub fn busy(&self) -> bool {
        self.job || self.browser.downloading() || !self.imports.is_empty()
    }
    pub fn preparing(&self) -> bool {
        self.busy()
            || self.browser_current.is_some()
            || !self.browser_queue.is_empty()
            || self.browser_request.is_some()
    }
    pub fn browser_visible(&self) -> bool {
        self.browser.visible
    }
    pub fn has_selection(&self) -> bool {
        self.selected.is_some()
    }
    pub fn suspend_browser(&self) {
        self.browser.suspend();
    }
    pub fn cancel_all(&mut self) {
        self.stop_queue();
        self.metadata_cancel.cancel();
    }
    /// The library folder moved: cached progress belongs to the old one.
    pub fn library_changed(&mut self) {
        self.progress.clear();
        self.detail = None;
        self.report = None;
    }
    fn stop_queue(&mut self) {
        self.cancel.cancel();
        self.browser.cancel();
        self.browser_queue.clear();
        self.browser_request = None;
        self.browser_current = None;
        self.imports.clear();
    }
    pub fn root(settings: &Settings) -> PathBuf {
        settings
            .quper_library
            .clone()
            .unwrap_or_else(|| settings.output.join("Quper"))
    }
    fn library(settings: &Settings) -> anyhow::Result<Library> {
        Library::open(&Self::root(settings))
    }
    fn fail(&mut self, message: impl Into<String>) {
        self.note = Some((Tone::Warn, message.into()));
    }
    fn inform(&mut self, message: impl Into<String>) {
        self.note = Some((Tone::Info, message.into()));
    }

    fn versions(&self, game: Game) -> Vec<String> {
        let mut versions = self
            .catalog
            .iter()
            .flat_map(|c| &c.combos)
            .filter(|c| c.game == game)
            .map(|c| c.game_version.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        versions.sort_by_key(|s| {
            std::cmp::Reverse(
                s.split('.')
                    .map(|p| p.parse::<u32>().unwrap_or_default())
                    .collect::<Vec<_>>(),
            )
        });
        versions
    }
    fn ensure_version(&mut self, game: Game) {
        let versions = self.versions(game);
        if !versions.contains(&self.version) {
            self.version = versions.first().cloned().unwrap_or_default();
            self.selected = None;
            self.plan = None;
        }
    }
    fn combo(&self) -> Option<&Combo> {
        self.catalog
            .as_ref()?
            .combos
            .iter()
            .find(|c| Some(&c.id) == self.selected.as_ref())
    }
    fn close_details(&mut self) {
        self.selected = None;
        self.plan = None;
        self.report = None;
        self.detail = None;
        self.show_problems = false;
    }

    fn refresh(&mut self, ctx: &egui::Context, runtime: &Runtime) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        let (sender, cancel, ctx) = (
            self.sender.clone(),
            self.metadata_cancel.clone(),
            ctx.clone(),
        );
        runtime.spawn(async move {
            let result = quper::source::fetch_catalog(&cancel)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = sender.send(Event::Catalog(result));
            ctx.request_repaint();
        });
    }

    fn cached_plan(combo: &Combo) -> Option<Plan> {
        // Cached plans are tied to the exact release and date, never to old maps.
        let path = maps::config_path(&format!("quper-plan-{}.json", combo.id)).ok()?;
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Plan>(&b).ok())
            .filter(|p| {
                p.validate().is_ok()
                    && p.combo.version == combo.version
                    && p.combo.date == combo.date
            })
    }

    fn select(&mut self, id: &str, ctx: &egui::Context, runtime: &Runtime) {
        if self.preparing() || self.selected.as_deref() == Some(id) {
            return;
        }
        let Some(combo) = self
            .catalog
            .iter()
            .flat_map(|c| &c.combos)
            .find(|c| c.id == id)
            .cloned()
        else {
            return;
        };
        self.close_details();
        self.selected = Some(id.into());
        self.files = FileFilter::All;
        self.note = None;
        self.plan = Self::cached_plan(&combo);
        self.loading_plan = true;
        let (sender, cancel, ctx) = (
            self.sender.clone(),
            self.metadata_cancel.clone(),
            ctx.clone(),
        );
        runtime.spawn(async move {
            let result = quper::source::fetch_plan(&combo, &cancel)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = sender.send(Event::Plan(combo.id, result));
            ctx.request_repaint();
        });
    }

    fn start_job(
        &mut self,
        ctx: &egui::Context,
        runtime: &Runtime,
        settings: &Settings,
        inputs: Option<Vec<PathBuf>>,
    ) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let library = match Self::library(settings) {
            Ok(l) => l,
            Err(e) => {
                self.fail(format!("{e:#}"));
                return;
            }
        };
        self.job = true;
        self.cancel = CancellationToken::new();
        self.note = None;
        download::set_state(&self.transfer, |s| {
            *s = Transfer {
                status: Status::Connecting,
                message: "Подготовка сборки Quper…".into(),
                ..Default::default()
            }
        });
        let (sender, cancel, ctx, transfer) = (
            self.sender.clone(),
            self.cancel.clone(),
            ctx.clone(),
            self.transfer.clone(),
        );
        runtime.spawn(async move {
            let result = if let Some(inputs) = inputs {
                library.import(&plan, &inputs, &transfer, &cancel).await
            } else {
                library.download(&plan, &transfer, &cancel).await
            };
            download::set_state(&transfer, |s| {
                s.status = if cancel.is_cancelled() {
                    Status::Cancelled
                } else if result.is_ok() {
                    Status::Complete
                } else {
                    Status::Error
                };
            });
            let _ = sender.send(Event::Job(result.map_err(|e| format!("{e:#}"))));
            ctx.request_repaint();
        });
    }

    fn next_browser(&mut self) {
        self.browser.cancel();
        self.browser_current = None;
        if let Some(next) = self.browser_queue.pop_front() {
            self.browser_request = Some(next);
        }
    }

    pub fn tick(
        &mut self,
        ctx: &egui::Context,
        runtime: &Runtime,
        settings: &Settings,
        dlc_busy: bool,
    ) {
        if settings.selected_game != self.game && !self.preparing() {
            self.game = settings.selected_game;
            self.version.clear();
            self.ensure_version(self.game);
            self.close_details();
        }
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::Catalog(result) => {
                    self.refreshing = false;
                    self.next_refresh = Instant::now()
                        + Duration::from_secs(if result.is_ok() { 1800 } else { 60 });
                    match result {
                        Ok(catalog) if !self.preparing() => {
                            if let Err(e) = maps::config_path("quper-catalog.json")
                                .and_then(|p| maps::write_json(&p, &catalog))
                            {
                                self.fail(format!("Не удалось сохранить каталог: {e:#}"));
                            }
                            if let Some(selected) = self.combo()
                                && catalog.combos.iter().any(|c| {
                                    c.id == selected.id
                                        && (c.version != selected.version
                                            || c.date != selected.date)
                                })
                            {
                                self.close_details();
                            }
                            self.catalog = Some(catalog);
                            self.progress.clear();
                            self.ensure_version(self.game);
                        }
                        Ok(_) => self.next_refresh = Instant::now(),
                        Err(e) => self.fail(format!(
                            "Не удалось обновить каталог Quper: {e}. Сохранённый каталог доступен."
                        )),
                    }
                }
                Event::Plan(id, result) if self.selected.as_ref() == Some(&id) => {
                    self.loading_plan = false;
                    match result {
                        Ok(plan) => {
                            if let Err(e) = maps::config_path(&format!("quper-plan-{id}.json"))
                                .and_then(|p| maps::write_json(&p, &plan))
                            {
                                self.fail(format!("Не удалось сохранить список файлов: {e:#}"));
                            }
                            self.plan = Some(plan);
                            self.detail = None;
                            self.progress.remove(&id);
                        }
                        Err(e) => self.fail(format!("Не удалось обновить список файлов: {e}")),
                    }
                }
                Event::Plan(_, _) => {}
                Event::Image(url, result) => {
                    if let Ok(image) = result {
                        self.images.insert(
                            url.clone(),
                            ctx.load_texture(url, image, egui::TextureOptions::LINEAR),
                        );
                    }
                }
                Event::Job(result) => {
                    self.job = false;
                    self.detail = None;
                    self.progress.clear();
                    match result {
                        Ok(report) => {
                            if report.home.is_some() {
                                self.inform("Сборка готова. Порядок модов от Quper применён к отдельному профилю.");
                                self.browser.cancel();
                                self.browser_queue.clear();
                                self.browser_current = None;
                                self.browser_request = None;
                            } else {
                                let total = self.plan.as_ref().map_or(0, |p| p.order.len());
                                self.inform(format!(
                                    "Подготовлено {} из {total} модов. Недостающие файлы показаны в списке.",
                                    total.saturating_sub(report.missing.len()),
                                ));
                                for problem in &report.problems {
                                    if let Some(url) = &problem.url
                                        && !self.browser_queue.iter().any(|(u, _)| u == url)
                                        && self.browser_current.as_ref() != Some(url)
                                    {
                                        self.browser_queue
                                            .push_back((url.clone(), problem.name.clone()));
                                    }
                                }
                                if let (Some(url), Some(plan)) = (&self.browser_current, &self.plan)
                                {
                                    let remaining = plan
                                        .files
                                        .iter()
                                        .filter(|r| r.required && r.urls.contains(url))
                                        .any(|r| {
                                            report
                                                .missing
                                                .iter()
                                                .any(|m| m.eq_ignore_ascii_case(&r.mod_id))
                                        });
                                    if !remaining {
                                        self.next_browser();
                                    }
                                } else if self.browser_request.is_none() {
                                    self.next_browser();
                                }
                            }
                            self.report = Some(report);
                        }
                        Err(e) => self.fail(e),
                    }
                }
            }
        }
        while let Ok(event) = self.browser_receiver.try_recv() {
            match event {
                mod_browser::Event::Complete(id, path) => {
                    if self.plan.as_ref().is_some_and(|p| p.combo.id == id) {
                        self.imports.push(path);
                    }
                }
                mod_browser::Event::Failed(e) => self.fail(e),
                mod_browser::Event::Started(id) => {
                    self.inform(format!("Скачивание файла сборки {id} со страницы автора…"))
                }
                mod_browser::Event::Navigate(url) => self.browser.navigate(&url),
                mod_browser::Event::Location(url) => self.browser.url = url,
                mod_browser::Event::PopupRequested(url) => self.browser.popup_requested(url),
            }
        }
        if !self.job && !self.browser.downloading() && !dlc_busy && !self.imports.is_empty() {
            let inputs = std::mem::take(&mut self.imports);
            self.start_job(ctx, runtime, settings, Some(inputs));
        }
        if !self.preparing() && !dlc_busy && Instant::now() >= self.next_refresh {
            self.refresh(ctx, runtime);
        }
    }

    fn request_image(&mut self, combo: &Combo, ctx: &egui::Context, runtime: &Runtime) {
        if !self.image_requests.insert(combo.preview.clone()) {
            return;
        }
        let (url, sender, ctx, cancel) = (
            combo.preview.clone(),
            self.sender.clone(),
            ctx.clone(),
            self.metadata_cancel.clone(),
        );
        runtime.spawn(async move {
            let result: anyhow::Result<_> = async {
                let (_, bytes) =
                    quper::source::get(&quper::source::client()?, &url, &cancel).await?;
                let mut reader =
                    image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(8192);
                limits.max_image_height = Some(8192);
                limits.max_alloc = Some(64 * 1024 * 1024);
                reader.limits(limits);
                let image = reader.decode()?.thumbnail(640, 360).to_rgba8();
                Ok(egui::ColorImage::from_rgba_unmultiplied(
                    [image.width() as usize, image.height() as usize],
                    image.as_raw(),
                ))
            }
            .await;
            let _ = sender.send(Event::Image(url, result.map_err(|e| e.to_string())));
            ctx.request_repaint();
        });
    }

    /// Progress badge of a card; reads only builds that already have a folder.
    fn card_progress(
        &mut self,
        combo: &Combo,
        settings: &Settings,
    ) -> Option<(usize, usize, bool)> {
        if let Some(value) = self.progress.get(&combo.id) {
            return *value;
        }
        let root = Self::root(settings);
        let value = (|| {
            if !root.join(&combo.id).is_dir() {
                return None;
            }
            let plan = Self::cached_plan(combo)?;
            let (done, ready) = Library::open(&root).ok()?.progress(&plan).ok()??;
            Some((done, plan.order.len(), ready))
        })();
        self.progress.insert(combo.id.clone(), value);
        value
    }

    fn detail_state(&mut self, plan: &Plan, settings: &Settings) -> (Vec<String>, Option<PathBuf>) {
        let stale = self.detail.as_ref().is_none_or(|d| {
            d.id != plan.combo.id
                || d.checked.elapsed() > Duration::from_millis(if self.job { 1500 } else { 5000 })
        });
        if stale {
            let library = Self::library(settings);
            let missing = library
                .as_ref()
                .ok()
                .and_then(|l| l.missing(plan).ok())
                .unwrap_or_else(|| plan.order.clone());
            let ready = library
                .as_ref()
                .ok()
                .and_then(|l| l.ready_home(plan).ok())
                .flatten();
            self.detail = Some(Detail {
                id: plan.combo.id.clone(),
                checked: Instant::now(),
                missing,
                ready,
            });
        }
        let detail = self.detail.as_ref().unwrap();
        (detail.missing.clone(), detail.ready.clone())
    }

    /// Draws the Quper section; returns true when the user asked to open the settings.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        runtime: &Runtime,
        settings: &mut Settings,
        dlc_busy: bool,
        p: &Palette,
        stripes: &egui::TextureHandle,
    ) -> bool {
        if let Some((url, title)) = self.browser_request.take() {
            let result = Self::library(settings).and_then(|l| {
                let plan = self.plan.as_ref().context("Не выбрана сборка")?;
                let output = l.release(&plan.combo)?.join("downloads").join("browser");
                self.browser.open_file(
                    frame,
                    &plan.combo.id,
                    &title,
                    &url,
                    output,
                    self.browser_sender.clone(),
                )
            });
            match result {
                Ok(()) => self.browser_current = Some(url),
                Err(e) => {
                    self.fail(format!("{e:#}"));
                    self.browser_queue.clear();
                }
            }
        }
        if self.browser.visible {
            self.browser_view(ui, p);
            return false;
        }
        if self.selected.is_some() {
            if let Some(combo) = self.combo().cloned() {
                self.details(ui, ctx, runtime, settings, dlc_busy, &combo, p, stripes);
            } else {
                self.close_details();
            }
            false
        } else {
            self.catalog_view(ui, ctx, runtime, settings, dlc_busy, p, stripes)
        }
    }

    fn browser_view(&mut self, ui: &mut egui::Ui, p: &Palette) {
        ui.spacing_mut().item_spacing.y = 12.0;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(ui::text(
                    "Загрузка со страницы автора",
                    13.0,
                    W::Semi,
                    p.muted,
                ));
                ui.label(ui::text(
                    format!("В очереди ещё {} источников", self.browser_queue.len()),
                    20.0,
                    W::Extra,
                    p.text,
                ));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if Button::new("Остановить очередь", Kind::Danger)
                    .icon(Icon::Stop)
                    .height(40.0)
                    .size(13.0)
                    .show(ui, p)
                    .clicked()
                {
                    self.stop_queue();
                }
                if Button::new("Следующий источник", Kind::Ghost)
                    .height(40.0)
                    .size(13.0)
                    .enabled(!self.busy())
                    .show(ui, p)
                    .clicked()
                {
                    self.next_browser();
                }
                if self.job {
                    ui.label(ui::text(
                        "Проверка и подключение скачанного файла…",
                        12.0,
                        W::Regular,
                        p.muted,
                    ));
                    ui.add(egui::Spinner::new().size(14.0).color(p.accent));
                }
            });
        });
        if let Some((tone, note)) = self.note.clone()
            && ui::banner(ui, p, tone, &note, Some("Скрыть"))
        {
            self.note = None;
        }
        self.browser.show(ui);
    }

    #[allow(clippy::too_many_arguments)]
    fn catalog_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        runtime: &Runtime,
        settings: &Settings,
        dlc_busy: bool,
        p: &Palette,
        stripes: &egui::TextureHandle,
    ) -> bool {
        let mut open_settings = false;
        // A click that closes the version menu must not also open the card under it.
        let menu_was_open = egui::Popup::is_any_open(ui.ctx());
        ui.horizontal(|ui| {
            ui::page_title(ui, p, "Готовые сборки модов для ETS2 и ATS", "Сборки Quper");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if Button::new(
                    if self.refreshing {
                        "Обновляем…"
                    } else {
                        "Обновить каталог"
                    },
                    Kind::Ghost,
                )
                .icon(Icon::Refresh)
                .enabled(!self.refreshing && !self.preparing())
                .show(ui, p)
                .clicked()
                {
                    self.refresh(ctx, runtime);
                }
                if self.refreshing {
                    ui.add(egui::Spinner::new().size(14.0).color(p.accent));
                } else if let Some(catalog) = &self.catalog {
                    ui.label(ui::text(
                        format!("Обновлено {}", ui::ago(catalog.fetched_at)),
                        12.0,
                        W::Regular,
                        p.muted,
                    ));
                }
            });
        });

        // Toolbar: search, game version and filter.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
            let filters_width = 330.0;
            let search_width = (ui.available_width() - filters_width - 190.0).max(280.0);
            ui::field(
                ui,
                p,
                &mut self.search,
                "Поиск по названию",
                false,
                search_width,
                Some(Icon::Search),
                p.panel,
            );
            self.version_picker(ui, p);
            let segments = [
                ui::Segment {
                    label: "Все",
                    extra: None,
                    enabled: true,
                },
                ui::Segment {
                    label: "Скачанные",
                    extra: None,
                    enabled: true,
                },
                ui::Segment {
                    label: "Конвой",
                    extra: None,
                    enabled: true,
                },
            ];
            let filters = [CardFilter::All, CardFilter::Downloaded, CardFilter::Convoy];
            let selected = filters.iter().position(|f| *f == self.filter).unwrap_or(0);
            if let Some(index) = ui::segmented(ui, p, selected, &segments, 36.0, 13.0, p.inset, 12)
            {
                self.filter = filters[index];
            }
        });

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui::icon(ui, Icon::Folder, 14.0, p.muted, 2.0);
            ui.label(ui::text("Папка сборок:", 12.0, W::Regular, p.muted));
            // Long paths are shortened so the link stays visible.
            let path_width = (ui.available_width() - 80.0).max(120.0);
            ui.allocate_ui_with_layout(
                vec2(path_width, 18.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(ui::mono_text(
                            Self::root(settings).display().to_string(),
                            12.0,
                            false,
                            p.text2,
                        ))
                        .truncate(),
                    );
                },
            );
            if ui::link(ui, p, "Изменить", 12.0, W::Semi, false).clicked() {
                open_settings = true;
            }
        });

        if let Some((tone, note)) = self.note.clone()
            && ui::banner(ui, p, tone, &note, Some("Скрыть"))
        {
            self.note = None;
        }

        let needle = self.search.to_lowercase();
        let combos = self
            .catalog
            .iter()
            .flat_map(|c| &c.combos)
            .filter(|c| {
                c.game == self.game
                    && c.game_version == self.version
                    && c.title.to_lowercase().contains(&needle)
                    && (self.filter != CardFilter::Convoy || c.convoy)
            })
            .cloned()
            .collect::<Vec<_>>();
        let combos = combos
            .into_iter()
            .filter_map(|c| {
                let progress = self.card_progress(&c, settings);
                (self.filter != CardFilter::Downloaded || progress.is_some())
                    .then_some((c, progress))
            })
            .collect::<Vec<_>>();
        if combos.is_empty() {
            ui::card(p).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(ui::text(
                    if self.refreshing && self.catalog.is_none() {
                        "Загрузка каталога Quper…"
                    } else if self.filter == CardFilter::Downloaded {
                        "Скачанных сборок для этой версии пока нет."
                    } else {
                        "Сборки не найдены. Обновите каталог или измените фильтры."
                    },
                    14.0,
                    W::Semi,
                    p.muted,
                ));
            });
            return open_settings;
        }
        let gap = 16.0;
        let width = ui.available_width();
        let columns = (((width + gap) / (280.0 + gap)).floor() as usize).clamp(1, 4);
        let card_width = (width - gap * (columns as f32 - 1.0)) / columns as f32;
        let enabled = !self.preparing() && !dlc_busy;
        for row in combos.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (combo, progress) in row {
                    self.request_image(combo, ctx, runtime);
                    let clicked = ui
                        .allocate_ui_with_layout(
                            vec2(card_width, 0.0),
                            Layout::top_down(Align::Min),
                            |ui| self.card(ui, p, stripes, combo, *progress, enabled),
                        )
                        .inner;
                    if clicked && !menu_was_open {
                        self.select(&combo.id, ctx, runtime);
                    }
                }
            });
            ui.add_space(gap - 20.0);
        }
        open_settings
    }

    fn version_picker(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let label =
            ui.painter()
                .layout_no_wrap("Версия игры".into(), ui::font(13.0, W::Regular), p.muted);
        let value = ui.painter().layout_no_wrap(
            if self.version.is_empty() {
                "—".into()
            } else {
                self.version.clone()
            },
            ui::mono(14.0, true),
            p.text,
        );
        let width = 14.0 + label.size().x + 10.0 + value.size().x + 8.0 + 14.0 + 12.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, 44.0), Sense::click());
        let fill = if response.hovered() { p.hover } else { p.panel };
        ui.painter().rect(
            rect,
            10,
            fill,
            Stroke::new(1.0_f32, p.line),
            egui::StrokeKind::Inside,
        );
        let mut x = rect.min.x + 14.0;
        let label_width = label.size().x;
        ui.painter().galley(
            pos2(x, rect.center().y - label.size().y / 2.0),
            label,
            p.muted,
        );
        x += label_width + 10.0;
        let value_width = value.size().x;
        ui.painter().galley(
            pos2(x, rect.center().y - value.size().y / 2.0),
            value,
            p.text,
        );
        x += value_width + 8.0;
        ui::paint_icon(
            ui.painter(),
            Rect::from_center_size(pos2(x + 7.0, rect.center().y), Vec2::splat(14.0)),
            Icon::ChevronDown,
            p.muted,
            2.2,
        );
        let enabled = !self.preparing();
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        if !enabled {
            return;
        }
        let versions = self.versions(self.game);
        let mut picked = None;
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(rect.width());
            for (index, version) in versions.iter().enumerate() {
                let label = if index == 0 {
                    version.clone()
                } else {
                    format!("{version} (архив)")
                };
                if ui
                    .selectable_label(
                        *version == self.version,
                        ui::mono_text(label, 13.0, *version == self.version, p.text),
                    )
                    .clicked()
                {
                    picked = Some(version.clone());
                }
            }
        });
        if let Some(version) = picked
            && version != self.version
        {
            self.version = version;
            self.close_details();
        }
    }

    fn card(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        stripes: &egui::TextureHandle,
        combo: &Combo,
        progress: Option<(usize, usize, bool)>,
        enabled: bool,
    ) -> bool {
        let width = ui.available_width();
        let frame = egui::Frame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.line))
            .corner_radius(14)
            .show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 0.0;
                let (cover, _) =
                    ui.allocate_exact_size(vec2(width, width * 9.0 / 16.0), Sense::hover());
                let top = CornerRadius {
                    nw: 14,
                    ne: 14,
                    sw: 0,
                    se: 0,
                };
                match self.images.get(&combo.preview) {
                    Some(image) => ui::paint_cover(ui, image, cover, top),
                    None => {
                        ui::paint_stripes(ui, stripes, cover, top);
                        ui::paint_text(
                            ui,
                            cover,
                            combo.game.short_name(),
                            ui::mono(12.0, false),
                            p.muted,
                        );
                    }
                }
                match progress {
                    Some((_, _, true)) => {
                        badge(ui, p, cover, "Готова", p.ok, Some(Icon::Check), false)
                    }
                    Some((done, total, false)) => {
                        badge(
                            ui,
                            p,
                            cover,
                            &format!("{done} / {total} модов"),
                            p.warn,
                            None,
                            true,
                        );
                        let (strip, _) = ui.allocate_exact_size(vec2(width, 3.0), Sense::hover());
                        ui.painter().rect_filled(strip, 0, p.line_soft);
                        let fraction = done as f32 / total.max(1) as f32;
                        ui.painter().rect_filled(
                            Rect::from_min_size(strip.min, vec2(width * fraction, 3.0)),
                            0,
                            p.warn,
                        );
                    }
                    None => {}
                }
                egui::Frame::new()
                    .inner_margin(Margin {
                        left: 16,
                        right: 16,
                        top: 14,
                        bottom: 16,
                    })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 8.0;
                        ui.add(
                            egui::Label::new(ui::text(&combo.title, 16.0, W::Extra, p.text))
                                .truncate(),
                        );
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                            ui::chip(
                                ui,
                                &format!("{} · {}", combo.game_version, release_label(combo)),
                                true,
                                p.accent,
                                p.accent_chip,
                                22.0,
                                11.0,
                            );
                            if combo.convoy {
                                ui::chip(ui, "Конвой", false, p.text2, p.line_soft, 22.0, 11.0);
                            }
                        });
                        ui.label(ui::text(
                            format!("Обновлено {}", combo.date),
                            12.0,
                            W::Regular,
                            p.muted,
                        ));
                    });
            });
        let rect = frame.response.rect;
        let response = ui.interact(
            rect,
            ui.id().with(("quper-card", &combo.id)),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if enabled && response.hovered() {
            ui.painter().rect_stroke(
                rect.expand(1.5),
                16,
                Stroke::new(3.0_f32, p.ring),
                egui::StrokeKind::Outside,
            );
            ui.painter().rect_stroke(
                rect,
                14,
                Stroke::new(1.0_f32, p.line_strong),
                egui::StrokeKind::Inside,
            );
        }
        if enabled {
            response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
        } else {
            response.on_hover_text("Дождитесь завершения текущей загрузки");
            false
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn details(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        runtime: &Runtime,
        settings: &Settings,
        dlc_busy: bool,
        combo: &Combo,
        p: &Palette,
        stripes: &egui::TextureHandle,
    ) {
        self.request_image(combo, ctx, runtime);
        let idle = !self.preparing() && !dlc_busy && !self.loading_plan;
        if Button::new("Все сборки", Kind::Ghost)
            .icon(Icon::ChevronLeft)
            .height(36.0)
            .size(13.0)
            .weight(W::Bold)
            .pad(10.0)
            .enabled(!self.preparing())
            .show(ui, p)
            .clicked()
        {
            self.close_details();
            return;
        }
        let plan = self.plan.clone();
        let state = plan.as_ref().map(|plan| self.detail_state(plan, settings));
        let total = plan.as_ref().map_or(0, |p| p.order.len());
        let done = state
            .as_ref()
            .map_or(0, |(missing, _)| total.saturating_sub(missing.len()));
        let ready = state.as_ref().and_then(|(_, ready)| ready.clone());

        // Hero.
        egui::Frame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.line))
            .corner_radius(16)
            .inner_margin(20)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let wide = ui.available_width() >= 680.0;
                let layout = if wide {
                    Layout::left_to_right(Align::Min)
                } else {
                    Layout::top_down(Align::Min)
                };
                ui.with_layout(layout, |ui| {
                    ui.spacing_mut().item_spacing = vec2(24.0, 16.0);
                    let cover_width = if wide { 300.0 } else { ui.available_width() };
                    let (cover, _) = ui.allocate_exact_size(
                        vec2(cover_width, cover_width * 9.0 / 16.0),
                        Sense::hover(),
                    );
                    match self.images.get(&combo.preview) {
                        Some(image) => ui::paint_cover(ui, image, cover, 10),
                        None => {
                            ui::paint_stripes(ui, stripes, cover, 10);
                            ui::paint_text(ui, cover, combo.game.short_name(), ui::mono(12.0, false), p.muted);
                        }
                    }
                    ui.allocate_ui_with_layout(
                        vec2(ui.available_width(), 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.add(egui::Label::new(ui::text(&combo.title, 28.0, W::Extra, p.text)).wrap());
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                ui::chip(
                                    ui,
                                    &format!(
                                        "{} · {} · {}",
                                        combo.game.short_name(),
                                        combo.game_version,
                                        release_label(combo)
                                    ),
                                    true,
                                    p.accent,
                                    p.accent_chip,
                                    24.0,
                                    12.0,
                                );
                                if combo.convoy {
                                    ui::chip(ui, "Конвой", false, p.text2, p.line_soft, 24.0, 12.0);
                                }
                                ui.label(ui::text(format!("Обновлено {}", combo.date), 12.0, W::Regular, p.muted));
                            });
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 16.0;
                                if ui::link(ui, p, "Страница сборки", 13.0, W::Semi, true).clicked() {
                                    let _ = open::that(&combo.page);
                                }
                                if ui::link(ui, p, "Таблица файлов", 13.0, W::Semi, true).clicked() {
                                    let _ = open::that(&combo.table);
                                }
                            });
                            ui.add_space(6.0);
                            if plan.is_some() {
                                ui.horizontal(|ui| {
                                    ui.label(ui::text("Модов готово", 13.0, W::Bold, p.text2));
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        ui.label(ui::mono_text(format!("/ {total}"), 15.0, true, p.muted));
                                        ui.label(ui::mono_text(done.to_string(), 15.0, true, p.text));
                                    });
                                });
                                ui::progress(ui, p, done as f32 / total.max(1) as f32, 10.0, p.accent);
                            } else {
                                ui.horizontal(|ui| {
                                    ui.add(egui::Spinner::new().size(14.0).color(p.accent));
                                    ui.label(ui::text("Читаем список файлов и порядок модов…", 13.0, W::Semi, p.muted));
                                });
                            }
                            if plan.is_some() && self.loading_plan {
                                ui.horizontal(|ui| {
                                    ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                                    ui.label(ui::text("Проверяем актуальность списка файлов…", 12.0, W::Regular, p.muted));
                                });
                            }
                            ui.add_space(6.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                                let can_act = idle && plan.is_some();
                                let primary = if ready.is_some() {
                                    "Проверить сборку"
                                } else if done > 0 {
                                    "Продолжить загрузку"
                                } else {
                                    "Скачать сборку"
                                };
                                if Button::new(primary, if ready.is_some() { Kind::Ghost } else { Kind::Primary })
                                    .icon(Icon::Download)
                                    .pad(if ready.is_some() { 16.0 } else { 20.0 })
                                    .enabled(can_act)
                                    .show(ui, p)
                                    .clicked()
                                {
                                    self.report = None;
                                    self.start_job(ctx, runtime, settings, None);
                                }
                                if Button::new("Импорт файлов…", Kind::Ghost)
                                    .enabled(can_act)
                                    .show(ui, p)
                                    .clicked()
                                    && let Some(inputs) = rfd::FileDialog::new()
                                        .add_filter(
                                            "Моды и архивы",
                                            &["scs", "zip", "rar", "7z", "001", "002", "003"],
                                        )
                                        .pick_files()
                                {
                                    self.start_job(ctx, runtime, settings, Some(inputs));
                                }
                                let play = Button::new("Играть", if ready.is_some() { Kind::Primary } else { Kind::Ghost })
                                    .icon(Icon::Play)
                                    .weight(W::Bold)
                                    .enabled(can_act && ready.is_some())
                                    .show(ui, p);
                                if ready.is_none() {
                                    play.on_hover_text("Доступно, когда готовы все моды");
                                } else if play.clicked()
                                    && let Some(plan) = &plan
                                {
                                    let result = Self::library(settings).and_then(|l| {
                                        l.launch(
                                            plan,
                                            settings
                                                .directory(combo.game)
                                                .context("Укажите папку игры в настройках")?,
                                        )
                                    });
                                    match result {
                                        Ok(()) => self.inform("Игра запущена со сборкой Quper"),
                                        Err(e) => self.fail(format!("{e:#}")),
                                    }
                                }
                                if let Some(home) = &ready
                                    && Button::new("Папка профиля", Kind::Ghost)
                                        .icon(Icon::Folder)
                                        .show(ui, p)
                                        .clicked()
                                {
                                    let _ = open::that(home.join(combo.game.name()));
                                }
                            });
                            ui.add(
                                egui::Label::new(ui::text(
                                    if ready.is_some() {
                                        format!("Сборка запускается в отдельном профиле с порядком модов Quper. Нужны DLC карт и версия игры {}.", combo.game_version)
                                    } else {
                                        format!("«Играть» станет доступна, когда будут готовы все моды. Нужны DLC карт и версия игры {}. Сборка запускается в отдельном профиле.", combo.game_version)
                                    },
                                    12.0,
                                    W::Regular,
                                    p.muted,
                                ))
                                .wrap(),
                            );
                        },
                    );
                });
            });

        if self.job {
            self.job_card(ui, p);
        }
        if let Some(url) = self.browser_current.clone()
            && !self.job
        {
            egui::Frame::new()
                .fill(p.inset)
                .stroke(Stroke::new(1.0_f32, p.line))
                .corner_radius(12)
                .inner_margin(Margin::symmetric(18, 14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(ui::text(
                            format!(
                                "Очередь загрузки с сайтов авторов: ещё {} источников",
                                self.browser_queue.len()
                            ),
                            13.0,
                            W::Semi,
                            p.text2,
                        ));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            if Button::new("Завершить очередь", Kind::Ghost)
                                .height(40.0)
                                .size(13.0)
                                .show(ui, p)
                                .clicked()
                            {
                                self.next_browser();
                            }
                            if Button::new("Продолжить на сайте", Kind::Primary)
                                .height(40.0)
                                .size(13.0)
                                .show(ui, p)
                                .clicked()
                            {
                                self.browser_request = Some((url, "Файлы сборки".into()));
                            }
                        });
                    });
                });
        }
        if let Some((tone, note)) = self.note.clone()
            && ui::banner(ui, p, tone, &note, Some("Скрыть"))
        {
            self.note = None;
        }
        let Some(plan) = plan else {
            return;
        };
        let missing = state.map(|(missing, _)| missing).unwrap_or_default();
        let rows = plan
            .files
            .iter()
            .filter(|r| r.required)
            .map(|row| (row, file_state(row, &missing)))
            .collect::<Vec<_>>();
        let count = |f: FileFilter| rows.iter().filter(|(_, s)| *s == f).count();
        let imports = count(FileFilter::Import);
        if imports > 0 {
            let message = format!(
                "{imports} {} нужно импортировать вручную — у них нет публичной ссылки или ссылка ведёт на Boosty.",
                plural(imports, ["файл", "файла", "файлов"])
            );
            if ui::banner(ui, p, Tone::Warn, &message, Some("Показать")) {
                self.files = FileFilter::Import;
            }
        }
        if let Some(report) = self.report.clone()
            && !report.problems.is_empty()
        {
            let message = format!("Источники, требующие внимания: {}", report.problems.len());
            if ui::banner(
                ui,
                p,
                Tone::Warn,
                &message,
                Some(if self.show_problems {
                    "Скрыть"
                } else {
                    "Показать"
                }),
            ) {
                self.show_problems = !self.show_problems;
            }
            if self.show_problems {
                ui::inset(p)
                    .inner_margin(Margin::symmetric(18, 14))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 6.0;
                        for problem in &report.problems {
                            ui.add(
                                egui::Label::new(ui::text(
                                    format!("{}: ", problem.name),
                                    13.0,
                                    W::Bold,
                                    p.text2,
                                ))
                                .wrap(),
                            );
                            ui.add(
                                egui::Label::new(ui::text(
                                    &problem.message,
                                    12.0,
                                    W::Regular,
                                    p.muted,
                                ))
                                .wrap(),
                            );
                        }
                    });
            }
        }

        // Files.
        egui::Frame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.line))
            .corner_radius(16)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                let header = egui::Frame::new()
                    .inner_margin(Margin::symmetric(20, 16))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(ui::text("Файлы сборки", 18.0, W::Extra, p.text));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let filters = [
                                    (FileFilter::All, "Все", rows.len()),
                                    (FileFilter::Ready, "Готово", count(FileFilter::Ready)),
                                    (FileFilter::Pending, "Ожидают", count(FileFilter::Pending)),
                                    (FileFilter::Import, "Нужен импорт", imports),
                                ];
                                let segments = filters
                                    .iter()
                                    .map(|(_, label, n)| ui::Segment {
                                        label,
                                        extra: Some((n.to_string(), true, p.muted)),
                                        enabled: true,
                                    })
                                    .collect::<Vec<_>>();
                                let selected =
                                    filters.iter().position(|f| f.0 == self.files).unwrap_or(0);
                                if let Some(index) = ui::segmented(
                                    ui, p, selected, &segments, 34.0, 13.0, p.inset, 12,
                                ) {
                                    self.files = filters[index].0;
                                }
                            });
                        });
                    });
                ui.painter().hline(
                    header.response.rect.x_range(),
                    header.response.rect.bottom(),
                    Stroke::new(1.0_f32, p.line),
                );
                let visible = rows
                    .iter()
                    .filter(|(_, s)| self.files == FileFilter::All || *s == self.files)
                    .collect::<Vec<_>>();
                if visible.is_empty() {
                    egui::Frame::new()
                        .inner_margin(Margin::symmetric(20, 18))
                        .show(ui, |ui| {
                            ui.label(ui::text(
                                "В этой категории файлов нет.",
                                13.0,
                                W::Semi,
                                p.muted,
                            ));
                        });
                }
                for (row, state) in visible {
                    if self.file_row(ui, p, row, *state, idle)
                        && let Some(url) = row.urls.first()
                    {
                        self.browser_request = Some((url.clone(), row.name.clone()));
                    }
                }
            });

        let unmapped = missing
            .iter()
            .filter(|id| !plan.files.iter().any(|r| r.mod_id.eq_ignore_ascii_case(id)))
            .cloned()
            .collect::<Vec<_>>();
        if !unmapped.is_empty() {
            ui.add(
                egui::Label::new(ui::text(
                    format!(
                        "Есть в порядке Quper, но нет отдельной строки таблицы (могут находиться внутри общих архивов): {}",
                        unmapped.join(", ")
                    ),
                    12.0,
                    W::Regular,
                    p.muted,
                ))
                .wrap(),
            );
        }
    }

    fn job_card(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let transfer = download::snapshot(&self.transfer);
        ui::card(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 14.0;
            let stop = Button::new("Остановить", Kind::Danger)
                .icon(Icon::Stop)
                .pad(18.0)
                .enabled(!self.cancel.is_cancelled());
            let width = stop.width(ui);
            ui::split_row(
                ui,
                width,
                |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                        ui.label(ui::text("Загрузка сборки", 18.0, W::Extra, p.text));
                    });
                    ui.add(
                        egui::Label::new(ui::text(&transfer.message, 13.0, W::Regular, p.muted))
                            .wrap(),
                    );
                },
                |ui| {
                    if stop.show(ui, p).clicked() {
                        self.cancel.cancel();
                        self.browser_queue.clear();
                    }
                },
            );
            ui::progress(ui, p, transfer.progress(), 10.0, p.accent);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                if !transfer.name.is_empty() {
                    ui.label(ui::text(&transfer.name, 12.0, W::Semi, p.text2));
                }
                let total = transfer
                    .total
                    .map(format_bytes)
                    .unwrap_or_else(|| "…".into());
                ui.label(ui::mono_text(
                    format!("{} / {total}", format_bytes(transfer.downloaded)),
                    12.0,
                    false,
                    p.muted,
                ));
                ui.label(ui::mono_text(
                    format!("{}/с", format_bytes(transfer.speed)),
                    12.0,
                    false,
                    p.muted,
                ));
            });
        });
    }

    /// One file of the build; returns true when "Открыть источник" was pressed.
    fn file_row(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        row: &FileRow,
        state: FileFilter,
        idle: bool,
    ) -> bool {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            Stroke::new(1.0_f32, p.line_soft),
        );
        let inner = rect.shrink2(vec2(20.0, 0.0));
        let y = rect.center().y;
        ui.painter().text(
            pos2(inner.min.x, y),
            egui::Align2::LEFT_CENTER,
            format!("{:02}", row.number),
            ui::mono(12.0, false),
            p.muted,
        );
        let status_width = 220.0;
        let host_width = if inner.width() > 560.0 { 120.0 } else { 0.0 };
        let name_rect = Rect::from_min_max(
            pos2(inner.min.x + 56.0, rect.min.y),
            pos2(inner.max.x - status_width - host_width - 24.0, rect.max.y),
        );
        let name = ui
            .scope_builder(
                egui::UiBuilder::new()
                    .max_rect(name_rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.add(egui::Label::new(ui::text(&row.name, 14.0, W::Semi, p.text)).truncate())
                },
            )
            .inner;
        if !row.description.is_empty() {
            name.on_hover_text(&row.description);
        }
        if host_width > 0.0 {
            ui.painter().text(
                pos2(inner.max.x - status_width - host_width - 12.0, y),
                egui::Align2::LEFT_CENTER,
                row.urls
                    .first()
                    .map(|u| host_label(u))
                    .unwrap_or_else(|| "—".into()),
                ui::font(12.0, W::Regular),
                p.muted,
            );
        }
        let status = Rect::from_min_max(
            pos2(inner.max.x - status_width, rect.min.y),
            pos2(inner.max.x, rect.max.y),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(status)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| match state {
                FileFilter::Ready => {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(ui::text("Готов", 13.0, W::Bold, p.ok));
                    ui::icon(ui, Icon::Check, 14.0, p.ok, 3.0);
                    false
                }
                FileFilter::Pending => Button::new("Открыть источник", Kind::Ghost)
                    .trailing(Icon::External)
                    .height(36.0)
                    .size(13.0)
                    .pad(12.0)
                    .radius(8)
                    .enabled(idle)
                    .show(ui, p)
                    .clicked(),
                _ => {
                    ui::chip(
                        ui,
                        if row.excluded_boosty {
                            "Boosty исключён · нужен импорт"
                        } else {
                            "Нет публичной ссылки"
                        },
                        false,
                        p.warn,
                        p.warn_chip,
                        28.0,
                        12.0,
                    );
                    false
                }
            },
        )
        .inner
    }
}

fn file_state(row: &FileRow, missing: &[String]) -> FileFilter {
    if !missing.iter().any(|m| m.eq_ignore_ascii_case(&row.mod_id)) {
        FileFilter::Ready
    } else if row.urls.is_empty() {
        FileFilter::Import
    } else {
        FileFilter::Pending
    }
}

/// Pill on top of a cover ("Готова", "41 / 58 модов").
fn badge(
    ui: &egui::Ui,
    p: &Palette,
    cover: Rect,
    label: &str,
    color: egui::Color32,
    icon: Option<Icon>,
    mono: bool,
) {
    let font = if mono {
        ui::mono(11.0, true)
    } else {
        ui::font(11.0, W::Extra)
    };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
    let icon_width = if icon.is_some() { 18.0 } else { 0.0 };
    let rect = Rect::from_min_size(
        cover.min + vec2(10.0, 10.0),
        vec2(galley.size().x + icon_width + 20.0, 24.0),
    );
    ui.painter().rect_filled(rect, 12, p.bg);
    let mut x = rect.min.x + 10.0;
    if let Some(icon) = icon {
        ui::paint_icon(
            ui.painter(),
            Rect::from_center_size(pos2(x + 6.0, rect.center().y), Vec2::splat(12.0)),
            icon,
            color,
            3.0,
        );
        x += icon_width;
    }
    ui.painter().galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}

fn release_label(combo: &Combo) -> String {
    if combo.version.starts_with(['v', 'V']) {
        combo.version.clone()
    } else {
        format!("v{}", combo.version)
    }
}

fn host_label(url: &str) -> String {
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let host = host.trim_start_matches("www.");
    match host {
        "sharemods.com" => "Sharemods".into(),
        "modsfire.com" => "ModsFire".into(),
        "" => "—".into(),
        other => other.to_owned(),
    }
}

fn plural(n: usize, forms: [&'static str; 3]) -> &'static str {
    match (n % 10, n % 100) {
        (1, rest) if rest != 11 => forms[0],
        (2..=4, rest) if !(12..=14).contains(&rest) => forms[1],
        _ => forms[2],
    }
}

use anyhow::Context;
