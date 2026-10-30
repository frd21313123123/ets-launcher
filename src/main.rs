#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browser_smoke;
mod mod_browser;
mod quper_ui;
mod ui;
#[cfg(debug_assertions)]
mod ui_script;

use anyhow::Context;

use eframe::egui::{
    self, Align, Color32, Layout, Margin, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2,
};
use ets_launcher::{
    SOURCE_PAGE,
    catalog::{CatalogCache, CatalogState, REFRESH_INTERVAL, RETRY_INTERVAL},
    download::{self, SharedTransfer, Source, Status, Transfer},
    format_bytes, game,
    game::{Channel, Game},
    install, playground,
    settings::{Settings, Theme},
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;
use ui::{Button, Icon, Kind, Palette, W};

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
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([1000.0, 660.0])
            .with_title("ETS & ATS Launcher")
            .with_icon(app_icon.clone()),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "ETS & ATS Launcher",
        native_options,
        Box::new(move |cc| {
            let settings = Settings::load();
            let palette = ui::palette(settings.theme);
            ui::install_fonts(&cc.egui_ctx);
            ui::apply_theme(&cc.egui_ctx, &palette);
            let mut rgba = app_icon.rgba.clone();
            ui::tint_logo(&mut rgba);
            let logo = cc.egui_ctx.load_texture(
                "truck-launcher-logo",
                egui::ColorImage::from_rgba_unmultiplied(
                    [app_icon.width as usize, app_icon.height as usize],
                    &rgba,
                ),
                egui::TextureOptions::LINEAR,
            );
            let stripes = stripes_texture(&cc.egui_ctx, &palette);
            let cache = CatalogCache::load();
            let catalogs = std::array::from_fn(|index| {
                let game = Game::ALL[index];
                CatalogState::new(cache.get(game, settings.source(game)))
            });
            let source_editor = settings.source(settings.selected_game).to_owned();
            let (catalog_sender, catalog_receiver) = mpsc::channel();
            let (game_sender, game_receiver) = mpsc::channel();
            let (closed_sender, closed_receiver) = mpsc::channel();
            let quper = quper_ui::QuperUi::new(&settings);
            let mut launcher = Launcher {
                runtime: Some(runtime),
                settings,
                state: Arc::new(Mutex::new(Transfer::default())),
                cancel: CancellationToken::new(),
                close_when_idle: false,
                note: None,
                searching: [false; 2],
                search_generation: [0; 2],
                game_sender,
                game_receiver,
                closed: [None, None],
                closed_checking: false,
                closed_sender,
                closed_receiver,
                next_closed_check: Instant::now(),
                cache,
                catalogs,
                catalog_sender,
                catalog_receiver,
                source_editor,
                logo,
                stripes,
                palette,
                quper,
                page: Page::Dlc,
                stage: 0,
                #[cfg(debug_assertions)]
                script: ui_script::Script::from_env(),
            };
            for game in Game::ALL {
                launcher.search_game(&cc.egui_ctx, game, true);
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

fn stripes_texture(ctx: &egui::Context, palette: &Palette) -> egui::TextureHandle {
    ctx.load_texture(
        "placeholder-stripes",
        ui::stripe_tile(palette),
        egui::TextureOptions {
            wrap_mode: egui::TextureWrapMode::Repeat,
            ..egui::TextureOptions::NEAREST
        },
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Dlc,
    Quper,
    Settings,
}

type GameSearch = (Game, u64, Option<PathBuf>);

struct Launcher {
    logo: egui::TextureHandle,
    stripes: egui::TextureHandle,
    palette: Palette,
    runtime: Option<Runtime>,
    settings: Settings,
    state: SharedTransfer,
    cancel: CancellationToken,
    close_when_idle: bool,
    note: Option<String>,
    searching: [bool; 2],
    search_generation: [u64; 2],
    game_sender: mpsc::Sender<GameSearch>,
    game_receiver: mpsc::Receiver<GameSearch>,
    /// Result of the last "is the game running" check per game.
    closed: [Option<Result<(), String>>; 2],
    closed_checking: bool,
    closed_sender: mpsc::Sender<(Game, Result<(), String>)>,
    closed_receiver: mpsc::Receiver<(Game, Result<(), String>)>,
    next_closed_check: Instant,
    cache: CatalogCache,
    catalogs: [CatalogState; 2],
    catalog_sender: mpsc::Sender<CatalogUpdate>,
    catalog_receiver: mpsc::Receiver<CatalogUpdate>,
    source_editor: String,
    quper: quper_ui::QuperUi,
    page: Page,
    /// Last installation stage reached, kept to show where an error happened.
    stage: usize,
    #[cfg(debug_assertions)]
    script: Option<ui_script::Script>,
}

struct CatalogUpdate {
    game: Game,
    source: String,
    generation: u64,
    result: Result<playground::Catalog, String>,
}

fn stage_of(status: &Status) -> Option<usize> {
    match status {
        Status::Connecting => Some(0),
        Status::Downloading => Some(1),
        Status::Extracting => Some(2),
        Status::Installing | Status::RollingBack => Some(3),
        Status::Complete => Some(4),
        _ => None,
    }
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

    fn search_game(&mut self, ctx: &egui::Context, game: Game, use_saved: bool) {
        let index = game.index();
        self.search_generation[index] += 1;
        self.searching[index] = true;
        let generation = self.search_generation[index];
        let saved = if use_saved {
            self.settings
                .directory(game)
                .map(std::path::Path::to_path_buf)
        } else {
            None
        };
        let (sender, ctx) = (self.game_sender.clone(), ctx.clone());
        self.runtime.as_ref().unwrap().spawn_blocking(move || {
            let _ = sender.send((game, generation, game::detect(game, saved.as_deref())));
            ctx.request_repaint();
        });
    }

    fn poll_game_search(&mut self) {
        while let Ok((game, generation, path)) = self.game_receiver.try_recv() {
            if generation != self.search_generation[game.index()] {
                continue;
            }
            self.searching[game.index()] = false;
            self.settings.set_directory(game, path);
            if let Err(error) = self.settings.save() {
                self.note = Some(error.to_string());
            }
        }
    }

    fn pick_game_folder(&mut self, game: Game) {
        let Some(folder) = rfd::FileDialog::new()
            .set_title(format!("Папка {}", game.name()))
            .pick_folder()
        else {
            return;
        };
        if game::is_installation(game, &folder) {
            // A manual choice wins over a search that is still running.
            self.search_generation[game.index()] += 1;
            self.searching[game.index()] = false;
            self.settings.set_directory(game, Some(folder));
            self.note = self.settings.save().err().map(|e| e.to_string());
        } else {
            self.note = Some(format!(
                "В папке не найден bin/win_x64/{}.",
                game.executable()
            ));
        }
    }

    fn check_closed(&mut self, ctx: &egui::Context) {
        if self.closed_checking || Instant::now() < self.next_closed_check {
            return;
        }
        self.closed_checking = true;
        let game = self.settings.selected_game;
        let (sender, ctx) = (self.closed_sender.clone(), ctx.clone());
        self.runtime.as_ref().unwrap().spawn_blocking(move || {
            let result = install::ensure_game_closed(game).map_err(|e| format!("{e:#}"));
            let _ = sender.send((game, result));
            ctx.request_repaint();
        });
    }

    fn poll_closed(&mut self) {
        while let Ok((game, result)) = self.closed_receiver.try_recv() {
            self.closed_checking = false;
            self.closed[game.index()] = Some(result);
            self.next_closed_check = Instant::now() + Duration::from_secs(4);
        }
    }

    fn select_game(&mut self, ctx: &egui::Context, game: Game) {
        if self.settings.selected_game == game {
            return;
        }
        self.settings.selected_game = game;
        self.note = None;
        self.source_editor = self.settings.source(game).to_owned();
        download::set_state(&self.state, |s| *s = Transfer::default());
        self.stage = 0;
        self.next_closed_check = Instant::now();
        if self.settings.directory(game).is_none() && !self.searching[game.index()] {
            self.search_game(ctx, game, true);
        }
        if let Err(error) = self.settings.save() {
            self.note = Some(error.to_string());
        }
    }

    fn set_theme(&mut self, ctx: &egui::Context, theme: Theme) {
        if self.settings.theme == theme {
            return;
        }
        self.settings.theme = theme;
        self.palette = ui::palette(theme);
        ui::apply_theme(ctx, &self.palette);
        self.stripes = stripes_texture(ctx, &self.palette);
        if let Err(error) = self.settings.save() {
            self.note = Some(error.to_string());
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
        self.stage = 0;
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

    fn game_status(&self, game: Game) -> &'static str {
        if self.searching[game.index()] && self.settings.directory(game).is_none() {
            "Поиск папки…"
        } else if self.settings.directory(game).is_some() {
            "Папка найдена"
        } else {
            "Папка не найдена"
        }
    }

    // ───────────────────────── Sidebar ─────────────────────────

    fn sidebar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, locked: bool) {
        let p = self.palette;
        // Theme and source stick to the bottom of the sidebar.
        egui::TopBottomPanel::bottom("navigation-footer")
            .frame(egui::Frame::new())
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 16.0;
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(ui::overline("ТЕМА", &p));
                    });
                    if let Some(theme) = self.theme_picker(ui) {
                        self.set_theme(ctx, theme);
                    }
                });
                egui::Frame::new()
                    .fill(p.panel)
                    .stroke(Stroke::new(1.0_f32, p.line_soft))
                    .corner_radius(12)
                    .inner_margin(14)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 8.0;
                        ui.label(ui::overline("ИСТОЧНИК", &p));
                        let (label, url, caption) = if self.page == Page::Quper {
                            (
                                "Quper Simulator",
                                ets_launcher::quper::SITE.to_owned(),
                                "Готовые сборки модов",
                            )
                        } else {
                            (
                                "PlayGround",
                                self.settings.source(self.settings.selected_game).to_owned(),
                                "Rust · встроенный BitTorrent",
                            )
                        };
                        if ui::link(ui, &p, label, 13.0, W::Semi, true).clicked()
                            && let Err(error) = open::that(url)
                        {
                            self.note = Some(error.to_string());
                        }
                        ui.label(ui::text(caption, 11.0, W::Regular, p.muted));
                    });
            });
        ui.spacing_mut().item_spacing.y = 24.0;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.add(
                egui::Image::new(&self.logo)
                    .fit_to_exact_size(Vec2::splat(40.0))
                    .corner_radius(10),
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(1.0);
                ui.label(ui::text("TRUCK", 17.0, W::Extra, p.accent));
                ui.label(ui::text("LAUNCHER", 12.0, W::Bold, p.muted));
            });
        });

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(ui::overline("ИГРА", &p));
            });
            ui.add_space(2.0);
            for game in Game::ALL {
                let selected = self.settings.selected_game == game;
                let enabled = selected || !locked;
                let response = self.game_option(ui, game, selected, enabled);
                if response.clicked() {
                    self.select_game(ctx, game);
                }
                if !enabled {
                    response.on_hover_text("Дождитесь завершения текущей операции");
                }
            }
        });

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(ui::overline("РАЗДЕЛЫ", &p));
            });
            ui.add_space(4.0);
            for (page, icon, label) in [
                (Page::Dlc, Icon::Download, "DLC карт"),
                (Page::Quper, Icon::Layers, "Сборки Quper"),
                (Page::Settings, Icon::Sliders, "Настройки"),
            ] {
                if self.nav_item(ui, icon, label, self.page == page).clicked() {
                    self.page = page;
                }
            }
        });
    }

    fn game_option(
        &self,
        ui: &mut egui::Ui,
        game: Game,
        selected: bool,
        enabled: bool,
    ) -> egui::Response {
        let p = self.palette;
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), 54.0),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let hovered = enabled && response.hovered() && !selected;
        let (fill, border) = if selected {
            (p.accent_sel, p.accent_border)
        } else if hovered {
            (p.raised, Color32::TRANSPARENT)
        } else {
            (Color32::TRANSPARENT, Color32::TRANSPARENT)
        };
        let painter = ui.painter();
        painter.rect(
            rect,
            10,
            fill,
            Stroke::new(1.0_f32, border),
            StrokeKind::Inside,
        );
        let badge = Rect::from_min_size(
            pos2(rect.min.x + 12.0, rect.center().y - 14.0),
            vec2(36.0, 28.0),
        );
        painter.rect_filled(badge, 6, if selected { p.accent } else { p.line_soft });
        ui::paint_text(
            ui,
            badge,
            game.short_name(),
            ui::font(11.0, W::Extra),
            if selected { p.on_accent } else { p.text3 },
        );
        let x = badge.max.x + 12.0;
        let name = match game {
            Game::Ets2 => "Euro Truck Sim 2",
            Game::Ats => "American Truck Sim",
        };
        painter.text(
            pos2(x, rect.center().y - 9.0),
            egui::Align2::LEFT_CENTER,
            name,
            ui::font(13.0, if selected { W::Bold } else { W::Semi }),
            if selected {
                p.text
            } else if enabled {
                p.text2
            } else {
                p.muted
            },
        );
        painter.text(
            pos2(x, rect.center().y + 9.0),
            egui::Align2::LEFT_CENTER,
            self.game_status(game),
            ui::font(11.0, W::Regular),
            p.muted,
        );
        if enabled && !selected {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            response
        }
    }

    fn nav_item(&self, ui: &mut egui::Ui, icon: Icon, label: &str, active: bool) -> egui::Response {
        let p = self.palette;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
        if active || response.hovered() {
            ui.painter().rect_filled(rect, 10, p.raised);
        }
        let color = if active { p.text } else { p.text3 };
        ui::paint_icon(
            ui.painter(),
            Rect::from_center_size(pos2(rect.min.x + 21.0, rect.center().y), Vec2::splat(18.0)),
            icon,
            if active { p.accent } else { color },
            2.0,
        );
        ui.painter().text(
            pos2(rect.min.x + 42.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            ui::font(14.0, if active { W::Bold } else { W::Semi }),
            color,
        );
        if active {
            ui.painter()
                .circle_filled(pos2(rect.max.x - 16.0, rect.center().y), 4.0, p.accent);
        }
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    fn theme_picker(&self, ui: &mut egui::Ui) -> Option<Theme> {
        let p = self.palette;
        let mut picked = None;
        egui::Frame::new()
            .fill(p.inset)
            .stroke(Stroke::new(1.0_f32, p.line_soft))
            .corner_radius(12)
            .inner_margin(4)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 4.0) / 2.0;
                    for (theme, label, swatch) in [
                        (Theme::Black, "Чёрная", Color32::BLACK),
                        (Theme::Light, "Светлая", Color32::from_rgb(0xF3, 0xE9, 0xDC)),
                    ] {
                        let active = self.settings.theme == theme;
                        let (rect, response) =
                            ui.allocate_exact_size(vec2(width, 48.0), Sense::click());
                        if active || response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                9,
                                if active { p.raised } else { p.hover },
                            );
                        }
                        let center = rect.center();
                        ui.painter().circle(
                            pos2(center.x, center.y - 8.0),
                            5.0,
                            swatch,
                            Stroke::new(1.0_f32, p.line_strong),
                        );
                        ui.painter().text(
                            pos2(center.x, center.y + 9.0),
                            egui::Align2::CENTER_CENTER,
                            label,
                            ui::font(11.0, if active { W::Extra } else { W::Semi }),
                            if active { p.text } else { p.text3 },
                        );
                        if response
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            picked = Some(theme);
                        }
                    }
                });
            });
        picked
    }

    // ───────────────────────── DLC page ─────────────────────────

    fn dlc_page(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        transfer: &Transfer,
        busy: bool,
    ) {
        let p = self.palette;
        let game = self.settings.selected_game;
        let state = &self.catalogs[game.index()];
        let (catalog, refreshing, catalog_error) =
            (state.catalog.clone(), state.refreshing, state.error.clone());

        ui.horizontal(|ui| {
            ui::page_title(ui, &p, game.name(), "DLC карт");
            ui.with_layout(Layout::right_to_left(Align::Max), |ui| {
                // The group sits on the title baseline; its items are centred on each other.
                ui.horizontal(|ui| {
                    if ui::icon_button(ui, &p, Icon::Refresh, 44.0, !busy && !refreshing)
                        .on_hover_text("Обновить данные источника")
                        .clicked()
                    {
                        self.refresh_catalog(ctx, game, true);
                    }
                    if refreshing {
                        ui.label(ui::text("Проверяем источник…", 12.0, W::Regular, p.muted));
                        ui.add(egui::Spinner::new().size(14.0).color(p.accent));
                    } else {
                        let stamp = catalog
                            .as_ref()
                            .map(|c| format!("Данные проверены {}", ui::ago(c.checked_at)))
                            .unwrap_or_else(|| "Нет сохранённых данных".into());
                        ui.label(ui::text(stamp, 12.0, W::Regular, p.muted));
                        ui::dot(
                            ui,
                            if catalog_error.is_some() {
                                p.warn
                            } else {
                                p.accent
                            },
                            None,
                        );
                    }
                });
            });
        });

        if let Some(error) = &catalog_error {
            ui::banner(
                ui,
                &p,
                ui::Tone::Warn,
                &format!(
                    "Не удалось обновить данные PlayGround: {error}{}",
                    if catalog.is_some() {
                        ". Показаны сохранённые данные; перед установкой источник будет проверен заново."
                    } else {
                        ""
                    }
                ),
                None,
            );
        }
        if let Some(note) = self.note.clone()
            && ui::banner(ui, &p, ui::Tone::Warn, &note, Some("Скрыть"))
        {
            self.note = None;
        }

        let channel = self.settings.channel();
        let release = catalog
            .as_ref()
            .and_then(|c| c.select(channel).ok())
            .cloned();
        ui::card(&p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let wide = ui.available_width() >= 640.0;
            let left = if wide {
                (ui.available_width() - 24.0) * 0.56
            } else {
                ui.available_width()
            };
            let layout = if wide {
                Layout::left_to_right(Align::Min)
            } else {
                Layout::top_down(Align::Min)
            };
            ui.with_layout(layout, |ui| {
                ui.spacing_mut().item_spacing = vec2(24.0, 24.0);
                ui.allocate_ui_with_layout(vec2(left, 0.0), Layout::top_down(Align::Min), |ui| {
                    self.release_info(ui, catalog.as_ref(), release.as_ref(), refreshing, busy);
                });
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), 0.0),
                    Layout::top_down(Align::Min),
                    |ui| self.install_panel(ui, release.is_some(), busy),
                );
            });
        });

        if transfer.status != Status::Ready || self.close_when_idle {
            self.progress_card(ui, transfer, busy);
        }
        ui.label(ui::text(
            "При закрытии окна лаунчер сначала остановит загрузку и вернёт оригинальные файлы.",
            12.0,
            W::Regular,
            p.muted,
        ));
    }

    fn release_info(
        &mut self,
        ui: &mut egui::Ui,
        catalog: Option<&playground::Catalog>,
        release: Option<&playground::Release>,
        refreshing: bool,
        busy: bool,
    ) {
        let p = self.palette;
        let game = self.settings.selected_game;
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(ui::text("ВЕРСИЯ НА ИСТОЧНИКЕ", 12.0, W::Bold, p.muted));
        match release {
            Some(release) => {
                ui.label(ui::mono_text(
                    release.version.as_deref().unwrap_or("не указана"),
                    40.0,
                    true,
                    p.accent,
                ));
                ui.add(
                    egui::Label::new(ui::mono_text(
                        format!(
                            "{} · {}",
                            release.file.name,
                            release
                                .size_label
                                .as_deref()
                                .unwrap_or("размер уточнится при загрузке")
                        ),
                        13.0,
                        false,
                        p.muted,
                    ))
                    .wrap(),
                );
            }
            None => {
                ui.label(ui::mono_text("—", 40.0, true, p.muted));
                ui.add(
                    egui::Label::new(ui::text(
                        if refreshing {
                            "Проверяем актуальные файлы PlayGround…"
                        } else {
                            "Для выбранной ветки нет доступного архива"
                        },
                        13.0,
                        W::Regular,
                        p.muted,
                    ))
                    .wrap(),
                );
            }
        }
        ui.add_space(14.0);
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.label(ui::text("Ветка игры в Steam", 13.0, W::Semi, p.text2));
        let channels = [(Channel::Stable, "Обычная"), (Channel::Beta, "Beta")];
        let segments = channels
            .iter()
            .map(|&(value, label)| {
                let release = catalog.and_then(|c| c.select(value).ok());
                ui::Segment {
                    label,
                    extra: Some(match release.and_then(|r| r.version.clone()) {
                        Some(version) => (version, true, p.accent),
                        None if release.is_some() => ("доступна".into(), false, p.muted),
                        None => ("недоступна".into(), false, p.muted),
                    }),
                    enabled: release.is_some() && !busy,
                }
            })
            .collect::<Vec<_>>();
        let selected = channels
            .iter()
            .position(|(c, _)| *c == self.settings.channel())
            .unwrap_or(0);
        if let Some(index) = ui::segmented(ui, &p, selected, &segments, 40.0, 14.0, p.bg, 12)
            && index != selected
        {
            self.settings.set_channel(game, channels[index].0);
            self.note = None;
            download::set_state(&self.state, |s| *s = Transfer::default());
            if let Err(error) = self.settings.save() {
                self.note = Some(error.to_string());
            }
        }
        ui.label(ui::text(
            "Выберите ту же ветку, что установлена в Steam.",
            12.0,
            W::Regular,
            p.muted,
        ));
    }

    fn install_panel(&mut self, ui: &mut egui::Ui, has_release: bool, busy: bool) {
        let p = self.palette;
        let game = self.settings.selected_game;
        let directory = self
            .settings
            .directory(game)
            .map(|d| d.display().to_string());
        let searching = self.searching[game.index()];
        ui::inset(&p).inner_margin(20).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
            let row = |ui: &mut egui::Ui,
                       icon: Option<(Icon, Color32)>,
                       title: &str,
                       detail: Option<&str>| {
                ui.horizontal_top(|ui| {
                    match icon {
                        Some((icon, color)) => {
                            ui::icon(ui, icon, 16.0, color, 2.4);
                        }
                        None => {
                            ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                        }
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add(egui::Label::new(ui::text(title, 13.0, W::Regular, p.text2)).wrap());
                        if let Some(detail) = detail {
                            ui.add(
                                egui::Label::new(ui::mono_text(detail, 11.0, false, p.muted))
                                    .wrap(),
                            );
                        }
                    });
                });
            };
            match &self.closed[game.index()] {
                Some(Ok(())) => row(ui, Some((Icon::Check, p.ok)), "Игра закрыта", None),
                Some(Err(message)) => row(ui, Some((Icon::Warning, p.warn)), message, None),
                None => row(ui, None, "Проверяем, закрыта ли игра…", None),
            }
            match (&directory, searching) {
                (Some(path), _) => row(
                    ui,
                    Some((Icon::Check, p.ok)),
                    "Папка игры найдена",
                    Some(path),
                ),
                (None, true) => row(ui, None, "Ищем папку игры…", None),
                (None, false) => {
                    row(
                        ui,
                        Some((Icon::Warning, p.warn)),
                        "Папка игры не найдена",
                        None,
                    );
                    ui.horizontal(|ui| {
                        ui.add_space(26.0);
                        if ui::link(ui, &p, "Указать в настройках", 12.0, W::Semi, false).clicked()
                        {
                            self.page = Page::Settings;
                        }
                    });
                }
            }
            row(
                ui,
                Some((Icon::Shield, p.accent)),
                "Оригиналы сохраняются в резервную копию",
                None,
            );
            ui.add_space(4.0);
            let ready = has_release && directory.is_some() && !searching;
            let response = Button::new(
                if busy {
                    "Идёт установка…"
                } else {
                    "Установить DLC"
                },
                Kind::Primary,
            )
            .icon(Icon::Download)
            .height(52.0)
            .size(16.0)
            .radius(12)
            .full_width()
            .enabled(!busy && ready)
            .show(ui, &p);
            if response.clicked() {
                self.start_install();
            } else if !busy && !ready {
                response.on_hover_text(if directory.is_none() {
                    "Укажите папку игры в настройках"
                } else if !has_release {
                    "Нет архива для выбранной ветки — обновите данные источника"
                } else {
                    "Идёт поиск папки игры"
                });
            }
        });
    }

    fn progress_card(&mut self, ui: &mut egui::Ui, transfer: &Transfer, busy: bool) {
        let p = self.palette;
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
        let subtitle = if self.close_when_idle {
            "Завершаем остановку и восстановление файлов перед закрытием…".to_owned()
        } else if transfer.status == Status::Downloading {
            "Части проверяются по хешам — после перезапуска загрузка продолжится".to_owned()
        } else {
            transfer.message.clone()
        };
        ui::card(&p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 20.0;
            let stop = Button::new("Остановить", Kind::Danger)
                .icon(Icon::Stop)
                .pad(18.0)
                .enabled(!self.cancel.is_cancelled());
            let open_folder = Button::new("Открыть папку игры", Kind::Ghost).icon(Icon::Folder);
            let can_stop = busy && transfer.status != Status::RollingBack;
            let can_open = !busy && transfer.status == Status::Complete;
            let actions = if can_stop {
                stop.width(ui)
            } else if can_open {
                open_folder.width(ui)
            } else {
                0.0
            };
            ui::split_row(
                ui,
                actions,
                |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.horizontal(|ui| {
                        if busy {
                            ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                        }
                        let color = match transfer.status {
                            Status::Complete => p.ok,
                            Status::Error => p.danger_text,
                            _ => p.text,
                        };
                        ui.label(ui::text(heading, 20.0, W::Extra, color));
                    });
                    ui.add(
                        egui::Label::new(ui::text(
                            &subtitle,
                            13.0,
                            W::Regular,
                            if transfer.status == Status::Error {
                                p.danger_text
                            } else {
                                p.muted
                            },
                        ))
                        .wrap(),
                    );
                },
                |ui| {
                    if can_stop {
                        if stop.show(ui, &p).clicked() {
                            self.cancel.cancel();
                        }
                    } else if can_open
                        && open_folder.show(ui, &p).clicked()
                        && let Some(path) = &transfer.saved_to
                        && let Err(error) = open::that(path)
                    {
                        self.note = Some(error.to_string());
                    }
                },
            );

            // Stage strip: Торрент → Загрузка → Распаковка → Установка.
            let current = stage_of(&transfer.status).unwrap_or(self.stage);
            let failed = matches!(transfer.status, Status::Error | Status::Cancelled);
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.columns(4, |columns| {
                for (index, (column, label)) in columns
                    .iter_mut()
                    .zip(["Торрент", "Загрузка", "Распаковка", "Установка"])
                    .enumerate()
                {
                    column.spacing_mut().item_spacing.y = 8.0;
                    let (bar, _) = column
                        .allocate_exact_size(vec2(column.available_width(), 4.0), Sense::hover());
                    let done = index < current;
                    let active = index == current && !failed;
                    column
                        .painter()
                        .rect_filled(bar, 2, if done { p.accent } else { p.line });
                    if active {
                        let fraction = if index == 1 || index == 3 {
                            transfer.progress().max(0.04)
                        } else {
                            0.5
                        };
                        column.painter().rect_filled(
                            Rect::from_min_size(bar.min, vec2(bar.width() * fraction, 4.0)),
                            2,
                            p.accent,
                        );
                    }
                    column.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if done {
                            ui::icon(ui, Icon::Check, 14.0, p.ok, 3.0);
                            ui.label(ui::text(label, 13.0, W::Bold, p.text2));
                        } else if index == current && failed {
                            ui::dot(ui, p.danger_text, None);
                            ui.label(ui::text(label, 13.0, W::Extra, p.danger_text));
                        } else if active {
                            ui::dot(ui, p.accent, Some(p.accent_glow));
                            ui.label(ui::text(label, 13.0, W::Extra, p.text));
                        } else {
                            ui.label(ui::text(label, 13.0, W::Semi, p.muted));
                        }
                    });
                }
            });

            let measurable = transfer.total.is_some()
                && matches!(
                    transfer.status,
                    Status::Downloading | Status::Installing | Status::Complete
                );
            if measurable {
                let fraction = if transfer.status == Status::Complete {
                    1.0
                } else {
                    transfer.progress()
                };
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label(ui::mono_text(
                            format!("{}", (fraction * 100.0).floor() as u32),
                            36.0,
                            true,
                            p.text,
                        ));
                        ui.label(ui::mono_text("%", 20.0, true, p.muted));
                        if transfer.status == Status::Downloading
                            && let Some(total) = transfer.total
                        {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let left = total.saturating_sub(transfer.downloaded);
                                let eta = match left.checked_div(transfer.speed) {
                                    None => "оценка времени…".to_owned(),
                                    Some(seconds) if seconds < 60 => "осталось < 1 мин".into(),
                                    Some(seconds) => {
                                        format!("осталось ≈ {} мин", seconds.div_ceil(60))
                                    }
                                };
                                ui.label(ui::text(eta, 13.0, W::Regular, p.muted));
                            });
                        }
                    });
                    ui::progress(ui, &p, fraction, 12.0, p.accent);
                });
            }
            if transfer.status == Status::Downloading {
                ui.spacing_mut().item_spacing.x = 12.0;
                let total = transfer
                    .total
                    .map(format_bytes)
                    .unwrap_or_else(|| "…".into());
                let stats = [
                    (
                        "Скачано",
                        format!("{} / {total}", format_bytes(transfer.downloaded)),
                    ),
                    ("Скорость", format!("{}/с", format_bytes(transfer.speed))),
                    ("Пиры", transfer.peers.to_string()),
                ];
                ui.columns(3, |columns| {
                    for (column, (label, value)) in columns.iter_mut().zip(stats) {
                        ui::inset(&p)
                            .inner_margin(Margin::symmetric(16, 14))
                            .show(column, |ui| {
                                ui.set_width(ui.available_width());
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.label(ui::text(label, 12.0, W::Regular, p.muted));
                                ui.label(ui::mono_text(value, 16.0, true, p.text));
                            });
                    }
                });
            }
        });
    }

    // ───────────────────────── Settings page ─────────────────────────

    fn settings_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, busy: bool) {
        let p = self.palette;
        let game = self.settings.selected_game;
        ui::page_title(
            ui,
            &p,
            &format!("{} · пути ETS2 и ATS хранятся отдельно", game.name()),
            "Настройки",
        );
        if let Some(note) = self.note.clone()
            && ui::banner(ui, &p, ui::Tone::Warn, &note, Some("Скрыть"))
        {
            self.note = None;
        }
        if busy {
            ui::banner(
                ui,
                &p,
                ui::Tone::Info,
                "Настройки станут доступны после завершения текущей загрузки или установки.",
                None,
            );
        }
        egui::Frame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.line))
            .corner_radius(16)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                let enabled = !busy;

                // Game folder.
                let searching = self.searching[game.index()];
                let directory = self
                    .settings
                    .directory(game)
                    .map(|d| d.display().to_string());
                let mut search = false;
                let mut pick = false;
                let find = Button::new("Найти автоматически", Kind::Ghost)
                    .height(40.0)
                    .size(13.0)
                    .pad(14.0)
                    .enabled(enabled && !searching);
                let choose = Button::new("Выбрать…", Kind::Raised)
                    .height(40.0)
                    .size(13.0)
                    .pad(14.0)
                    .enabled(enabled);
                let actions = find.width(ui) + choose.width(ui) + 8.0;
                settings_row(ui, &p, true, |ui| {
                    ui::split_row(
                        ui,
                        actions,
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.label(ui::text("Папка игры", 15.0, W::Bold, p.text));
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                if searching {
                                    ui.add(egui::Spinner::new().size(14.0).color(p.accent));
                                    ui.label(ui::text(
                                        "Ищем установку в Steam…",
                                        12.0,
                                        W::Bold,
                                        p.muted,
                                    ));
                                } else if directory.is_some() {
                                    ui::icon(ui, Icon::Check, 14.0, p.ok, 3.0);
                                    ui.label(ui::text(
                                        format!("Найден bin/win_x64/{}", game.executable()),
                                        12.0,
                                        W::Bold,
                                        p.ok,
                                    ));
                                } else {
                                    ui::icon(ui, Icon::Warning, 14.0, p.warn, 2.4);
                                    ui.label(ui::text(
                                        "Папка не найдена — выберите её вручную",
                                        12.0,
                                        W::Bold,
                                        p.warn,
                                    ));
                                }
                            });
                            if let Some(path) = &directory {
                                ui.add(
                                    egui::Label::new(ui::mono_text(path, 12.0, false, p.text2))
                                        .wrap(),
                                );
                            }
                        },
                        |ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            search = find.show(ui, &p).clicked();
                            pick = choose.show(ui, &p).clicked();
                        },
                    );
                });
                if search {
                    self.search_game(ctx, game, false);
                }
                if pick {
                    self.pick_game_folder(game);
                }

                // PlayGround source page.
                let mut apply = false;
                let mut reset = false;
                settings_row(ui, &p, true, |ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
                    ui.label(ui::text(
                        "Страница источника PlayGround",
                        15.0,
                        W::Bold,
                        p.text,
                    ));
                    ui.add(
                        egui::Label::new(ui::text(
                            "Замените, если автор выпустил новую публикацию. Перенаправления со старого адреса обрабатываются автоматически.",
                            12.0,
                            W::Regular,
                            p.muted,
                        ))
                        .wrap(),
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let width = (ui.available_width() - 228.0).max(200.0);
                        ui.add_enabled_ui(enabled, |ui| {
                            ui::field(
                                ui,
                                &p,
                                &mut self.source_editor,
                                "https://www.playground.ru/…",
                                true,
                                width,
                                None,
                                p.bg,
                            );
                        });
                        apply = Button::new("Применить", Kind::Primary)
                            .size(13.0)
                            .enabled(enabled)
                            .show(ui, &p)
                            .clicked();
                        reset = Button::new("Сбросить", Kind::Ghost)
                            .size(13.0)
                            .pad(14.0)
                            .enabled(enabled)
                            .show(ui, &p)
                            .clicked();
                    });
                });
                if apply {
                    match self.settings.set_source(game, &self.source_editor) {
                        Ok(()) => {
                            self.note = self.settings.save().err().map(|e| e.to_string());
                            self.source_editor = self.settings.source(game).to_owned();
                            self.refresh_catalog(ctx, game, true);
                        }
                        Err(error) => self.note = Some(error.to_string()),
                    }
                }
                if reset {
                    let _ = self.settings.set_source(game, "");
                    self.source_editor = self.settings.source(game).to_owned();
                    self.note = self.settings.save().err().map(|e| e.to_string());
                    self.refresh_catalog(ctx, game, true);
                }

                // Download cache.
                let cache = self.settings.download_directory(game).display().to_string();
                if path_row(
                    ui,
                    &p,
                    "Кэш загрузок",
                    Some("Готовый архив остаётся здесь после установки"),
                    &cache,
                    enabled,
                ) && let Some(folder) = rfd::FileDialog::new()
                    .set_title("Папка кэша загрузок")
                    .set_directory(&self.settings.output)
                    .pick_folder()
                {
                    self.settings.output = folder;
                    self.note = self.settings.save().err().map(|e| e.to_string());
                }

                // Quper library.
                let library = quper_ui::QuperUi::root(&self.settings)
                    .display()
                    .to_string();
                if path_row(
                    ui,
                    &p,
                    "Папка сборок Quper",
                    None,
                    &library,
                    enabled && !self.quper.preparing(),
                ) && let Some(folder) = rfd::FileDialog::new()
                    .set_title("Папка готовых сборок Quper")
                    .pick_folder()
                {
                    self.settings.quper_library = Some(folder);
                    self.note = self.settings.save().err().map(|e| e.to_string());
                    self.quper.library_changed();
                }

                settings_row(ui, &p, false, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        ui::icon(ui, Icon::Shield, 20.0, p.accent, 2.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.label(ui::text("Резервные копии", 15.0, W::Bold, p.text));
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.label(ui::text(
                                    "Оригиналы заменяемых файлов сохраняются в ",
                                    12.0,
                                    W::Regular,
                                    p.muted,
                                ));
                                ui.label(ui::mono_text(
                                    ".ets-launcher/backups",
                                    12.0,
                                    false,
                                    p.text2,
                                ));
                                ui.label(ui::text(
                                    " внутри папки игры. При отмене или ошибке они восстанавливаются автоматически.",
                                    12.0,
                                    W::Regular,
                                    p.muted,
                                ));
                            });
                        });
                    });
                });
            });
    }
}

