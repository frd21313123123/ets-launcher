//! Diagnostic for the native download callbacks. It neither logs into a site nor
//! touches the user's mod library; all output goes to an explicitly new folder.
use crate::mod_browser::{Browser, Event};
use anyhow::{Context, Result, ensure};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

pub fn run(root: PathBuf) -> Result<()> {
    let entry = serde_json::from_value(serde_json::json!({
        "id":"browser-smoke","name":"Проверка браузерной загрузки","region":"QA","author":"GitHub","description":"Public test archive",
        "source":{"kind":"truckymods","url":ets_launcher::TEST_URL,"selector":"body","version_pattern":"(1)"},
        "release":{"version":"1","game_versions":["1"],"packages":[{"pattern":".*"}]}
    }))?;
    run_entry(root, entry, 60, None, false)
}

pub fn run_map(root: PathBuf, id: &str) -> Result<()> {
    let catalog = ets_launcher::maps::Catalog::bundled()?;
    let mut entry = catalog.entry(id)?.clone();
    let prepared = tokio::runtime::Runtime::new()?.block_on(
        ets_launcher::maps::source::prepare(&entry, &tokio_util::sync::CancellationToken::new()),
    )?;
    entry.source.browser_url = Some(prepared.page.clone());
    run_entry(root, entry, 1800, Some(prepared), false)
}

pub fn run_public(root: PathBuf, url: &str) -> Result<()> {
    ets_launcher::quper::source::public_url(url)?;
    let entry = serde_json::from_value(serde_json::json!({
        "id":"quper-file-smoke", "name":"Проверка бесплатной загрузки Quper", "region":"QA", "author":"Quper", "description":"Public download",
        "source":{"kind":"quper", "url":url, "selector":"main", "version_pattern":"(1)"},
        "release":{"version":"1", "game_versions":[], "packages":[]}
    }))?;
    run_entry(root, entry, 180, None, true)
}

fn run_entry(
    root: PathBuf,
    entry: ets_launcher::maps::MapEntry,
    seconds: u64,
    prepared: Option<ets_launcher::maps::source::PreparedDownload>,
    automatic: bool,
) -> Result<()> {
    ensure!(!root.exists(), "Папка диагностики уже существует");
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let (tx, rx) = mpsc::channel();
    let visible = prepared.is_some() || automatic;
    let app = Smoke {
        root: root.clone(),
        entry,
        tx,
        rx,
        browser: Browser::default(),
        started: false,
        deadline: Instant::now() + Duration::from_secs(seconds),
        prepared,
        automatic,
        next_inspection: Instant::now() + Duration::from_secs(8),
    };
    eframe::run_native(
        "Browser download diagnostic",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(if visible {
                    [1100.0, 780.0]
                } else {
                    [700.0, 400.0]
                })
                .with_visible(visible),
            ..Default::default()
        },
        Box::new(move |cc| {
            crate::configure(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let report: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("result.json")).context("Нет отчёта браузера")?,
    )?;
    ensure!(
        report["ok"] == true,
        "Проверка браузера: {}",
        report["message"]
    );
    Ok(())
}

struct Smoke {
    root: PathBuf,
    entry: ets_launcher::maps::MapEntry,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    browser: Browser,
    started: bool,
    deadline: Instant,
    prepared: Option<ets_launcher::maps::source::PreparedDownload>,
    automatic: bool,
    next_inspection: Instant,
}

impl Smoke {
    fn finish(&mut self, ctx: &egui::Context, result: Result<PathBuf>) {
        let report = match result {
            Ok(path) => {
                serde_json::json!({"ok":true,"filename":path.file_name().unwrap().to_string_lossy(),"bytes":std::fs::metadata(path).map(|m|m.len()).unwrap_or(0)})
            }
            Err(e) => serde_json::json!({"ok":false,"message":format!("{e:#}")}),
        };
        let _ = ets_launcher::maps::write_json(&self.root.join("result.json"), &report);
        self.browser.cancel();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for Smoke {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.started {
            self.started = true;
            let result = if self.automatic {
                self.browser.open_file(
                    frame,
                    &self.entry.id,
                    &self.entry.name,
                    &self.entry.source.url,
                    self.root.clone(),
                    self.tx.clone(),
                )
            } else {
                self.browser.open(
                    frame,
                    &self.entry,
                    self.root.clone(),
                    self.tx.clone(),
                    self.prepared.as_ref(),
                )
            };
            if let Err(e) = result {
                self.finish(ctx, Err(e));
                return;
            }
        }
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Started(_) => {
                    self.deadline = Instant::now() + Duration::from_secs(1800);
                    let _ = ets_launcher::maps::write_json(
                        &self.root.join("progress.json"),
                        &serde_json::json!({"status":"downloading","started_at":ets_launcher::maps::source::now()}),
                    );
                }
                Event::Complete(_, path) => {
                    self.finish(ctx, Ok(path));
                    return;
                }
                Event::Failed(e) => {
                    self.finish(ctx, Err(anyhow::anyhow!(e)));
                    return;
                }
                Event::Navigate(url) => self.browser.navigate(&url),
                Event::Location(url) => self.browser.url = url,
                Event::PopupRequested(url) => self.browser.popup_requested(url),
            }
        }
        if self.automatic && Instant::now() >= self.next_inspection {
            self.browser
                .inspect_public_controls(self.root.join("controls.json"));
            self.next_inspection = Instant::now() + Duration::from_secs(30);
        }
        if Instant::now() > self.deadline {
            self.finish(ctx, Err(anyhow::anyhow!("Истекло время ожидания загрузки")));
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(100));
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Проверка WebView2…");
            if self.prepared.is_some() || self.automatic {
                self.browser.show(ui);
            }
        });
    }
}
