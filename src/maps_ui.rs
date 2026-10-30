use crate::mod_browser::{self, Browser};
use anyhow::{Context, Result};
use eframe::egui::{self, Color32, RichText};
use ets_launcher::{
    download::{self, SharedTransfer, Status, Transfer},
    format_bytes,
    maps::{
        self, Catalog, ModSet, SourceState,
        library::{self, ActiveState, Library},
        source,
    },
    settings::Settings,
};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

const GREEN: Color32 = Color32::from_rgb(104, 224, 164);
const MUTED: Color32 = Color32::from_rgb(143, 160, 175);
const AMBER: Color32 = Color32::from_rgb(255, 184, 120);

#[derive(Serialize, Deserialize)]
struct CatalogSnapshot {
    url: String,
    catalog: Catalog,
}

enum Event {
    Refreshed {
        generation: u64,
        snapshot: Box<CatalogSnapshot>,
        sources: BTreeMap<String, SourceState>,
        error: Option<String>,
    },
    FailedRefresh(u64, String),
    Applied(Box<ActiveState>),
    NeedBrowser(Box<source::PreparedDownload>),
    Imported(String),
    ImportFailed(String),
    Failed(String),
}

pub struct MapsUi {
    atlas: crate::maps_atlas::Atlas,
    view: usize,
    catalog: Option<Catalog>,
    sources: BTreeMap<String, SourceState>,
    sets: Vec<ModSet>,
    selected: usize,
    pub note: Option<String>,
    detail: Option<String>,
    search: String,
    filter: usize,
    sort: usize,
    refreshing: bool,
    generation: u64,
    next_refresh: Instant,
    refresh_cancel: CancellationToken,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    job: bool,
    cancel: CancellationToken,
    pending: Option<ModSet>,
    browser_request: Option<String>,
    browser_download_url: Option<String>,
    browser_prepared: Option<source::PreparedDownload>,
    imports: Vec<(String, Vec<PathBuf>)>,
    browser: Browser,
    browser_sender: mpsc::Sender<mod_browser::Event>,
    browser_receiver: mpsc::Receiver<mod_browser::Event>,
    transfer: SharedTransfer,
    source_editor: String,
    active: Option<ActiveState>,
    detected_versions: std::collections::BTreeSet<String>,
}

fn new_set() -> ModSet {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    ModSet {
        id: format!("set-{stamp:x}"),
        name: "Моя карта ETS2".into(),
        selected: Default::default(),
        game_version: String::new(),
    }
}