/// One row of the settings card with the divider below it.
fn settings_row(
    ui: &mut egui::Ui,
    p: &Palette,
    divider: bool,
    content: impl FnOnce(&mut egui::Ui),
) {
    let response = egui::Frame::new()
        .inner_margin(Margin::symmetric(22, 20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            content(ui);
        })
        .response;
    if divider {
        ui.painter().hline(
            response.rect.x_range(),
            response.rect.bottom(),
            Stroke::new(1.0_f32, p.line_soft),
        );
    }
}

/// A settings row showing a folder with an "Изменить…" button; returns true when pressed.
fn path_row(
    ui: &mut egui::Ui,
    p: &Palette,
    title: &str,
    caption: Option<&str>,
    path: &str,
    enabled: bool,
) -> bool {
    let button = Button::new("Изменить…", Kind::Raised)
        .height(40.0)
        .size(13.0)
        .pad(14.0)
        .enabled(enabled);
    let width = button.width(ui);
    let mut clicked = false;
    settings_row(ui, p, true, |ui| {
        ui::split_row(
            ui,
            width,
            |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(ui::text(title, 15.0, W::Bold, p.text));
                if let Some(caption) = caption {
                    ui.label(ui::text(caption, 12.0, W::Regular, p.muted));
                }
                ui.add(egui::Label::new(ui::mono_text(path, 12.0, false, p.text2)).wrap());
            },
            |ui| clicked = button.show(ui, p).clicked(),
        );
    });
    clicked
}

