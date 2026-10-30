use crate::{BG, GREEN, LINE, MUTED, PANEL, mod_browser};
use eframe::egui::{self, Color32, RichText, Vec2};
use ets_launcher::{
    download::{self, SharedTransfer, Status, Transfer},
    format_bytes,
    game::Game,
    maps,
    quper::{
        self, Catalog, Combo, Plan,
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

pub struct QuperUi {
    catalog: Option<Catalog>,
    plan: Option<Plan>,
    selected: Option<String>,
    version: String,
    search: String,
    refreshing: bool,
    loading_plan: bool,
    next_refresh: Instant,
    job: bool,
    transfer: SharedTransfer,
    cancel: CancellationToken,
    metadata_cancel: CancellationToken,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    note: Option<String>,
    report: Option<Report>,
    images: BTreeMap<String, egui::TextureHandle>,
    image_requests: BTreeSet<String>,
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
            version: String::new(),
            search: String::new(),
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
    fn preparing(&self) -> bool {
        self.busy()
            || self.browser_current.is_some()
            || !self.browser_queue.is_empty()
            || self.browser_request.is_some()
    }
    pub fn suspend_browser(&self) {
        self.browser.suspend();
    }
    pub fn cancel_all(&mut self) {
        self.stop_queue();
        self.metadata_cancel.cancel();
    }
    fn stop_queue(&mut self) {
        self.cancel.cancel();
        self.browser.cancel();
        self.browser_queue.clear();
        self.browser_request = None;
        self.browser_current = None;
        self.imports.clear();
    }
    fn root(settings: &Settings) -> PathBuf {
        settings
            .quper_library
            .clone()
            .unwrap_or_else(|| settings.output.join("Quper"))
    }
    fn library(settings: &Settings) -> anyhow::Result<Library> {
        Library::open(&Self::root(settings))
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
        self.selected = Some(id.into());
        self.plan = None;
        self.report = None;
        self.note = None;
        // Cached plans are tied to the exact release and date, never to old maps.
        if let Ok(path) = maps::config_path(&format!("quper-plan-{}.json", combo.id)) {
            self.plan = std::fs::read(path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Plan>(&b).ok())
                .filter(|p| {
                    p.validate().is_ok()
                        && p.combo.version == combo.version
                        && p.combo.date == combo.date
                });
        }
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
                self.note = Some(format!("{e:#}"));
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
                                self.note = Some(format!("Не удалось сохранить каталог: {e:#}"));
                            }
                            if let Some(selected) = self.combo()
                                && catalog.combos.iter().any(|c| {
                                    c.id == selected.id
                                        && (c.version != selected.version
                                            || c.date != selected.date)
                                })
                            {
                                self.selected = None;
                                self.plan = None;
                            }
                            self.catalog = Some(catalog);
                            self.ensure_version(settings.selected_game);
                        }
                        Ok(_) => self.next_refresh = Instant::now(),
                        Err(e) => {
                            self.note = Some(format!(
                                "Не удалось обновить Quper: {e}. Сохранённый каталог доступен."
                            ))
                        }
                    }
                }
                Event::Plan(id, result) if self.selected.as_ref() == Some(&id) => {
                    self.loading_plan = false;
                    match result {
                        Ok(plan) => {
                            if let Err(e) = maps::config_path(&format!("quper-plan-{id}.json"))
                                .and_then(|p| maps::write_json(&p, &plan))
                            {
                                self.note =
                                    Some(format!("Не удалось сохранить список файлов: {e:#}"));
                            }
                            self.plan = Some(plan);
                        }
                        Err(e) => {
                            self.note = Some(format!("Не удалось обновить список файлов: {e}"))
                        }
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
                    match result {
                        Ok(report) => {
                            if report.home.is_some() {
                                self.note = Some("Сборка готова. Порядок модов от Quper применён к отдельному профилю.".into());
                                self.browser.cancel();
                                self.browser_queue.clear();
                                self.browser_current = None;
                                self.browser_request = None;
                            } else {
                                self.note = Some(format!(
                                    "Подготовлено {} из {} модов. Недостающие файлы показаны в списке.",
                                    self.plan.as_ref().map_or(0, |p| p
                                        .order
                                        .len()
                                        .saturating_sub(report.missing.len())),
                                    self.plan.as_ref().map_or(0, |p| p.order.len())
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
                        Err(e) => self.note = Some(e),
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
                mod_browser::Event::Failed(e) => self.note = Some(e),
                mod_browser::Event::Started(id) => {
                    self.note = Some(format!("Скачивание файла сборки {id} со страницы автора…"))
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

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        runtime: &Runtime,
        settings: &mut Settings,
        dlc_busy: bool,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.heading(RichText::new("Сборки Quper").size(30.0));
            ui.hyperlink_to("qupersimulator.ru ↗", quper::SITE);
            if ui
                .add_enabled(
                    !self.refreshing && !self.preparing(),
                    egui::Button::new("Обновить каталог"),
                )
                .clicked()
            {
                self.refresh(ctx, runtime);
            }
            if self.refreshing {
                ui.spinner();
            }
        });
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
                    self.note = Some(format!("{e:#}"));
                    self.browser_queue.clear();
                }
            }
        }
        if self.browser.visible {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "В очереди ещё {} источников",
                    self.browser_queue.len()
                ));
                if ui
                    .add_enabled(!self.busy(), egui::Button::new("Следующий источник"))
                    .clicked()
                {
                    self.next_browser();
                }
                if ui.button("Остановить очередь").clicked() {
                    self.stop_queue();
                }
            });
            if self.job {
                ui.spinner();
                ui.label("Проверка и подключение скачанного файла…");
            }
            if let Some(note) = &self.note {
                ui.label(RichText::new(note).color(MUTED));
            }
            self.browser.show(ui);
            return;
        }
        ui.label(
            RichText::new(
                "Готовые сборки ETS2 и ATS: обложки, версии и списки файлов с сайта Quper.",
            )
            .color(MUTED),
        );
        ui.add_space(8.0);
        let previous_game = settings.selected_game;
        ui.add_enabled_ui(!self.preparing() && !dlc_busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(
                    &mut settings.selected_game,
                    Game::Ets2,
                    "Euro Truck Simulator 2",
                );
                ui.selectable_value(
                    &mut settings.selected_game,
                    Game::Ats,
                    "American Truck Simulator",
                );
                let old_version = self.version.clone();
                egui::ComboBox::from_id_salt("quper-version")
                    .selected_text(format!("Версия игры {}", self.version))
                    .show_ui(ui, |ui| {
                        for version in self.versions(settings.selected_game) {
                            ui.selectable_value(&mut self.version, version.clone(), &version);
                        }
                    });
                if self.version != old_version {
                    self.selected = None;
                    self.plan = None;
                    self.report = None;
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Поиск сборки")
                        .desired_width(190.0),
                );
            });
        });
        if previous_game != settings.selected_game {
            self.version.clear();
            self.ensure_version(settings.selected_game);
            self.selected = None;
            self.plan = None;
            self.report = None;
            let _ = settings.save();
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("Папка сборок: {}", Self::root(settings).display()))
                    .size(12.0)
                    .color(MUTED),
            );
            if ui
                .add_enabled(!self.preparing(), egui::Button::new("Выбрать папку…"))
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("Папка готовых сборок Quper")
                    .pick_folder()
            {
                settings.quper_library = Some(path);
                if let Err(e) = settings.save() {
                    self.note = Some(format!("{e:#}"));
                }
                self.report = None;
            }
        });
        if let Some(note) = &self.note {
            ui.label(RichText::new(note).color(Color32::from_rgb(230, 194, 123)));
        }
        if self.job {
            let transfer = download::snapshot(&self.transfer);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(&transfer.message);
            });
            ui.label(format!(
                "{} · {} · {}/с",
                transfer.name,
                format_bytes(transfer.downloaded),
                format_bytes(transfer.speed)
            ));
            ui.add(egui::ProgressBar::new(transfer.progress()).animate(true));
            if ui.button("Остановить скачивание").clicked() {
                self.cancel.cancel();
                self.browser_queue.clear();
            }
        }
        if let Some(url) = self.browser_current.clone() {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!self.job, egui::Button::new("Продолжить на сайте"))
                    .clicked()
                {
                    self.browser_request = Some((url, "Файлы сборки".into()));
                }
                if ui
                    .add_enabled(!self.job, egui::Button::new("Завершить очередь"))
                    .clicked()
                {
                    self.next_browser();
                }
            });
        }
        ui.add_space(10.0);
        if self.selected.is_some() {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.preparing(), egui::Button::new("← Все сборки"))
                    .clicked()
                {
                    self.selected = None;
                    self.plan = None;
                    self.report = None;
                }
            });
            if let Some(combo) = self.combo().cloned() {
                self.details(ui, ctx, runtime, settings, dlc_busy, &combo);
            }
        } else {
            let needle = self.search.to_lowercase();
            let combos = self
                .catalog
                .iter()
                .flat_map(|c| &c.combos)
                .filter(|c| {
                    c.game == settings.selected_game
                        && c.game_version == self.version
                        && c.title.to_lowercase().contains(&needle)
                })
                .cloned()
                .collect::<Vec<_>>();
            if combos.is_empty() {
                ui.label(if self.refreshing {
                    "Загрузка каталога Quper…"
                } else {
                    "Сборки не найдены. Обновите каталог или измените фильтры."
                });
            }
            let columns = if ui.available_width() >= 850.0 {
                3
            } else if ui.available_width() >= 530.0 {
                2
            } else {
                1
            };
            for row in combos.chunks(columns) {
                ui.columns(columns, |cols| {
                    for (i, combo) in row.iter().enumerate() {
                        self.request_image(combo, ctx, runtime);
                        egui::Frame::new()
                            .fill(PANEL)
                            .stroke(egui::Stroke::new(1.0_f32, LINE))
                            .corner_radius(10)
                            .inner_margin(12)
                            .show(&mut cols[i], |ui| {
                                let width = ui.available_width();
                                if let Some(image) = self.images.get(&combo.preview) {
                                    ui.add(
                                        egui::Image::new(image).fit_to_exact_size(Vec2::new(
                                            width,
                                            width * 9.0 / 16.0,
                                        )),
                                    );
                                } else {
                                    let (rect, _) = ui.allocate_exact_size(
                                        Vec2::new(width, width * 9.0 / 16.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(rect, 6.0, BG);
                                    ui.painter().text(
                                        rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        combo.game.short_name(),
                                        egui::FontId::proportional(24.0),
                                        MUTED,
                                    );
                                }
                                ui.label(RichText::new(&combo.title).strong().size(17.0));
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {}-{}",
                                        combo.game.short_name(),
                                        combo.game_version,
                                        combo.version
                                    ))
                                    .color(GREEN),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "Обновлено: {}{}",
                                        combo.date,
                                        if combo.convoy { " · Конвой" } else { "" }
                                    ))
                                    .size(12.0)
                                    .color(MUTED),
                                );
                                if ui
                                    .add_enabled(
                                        !self.preparing() && !dlc_busy,
                                        egui::Button::new("Выбрать сборку")
                                            .min_size(Vec2::new(width, 36.0)),
                                    )
                                    .clicked()
                                {
                                    self.select(&combo.id, ctx, runtime);
                                }
                            });
                    }
                });
                ui.add_space(8.0);
            }
        }
    }

    fn details(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        runtime: &Runtime,
        settings: &Settings,
        dlc_busy: bool,
        combo: &Combo,
    ) {
        self.request_image(combo, ctx, runtime);
        ui.horizontal(|ui| {
            if let Some(image) = self.images.get(&combo.preview) {
                ui.add(egui::Image::new(image).fit_to_exact_size(Vec2::new(250.0, 140.625)));
            }
            ui.vertical(|ui| {
                ui.heading(&combo.title);
                ui.label(
                    RichText::new(format!(
                        "{} · {}-{}",
                        combo.game.short_name(),
                        combo.game_version,
                        combo.version
                    ))
                    .color(GREEN),
                );
                ui.label(format!("Обновлено: {}", combo.date));
                ui.hyperlink_to("Страница сборки ↗", &combo.page);
                ui.hyperlink_to("Список файлов Quper ↗", &combo.table);
            });
        });
        if self.loading_plan {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Читаем список файлов и порядок модов…");
            });
        }
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let library = Self::library(settings);
        let missing = library
            .as_ref()
            .ok()
            .and_then(|l| l.missing(&plan).ok())
            .unwrap_or_else(|| plan.order.clone());
        let ready = library
            .as_ref()
            .ok()
            .and_then(|l| l.ready_home(&plan).ok())
            .flatten();
        ui.label(format!(
            "Модов готово: {} / {}",
            plan.order.len().saturating_sub(missing.len()),
            plan.order.len()
        ));
        ui.label(RichText::new("Нужны DLC карт и указанная версия игры. Сборка использует отдельный профиль с порядком Quper.").color(MUTED).size(12.0));
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.preparing() && !dlc_busy && !self.loading_plan,
                    egui::Button::new(
                        RichText::new(if ready.is_some() {
                            "Проверить / продолжить"
                        } else {
                            "Скачать сборку"
                        })
                        .color(GREEN),
                    ),
                )
                .clicked()
            {
                self.report = None;
                self.start_job(ctx, runtime, settings, None);
            }
            if ui
                .add_enabled(
                    !self.preparing() && !dlc_busy && !self.loading_plan,
                    egui::Button::new("Импорт файлов…"),
                )
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
            if ui
                .add_enabled(
                    !self.preparing() && !dlc_busy && !self.loading_plan && ready.is_some(),
                    egui::Button::new("Играть"),
                )
                .clicked()
            {
                let result = library
                    .as_ref()
                    .map_err(|e| anyhow::anyhow!("{e:#}"))
                    .and_then(|l| {
                        l.launch(
                            &plan,
                            settings
                                .directory(combo.game)
                                .context("Укажите папку игры в разделе DLC и настроек")?,
                        )
                    });
                self.note = Some(match result {
                    Ok(()) => "Игра запущена со сборкой Quper".into(),
                    Err(e) => format!("{e:#}"),
                });
            }
            if let Some(home) = &ready
                && ui.button("Папка профиля ↗").clicked()
            {
                let _ = open::that(home.join(combo.game.name()));
            }
        });
        if let Some(report) = &self.report
            && !report.problems.is_empty()
        {
            ui.collapsing(
                format!("Источники, требующие внимания: {}", report.problems.len()),
                |ui| {
                    for problem in &report.problems {
                        ui.label(format!("{}: {}", problem.name, problem.message));
                    }
                },
            );
        }
        ui.separator();
        ui.heading("Файлы сборки");
        for row in &plan.files {
            let present = !missing.iter().any(|m| m.eq_ignore_ascii_case(&row.mod_id));
            if !row.required {
                continue;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(if present { "✓" } else { "○" }).color(if present {
                        GREEN
                    } else {
                        MUTED
                    }),
                );
                ui.label(format!("{}. {}", row.number, row.name))
                    .on_hover_text(&row.description);
                if !present {
                    if let Some(url) = row.urls.first() {
                        if ui
                            .add_enabled(
                                !self.preparing() && !dlc_busy && !self.loading_plan,
                                egui::Button::new("Открыть источник"),
                            )
                            .clicked()
                        {
                            self.browser_request = Some((url.clone(), row.name.clone()));
                        }
                    } else {
                        ui.label(
                            RichText::new(if row.excluded_boosty {
                                "Boosty исключён · нужен импорт"
                            } else {
                                "Нет публичной ссылки · нужен импорт"
                            })
                            .size(12.0)
                            .color(MUTED),
                        );
                    }
                }
            });
        }
        let unmapped = missing
            .iter()
            .filter(|id| !plan.files.iter().any(|r| r.mod_id.eq_ignore_ascii_case(id)))
            .collect::<Vec<_>>();
        if !unmapped.is_empty() {
            ui.label(RichText::new(format!("Есть в порядке Quper, но нет отдельной строки таблицы (могут находиться внутри общих архивов): {}", unmapped.into_iter().cloned().collect::<Vec<_>>().join(", "))).size(12.0).color(MUTED));
        }
    }
}

use anyhow::Context;
