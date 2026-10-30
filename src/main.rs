#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browser_smoke;
mod mod_browser;
mod quper_ui;

use anyhow::Context;

use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2};
use ets_launcher::{
    SOURCE_PAGE,
    catalog::{CatalogCache, CatalogState, REFRESH_INTERVAL, RETRY_INTERVAL},
    download::{self, SharedTransfer, Source, Status, Transfer},
    format_bytes, game,
    game::{Channel, Game},
    install, playground,
    settings::Settings,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

const BG: Color32 = Color32::from_rgb(13, 18, 24);
const PANEL: Color32 = Color32::from_rgb(20, 27, 35);
const LINE: Color32 = Color32::from_rgb(42, 54, 66);
const MUTED: Color32 = Color32::from_rgb(143, 160, 175);
const GREEN: Color32 = Color32::from_rgb(104, 224, 164);

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--quper-browser-file") {
        anyhow::ensure!(
            args.len() == 4,
            "--quper-browser-file <public-url> <new-output-folder>"
        );
        return browser_smoke::run_public(PathBuf::from(&args[3]), &args[2]);
    }
    if args.get(1).is_some_and(|a| {
        matches!(
            a.as_str(),
            "--quper-catalog" | "--quper-plan" | "--quper-download" | "--quper-file"
        )
    }) {
        return quper_cli(&args);
    }
    if args.get(1).is_some_and(|arg| arg == "--map-import-test") {
        let catalog = ets_launcher::maps::Catalog::bundled()?;
        let entry = catalog.entry(
            args.get(2)
                .context("--map-import-test <id> <library> <report>")?,
        )?;
        let library = ets_launcher::maps::library::Library::open(std::path::Path::new(
            args.get(3).context("Нужна библиотека")?,
        ))?;
        let record =
            Runtime::new()?.block_on(library.import(entry, &[], &CancellationToken::new()))?;
        ets_launcher::maps::write_json(
            std::path::Path::new(args.get(4).context("Нужен отчёт")?),
            &record,
        )?;
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--map-apply-test") {
        let report = ets_launcher::maps::audit::apply_test(
            &ets_launcher::maps::Catalog::bundled()?,
            args.get(2)
                .context("--map-apply-test <id> <library> <game> <report>")?,
            std::path::Path::new(args.get(3).context("Нужна библиотека")?),
            std::path::Path::new(args.get(4).context("Нужна ETS2")?),
        )?;
        ets_launcher::maps::write_json(
            std::path::Path::new(args.get(5).context("Нужен отчёт")?),
            &report,
        )?;
        return Ok(());
    }
    if args
        .get(1)
        .is_some_and(|arg| arg == "--browser-map-download")
    {
        return browser_smoke::run_map(
            PathBuf::from(
                args.get(3)
                    .context("--browser-map-download <id> <new-directory>")?,
            ),
            args.get(2).context("Нужен ID карты")?,
        );
    }
    if args.get(1).is_some_and(|arg| arg == "--map-audit") {
        let catalog = ets_launcher::maps::Catalog::bundled()?;
        let report = Runtime::new()?.block_on(ets_launcher::maps::audit::inspect(
            &catalog,
            &CancellationToken::new(),
        ));
        ets_launcher::maps::write_json(
            std::path::Path::new(args.get(2).context("--map-audit <report.json>")?),
            &report,
        )?;
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--map-download") {
        let id = args
            .get(2)
            .context("--map-download <id> <new-library> <report.json>")?;
        let root = PathBuf::from(args.get(3).context("Нужна новая библиотека")?);
        let output = PathBuf::from(args.get(4).context("Нужен путь отчёта")?);
        let result = Runtime::new()?.block_on(ets_launcher::maps::audit::download(
            &ets_launcher::maps::Catalog::bundled()?,
            id,
            &root,
            &CancellationToken::new(),
        ));
        let report = match &result {
            Ok(report) => report.clone(),
            Err(error) => {
                serde_json::json!({"id":id,"status":"error","message":format!("{error:#}")})
            }
        };
        ets_launcher::maps::write_json(&output, &report)?;
        result?;
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--browser-smoke") {
        return browser_smoke::run(PathBuf::from(
            args.get(2).context("--browser-smoke <new-directory>")?,
        ));
    }
    if args.get(1).is_some_and(|arg| arg == "--map-sources") {
        let output = args.get(2).context("--map-sources <report.json>")?;
        let catalog = ets_launcher::maps::Catalog::bundled()?;
        let states = Runtime::new()?.block_on(async {
            use futures_util::{StreamExt, stream};
            stream::iter(catalog.maps)
                .map(|entry| async move {
                    let result =
                        ets_launcher::maps::source::fetch(&entry, &CancellationToken::new()).await;
                    (
                        entry.id,
                        match result {
                            Ok(state) => state,
                            Err(error) => ets_launcher::maps::SourceState {
                                error: Some(format!("{error:#}")),
                                ..Default::default()
                            },
                        },
                    )
                })
                .buffer_unordered(3)
                .collect::<std::collections::BTreeMap<_, _>>()
                .await
        });
        ets_launcher::maps::write_json(std::path::Path::new(output), &states)?;
        return Ok(());
    }
    if args
        .get(1)
        .is_some_and(|arg| arg == "--create-test-profile")
    {
        let home = PathBuf::from(args.get(2).context("--create-test-profile <new-home>")?);
        anyhow::ensure!(!home.exists(), "Тестовая папка уже существует");
        let name = "Launcher QA";
        let folder = home
            .join("Euro Truck Simulator 2/profiles")
            .join(ets_launcher::maps::profile::folder_name(name));
        std::fs::create_dir_all(&folder)?;
        std::fs::create_dir(home.join("Euro Truck Simulator 2/mod"))?;
        std::fs::write(
            folder.join("profile.sii"),
            ets_launcher::maps::profile::create(name, "/map/europe.mbd", &[])?,
        )?;
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--catalog") {
        let game = cli_game(&args, 3)?;
        let output = args
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| "artifacts/catalog.json".into());
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let settings = Settings::load();
        let catalog = Runtime::new()?.block_on(playground::fetch_catalog(
            &playground::client()?,
            settings.source(game),
            &CancellationToken::new(),
        ))?;
        catalog.validate_for(game)?;
        std::fs::write(output, serde_json::to_vec_pretty(&catalog)?)?;
        return Ok(());
    }
    // Headless diagnostic: always requires an explicitly supplied archive and game directory.
    if args.get(1).is_some_and(|arg| arg == "--install-archive") {
        anyhow::ensure!(
            matches!(args.len(), 5 | 6),
            "--install-archive <archive> <game directory> <report file> [ets2|ats]"
        );
        let state = Arc::new(Mutex::new(Transfer::default()));
        let result = Runtime::new()?.block_on(async {
            install::install_archive(
                install::prepare(cli_game(&args, 5)?, std::path::Path::new(&args[3]))?,
                PathBuf::from(&args[2]),
                state,
                CancellationToken::new(),
            )
            .await
        });
        let report = match &result {
            Ok(report) => format!(
                "Installed: {}\nGame: {}\nBackup: {}\n",
                report.files,
                report.game.display(),
                report.backup.display()
            ),
            Err(error) => format!("ERROR: {error:#}\n"),
        };
        std::fs::write(&args[4], report)?;
        result?;
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--fetch-torrent") {
        let output = args
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("artifacts/site-smoke"));
        std::fs::create_dir_all(&output)?;
        let state = Arc::new(Mutex::new(Transfer::default()));
        Runtime::new()?.block_on(download::run(
            Source::Playground {
                game: cli_game(&args, 3)?,
                channel: args
                    .get(4)
                    .map(|value| value.parse())
                    .transpose()?
                    .unwrap_or_default(),
                page_url: Settings::load().source(cli_game(&args, 3)?).to_owned(),
                torrent_only: true,
            },
            output.clone(),
            state.clone(),
            CancellationToken::new(),
        ));
        let state = download::snapshot(&state);
        std::fs::write(
            output.join("result.txt"),
            format!(
                "Status: {:?}\n{}\n{}\nSaved: {}\n",
                state.status,
                state.name,
                state.message,
                state
                    .saved_to
                    .as_deref()
                    .unwrap_or(std::path::Path::new(""))
                    .display()
            ),
        )?;
        anyhow::ensure!(state.status == Status::Complete, "{}", state.message);
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--detect-game") {
        let output = args
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("artifacts/game-path.txt"));
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            output,
            game::detect(cli_game(&args, 3)?, None)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "NOT_FOUND".into()),
        )?;
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--fetch-page") {
        let output = args
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("artifacts/playground-page.html"));
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let page_url = args.get(3).map(String::as_str).unwrap_or(SOURCE_PAGE);
        let result = Runtime::new()?.block_on(async {
            let response = reqwest::Client::builder()
                .timeout(Duration::from_secs(40))
                .user_agent("Mozilla/5.0 ETS-Launcher/0.1")
                .build()?
                .get(page_url)
                .send()
                .await?
                .error_for_status()?;
            Ok::<_, anyhow::Error>(response.text().await?)
        });
        match result {
            Ok(html) => std::fs::write(output, html)?,
            Err(error) => {
                std::fs::write(output.with_extension("error.txt"), format!("{error:#}"))?;
                return Err(error);
            }
        }
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--test-download") {
        let output = args
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("downloads"));
        let runtime = Runtime::new()?;
        let state = Arc::new(Mutex::new(Transfer::default()));
        let path = runtime.block_on(download::download_http(
            ets_launcher::TEST_URL,
            &output,
            ets_launcher::TEST_FILENAME,
            &state,
            &CancellationToken::new(),
        ))?;
        std::fs::write(
            output.join("test-result.txt"),
            format!(
                "Downloaded: {}\n{} bytes\n",
                path.display(),
                std::fs::metadata(&path)?.len()
            ),
        )?;
        return Ok(());
    }
    let runtime = Runtime::new()?;
    let app_icon = Arc::new(eframe::icon_data::from_png_bytes(include_bytes!(
        "../assets/branding/icon.png"
    ))?);
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 850.0])
            .with_min_inner_size([980.0, 640.0])
            .with_title("ETS & ATS Launcher")
            .with_icon(app_icon.clone()),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "ETS & ATS Launcher",
        native_options,
        Box::new(move |cc| {
            configure(&cc.egui_ctx);
            let logo = cc.egui_ctx.load_texture(
                "truck-launcher-logo",
                egui::ColorImage::from_rgba_unmultiplied(
                    [app_icon.width as usize, app_icon.height as usize],
                    &app_icon.rgba,
                ),
                egui::TextureOptions::LINEAR,
            );
            let settings = Settings::load();
            let cache = CatalogCache::load();
            let catalogs = std::array::from_fn(|index| {
                let game = Game::ALL[index];
                CatalogState::new(cache.get(game, settings.source(game)))
            });
            let source_editor = settings.source(settings.selected_game).to_owned();
            let (catalog_sender, catalog_receiver) = mpsc::channel();
            let mut launcher = Launcher {
                runtime: Some(runtime),
                settings,
                state: Arc::new(Mutex::new(Transfer::default())),
                cancel: CancellationToken::new(),
                close_when_idle: false,
                note: None,
                game_search: None,
                cache,
                catalogs,
                catalog_sender,
                catalog_receiver,
                source_editor,
                logo,
                maps: quper_ui::QuperUi::new(&Settings::load()),
                maps_open: false,
            };
            launcher.search_game(&cc.egui_ctx, true);
            for game in Game::ALL {
                launcher.refresh_catalog(&cc.egui_ctx, game, true);
            }
            Ok(Box::new(launcher))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Не удалось открыть окно: {error}"))
}

fn cli_game(args: &[String], index: usize) -> anyhow::Result<Game> {
    args.get(index)
        .map(|value| value.parse())
        .transpose()
        .map(|game| game.unwrap_or_default())
}

fn quper_cli(args: &[String]) -> anyhow::Result<()> {
    use ets_launcher::quper;
    let runtime = Runtime::new()?;
    let cancel = CancellationToken::new();
    if args[1] == "--quper-file" {
        anyhow::ensure!(
            args.len() == 5,
            "--quper-file <public-url> <download-folder> <report.json>"
        );
        let state = Arc::new(Mutex::new(Transfer::default()));
        let result = runtime.block_on(quper::files::download_source(
            &args[2],
            &PathBuf::from(&args[3]),
            &state,
            &cancel,
        ));
        let report = match &result {
            Ok(files) => serde_json::json!({"status":"downloaded", "files":files}),
            Err(e) => serde_json::json!({"status":"error", "message":format!("{e:#}")}),
        };
        ets_launcher::maps::write_json(std::path::Path::new(&args[4]), &report)?;
        result?;
        return Ok(());
    }
    let catalog = runtime.block_on(quper::source::fetch_catalog(&cancel))?;
    if args[1] == "--quper-catalog" {
        anyhow::ensure!(args.len() == 3, "--quper-catalog <report.json>");
        return ets_launcher::maps::write_json(std::path::Path::new(&args[2]), &catalog);
    }
    let combo = catalog
        .combos
        .iter()
        .find(|c| Some(&c.id) == args.get(2))
        .context("Сборка не найдена в публичном каталоге Quper")?;
    let plan = runtime.block_on(quper::source::fetch_plan(combo, &cancel))?;
    if args[1] == "--quper-plan" {
        anyhow::ensure!(args.len() == 4, "--quper-plan <combo-id> <report.json>");
        return ets_launcher::maps::write_json(std::path::Path::new(&args[3]), &plan);
    }
    anyhow::ensure!(
        args.len() == 5,
        "--quper-download <combo-id> <library-folder> <report.json>"
    );
    let library = quper::library::Library::open(std::path::Path::new(&args[3]))?;
    let transfer = Arc::new(Mutex::new(Transfer::default()));
    let result = runtime.block_on(library.download(&plan, &transfer, &cancel));
    let report = match &result {
        Ok(report) => serde_json::to_value(report)?,
        Err(e) => serde_json::json!({"status":"error", "message":format!("{e:#}")}),
    };
    ets_launcher::maps::write_json(std::path::Path::new(&args[4]), &report)?;
    result?;
    Ok(())
}

fn configure(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.override_text_color = Some(Color32::from_rgb(231, 237, 242));
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(31, 41, 52);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(45, 60, 73);
    visuals.selection.bg_fill = Color32::from_rgb(43, 100, 77);
    visuals.hyperlink_color = GREEN;
    ctx.set_visuals(visuals);
    ctx.style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(10.0, 10.0);
        style.spacing.button_padding = Vec2::new(16.0, 11.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(15.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(15.0));
    });
    // Use the platform's Cyrillic font when available; built-in fonts remain a fallback.
    if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("segoe".into(), egui::FontData::from_owned(bytes).into());
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "segoe".into());
        ctx.set_fonts(fonts);
    }
}