impl eframe::App for Launcher {
    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(script) = &mut self.script {
            script.input(ctx, raw_input);
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.poll_game_search();
        self.poll_catalogs();
        self.poll_closed();
        let transfer = download::snapshot(&self.state);
        if let Some(stage) = stage_of(&transfer.status) {
            self.stage = stage;
        }
        self.quper.tick(
            ctx,
            self.runtime.as_ref().unwrap(),
            &self.settings,
            transfer.active(),
        );
        let busy = transfer.active() || self.quper.busy();
        if !busy {
            for game in Game::ALL {
                self.refresh_catalog(ctx, game, false);
            }
            if self.page == Page::Dlc {
                self.check_closed(ctx);
            }
        }
        ctx.request_repaint_after(Duration::from_secs(1));
        if ctx.input(|i| i.viewport().close_requested()) && busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_when_idle = true;
            self.cancel.cancel();
            self.quper.cancel_all();
        }
        if self.close_when_idle && !busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if busy || self.searching.iter().any(|s| *s) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        if self.page != Page::Quper {
            self.quper.suspend_browser();
        }
        let p = self.palette;
        let locked = transfer.active() || self.quper.preparing();
        egui::SidePanel::left("navigation")
            .exact_width(248.0)
            .resizable(false)
            .show_separator_line(true)
            .frame(
                egui::Frame::new()
                    .fill(p.side)
                    .inner_margin(Margin::symmetric(16, 24)),
            )
            .show(ctx, |ui| self.sidebar(ui, ctx, locked));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg))
            .show(ctx, |ui| {
                if self.page == Page::Quper && self.quper.browser_visible() {
                    egui::Frame::new()
                        .inner_margin(Margin {
                            left: 40,
                            right: 40,
                            top: 28,
                            bottom: 20,
                        })
                        .show(ui, |ui| {
                            self.quper.show(
                                ui,
                                ctx,
                                frame,
                                self.runtime.as_ref().unwrap(),
                                &mut self.settings,
                                transfer.active(),
                                &p,
                                &self.stripes,
                            );
                        });
                    return;
                }
                let (id, max_width, top) = match self.page {
                    Page::Dlc => ("dlc-page", 940.0_f32, 32),
                    Page::Quper if self.quper.has_selection() => ("quper-build", 1000.0, 28),
                    Page::Quper => ("quper-page", 1000.0, 32),
                    Page::Settings => ("settings-page", 820.0, 32),
                };
                egui::ScrollArea::vertical()
                    .id_salt(id)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(Margin {
                                left: 40,
                                right: 40,
                                top,
                                bottom: 40,
                            })
                            .show(ui, |ui| {
                                ui.set_max_width(max_width.min(ui.available_width()));
                                ui.spacing_mut().item_spacing.y = 20.0;
                                match self.page {
                                    Page::Dlc => {
                                        self.dlc_page(ui, ctx, &transfer, transfer.active())
                                    }
                                    Page::Quper => {
                                        if self.quper.show(
                                            ui,
                                            ctx,
                                            frame,
                                            self.runtime.as_ref().unwrap(),
                                            &mut self.settings,
                                            transfer.active(),
                                            &p,
                                            &self.stripes,
                                        ) {
                                            self.page = Page::Settings;
                                        }
                                    }
                                    Page::Settings => self.settings_page(ui, ctx, busy),
                                }
                            });
                    });
            });
    }
}

impl Drop for Launcher {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.quper.cancel_all();
        for state in &self.catalogs {
            state.cancel.cancel();
        }
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(3));
        }
    }
}
