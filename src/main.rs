#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2};
use ets_launcher::{
    SOURCE_PAGE,
    download::{self, SharedTransfer, Source, Status, Transfer},
    format_bytes, game, install,
    settings::Settings,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
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
    // Headless diagnostic: always requires an explicitly supplied archive and game directory.
    if args.get(1).is_some_and(|arg| arg == "--install-archive") {
        anyhow::ensure!(
            args.len() == 5,
            "--install-archive <archive> <game directory> <report file>"
        );
        let state = Arc::new(Mutex::new(Transfer::default()));
        let result = Runtime::new()?.block_on(async {
            install::install_archive(
                install::prepare(std::path::Path::new(&args[3]))?,
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
            Source::Playground { torrent_only: true },
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
            game::detect(None)
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
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([840.0, 640.0])
            .with_min_inner_size([720.0, 540.0])
            .with_title("ETS Launcher"),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "ETS Launcher",
        native_options,
        Box::new(move |cc| {
            configure(&cc.egui_ctx);
            let mut launcher = Launcher {
                runtime: Some(runtime),
                settings: Settings::load(),
                state: Arc::new(Mutex::new(Transfer::default())),
                cancel: CancellationToken::new(),
                close_when_idle: false,
                note: None,
                game_search: None,
            };
            launcher.search_game(&cc.egui_ctx, true);
            Ok(Box::new(launcher))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Не удалось открыть окно: {error}"))
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
    runtime: Option<Runtime>,
    settings: Settings,
    state: SharedTransfer,
    cancel: CancellationToken,
    close_when_idle: bool,
    note: Option<String>,
    game_search: Option<mpsc::Receiver<Option<PathBuf>>>,
}

impl Launcher {
    fn search_game(&mut self, ctx: &egui::Context, use_saved: bool) {
        let (sender, receiver) = mpsc::channel();
        self.game_search = Some(receiver);
        let saved = if use_saved {
            self.settings.game_dir.clone()
        } else {
            None
        };
        let ctx = ctx.clone();
        self.runtime.as_ref().unwrap().spawn_blocking(move || {
            let _ = sender.send(game::detect(saved.as_deref()));
            ctx.request_repaint();
        });
    }

    fn poll_game_search(&mut self) {
        match self.game_search.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(path)) => {
                self.game_search = None;
                self.settings.game_dir = path;
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
        let Some(game) = self.settings.game_dir.clone() else {
            self.note = Some("Не найдена ETS2. Укажите папку игры в настройках.".into());
            return;
        };
        self.note = None;
        self.cancel = CancellationToken::new();
        download::set_state(&self.state, |s| {
            *s = Transfer {
                status: Status::Connecting,
                name: "DLC Maps".into(),
                message: "Подготовка к установке…".into(),
                ..Default::default()
            }
        });
        if let Err(error) = self.settings.save() {
            self.note = Some(error.to_string());
        }
        self.runtime.as_ref().unwrap().spawn(install::run(
            self.settings.output.clone(),
            game,
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_game_search();
        let transfer = download::snapshot(&self.state);
        let busy = transfer.active();
        if ctx.input(|i| i.viewport().close_requested()) && busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_when_idle = true;
            self.cancel.cancel();
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
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("ETS").size(28.0).strong().color(GREEN));
                        ui.label(RichText::new("LAUNCHER").size(16.0).strong());
                    });
                    ui.label(RichText::new("Euro Truck Simulator 2").color(MUTED));
                    ui.add_space(16.0);
                    card().show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(RichText::new("DLC Maps").size(34.0).strong());
                        ui.label(
                            RichText::new("Скачивание и установка дополнений одной кнопкой")
                                .color(MUTED),
                        );
                        ui.add_space(14.0);
                        let game_label = if self.game_search.is_some() {
                            "Поиск игры…".into()
                        } else {
                            self.settings
                                .game_dir
                                .as_ref()
                                .map(|p| format!("ETS2: {}", p.display()))
                                .unwrap_or_else(|| {
                                    "ETS2 не найдена — выберите папку в настройках".into()
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
                                    && self.settings.game_dir.is_some()
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
                                if ui.button("Выбрать папку ETS2").clicked()
                                    && let Some(folder) = rfd::FileDialog::new()
                                        .set_title("Папка Euro Truck Simulator 2")
                                        .pick_folder()
                                {
                                    if game::is_installation(&folder) {
                                        self.game_search = None;
                                        self.settings.game_dir = Some(folder);
                                    } else {
                                        self.note = Some(
                                            "В папке не найден bin/win_x64/eurotrucks2.exe.".into(),
                                        );
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
                                    self.settings.output.display()
                                ))
                                .size(12.0)
                                .color(MUTED),
                            );
                            if ui.button("Изменить папку загрузки").clicked()
                                && let Some(folder) = rfd::FileDialog::new()
                                    .set_directory(&self.settings.output)
                                    .pick_folder()
                            {
                                self.settings.output = folder;
                            }
                            if let Err(error) = self.settings.save() {
                                self.note = Some(error.to_string());
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
                            && let Err(error) = open::that(SOURCE_PAGE)
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
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(3));
        }
    }
}