struct Launcher {
    logo: egui::TextureHandle,
    runtime: Option<Runtime>,
    settings: Settings,
    state: SharedTransfer,
    cancel: CancellationToken,
    close_when_idle: bool,
    note: Option<String>,
    game_search: Option<mpsc::Receiver<(Game, Option<PathBuf>)>>,
    cache: CatalogCache,
    catalogs: [CatalogState; 2],
    catalog_sender: mpsc::Sender<CatalogUpdate>,
    catalog_receiver: mpsc::Receiver<CatalogUpdate>,
    source_editor: String,
    maps: quper_ui::QuperUi,
    maps_open: bool,
}

struct CatalogUpdate {
    game: Game,
    source: String,
    generation: u64,
    result: Result<playground::Catalog, String>,
}

impl Launcher {
    fn refresh_catalog(&mut self, ctx: &egui::Context, game: Game, force: bool) {
        let state = &mut self.catalogs[game.index()];
        if !force && (state.refreshing || Instant::now() < state.next_refresh) {
            return;
        }
        let source = self.settings.source(game).to_owned();
        if state
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.source_url != source)
        {
            state.catalog = None;
        }
        state.cancel.cancel();
        state.cancel = CancellationToken::new();
        state.generation = state.generation.wrapping_add(1);
        state.refreshing = true;
        let cancel = state.cancel.clone();
        let generation = state.generation;
        let sender = self.catalog_sender.clone();
        let ctx = ctx.clone();
        self.runtime.as_ref().unwrap().spawn(async move {
            let result: anyhow::Result<_> = async {
                let client = playground::client()?;
                let catalog = playground::fetch_catalog(&client, &source, &cancel).await?;
                catalog.validate_for(game)?;
                Ok(catalog)
            }
            .await;
            let _ = sender.send(CatalogUpdate {
                game,
                source,
                generation,
                result: result.map_err(|e| format!("{e:#}")),
            });
            ctx.request_repaint();
        });
    }

    fn poll_catalogs(&mut self) {
        for update in self.catalog_receiver.try_iter() {
            let state = &mut self.catalogs[update.game.index()];
            if update.generation != state.generation
                || update.source != self.settings.source(update.game)
            {
                continue;
            }
            state.refreshing = false;
            match update.result {
                Ok(catalog) => {
                    state.error = None;
                    state.catalog = Some(catalog.clone());
                    state.next_refresh = Instant::now() + REFRESH_INTERVAL;
                    self.cache.set(update.game, catalog);
                    if let Err(error) = self.cache.save() {
                        self.note = Some(format!("Не удалось сохранить данные источника: {error}"));
                    }
                }
                Err(error) => {
                    state.error = Some(error);
                    state.next_refresh = Instant::now() + RETRY_INTERVAL;
                }
            }
        }
    }

    fn search_game(&mut self, ctx: &egui::Context, use_saved: bool) {
        let (sender, receiver) = mpsc::channel();
        self.game_search = Some(receiver);
        let game = self.settings.selected_game;
        let saved = if use_saved {
            self.settings
                .directory(game)
                .map(std::path::Path::to_path_buf)
        } else {
            None
        };
        let ctx = ctx.clone();
        self.runtime.as_ref().unwrap().spawn_blocking(move || {
            let _ = sender.send((game, game::detect(game, saved.as_deref())));
            ctx.request_repaint();
        });
    }

    fn poll_game_search(&mut self) {
        match self.game_search.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok((game, path))) => {
                self.game_search = None;
                self.settings.set_directory(game, path);
                if let Err(error) = self.settings.save() {
                    self.note = Some(error.to_string());
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.game_search = None;
                self.note = Some("Выберите папку игры вручную в настройках.".into());
            }
            _ => {}
        }
    }

    fn start_install(&mut self) {
        if download::snapshot(&self.state).active() {
            return;
        }
        let game = self.settings.selected_game;
        let Some(directory) = self
            .settings
            .directory(game)
            .map(std::path::Path::to_path_buf)
        else {
            self.note = Some(format!(
                "Не найдена {}. Укажите папку игры в настройках.",
                game.short_name()
            ));
            return;
        };
        self.note = None;
        self.cancel = CancellationToken::new();
        download::set_state(&self.state, |s| {
            *s = Transfer {
                status: Status::Connecting,
                name: format!("{} DLC Maps", game.short_name()),
                message: "Подготовка к установке…".into(),
                ..Default::default()
            }
        });
        if let Err(error) = self.settings.save() {
            self.note = Some(error.to_string());
        }
        self.runtime.as_ref().unwrap().spawn(install::run(
            game,
            self.settings.channel(),
            self.settings.source(game).to_owned(),
            self.settings.download_directory(game),
            directory,
            self.state.clone(),
            self.cancel.clone(),
        ));
    }
}

fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, LINE))
        .corner_radius(12)
        .inner_margin(24)
}

impl eframe::App for Launcher {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.poll_game_search();
        self.poll_catalogs();
        let transfer = download::snapshot(&self.state);
        self.maps.tick(
            ctx,
            self.runtime.as_ref().unwrap(),
            &self.settings,
            transfer.active(),
        );
        let busy = transfer.active() || self.maps.busy();
        if !busy {
            for game in Game::ALL {
                self.refresh_catalog(ctx, game, false);
            }
        }
        ctx.request_repaint_after(Duration::from_secs(1));
        if ctx.input(|i| i.viewport().close_requested()) && busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_when_idle = true;
            self.cancel.cancel();
            self.maps.cancel_all();
        }
        if self.close_when_idle && !busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if busy || self.game_search.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(30))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.maps_open, false, "DLC и настройки игры");
                    ui.selectable_value(&mut self.maps_open, true, "Сборки Quper");
                });
                ui.add_space(12.0);
                if self.maps_open {
                    egui::ScrollArea::vertical().id_salt("maps-page").show(ui, |ui| {
                        self.maps.show(ui, ctx, frame, self.runtime.as_ref().unwrap(), &mut self.settings, transfer.active());
                    });
                    return;
                }
                self.maps.suspend_browser();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Image::new(&self.logo).fit_to_exact_size(Vec2::splat(42.0)));
                        ui.label(RichText::new("TRUCK").size(28.0).strong().color(GREEN));
                        ui.label(RichText::new("LAUNCHER").size(16.0).strong());
                    });
                    let previous = self.settings.selected_game;
                    let mut selected = previous;
                    ui.add_enabled_ui(!busy, |ui| {
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut selected,
                                Game::Ets2,
                                "Euro Truck Simulator 2",
                            );
                            ui.selectable_value(
                                &mut selected,
                                Game::Ats,
                                "American Truck Simulator",
                            );
                        });
                    });
                    if selected != previous {
                        self.settings.selected_game = selected;
                        self.note = None;
                        self.source_editor = self.settings.source(selected).to_owned();
                        download::set_state(&self.state, |s| *s = Transfer::default());
                        self.search_game(ctx, true);
                        if let Err(error) = self.settings.save() {
                            self.note = Some(error.to_string());
                        }
                    }
                    let catalog = self.catalogs[selected.index()].catalog.clone();
                    let refreshing = self.catalogs[selected.index()].refreshing;
                    let catalog_error = self.catalogs[selected.index()].error.clone();
                    if catalog.as_ref().is_some_and(|c| c.select(Channel::Beta).is_ok()) || self.settings.channel() == Channel::Beta {
                        let previous_channel = self.settings.channel();
                        let mut channel = previous_channel;
                        ui.add_enabled_ui(!busy, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("Ветка:").size(12.0).color(MUTED));
                                for (value, name) in [(Channel::Stable, "Обычная"), (Channel::Beta, "Beta")] {
                                    let release = catalog.as_ref().and_then(|c| c.select(value).ok());
                                    let label = release.and_then(|r| r.version.as_ref()).map(|version| format!("{name} · {version}")).unwrap_or_else(|| format!("{name} · недоступна"));
                                    ui.add_enabled_ui(release.is_some(), |ui| { ui.selectable_value(&mut channel, value, label); });
                                }
                            });
                        });
                        ui.label(
                            RichText::new("Выберите ту же ветку, что установлена в Steam.")
                                .size(11.0)
                                .color(MUTED),
                        );
                        if previous_channel != channel {
                            self.settings.set_channel(selected, channel);
                            self.note = None;
                            download::set_state(&self.state, |s| *s = Transfer::default());
                            if let Err(error) = self.settings.save() {
                                self.note = Some(error.to_string());
                            }
                        }
                    }
                    let release = catalog.as_ref().and_then(|c| c.select(self.settings.channel()).ok());
                    ui.add_space(16.0);
                    card().show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            RichText::new(format!("{} · DLC Maps", selected.short_name()))
                                .size(34.0)
                                .strong(),
                        );
                        if let Some(release) = release {
                            ui.label(RichText::new(format!("Версия на источнике: {}", release.version.as_deref().unwrap_or("не указана"))).color(GREEN));
                            ui.add(egui::Label::new(RichText::new(format!("{} · {}", release.file.name, release.size_label.as_deref().unwrap_or("размер уточнится при загрузке"))).size(12.0).color(MUTED)).wrap());
                        } else {
                            ui.label(RichText::new(if refreshing { "Проверяем актуальные файлы PlayGround…" } else { "Для выбранной ветки нет доступного архива" }).color(MUTED));
                        }
                        ui.horizontal(|ui| {
                            if refreshing { ui.spinner(); }
                            let stamp = catalog.as_ref().map(|c| {
                                let age = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs().saturating_sub(c.checked_at);
                                if age < 60 { "Данные обновлены только что".into() } else { format!("Данные проверены {} мин назад", age / 60) }
                            }).unwrap_or_else(|| "Нет сохранённых данных".into());
                            ui.label(RichText::new(stamp).size(11.0).color(MUTED));
                            if ui.add_enabled(!busy && !refreshing, egui::Button::new("Обновить данные")).clicked() { self.refresh_catalog(ctx, selected, true); }
                        });
                        if let Some(error) = &catalog_error {
                            ui.add(egui::Label::new(RichText::new(format!("Не удалось обновить: {error}{}", if catalog.is_some() { ". Показаны сохранённые данные; перед установкой источник будет проверен заново." } else { "" })).size(11.0).color(Color32::from_rgb(255,184,120))).wrap());
                        }
                        ui.label(
                            RichText::new("Скачивание и установка дополнений одной кнопкой")
                                .color(MUTED),
                        );
                        ui.add_space(14.0);
                        let game_label = if self.game_search.is_some() {
                            "Поиск игры…".into()
                        } else {
                            self.settings
                                .directory(selected)
                                .map(|p| format!("{}: {}", selected.short_name(), p.display()))
                                .unwrap_or_else(|| {
                                    format!(
                                        "{} не найдена — выберите папку в настройках",
                                        selected.short_name()
                                    )
                                })
                        };
                        ui.add(
                            egui::Label::new(RichText::new(game_label).size(12.0).color(MUTED))
                                .wrap(),
                        );
                        ui.add_space(12.0);
                        let button = egui::Button::new(
                            RichText::new("↓   Установить DLC")
                                .strong()
                                .size(18.0)
                                .color(BG),
                        )
                        .fill(GREEN)
                        .corner_radius(8)
                        .min_size(Vec2::new(ui.available_width(), 52.0));
                        if ui
                            .add_enabled(
                                !busy
                                    && release.is_some()
                                    && self.settings.directory(selected).is_some()
                                    && self.game_search.is_none(),
                                button,
                            )
                            .clicked()
                        {
                            self.start_install();
                        }
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Торрент → загрузка архива → распаковка → установка")
                                .size(12.0)
                                .color(MUTED),
                        );
                    });
                    ui.add_space(6.0);
                    card().show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        let heading = match transfer.status {
                            Status::Ready => "Готов к установке",
                            Status::Connecting => "Подготовка загрузки",
                            Status::Downloading => "Скачивание архива",
                            Status::Extracting => "Распаковка архива",
                            Status::Installing => "Установка файлов",
                            Status::RollingBack => "Восстановление файлов",
                            Status::Complete => "DLC установлены",
                            Status::Cancelled => "Установка остановлена",
                            Status::Error => "Не удалось установить DLC",
                        };
                        let color = match transfer.status {
                            Status::Complete => GREEN,
                            Status::Error => Color32::from_rgb(255, 135, 135),
                            _ => Color32::WHITE,
                        };
                        ui.horizontal(|ui| {
                            if busy {
                                ui.spinner();
                            }
                            ui.label(RichText::new(heading).strong().color(color));
                        });
                        if matches!(
                            transfer.status,
                            Status::Downloading | Status::Installing | Status::Complete
                        ) {
                            ui.add(
                                egui::ProgressBar::new(transfer.progress())
                                    .fill(GREEN)
                                    .show_percentage(),
                            );
                        }
                        if transfer.status == Status::Downloading {
                            let total = transfer
                                .total
                                .map(format_bytes)
                                .unwrap_or_else(|| "…".into());
                            ui.label(
                                RichText::new(format!(
                                    "{} / {total}  ·  {} / с  ·  пиров: {}",
                                    format_bytes(transfer.downloaded),
                                    format_bytes(transfer.speed),
                                    transfer.peers
                                ))
                                .size(12.0)
                                .color(MUTED),
                            );
                        }
                        let message = if self.close_when_idle {
                            "Завершаем остановку и восстановление файлов перед закрытием…"
                        } else if transfer.status == Status::Ready {
                            "Нажмите «Установить DLC». Игра должна быть закрыта."
                        } else {
                            transfer.message.as_str()
                        };
                        if busy && !transfer.name.is_empty() { ui.add(egui::Label::new(RichText::new(&transfer.name).size(12.0).color(MUTED)).wrap()); }
                        ui.add(
                            egui::Label::new(RichText::new(message).size(12.0).color(MUTED)).wrap(),
                        );
                        if busy && transfer.status != Status::RollingBack {
                            if ui
                                .add_enabled(
                                    !self.cancel.is_cancelled(),
                                    egui::Button::new("Остановить"),
                                )
                                .clicked()
                            {
                                self.cancel.cancel();
                            }
                        } else if transfer.status == Status::Complete
                            && ui.small_button("Открыть папку игры").clicked()
                            && let Some(path) = &transfer.saved_to
                            && let Err(error) = open::that(path)
                        {
                            self.note = Some(error.to_string());
                        }
                    });
                    ui.add_space(8.0);
                    ui.add_enabled_ui(!busy, |ui| {
                        egui::CollapsingHeader::new("Настройки").show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if ui
                                    .button(format!("Выбрать папку {}", selected.short_name()))
                                    .clicked()
                                    && let Some(folder) = rfd::FileDialog::new()
                                        .set_title(format!("Папка {}", selected.name()))
                                        .pick_folder()
                                {
                                    if game::is_installation(selected, &folder) {
                                        self.game_search = None;
                                        self.settings.set_directory(selected, Some(folder));
                                        if let Err(error) = self.settings.save() {
                                            self.note = Some(error.to_string());
                                        }
                                    } else {
                                        self.note = Some(format!(
                                            "В папке не найден bin/win_x64/{}.",
                                            selected.executable()
                                        ));
                                    }
                                }
                                if ui
                                    .add_enabled(
                                        self.game_search.is_none(),
                                        egui::Button::new("Найти автоматически"),
                                    )
                                    .clicked()
                                {
                                    self.search_game(ctx, false);
                                }
                            });
                            ui.label(
                                RichText::new(format!(
                                    "Кэш загрузок: {}",
                                    self.settings.download_directory(selected).display()
                                ))
                                .size(12.0)
                                .color(MUTED),
                            );
                            ui.label(RichText::new("Страница источника PlayGround").size(12.0).color(MUTED));
                            ui.add(egui::TextEdit::singleline(&mut self.source_editor).desired_width(ui.available_width()));
                            ui.horizontal(|ui| {
                                if ui.button("Применить ссылку").clicked() {
                                    match self.settings.set_source(selected, &self.source_editor) {
                                        Ok(()) => {
                                            self.note = self.settings.save().err().map(|e| e.to_string());
                                            self.source_editor = self.settings.source(selected).to_owned();
                                            self.refresh_catalog(ctx, selected, true);
                                        },
                                        Err(error) => self.note = Some(error.to_string()),
                                    }
                                }
                                if ui.button("Сбросить ссылку").clicked() {
                                    let _ = self.settings.set_source(selected, "");
                                    self.source_editor = self.settings.source(selected).to_owned();
                                    self.note = self.settings.save().err().map(|e| e.to_string());
                                    self.refresh_catalog(ctx, selected, true);
                                }
                            });
                            if ui.button("Изменить папку загрузки").clicked()
                                && let Some(folder) = rfd::FileDialog::new()
                                    .set_directory(&self.settings.output)
                                    .pick_folder()
                            {
                                self.settings.output = folder;
                                if let Err(error) = self.settings.save() {
                                    self.note = Some(error.to_string());
                                }
                            }
                            ui.label(
                                RichText::new(
                                    "Резервные копии: .ets-launcher/backups в папке игры",
                                )
                                .size(12.0)
                                .color(MUTED),
                            );
                        });
                    });
                    if let Some(note) = &self.note {
                        ui.colored_label(Color32::from_rgb(255, 184, 120), note);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Источник:").size(11.0).color(MUTED));
                        if ui.small_button("PlayGround ↗").clicked()
                            && let Err(error) = open::that(self.settings.source(selected))
                        {
                            self.note = Some(error.to_string());
                        }
                        ui.label(
                            RichText::new("Rust · встроенный BitTorrent")
                                .size(11.0)
                                .color(MUTED),
                        );
                    });
                });
            });
    }
}

impl Drop for Launcher {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.maps.cancel_all();
        for state in &self.catalogs {
            state.cancel.cancel();
        }
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(3));
        }
    }
}