impl MapsUi {
    fn live_catalog(&self) -> Option<Catalog> {
        self.catalog.as_ref().map(|c| c.with_sources(&self.sources))
    }
    pub fn new(settings: &Settings) -> Self {
        let url = settings
            .mod_catalog_source
            .as_deref()
            .unwrap_or(maps::CATALOG_URL);
        let cached = maps::config_path("maps-catalog.json")
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<CatalogSnapshot>(&b).ok())
            .filter(|c| c.url == url && c.catalog.validate().is_ok());
        let bundled = Catalog::bundled();
        let loaded = match (cached, bundled) {
            (Some(cached), Ok(bundled))
                if settings.mod_catalog_source.is_none()
                    && bundled.revision > cached.catalog.revision =>
            {
                Ok(bundled)
            }
            (Some(cached), _) => Ok(cached.catalog),
            (None, bundled) => bundled,
        };
        let note = loaded.as_ref().err().map(|e| format!("{e:#}"));
        let sources = maps::config_path("maps-sources.json")
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let sets: Vec<ModSet> = maps::config_path("mod-sets.json")
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| vec![new_set()]);
        let (sender, receiver) = mpsc::channel();
        let (browser_sender, browser_receiver) = mpsc::channel();
        let mut this = Self {
            atlas: Default::default(),
            view: 0,
            catalog: loaded.ok(),
            sources,
            sets: if sets.is_empty() {
                vec![new_set()]
            } else {
                sets
            },
            selected: 0,
            note,
            detail: None,
            search: String::new(),
            filter: 0,
            sort: 0,
            refreshing: false,
            generation: 0,
            next_refresh: Instant::now(),
            refresh_cancel: CancellationToken::new(),
            sender,
            receiver,
            job: false,
            cancel: CancellationToken::new(),
            pending: None,
            browser_request: None,
            browser_download_url: None,
            browser_prepared: None,
            imports: Vec::new(),
            browser: Browser::default(),
            browser_sender,
            browser_receiver,
            transfer: Arc::new(Mutex::new(Transfer::default())),
            source_editor: url.into(),
            active: None,
            detected_versions: Default::default(),
        };
        this.reload_active(settings);
        this
    }

    fn save_sets(&mut self) {
        if let Err(e) =
            maps::config_path("mod-sets.json").and_then(|p| maps::write_json(&p, &self.sets))
        {
            self.note = Some(format!("Не удалось сохранить сборки: {e:#}"));
        }
    }

    fn library(settings: &Settings) -> Result<Library> {
        Library::open(
            settings
                .mod_library
                .as_deref()
                .context("Выберите папку библиотеки модов")?,
        )
    }

    fn reload_active(&mut self, settings: &Settings) {
        self.active = settings
            .mod_library
            .as_ref()
            .and_then(|p| Library::open(p).ok())
            .and_then(|l| l.active(&self.sets[self.selected].id).ok());
    }

    pub fn busy(&self) -> bool {
        self.job || self.browser.downloading()
    }

    pub fn suspend_browser(&self) {
        self.browser.suspend();
    }

    pub fn cancel_all(&mut self) {
        self.cancel.cancel();
        self.refresh_cancel.cancel();
        self.pending = None;
        self.imports.clear();
        self.browser_request = None;
        self.browser_download_url = None;
        self.browser_prepared = None;
        self.browser.cancel();
    }

    fn refresh(&mut self, ctx: &egui::Context, runtime: &Runtime, settings: &Settings) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.refresh_cancel = CancellationToken::new();
        let cancel = self.refresh_cancel.clone();
        let tx = self.sender.clone();
        let ctx = ctx.clone();
        let current = self.catalog.clone();
        let url = settings
            .mod_catalog_source
            .clone()
            .unwrap_or_else(|| maps::CATALOG_URL.into());
        let old = self.sources.clone();
        runtime.spawn(async move {
            let result = source::fetch_catalog(&url, &cancel).await;
            let error = result.as_ref().err().map(|_| {
                "Правила стыковок: используется локальная копия каталога. Данные авторов проверяются отдельно.".to_owned()
            });
            let catalog = result.ok().or(current);
            if let Some(catalog) = catalog {
                let sources = stream::iter(catalog.maps.clone())
                    .map(|entry| {
                        let cancel = cancel.clone();
                        let previous = old.get(&entry.id).cloned();
                        async move {
                            let state = match source::fetch(&entry, &cancel).await {
                                Ok(state) => state,
                                Err(e) => SourceState {
                                    error: Some(format!("{e:#}")),
                                    ..previous.unwrap_or_default()
                                },
                            };
                            (entry.id, state)
                        }
                    })
                    .buffer_unordered(3)
                    .collect::<BTreeMap<_, _>>()
                    .await;
                let _ = tx.send(Event::Refreshed {
                    generation,
                    snapshot: Box::new(CatalogSnapshot { url, catalog }),
                    sources,
                    error,
                });
            } else {
                let _ = tx.send(Event::FailedRefresh(
                    generation,
                    error.unwrap_or_else(|| "Нет каталога карт".into()),
                ));
            }
            ctx.request_repaint();
        });
    }

    pub fn tick(
        &mut self,
        ctx: &egui::Context,
        runtime: &Runtime,
        settings: &Settings,
        dlc_busy: bool,
    ) {
        let set = &mut self.sets[self.selected];
        if set.game_version.is_empty()
            && settings.game_dir.is_some()
            && self.detected_versions.insert(set.id.clone())
            && let Some(version) = settings.game_dir.as_deref().and_then(library::game_version)
        {
            set.game_version = version;
            self.save_sets();
        }
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::Refreshed { generation, .. }
                    if generation == self.generation
                        && (self.busy() || self.pending.is_some() || self.browser.visible) =>
                {
                    // Keep the release snapshot stable throughout browser/DEF
                    // downloads and import; refresh again after the transaction.
                    self.refreshing = false;
                    self.next_refresh = Instant::now();
                }
                Event::Refreshed {
                    generation,
                    snapshot,
                    sources,
                    error,
                } if generation == self.generation => {
                    self.refreshing = false;
                    self.next_refresh = Instant::now() + Duration::from_secs(maps::REFRESH_SECONDS);
                    self.catalog = Some(snapshot.catalog.clone());
                    self.sources = sources;
                    self.note = error;
                    if let Err(e) = maps::config_path("maps-catalog.json")
                        .and_then(|p| maps::write_json(&p, &snapshot))
                    {
                        self.note = Some(e.to_string());
                    }
                    if let Err(e) = maps::config_path("maps-sources.json")
                        .and_then(|p| maps::write_json(&p, &self.sources))
                    {
                        self.note = Some(e.to_string());
                    }
                }
                Event::FailedRefresh(g, e) if g == self.generation => {
                    self.refreshing = false;
                    self.note = Some(e);
                    self.next_refresh = Instant::now() + Duration::from_secs(60);
                }
                Event::Applied(active) => {
                    self.job = false;
                    self.pending = None;
                    self.sets[self.selected] = active.set.clone();
                    self.active = Some(*active);
                    self.save_sets();
                    self.note =
                        Some("Сборка применена. Профиль готов к запуску через «Играть».".into());
                }
                Event::NeedBrowser(prepared) => {
                    self.job = false;
                    self.browser_request = Some(prepared.entry.id.clone());
                    self.browser_download_url = Some(prepared.page.clone());
                    self.note = Some(prepared.message.clone());
                    self.browser_prepared = Some(*prepared);
                }
                Event::Imported(name) => {
                    self.job = false;
                    self.browser.hide();
                    self.note = Some(format!("{name}: пакеты проверены и добавлены в библиотеку"));
                }
                Event::ImportFailed(e) => {
                    self.job = false;
                    self.note = Some(e);
                }
                Event::Failed(e) => {
                    self.job = false;
                    self.note = Some(e);
                    self.pending = None;
                }
                _ => {}
            }
        }
        while let Ok(event) = self.browser_receiver.try_recv() {
            match event {
                mod_browser::Event::Started(id) => {
                    self.note = Some(format!("Скачивание {id} во встроенном браузере…"))
                }
                mod_browser::Event::Complete(id, path) => self.imports.push((id, vec![path])),
                mod_browser::Event::Failed(e) => self.note = Some(e),
                mod_browser::Event::Navigate(url) => self.browser.navigate(&url),
                mod_browser::Event::Location(url) => self.browser.url = url,
                mod_browser::Event::PopupRequested(url) => self.browser.popup_requested(url),
            }
        }
        if !self.job && !self.browser.downloading() && !dlc_busy && !self.imports.is_empty() {
            let (id, inputs) = self.imports.remove(0);
            self.import(runtime, ctx, settings, &id, inputs);
        } else if !self.busy()
            && !dlc_busy
            && self.pending.is_some()
            && self.browser_request.is_none()
            && !self.browser.visible
        {
            let set = self.pending.clone().unwrap();
            self.apply(runtime, ctx, settings, set);
        }
        if !self.busy()
            && self.pending.is_none()
            && !dlc_busy
            && Instant::now() >= self.next_refresh
        {
            self.refresh(ctx, runtime, settings);
        }
    }

    fn begin_job(&mut self, message: &str) {
        self.job = true;
        self.cancel = CancellationToken::new();
        self.note = None;
        download::set_state(&self.transfer, |s| {
            *s = Transfer {
                status: Status::Connecting,
                message: message.into(),
                ..Default::default()
            }
        });
    }

    fn import(
        &mut self,
        runtime: &Runtime,
        ctx: &egui::Context,
        settings: &Settings,
        id: &str,
        inputs: Vec<PathBuf>,
    ) {
        let result = Self::library(settings).and_then(|l| {
            Ok((
                l,
                self.live_catalog()
                    .as_ref()
                    .context("Нет каталога")?
                    .entry(id)?
                    .clone(),
            ))
        });
        let Ok((library, entry)) = result else {
            self.note = result.err().map(|e| format!("{e:#}"));
            return;
        };
        self.begin_job("Проверка и импорт пакетов");
        let tx = self.sender.clone();
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        let state = self.transfer.clone();
        runtime.spawn(async move {
            download::set_state(&state, |s| {
                s.status = Status::Extracting;
                s.name = entry.name.clone();
            });
            let result: Result<_> = async {
                let mut archives = Vec::new();
                for path in inputs {
                    if path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("torrent"))
                    {
                        let bytes = std::fs::read(path)?;
                        download::run(
                            download::Source::TorrentBytes(bytes),
                            library.incoming(&entry)?,
                            state.clone(),
                            cancel.clone(),
                        )
                        .await;
                        let transfer = download::snapshot(&state);
                        anyhow::ensure!(
                            transfer.status == Status::Complete,
                            "{}",
                            transfer.message
                        );
                        archives.push(transfer.saved_to.context("Торрент не вернул архив")?);
                    } else {
                        archives.push(path);
                    }
                }
                library::import_downloaded(&library, &entry, &archives, &state, &cancel).await
            }
            .await;
            let event = match result {
                Ok(_) => Event::Imported(entry.name),
                Err(e) => Event::ImportFailed(format!("{e:#}")),
            };
            let _ = tx.send(event);
            ctx.request_repaint();
        });
    }

    fn apply(
        &mut self,
        runtime: &Runtime,
        ctx: &egui::Context,
        settings: &Settings,
        mut set: ModSet,
    ) {
        let result: Result<_> = (|| {
            let library = Self::library(settings)?;
            let catalog = self.live_catalog().context("Нет каталога")?;
            let game = settings
                .game_dir
                .clone()
                .context("Выберите папку ETS2 в разделе DLC → Настройки")?;
            let plan = catalog.resolve(&set, &library::installed_dlcs(&game))?;
            if let Some(version) = library::game_version(&game) {
                anyhow::ensure!(
                    maps::version_matches(&version, &set.game_version),
                    "ETS2 {version} не соответствует версии сборки {}",
                    set.game_version
                );
            }
            Ok((library, catalog, plan))
        })();
        let Ok((library, catalog, plan)) = result else {
            self.note = result.err().map(|e| format!("{e:#}"));
            self.pending = None;
            return;
        };
        if let Some(active) = &self.active
            && active.module != plan.module
        {
            let mut fresh = new_set();
            fresh.name = format!(
                "{} — новая карта",
                set.name.chars().take(20).collect::<String>()
            );
            fresh.selected = set.selected.clone();
            fresh.game_version = set.game_version.clone();
            self.sets[self.selected] = active.set.clone();
            self.sets.push(fresh.clone());
            self.selected = self.sets.len() - 1;
            self.active = None;
            set = fresh;
        }
        self.pending = Some(set.clone());
        self.save_sets();
        self.begin_job("Проверяем сборку и источники…");
        let tx = self.sender.clone();
        let ctx = ctx.clone();
        let cancel = self.cancel.clone();
        let transfer = self.transfer.clone();
        runtime.spawn(async move {
            let result: Result<Option<ActiveState>> = async {
                for id in &plan.ordered {
                    let entry = catalog.entry(id)?;
                    if library.record(id, &entry.release.version).is_ok() {
                        continue;
                    }
                    download::set_state(&transfer, |s| {
                        s.message = format!("Проверяем {}", entry.name);
                        s.name = entry.name.clone();
                    });
                    let prepared = source::prepare(entry, &cancel).await?;
                    if prepared.entry.release.downloads.is_empty() {
                        let _ = tx.send(Event::NeedBrowser(Box::new(prepared)));
                        return Ok(None);
                    }
                    download::set_state(&transfer, |s| s.message = prepared.message);
                    library::download_packages(&library, &prepared.entry, &transfer, &cancel)
                        .await?;
                }
                download::set_state(&transfer, |s| {
                    s.status = Status::Installing;
                    s.message = "Подключение пакетов и подготовка профиля".into();
                });
                let active = tokio::task::spawn_blocking(move || {
                    library.apply(&catalog, &set, &plan, &cancel)
                })
                .await??;
                Ok(Some(active))
            }
            .await;
            match result {
                Ok(Some(active)) => {
                    let _ = tx.send(Event::Applied(Box::new(active)));
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = tx.send(Event::Failed(format!("{e:#}")));
                }
            }
            ctx.request_repaint();
        });
    }

    fn rollback(&mut self, runtime: &Runtime, ctx: &egui::Context, settings: &Settings) {
        let library = match Self::library(settings) {
            Ok(l) => l,
            Err(e) => {
                self.note = Some(e.to_string());
                return;
            }
        };
        let id = self.sets[self.selected].id.clone();
        self.begin_job("Восстановление предыдущей сборки");
        let tx = self.sender.clone();
        let ctx = ctx.clone();
        let cancel = self.cancel.clone();
        runtime.spawn_blocking(move || {
            let event = match library.rollback(&id, &cancel) {
                Ok(s) => Event::Applied(Box::new(s)),
                Err(e) => Event::Failed(format!("{e:#}")),
            };
            let _ = tx.send(event);
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
        if let Some(id) = self.browser_request.take() {
            let result = Self::library(settings).and_then(|l| {
                let mut entry = self
                    .live_catalog()
                    .as_ref()
                    .context("Нет каталога")?
                    .entry(&id)?
                    .clone();
                if let Some(url) = self.browser_download_url.take() {
                    entry.source.browser_url = Some(url);
                }
                self.browser.open(
                    frame,
                    &entry,
                    l.incoming(&entry)?,
                    self.browser_sender.clone(),
                    self.browser_prepared.take().as_ref(),
                )
            });
            if let Err(e) = result {
                self.note = Some(format!("{e:#}"));
                self.pending = None;
            }
        }
        if self.browser.visible {
            self.browser.show(ui);
            return;
        }
        ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
        ui.heading(RichText::new("Карты ETS2").size(30.0));
        ui.label(
            RichText::new("Общая библиотека · отдельные профили · автоматические зависимости")
                .color(MUTED),
        );
        ui.add_space(10.0);
        let idle = !self.busy() && !dlc_busy;
        ui.horizontal_wrapped(|ui| {
            ui.label("Библиотека:");
            ui.label(
                RichText::new(
                    settings
                        .mod_library
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "папка не выбрана".into()),
                )
                .small(),
            );
            if ui
                .add_enabled(
                    idle && self.pending.is_none(),
                    egui::Button::new("Выбрать папку…"),
                )
                .clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                match Library::open(&path) {
                    Ok(l) => {
                        settings.mod_library = Some(l.root);
                        if let Err(e) = settings.save() {
                            self.note = Some(e.to_string());
                        }
                        self.reload_active(settings);
                    }
                    Err(e) => self.note = Some(format!("{e:#}")),
                }
            }
        });
        let mut changed = false;
        let mut selection = self.selected;
        ui.add_enabled_ui(idle && self.pending.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("mod-set")
                    .selected_text(&self.sets[self.selected].name)
                    .show_ui(ui, |ui| {
                        for (i, s) in self.sets.iter().enumerate() {
                            ui.selectable_value(&mut selection, i, &s.name);
                        }
                    });
                if ui.button("+ Сборка").clicked() {
                    self.sets.push(new_set());
                    selection = self.sets.len() - 1;
                    changed = true;
                }
                if selection != self.selected {
                    self.selected = selection;
                    self.reload_active(settings);
                }
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.sets[self.selected].name)
                            .hint_text("Название сборки")
                            .desired_width(180.0),
                    )
                    .changed();
                ui.label("ETS2:");
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.sets[self.selected].game_version)
                            .hint_text("1.61")
                            .desired_width(65.0),
                    )
                    .changed();
                if ui.button("Определить").clicked() {
                    self.sets[self.selected].game_version = settings
                        .game_dir
                        .as_ref()
                        .and_then(|p| detect_version(p))
                        .unwrap_or_default();
                    if self.sets[self.selected].game_version.is_empty() {
                        self.note = Some(
                            "Версия ETS2 не определена. Введите её с главного экрана игры.".into(),
                        );
                    }
                    changed = true;
                }
            });
        });
        let Some(catalog) = self.live_catalog() else {
            ui.colored_label(AMBER, self.note.as_deref().unwrap_or("Нет каталога"));
            return;
        };
        let dlcs = settings
            .game_dir
            .as_ref()
            .map(|p| library::installed_dlcs(p))
            .unwrap_or_default();
        let plan = catalog.resolve(&self.sets[self.selected], &dlcs);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    idle && settings.mod_library.is_some() && plan.is_ok(),
                    egui::Button::new(RichText::new("Применить / обновить сборку").color(GREEN)),
                )
                .clicked()
            {
                self.apply(runtime, ctx, settings, self.sets[self.selected].clone());
            }
            if ui
                .add_enabled(idle && self.active.is_some(), egui::Button::new("Играть"))
                .clicked()
            {
                let r = Self::library(settings).and_then(|l| {
                    l.launch(
                        &self.sets[self.selected].id,
                        settings.game_dir.as_deref().context("Не найдена ETS2")?,
                    )
                });
                if let Err(e) = r {
                    self.note = Some(format!("{e:#}"));
                }
            }
            if ui
                .add_enabled(idle && self.active.is_some(), egui::Button::new("Откатить"))
                .clicked()
            {
                self.rollback(runtime, ctx, settings);
            }
            if ui
                .add_enabled(
                    idle && !self.refreshing && self.pending.is_none(),
                    egui::Button::new("Проверить обновления"),
                )
                .clicked()
            {
                self.refresh(ctx, runtime, settings);
            }
            if self.refreshing {
                ui.spinner();
                ui.label(RichText::new("Проверяем авторов…").small().color(MUTED));
            }
        });
        if !self.sets[self.selected].selected.is_empty()
            && let Err(e) = &plan
        {
            ui.colored_label(AMBER, format!("{e:#}"));
        }
        if let Some(active) = &self.active {
            ui.label(
                RichText::new(format!(
                    "Профиль: {} · {} · обновления применяются по кнопке",
                    active.profile_name, active.module
                ))
                .small()
                .color(MUTED),
            );
        }
        if self.busy() {
            let t = download::snapshot(&self.transfer);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("{} · {}", t.name, t.message));
                if ui.button("Остановить").clicked() {
                    self.cancel_all();
                }
            });
            if t.total.is_some() {
                ui.add(egui::ProgressBar::new(t.progress()).show_percentage());
            }
        } else if self.pending.is_some() && ui.button("Отменить подготовку сборки").clicked()
        {
            self.cancel_all();
        }
        if let Some(note) = &self.note {
            ui.add(egui::Label::new(RichText::new(note).color(AMBER).small()).wrap());
        }
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.view, 0, "Карта регионов");
            ui.selectable_value(&mut self.view, 1, "Таблица");
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Поиск карты, региона или автора…")
                    .desired_width(270.0),
            );
            egui::ComboBox::from_id_salt("map-filter")
                .selected_text(
                    [
                        "Все карты",
                        "Выбранные",
                        "Установленные",
                        "Обновления",
                        "Зависимости",
                    ][self.filter],
                )
                .show_ui(ui, |ui| {
                    for (i, label) in [
                        "Все карты",
                        "Выбранные",
                        "Установленные",
                        "Обновления",
                        "Зависимости",
                    ]
                    .iter()
                    .enumerate()
                    {
                        ui.selectable_value(&mut self.filter, i, *label);
                    }
                });
            egui::ComboBox::from_id_salt("map-sort")
                .selected_text(if self.sort == 0 {
                    "По названию"
                } else {
                    "По региону"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sort, 0, "По названию");
                    ui.selectable_value(&mut self.sort, 1, "По региону");
                });
        });
        let library = settings
            .mod_library
            .as_ref()
            .and_then(|p| Library::open(p).ok());
        let search = self.search.to_lowercase();
        let mut entries: Vec<_> = catalog
            .maps
            .iter()
            .filter(|m| {
                search.is_empty()
                    || format!("{} {} {}", m.name, m.region, m.author)
                        .to_lowercase()
                        .contains(&search)
            })
            .filter(|m| match self.filter {
                1 => {
                    self.sets[self.selected].selected.contains(&m.id)
                        || plan.as_ref().is_ok_and(|p| p.reasons.contains_key(&m.id))
                }
                2 => {
                    library
                        .as_ref()
                        .is_some_and(|l| l.record(&m.id, &m.release.version).is_ok())
                        || self
                            .active
                            .as_ref()
                            .is_some_and(|a| a.packages.iter().any(|p| p.id == m.id))
                }
                3 => {
                    self.sources
                        .get(&m.id)
                        .and_then(|s| s.version.as_ref())
                        .is_some_and(|v| v != &m.release.version)
                        || self.active.as_ref().is_some_and(|a| {
                            a.packages
                                .iter()
                                .any(|p| p.id == m.id && p.version != m.release.version)
                        })
                }
                4 => m.support,
                _ => !m.support,
            })
            .collect();
        entries.sort_by(|a, b| {
            if self.sort == 0 {
                a.name.cmp(&b.name)
            } else {
                a.region.cmp(&b.region).then(a.name.cmp(&b.name))
            }
        });
        let mut action: Option<(String, usize)> = None;
        if self.view == 0 {
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("atlas-map")
                    .selected_text(
                        self.detail
                            .as_deref()
                            .and_then(|id| catalog.entry(id).ok())
                            .map(|m| m.name.as_str())
                            .unwrap_or("Выбрать карту…"),
                    )
                    .show_ui(ui, |ui| {
                        for entry in &entries {
                            ui.selectable_value(
                                &mut self.detail,
                                Some(entry.id.clone()),
                                &entry.name,
                            );
                        }
                    });
                if let Some(entry) = self.detail.as_deref().and_then(|id| catalog.entry(id).ok()) {
                    let size = self
                        .sources
                        .get(&entry.id)
                        .and_then(|s| s.download_size)
                        .or(entry.release.size);
                    ui.label(format!(
                        "{} · {}",
                        entry.release.version,
                        size.map(format_bytes)
                            .unwrap_or_else(|| "Размер: сайт автора".into())
                    ));
                    if ui
                        .add_enabled(
                            idle && settings.mod_library.is_some(),
                            egui::Button::new("Скачать и подключить"),
                        )
                        .clicked()
                    {
                        action = Some((entry.id.clone(), 0));
                    }
                    let mut enabled = self.sets[self.selected].selected.contains(&entry.id);
                    if ui
                        .add_enabled(
                            idle && self.pending.is_none() && !entry.support,
                            egui::Checkbox::new(&mut enabled, "В сборке"),
                        )
                        .changed()
                    {
                        if enabled {
                            self.sets[self.selected].selected.insert(entry.id.clone());
                        } else {
                            self.sets[self.selected].selected.remove(&entry.id);
                        }
                        changed = true;
                    }
                }
            });
            let selected = plan
                .as_ref()
                .map(|p| p.ordered.iter().cloned().collect())
                .unwrap_or_else(|_| self.sets[self.selected].selected.clone());
            if let Some(id) = self.atlas.show(
                ui,
                &catalog,
                &selected,
                self.detail.as_deref(),
                &dlcs,
                &self.sources,
            ) {
                self.detail = Some(id);
            }
            if let Some(coverage) = self
                .detail
                .as_deref()
                .and_then(|id| catalog.entry(id).ok())
                .and_then(|m| m.coverage.as_ref())
            {
                ui.label(&coverage.summary);
            }
        } else {
            let table_height = if self.detail.is_some() {
                230.0
            } else {
                (ctx.content_rect().height() - 390.0).clamp(160.0, 500.0)
            };
            egui_extras::TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(egui_extras::Column::exact(28.0).clip(true))
                .column(egui_extras::Column::remainder().at_least(150.0).clip(true))
                .column(egui_extras::Column::initial(95.0).clip(true))
                .column(egui_extras::Column::initial(115.0).clip(true))
                .column(egui_extras::Column::initial(115.0).clip(true))
                .column(egui_extras::Column::initial(105.0).clip(true))
                .column(egui_extras::Column::initial(65.0).clip(true))
                .column(egui_extras::Column::initial(96.0).clip(true))
                .max_scroll_height(table_height)
                .header(28.0, |mut h| {
                    for text in [
                        "",
                        "Карта",
                        "Регион",
                        "Версии",
                        "Совместимость",
                        "Состояние",
                        "Размер",
                        "Действия",
                    ] {
                        h.col(|ui| {
                            ui.strong(text);
                        });
                    }
                })
                .body(|mut body| {
                    for m in entries {
                        let required = plan.as_ref().ok().and_then(|p| p.reasons.get(&m.id));
                        let installed = library
                            .as_ref()
                            .and_then(|l| l.record(&m.id, &m.release.version).ok());
                        let current = self
                            .active
                            .as_ref()
                            .and_then(|a| a.packages.iter().find(|p| p.id == m.id))
                            .map(|p| p.version.as_str());
                        let available = self
                            .sources
                            .get(&m.id)
                            .and_then(|s| s.version.as_deref())
                            .unwrap_or(&m.release.version);
                        let supported = m.release.game_versions.iter().any(|v| {
                            maps::version_matches(&self.sets[self.selected].game_version, v)
                        });
                        body.row(52.0, |mut row| {
                            row.col(|ui| {
                                let mut checked = self.sets[self.selected].selected.contains(&m.id)
                                    || required.is_some();
                                if ui
                                    .add_enabled(
                                        idle && self.pending.is_none()
                                            && required.is_none()
                                            && !m.support,
                                        egui::Checkbox::without_text(&mut checked),
                                    )
                                    .on_hover_text(
                                        required
                                            .cloned()
                                            .unwrap_or_else(|| "Включить в сборку".into()),
                                    )
                                    .changed()
                                {
                                    if checked {
                                        self.sets[self.selected].selected.insert(m.id.clone());
                                    } else {
                                        self.sets[self.selected].selected.remove(&m.id);
                                    }
                                    changed = true;
                                }
                            });
                            row.col(|ui| {
                                if ui
                                    .add(
                                        egui::Button::selectable(
                                            self.detail.as_deref() == Some(&m.id),
                                            &m.name,
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(&m.name)
                                    .clicked()
                                {
                                    self.detail = if self.detail.as_deref() == Some(&m.id) {
                                        None
                                    } else {
                                        Some(m.id.clone())
                                    };
                                }
                            });
                            row.col(|ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(&m.region).size(12.0))
                                        .truncate(),
                                )
                                .on_hover_text(&m.region);
                            });
                            row.col(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} → {}",
                                            current.unwrap_or("—"),
                                            available
                                        ))
                                        .size(12.0),
                                    );
                                });
                            });
                            row.col(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(if available != m.release.version {
                                            "Нужна проверка"
                                        } else if supported {
                                            "Совместима"
                                        } else {
                                            "Другая версия"
                                        })
                                        .size(12.0)
                                        .color(
                                            if supported && available == m.release.version {
                                                GREEN
                                            } else {
                                                AMBER
                                            },
                                        ),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "ETS2 {}",
                                            m.release.game_versions.join(", ")
                                        ))
                                        .size(12.0)
                                        .color(MUTED),
                                    );
                                });
                            });
                            row.col(|ui| {
                                let status = if available != m.release.version {
                                    "Нужна проверка"
                                } else if self.sources.get(&m.id).is_some_and(|s| s.error.is_some())
                                {
                                    "Источник недоступен"
                                } else if installed.is_some() {
                                    "В библиотеке"
                                } else if !supported {
                                    "Другая версия ETS2"
                                } else if required.is_some() {
                                    "Зависимость"
                                } else {
                                    "Не скачана"
                                };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(status)
                                            .size(12.0)
                                            .color(if installed.is_some() { GREEN } else { MUTED }),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(status);
                            });
                            row.col(|ui| {
                                ui.label(
                                    RichText::new(
                                        self.sources
                                            .get(&m.id)
                                            .and_then(|s| s.size_label.clone())
                                            .or_else(|| m.release.size.map(format_bytes))
                                            .unwrap_or_else(|| "—".into()),
                                    )
                                    .size(12.0),
                                );
                            });
                            row.col(|ui| {
                                ui.vertical(|ui| {
                                    if ui
                                        .add_enabled(
                                            idle && settings.mod_library.is_some(),
                                            egui::Button::new("Скачать").small(),
                                        )
                                        .clicked()
                                    {
                                        action = Some((m.id.clone(), 0));
                                    }
                                    if ui
                                        .add_enabled(
                                            idle && settings.mod_library.is_some(),
                                            egui::Button::new("Импорт").small(),
                                        )
                                        .clicked()
                                    {
                                        action = Some((m.id.clone(), 1));
                                    }
                                });
                            });
                        });
                    }
                });
        }
        if changed {
            self.save_sets();
        }
        if let Some((id, kind)) = action {
            if kind == 0 {
                if let Ok(entry) = catalog.entry(&id) {
                    if entry.module != "/map/europe.mbd" {
                        self.sets[self.selected].selected.clear();
                    } else {
                        self.sets[self.selected].selected.retain(|id| {
                            catalog
                                .entry(id)
                                .is_ok_and(|m| m.module == "/map/europe.mbd")
                        });
                    }
                    self.sets[self.selected].selected.insert(id);
                    self.apply(runtime, ctx, settings, self.sets[self.selected].clone());
                }
            } else if let Some(paths) = rfd::FileDialog::new()
                .set_title("Все пакеты карты, архивы или части многотомного архива")
                .pick_files()
            {
                self.imports.push((id, paths));
            }
        }
        if let Some(id) = self.detail.clone()
            && let Ok(m) = catalog.entry(&id)
        {
            egui::ScrollArea::vertical()
                .id_salt("map-detail")
                .max_height(200.0)
                .show(ui, |ui| {
                    ui.strong(format!("{} · {}", m.name, m.author));
                    ui.label(&m.description);
                    ui.label(
                        RichText::new(format!(
                            "DLC: {}",
                            if m.release.dlcs.is_empty() {
                                "не требуются".into()
                            } else {
                                m.release.dlcs.join(", ")
                            }
                        ))
                        .small()
                        .color(MUTED),
                    );
                    if let Ok(plan) = &plan {
                        ui.label(
                            RichText::new(format!(
                                "Порядок снизу вверх: {}",
                                plan.ordered
                                    .iter()
                                    .filter_map(|id| catalog.entry(id).ok())
                                    .map(|m| m.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" → ")
                            ))
                            .small(),
                        );
                    }
                    if let Some(s) = self.sources.get(&id) {
                        if let Some(date) = &s.published_at {
                            ui.label(RichText::new(format!("Релиз автора: {date}")).small());
                        }
                        if let Some(requirements) = &s.requirements {
                            ui.label(
                                RichText::new(format!("На источнике: {requirements}")).small(),
                            );
                        }
                        ui.label(
                            RichText::new(format!(
                                "Проверено: {} мин назад",
                                source::now().saturating_sub(s.checked_at) / 60
                            ))
                            .small(),
                        );
                        if let Some(e) = &s.error {
                            ui.colored_label(AMBER, e);
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        if ui.small_button("Сайт автора ↗").clicked() {
                            let _ = open::that(
                                m.source.browser_url.as_deref().unwrap_or(&m.source.url),
                            );
                        }
                        if ui
                            .add_enabled(idle, egui::Button::new("Импорт папки мода").small())
                            .clicked()
                            && let Some(path) = rfd::FileDialog::new().pick_folder()
                        {
                            self.imports.push((id.clone(), vec![path]));
                        }
                        if ui
                            .add_enabled(
                                idle && self.pending.is_none(),
                                egui::Button::new("Сбросить незавершённую загрузку").small(),
                            )
                            .clicked()
                        {
                            self.note = Self::library(settings)
                                .and_then(|l| l.clear_incoming(m))
                                .err()
                                .map(|e| format!("{e:#}"));
                        }
                    });
                });
        }
        egui::CollapsingHeader::new("Источники и встроенный браузер").show(ui, |ui| {
            ui.label("JSON-каталог совместимости по HTTPS");
            ui.add(
                egui::TextEdit::singleline(&mut self.source_editor)
                    .desired_width(ui.available_width()),
            );
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        idle && !self.refreshing,
                        egui::Button::new("Применить источник"),
                    )
                    .clicked()
                {
                    match maps::https(&self.source_editor) {
                        Ok(_) => {
                            settings.mod_catalog_source = Some(self.source_editor.trim().into());
                            if let Err(e) = settings.save() {
                                self.note = Some(e.to_string());
                            }
                            self.refresh(ctx, runtime, settings);
                        }
                        Err(e) => self.note = Some(e.to_string()),
                    }
                }
                if ui.button("Установить WebView2 ↗").clicked() {
                    let _ = open::that(
                        "https://developer.microsoft.com/microsoft-edge/webview2/#download-section",
                    );
                }
                if ui
                    .add_enabled(idle, egui::Button::new("Остановить браузерные загрузки"))
                    .clicked()
                {
                    self.browser.cancel();
                }
            });
        });
    }
}

fn detect_version(game: &std::path::Path) -> Option<String> {
    library::game_version(game)
}
