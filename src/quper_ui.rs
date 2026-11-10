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
        library::{Library, LocalProfile, Report},
        size::Estimate,
    },
    settings::Settings,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

enum Event {
    Catalog(Result<Catalog, String>),
    Plan(String, Result<Plan, String>),
    Image(String, Result<egui::ColorImage, String>),
    Job(Result<Report, String>),
    Size(String, Result<Estimate, String>),
    Profile(Result<LocalProfile, String>),
    /// An author's page and the file-host link behind its "Download" button.
    Resolved(String, Option<String>),
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

/// Files the visitor downloads in the system browser (when a site's security
/// check does not pass inside the launcher) are taken from the Downloads folder.
/// Files the launcher cannot download itself, opened one by one in the
/// visitor's own browser, where Cloudflare's checks pass normally. The next file opens as soon as the
/// browser starts the previous one; finished files are imported as they arrive.
struct BrowserQueue {
    pending: VecDeque<(String, String)>,
    current: Option<(String, String)>,
    total: usize,
    /// Set when a download started or a file arrived; the next page opens shortly after.
    advance_at: Option<Instant>,
    /// When the current page was opened; it is skipped after [`SKIP_AFTER`].
    opened_at: Instant,
    /// Files whose download never started (the host refused or nobody passed its check).
    skipped: Vec<(String, String)>,
}

/// A page that has not started a download in this time is skipped, so one host
/// that refuses a file does not hold up the rest of the build.
const SKIP_AFTER: Duration = Duration::from_secs(5 * 60);

fn is_modsfire(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.") == "modsfire.com")
        })
        .unwrap_or(false)
}

use crate::browsers::{downloads_folder, open_url as open_in_browser};

struct DownloadWatch {
    folder: PathBuf,
    since: std::time::SystemTime,
    until: Instant,
    next_scan: Instant,
    /// Size last seen and when it last changed; a file is taken once it settles.
    pending: BTreeMap<PathBuf, (u64, Instant)>,
    taken: BTreeSet<PathBuf>,
    /// Unfinished downloads already seen; a new one means the browser started a file.
    started: BTreeSet<PathBuf>,
    /// Final names (lowercase) of downloads seen unfinished: their arrival is
    /// the end of a download already counted, not the start of a new one.
    expected: BTreeSet<String>,
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
    downloads: Option<DownloadWatch>,
    paste: crate::clipboard_files::PasteKey,
    external: Option<BrowserQueue>,
    /// Files the last browser queue skipped, listed until the next queue starts.
    browser_skipped: Vec<(String, String)>,
    /// Author pages resolved to the file host behind them.
    resolved: BTreeMap<String, String>,
    resolving: BTreeSet<String>,
    external_downloads: bool,
    script_help: bool,
    /// Download size per build, `None` while unknown; read from cache on demand.
    sizes: BTreeMap<String, Option<Estimate>>,
    /// Build whose size is being estimated: (id, finished sources, all sources).
    sizing: Option<(String, Arc<AtomicUsize>, usize)>,
    /// Builds whose estimate failed in this session; not retried automatically.
    size_failed: BTreeSet<String>,
    job_title: &'static str,
    /// Folder of the local profile created in this session.
    local_profile: Option<PathBuf>,
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
            downloads: None,
            paste: Default::default(),
            external: None,
            browser_skipped: Vec::new(),
            resolved: BTreeMap::new(),
            resolving: BTreeSet::new(),
            external_downloads: settings.external_downloads,
            script_help: false,
            sizes: BTreeMap::new(),
            sizing: None,
            size_failed: BTreeSet::new(),
            job_title: "Загрузка сборки",
            local_profile: None,
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
        self.downloads = None;
        self.external = None;
    }

    /// Starts opening the build's ModsFire files in the system browser.
    /// The address to download from: an author's page is replaced by the
    /// file-host link behind it once known.
    fn target(&self, url: &str) -> String {
        self.resolved
            .get(url)
            .cloned()
            .unwrap_or_else(|| url.to_owned())
    }

    /// Finds the file-host link behind each author's page of the open build.
    fn resolve_author_pages(&mut self, ctx: &egui::Context, runtime: &Runtime) {
        let Some(plan) = &self.plan else { return };
        let pages = plan
            .files
            .iter()
            .filter(|r| r.required)
            .filter_map(|r| r.urls.first())
            .filter(|url| {
                !self.resolved.contains_key(*url)
                    && !self.resolving.contains(*url)
                    && reqwest::Url::parse(url).is_ok_and(|u| !quper::files::is_file_host(&u))
            })
            .cloned()
            .collect::<Vec<_>>();
        for page in pages {
            self.resolving.insert(page.clone());
            let (sender, cancel, ctx) = (
                self.sender.clone(),
                self.metadata_cancel.clone(),
                ctx.clone(),
            );
            runtime.spawn(async move {
                let target = async {
                    let client = quper::source::client().ok()?;
                    let (final_url, html) =
                        quper::source::get(&client, &page, &cancel).await.ok()?;
                    let base = reqwest::Url::parse(&final_url).ok()?;
                    quper::files::primary_download(&base, &String::from_utf8_lossy(&html))
                        .map(|u| u.to_string())
                }
                .await;
                let _ = sender.send(Event::Resolved(page, target));
                ctx.request_repaint();
            });
        }
    }

    /// Adds ModsFire files to the browser queue, starting it when needed.
    fn queue_external(&mut self, files: Vec<(String, String)>) {
        if let Some(queue) = &mut self.external {
            for file in files {
                let known = queue.current.as_ref().is_some_and(|(u, _)| *u == file.0)
                    || queue.pending.iter().any(|(u, _)| *u == file.0);
                if !known {
                    queue.pending.push_back(file);
                    queue.total += 1;
                }
            }
        } else {
            self.start_external(files);
        }
    }

    fn start_external(&mut self, files: Vec<(String, String)>) {
        if files.is_empty() {
            return;
        }
        self.watch_downloads();
        if self.downloads.is_none() {
            return;
        }
        self.browser_skipped.clear();
        self.external = Some(BrowserQueue {
            total: files.len(),
            pending: files.into(),
            current: None,
            advance_at: None,
            opened_at: Instant::now(),
            skipped: Vec::new(),
        });
        self.open_next_external();
    }

    fn open_next_external(&mut self) {
        let Some(queue) = &mut self.external else {
            return;
        };
        queue.advance_at = None;
        let Some((url, name)) = queue.pending.pop_front() else {
            queue.current = None;
            let skipped = std::mem::take(&mut queue.skipped);
            self.external = None;
            if skipped.is_empty() {
                self.inform("Все файлы открыты в браузере. Загрузки подключатся к сборке, как только завершатся.");
            } else {
                self.fail(format!(
                    "Не скачались: {}. Откройте их ещё раз или скачайте сами и добавьте через «Импорт файлов…»; можно и создать профиль без них.",
                    skipped.iter().map(|(_, n)| format!("«{n}»")).collect::<Vec<_>>().join(", ")
                ));
            }
            self.browser_skipped = skipped;
            return;
        };
        queue.current = Some((url.clone(), name));
        queue.opened_at = Instant::now();
        // A long queue keeps the Downloads folder watched while it runs.
        if let Some(watch) = &mut self.downloads {
            watch.until = Instant::now() + Duration::from_secs(30 * 60);
        }
        if let Err(e) = open_in_browser(&crate::userscript::tagged(&url)) {
            self.external = None;
            self.fail(format!("Не удалось открыть браузер: {e}"));
        }
    }

    /// Leaves the current page without a download and goes on with the queue.
    fn skip_external(&mut self) {
        if let Some(queue) = &mut self.external
            && let Some(current) = queue.current.take()
        {
            queue.skipped.push(current);
        }
        self.open_next_external();
    }

    fn install_script(&mut self, runtime: &Runtime) {
        match crate::userscript::serve(runtime).and_then(|url| Ok(open_in_browser(&url)?)) {
            Ok(()) => self.inform(
                "В браузере откроется окно Tampermonkey — нажмите «Установить». Если вместо него показан текст скрипта, сначала установите Tampermonkey и повторите.",
            ),
            Err(e) => self.fail(format!("Не удалось открыть установку скрипта: {e}")),
        }
    }

    /// Archives and packages the import accepts, for file dialogs.
    const IMPORT_EXTENSIONS: [&str; 7] = ["scs", "zip", "rar", "7z", "001", "002", "003"];

    /// Files the visitor already has (dropped, pasted or picked) go straight to
    /// the import queue of the open build instead of being downloaded.
    fn import_files(&mut self, files: Vec<PathBuf>) {
        if self.plan.is_none() {
            self.fail("Сначала откройте сборку, в которую нужно добавить файлы.");
            return;
        }
        let (accepted, rejected): (Vec<_>, Vec<_>) = files.into_iter().partition(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(quper::files::allowed_filename)
        });
        if accepted.is_empty() {
            self.fail(
                "Подходят только моды и архивы: .scs, .zip, .rar, .7z или части архива .001, .002…",
            );
            return;
        }
        let names = accepted
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect::<Vec<_>>()
            .join(", ");
        self.inform(format!(
            "Добавляем в сборку: {names}{}. Файл будет проверен и подключён автоматически.",
            if rejected.is_empty() {
                String::new()
            } else {
                format!(" (пропущено неподходящих: {})", rejected.len())
            }
        ));
        for path in accepted {
            if !self.imports.contains(&path) {
                self.imports.push(path);
            }
        }
    }

    /// Dropped or pasted files while a build is open.
    fn take_dropped_and_pasted(&mut self, ctx: &egui::Context, p: &Palette) {
        let (dropped, hovering, focused) = ctx.input(|i| {
            (
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect::<Vec<_>>(),
                !i.raw.hovered_files.is_empty(),
                i.focused,
            )
        });
        if !dropped.is_empty() {
            self.import_files(dropped);
        }
        // A text field keeps its own Ctrl+V.
        if self.paste.pressed() && focused && !ctx.wants_keyboard_input() {
            let files = crate::clipboard_files::files();
            if !files.is_empty() {
                self.import_files(files);
            }
        }
        if hovering {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("quper-drop"),
            ));
            painter.rect_filled(screen, 0, p.bg.gamma_multiply(0.85));
            let target = screen.shrink(32.0);
            painter.rect_stroke(
                target,
                16,
                Stroke::new(2.0_f32, p.accent),
                egui::StrokeKind::Inside,
            );
            painter.text(
                target.center() - vec2(0.0, 14.0),
                egui::Align2::CENTER_CENTER,
                "Отпустите, чтобы добавить файлы в сборку",
                ui::font(22.0, W::Extra),
                p.text,
            );
            painter.text(
                target.center() + vec2(0.0, 18.0),
                egui::Align2::CENTER_CENTER,
                "Лаунчер сам определит, каким модам они соответствуют",
                ui::font(14.0, W::Regular),
                p.muted,
            );
        }
    }

    /// Opens the current page in the selected browser and starts taking the
    /// downloaded file from the Downloads folder.
    fn open_external(&mut self) {
        if let Err(e) = self.browser.open_external() {
            self.fail(format!("Не удалось открыть браузер: {e}"));
        }
    }

    fn watch_downloads(&mut self) {
        let Some(folder) = downloads_folder() else {
            self.fail(
                "Не найдена папка «Загрузки». Скачанный файл добавьте через «Импорт файлов…».",
            );
            return;
        };
        self.inform(format!(
            "Скачайте файл в открывшемся браузере — лаунчер сам заберёт его из папки {}.",
            folder.display()
        ));
        let now = Instant::now();
        let (taken, mut started, mut expected) = self
            .downloads
            .take()
            .map(|w| (w.taken, w.started, w.expected))
            .unwrap_or_default();
        // Downloads already running belong to earlier pages: neither they nor
        // their completion may count as the next page's start.
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(target) = quper::files::download_target(&name) {
                started.insert(entry.path());
                expected.insert(target);
            }
        }
        self.downloads = Some(DownloadWatch {
            folder,
            // Files that appear from now on; a little slack for clock rounding.
            since: std::time::SystemTime::now() - Duration::from_secs(5),
            until: now + Duration::from_secs(30 * 60),
            next_scan: now,
            pending: BTreeMap::new(),
            taken,
            started,
            expected,
        });
    }

    /// New finished mod archives in the Downloads folder go to the import queue.
    fn scan_downloads(&mut self) {
        let missing = self
            .detail
            .as_ref()
            .map(|d| d.missing.clone())
            .unwrap_or_default();
        let Some(watch) = &mut self.downloads else {
            return;
        };
        let now = Instant::now();
        if now >= watch.until {
            self.downloads = None;
            return;
        }
        if now < watch.next_scan {
            return;
        }
        watch.next_scan = now + Duration::from_secs(2);
        let Ok(entries) = std::fs::read_dir(&watch.folder) else {
            return;
        };
        let mut ready = Vec::new();
        // Split archives (ProMods ships "….zip.001", "….002"…) are taken only
        // once every part is there and no download is still running.
        let mut parts: BTreeMap<String, BTreeMap<u32, PathBuf>> = BTreeMap::new();
        // Parts downloaded before the watch started: complete, but they join
        // a group only when a new part of it arrives.
        let mut old_parts = BTreeSet::new();
        let mut downloading = false;
        let mut unfinished = Vec::new();
        // The browser began a new mod download: the queue moves on without
        // waiting for it to finish, and the file is imported when it arrives.
        let mut began = false;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if quper::files::unfinished_download(&name) {
                let modified = entry.metadata().and_then(|m| m.modified()).ok();
                // A ".part" untouched for 10 minutes is an abandoned download
                // (a closed browser, a failed server); it must not hold back
                // split archives forever.
                let active = modified.is_some_and(|m| {
                    m.elapsed()
                        .is_ok_and(|age| age < Duration::from_secs(10 * 60))
                });
                if active {
                    downloading = true;
                    unfinished.push(name.to_lowercase());
                }
                let fresh = modified.is_some_and(|m| m >= watch.since);
                if fresh && quper::files::unfinished_mod(&name) && watch.started.insert(path) {
                    began = true;
                }
                if let Some(target) = quper::files::download_target(&name) {
                    watch.expected.insert(target);
                }
                continue;
            }
            // Browsers write .crdownload/.part files first; only the final name counts.
            if !quper::files::allowed_filename(&name) || watch.taken.contains(&path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            // Firefox and Zen create the final name empty while the data goes to
            // a separate ".part" file; an empty file is never a finished download.
            if !meta.is_file() || meta.len() == 0 {
                continue;
            }
            // New files are taken; an older one only when it is a mod the open
            // build still misses (downloaded earlier, possibly as "mod(1).scs").
            let fresh = !meta.modified().is_ok_and(|m| m < watch.since);
            if let Some((base, number)) = quper::files::split_part(&name) {
                let group = parts.entry(base).or_default();
                // A part saved again ("pack.7z(1).001") replaces an older copy.
                if fresh || !group.contains_key(&number) {
                    group.insert(number, path.clone());
                }
                if !fresh {
                    old_parts.insert(path.clone());
                    continue;
                }
            }
            let wanted = || {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                let stem = quper::files::browser_copy_base(stem).unwrap_or(stem);
                missing.iter().any(|id| id.eq_ignore_ascii_case(stem))
            };
            if !fresh && !wanted() {
                continue;
            }
            match watch.pending.get(&path) {
                Some((size, changed)) if *size == meta.len() => {
                    if changed.elapsed() >= Duration::from_secs(3) {
                        ready.push(path);
                    }
                }
                _ => {
                    // A small file can finish between two scans without a
                    // visible ".part"; its first sighting counts as a start.
                    // A file whose ".part" was seen is the end of a download
                    // already counted: a large file finishing must not open
                    // the next page while the current one has not started.
                    began |= fresh
                        && !watch.pending.contains_key(&path)
                        && !watch.expected.contains(&name.to_lowercase());
                    watch.pending.insert(path, (meta.len(), now));
                }
            }
        }
        let settled: BTreeSet<PathBuf> = ready.iter().cloned().collect();
        ready.retain(|path| {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let Some((base, _)) = quper::files::split_part(name) else {
                // Firefox writes "name.XXXX.ext.part" next to the final "name.ext".
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_lowercase();
                return !unfinished
                    .iter()
                    .any(|u| u.starts_with(&format!("{stem}.")));
            };
            let group = &parts[&base];
            let contiguous = group.keys().copied().eq(1..=group.len() as u32);
            let all_settled = group.values().all(|part| {
                settled.contains(part) || watch.taken.contains(part) || old_parts.contains(part)
            });
            !downloading && contiguous && all_settled
        });
        let mut groups = BTreeSet::new();
        let mut failures = Vec::new();
        for path in ready {
            watch.pending.remove(&path);
            watch.taken.insert(path.clone());
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            match quper::files::split_part(name) {
                Some((base, _)) => {
                    groups.insert(base);
                }
                None => self.imports.push(path),
            }
        }
        // A whole split archive goes to the import under one name, as the
        // archiver finds the other parts by the first one's name.
        for base in groups {
            let group = &parts[&base];
            for part in group.values() {
                watch.taken.insert(part.clone());
            }
            match quper::files::uniform_parts(&base, group) {
                Ok(paths) => self.imports.extend(paths),
                Err(e) => failures.push(format!("Не удалось собрать части архива {base}: {e:#}")),
            }
        }
        for failure in failures {
            self.fail(failure);
        }
        // Only a start moves the queue on: a file arriving is the end of a
        // download some earlier page began (a large one can take minutes),
        // not a sign that the current page got going.
        if began
            && let Some(queue) = &mut self.external
            && queue.current.is_some()
            && queue.advance_at.is_none()
        {
            queue.advance_at = Some(now + Duration::from_secs(2));
        }
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

    /// Size of a build from memory or the cache of an earlier estimate.
    fn cached_size(&mut self, combo: &Combo) -> Option<Estimate> {
        if let Some(value) = self.sizes.get(&combo.id) {
            return value.clone();
        }
        let value = maps::config_path(&format!("quper-size-{}.json", combo.id))
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Estimate>(&b).ok())
            .filter(|e| e.matches(&combo.version, &combo.date));
        self.sizes.insert(combo.id.clone(), value.clone());
        value
    }

    /// Estimates the size of the opened build once, without the user asking.
    fn ensure_size(&mut self, ctx: &egui::Context, runtime: &Runtime) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let id = plan.combo.id.clone();
        if self.sizing.is_some()
            || self.size_failed.contains(&id)
            || self.cached_size(&plan.combo).is_some()
        {
            return;
        }
        let sources = plan
            .files
            .iter()
            .filter(|r| r.required)
            .filter_map(|r| r.urls.first())
            .collect::<BTreeSet<_>>()
            .len();
        let done = Arc::new(AtomicUsize::new(0));
        self.sizing = Some((id.clone(), done.clone(), sources));
        let (sender, cancel, ctx) = (
            self.sender.clone(),
            self.metadata_cancel.clone(),
            ctx.clone(),
        );
        runtime.spawn(async move {
            let result = quper::size::estimate(&plan, done, &cancel)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = sender.send(Event::Size(id, result));
            ctx.request_repaint();
        });
    }

    /// Adds the downloaded build to the ordinary game folder as a local profile.
    /// `partial` leaves out the mods that are not downloaded.
    fn start_local_profile(
        &mut self,
        ctx: &egui::Context,
        runtime: &Runtime,
        settings: &Settings,
        partial: bool,
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
        let Some(documents) = documents_dir() else {
            self.fail("Не найдена папка «Документы» Windows");
            return;
        };
        self.job = true;
        self.job_title = "Создание локального профиля";
        self.cancel = CancellationToken::new();
        self.note = None;
        download::set_state(&self.transfer, |s| {
            *s = Transfer {
                status: Status::Connecting,
                message: "Проверяем, что игра закрыта…".into(),
                ..Default::default()
            }
        });
        let (sender, cancel, ctx, transfer) = (
            self.sender.clone(),
            self.cancel.clone(),
            ctx.clone(),
            self.transfer.clone(),
        );
        runtime.spawn_blocking(move || {
            // The game rewrites its profiles on exit, so it must not be running.
            let result =
                ets_launcher::install::ensure_game_closed(plan.combo.game).and_then(|()| {
                    if partial {
                        library.create_partial_profile(&plan, &documents, &transfer, &cancel)
                    } else {
                        library.create_local_profile(&plan, &documents, &transfer, &cancel)
                    }
                });
            if result.is_err() {
                download::set_state(&transfer, |s| s.status = Status::Error);
            }
            let _ = sender.send(Event::Profile(result.map_err(|e| format!("{e:#}"))));
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
        self.job_title = "Загрузка сборки";
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
        let mut handed = Vec::new();
        while let Some((url, name)) = self.browser_queue.pop_front() {
            // Whatever the launcher could not download itself goes to the
            // visitor's own browser with the script; Cloudflare's checks rarely
            // pass in the built-in one.
            if self.external_downloads {
                handed.push((self.target(&url), name));
                continue;
            }
            self.browser_request = Some((url, name));
            break;
        }
        if !handed.is_empty() {
            self.queue_external(handed);
        }
    }

    pub fn tick(
        &mut self,
        ctx: &egui::Context,
        runtime: &Runtime,
        settings: &Settings,
        dlc_busy: bool,
    ) {
        self.external_downloads = settings.external_downloads;
        self.resolve_author_pages(ctx, runtime);
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
                Event::Resolved(page, target) => {
                    self.resolving.remove(&page);
                    // An unresolved page stays as it is and opens in the built-in browser.
                    self.resolved.insert(page.clone(), target.unwrap_or(page));
                }
                Event::Image(url, result) => {
                    if let Ok(image) = result {
                        self.images.insert(
                            url.clone(),
                            ctx.load_texture(url, image, egui::TextureOptions::LINEAR),
                        );
                    }
                }
                Event::Size(id, result) => {
                    if self.sizing.as_ref().is_some_and(|(s, _, _)| *s == id) {
                        self.sizing = None;
                    }
                    match result {
                        Ok(estimate) => {
                            if let Ok(path) = maps::config_path(&format!("quper-size-{id}.json")) {
                                let _ = maps::write_json(&path, &estimate);
                            }
                            self.sizes.insert(id, Some(estimate));
                        }
                        Err(_) => {
                            self.size_failed.insert(id);
                        }
                    }
                }
                Event::Profile(result) => {
                    self.job = false;
                    match result {
                        Ok(created) => {
                            let copied = if created.copied > 0 {
                                format!(
                                    " Скопировано {} — папка сборок на другом диске, поэтому жёсткие ссылки недоступны.",
                                    format_bytes(created.copied)
                                )
                            } else {
                                String::new()
                            };
                            let skipped = if created.skipped > 0 {
                                format!(
                                    " Без {} {} — их можно скачать или импортировать позже и снова нажать «Создать локальный профиль»: карьера сохранится.",
                                    created.skipped,
                                    plural(
                                        created.skipped,
                                        [
                                            "недостающего мода",
                                            "недостающих модов",
                                            "недостающих модов"
                                        ]
                                    )
                                )
                            } else {
                                String::new()
                            };
                            self.inform(format!(
                                "Профиль «{}» {}: {}. В игре выберите его в обычном списке профилей — {} модов уже включены в порядке Quper.{skipped}{copied}",
                                created.name,
                                if created.updated {
                                    "обновлён, карьера сохранена"
                                } else {
                                    "создан"
                                },
                                created.profile.display(),
                                created.mods,
                            ));
                            self.local_profile = Some(created.profile);
                        }
                        Err(e) => self.fail(e),
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
        if std::mem::take(&mut self.browser.external_requested) {
            self.watch_downloads();
        }
        self.scan_downloads();
        if self
            .external
            .as_ref()
            .and_then(|q| q.advance_at)
            .is_some_and(|at| Instant::now() >= at)
        {
            self.open_next_external();
        } else if self.external.as_ref().is_some_and(|q| {
            q.current.is_some() && q.advance_at.is_none() && q.opened_at.elapsed() >= SKIP_AFTER
        }) {
            self.skip_external();
        }
        if self.external.is_some() || self.downloads.is_some() {
            ctx.request_repaint_after(Duration::from_secs(1));
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
        if self.selected.is_some() {
            self.take_dropped_and_pasted(ctx, p);
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
                if Button::new("Импортировать файл…", Kind::Ghost)
                    .height(40.0)
                    .size(13.0)
                    .show(ui, p)
                    .on_hover_text("Файл уже скачан? Выберите его, перетащите в окно или вставьте через Ctrl+V")
                    .clicked()
                    && let Some(files) = rfd::FileDialog::new()
                        .set_title(format!("Файл для «{}»", self.browser.title))
                        .add_filter("Моды и архивы", &Self::IMPORT_EXTENSIONS)
                        .pick_files()
                {
                    self.import_files(files);
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
        if let Some(since) = self.browser.challenge {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
            let message = if since.elapsed() < Duration::from_secs(20) {
                "Сайт ожидает проверку CAPTCHA или Cloudflare. Пройдите её на странице — дальше лаунчер продолжит сам."
            } else {
                "Проверка повторяется или не загружается? Откройте страницу в выбранном браузере и пройдите её там — лаунчер сам заберёт скачанный файл из папки загрузок."
            };
            if ui::banner(ui, p, Tone::Warn, message, Some("Открыть в браузере")) {
                self.open_external();
            }
        }
        if let Some(watch) = &self.downloads {
            let folder = watch.folder.display().to_string();
            if ui::banner(
                ui,
                p,
                Tone::Info,
                &format!(
                    "Ждём скачанный файл в папке {folder}. Как только загрузка в браузере закончится, файл будет добавлен в сборку."
                ),
                Some("Не ждать"),
            ) {
                self.downloads = None;
            }
        }
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
                    let size = self.cached_size(combo);
                    let clicked = ui
                        .allocate_ui_with_layout(
                            vec2(card_width, 0.0),
                            Layout::top_down(Align::Min),
                            |ui| self.card(ui, p, stripes, combo, *progress, size, enabled),
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

    #[allow(clippy::too_many_arguments)]
    fn card(
        &self,
        ui: &mut egui::Ui,
        p: &Palette,
        stripes: &egui::TextureHandle,
        combo: &Combo,
        progress: Option<(usize, usize, bool)>,
        size: Option<Estimate>,
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
                            if let Some(estimate) = size.filter(|e| e.bytes > 0) {
                                let prefix = if estimate.unknown == 0 { "≈" } else { "от" };
                                ui::chip(
                                    ui,
                                    &format!("{prefix} {}", format_bytes(estimate.bytes)),
                                    true,
                                    p.text2,
                                    p.line_soft,
                                    22.0,
                                    11.0,
                                );
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
        self.ensure_size(ctx, runtime);
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
                            self.size_row(ui, p, combo);
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
                                        .add_filter("Моды и архивы", &Self::IMPORT_EXTENSIONS)
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
                                let complete = plan.is_some()
                                    && total > 0
                                    && state.as_ref().is_some_and(|(missing, _)| missing.is_empty());
                                let local = Button::new("Создать локальный профиль", Kind::Ghost)
                                    .icon(Icon::Layers)
                                    .enabled(can_act && complete)
                                    .show(ui, p);
                                if !complete {
                                    local.on_hover_text("Доступно, когда готовы все моды");
                                } else if local
                                    .on_hover_text("Профиль появится в обычном списке профилей игры, моды включены в порядке Quper")
                                    .clicked()
                                {
                                    self.start_local_profile(ctx, runtime, settings, false);
                                }
                                // Files no source would give: the build can still
                                // be played with the mods that did arrive.
                                let lacking = state.as_ref().map_or(0, |(missing, _)| missing.len());
                                if !complete
                                    && done > 0
                                    && lacking > 0
                                    && Button::new(&format!("Профиль без недостающих ({lacking})"), Kind::Ghost)
                                        .icon(Icon::Layers)
                                        .enabled(can_act)
                                        .show(ui, p)
                                        .on_hover_text("Локальный профиль из уже скачанных модов в порядке Quper. Недостающие можно добавить позже: импортируйте их и создайте профиль снова — карьера сохранится.")
                                        .clicked()
                                {
                                    self.start_local_profile(ctx, runtime, settings, true);
                                }
                                if let Some(dir) = self.local_profile.clone()
                                    && Button::new("Папка локального профиля", Kind::Ghost)
                                        .icon(Icon::Folder)
                                        .show(ui, p)
                                        .clicked()
                                {
                                    let _ = open::that(dir);
                                }
                            });
                            ui.add(
                                egui::Label::new(ui::text(
                                    if ready.is_some() {
                                        format!("«Играть» запускает сборку в отдельном окружении. «Создать локальный профиль» добавляет её в обычную папку игры: профиль появится в списке профилей, моды включены в порядке Quper. Нужны DLC карт и версия игры {}.", combo.game_version)
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
        let failed = self
            .report
            .as_ref()
            .map(|r| {
                r.problems
                    .iter()
                    .filter_map(|p| p.url.clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let mut external_files = Vec::new();
        for (row, state) in &rows {
            if *state == FileFilter::Pending
                && let Some(url) = row.urls.first()
                && let target = self.target(url)
                && (is_modsfire(&target) || row.urls.iter().any(|u| failed.contains(u)))
                && !external_files
                    .iter()
                    .any(|(u, _): &(String, String)| *u == target)
            {
                external_files.push((target, row.name.clone()));
            }
        }
        if !external_files.is_empty() || self.external.is_some() {
            // The list on screen is enough; a refresh of it need not hold the queue back.
            let free = !self.preparing() && !dlc_busy;
            self.external_card(ui, p, runtime, external_files, free);
        }
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
                        ui.add_space(6.0);
                        ui.add(
                            egui::Label::new(ui::text(
                                "Файл не скачивается? Перетащите его в окно, вставьте через Ctrl+V (скопировав в Проводнике) или нажмите «Импорт…» в строке мода.",
                                12.0,
                                W::Regular,
                                p.muted,
                            ))
                            .wrap(),
                        );
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
                    match self.file_row(ui, p, row, *state, idle) {
                        RowAction::Open => {
                            if let Some(url) = row.urls.first() {
                                let target = self.target(url);
                                if self.external_downloads {
                                    self.queue_external(vec![(target, row.name.clone())]);
                                } else {
                                    self.browser_request = Some((url.clone(), row.name.clone()));
                                }
                            }
                        }
                        RowAction::Import => {
                            if let Some(files) = rfd::FileDialog::new()
                                .set_title(format!("Файл для «{}»", row.name))
                                .add_filter("Моды и архивы", &Self::IMPORT_EXTENSIONS)
                                .pick_files()
                            {
                                self.import_files(files);
                            }
                        }
                        RowAction::None => {}
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

    /// "Размер сборки ≈ 24 ГиБ", with progress while the hosts are being asked.
    fn size_row(&mut self, ui: &mut egui::Ui, p: &Palette, combo: &Combo) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(ui::text("Размер сборки", 13.0, W::Semi, p.text2));
            if let Some((id, done, sources)) = &self.sizing
                && *id == combo.id
            {
                ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                ui.label(ui::text(
                    format!(
                        "считаем… {} / {sources}",
                        done.load(Ordering::Relaxed).min(*sources)
                    ),
                    12.0,
                    W::Regular,
                    p.muted,
                ));
                return;
            }
            match self.cached_size(combo) {
                Some(estimate) if estimate.bytes > 0 => {
                    let prefix = if estimate.unknown == 0 { "≈" } else { "от" };
                    ui.label(ui::mono_text(
                        format!("{prefix} {}", format_bytes(estimate.bytes)),
                        14.0,
                        true,
                        p.accent,
                    ));
                    if estimate.unknown > 0 {
                        ui.label(ui::text(
                            format!(
                                "без {} {} — хостинг не сообщает размер",
                                estimate.unknown,
                                plural(estimate.unknown, ["файла", "файлов", "файлов"])
                            ),
                            12.0,
                            W::Regular,
                            p.muted,
                        ));
                    }
                }
                Some(_) => {
                    ui.label(ui::text(
                        "неизвестен — хостинги не сообщают размер",
                        12.0,
                        W::Regular,
                        p.muted,
                    ));
                }
                None if self.size_failed.contains(&combo.id) => {
                    ui.label(ui::text("не удалось узнать", 12.0, W::Regular, p.muted));
                }
                None => {
                    ui.label(ui::text("…", 12.0, W::Regular, p.muted));
                }
            }
            if !self.preparing()
                && self.sizing.is_none()
                && ui::link(ui, p, "Пересчитать", 12.0, W::Semi, false).clicked()
            {
                self.sizes.insert(combo.id.clone(), None);
                self.size_failed.remove(&combo.id);
                if let Ok(path) = maps::config_path(&format!("quper-size-{}.json", combo.id)) {
                    let _ = std::fs::remove_file(path);
                }
            }
        });
    }

    fn external_card(
        &mut self,
        ui: &mut egui::Ui,
        p: &Palette,
        runtime: &Runtime,
        files: Vec<(String, String)>,
        idle: bool,
    ) {
        ui::card(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 12.0;
            let running = self.external.as_ref().map(|q| {
                (
                    q.current.clone(),
                    q.total - q.pending.len(),
                    q.total,
                    q.advance_at.is_some(),
                    SKIP_AFTER.saturating_sub(q.opened_at.elapsed()),
                )
            });
            match running {
                Some((current, opened, total, arrived, left)) => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                        ui.label(ui::text(
                            format!("Загрузка через браузер: {opened} из {total}"),
                            18.0,
                            W::Extra,
                            p.text,
                        ));
                    });
                    let name = current.as_ref().map(|(_, n)| n.as_str()).unwrap_or("—");
                    ui.add(
                        egui::Label::new(ui::text(
                            if arrived {
                                format!("Загрузка «{name}» началась, открываем следующий файл…")
                            } else {
                                format!(
                                    "В браузере открыт «{name}». Скрипт нажмёт кнопки сайта; если Cloudflare попросит, отметьте «Я человек». Как только загрузка начнётся, вкладка закроется и откроется следующий файл; скачанные файлы подключатся к сборке сами. Если загрузка не начнётся за {} мин, файл будет пропущен.",
                                    left.as_secs().div_ceil(60).max(1)
                                )
                            },
                            13.0,
                            W::Regular,
                            p.muted,
                        ))
                        .wrap(),
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                        if let Some((url, _)) = &current
                            && Button::new("Открыть ещё раз", Kind::Ghost)
                                .icon(Icon::External)
                                .height(40.0)
                                .size(13.0)
                                .show(ui, p)
                                .clicked()
                            && let Err(e) = open_in_browser(&crate::userscript::tagged(url))
                        {
                            self.fail(format!("Не удалось открыть браузер: {e}"));
                        }
                        if Button::new("Пропустить файл", Kind::Ghost)
                            .height(40.0)
                            .size(13.0)
                            .show(ui, p)
                            .on_hover_text("Файл попадёт в список нескачанных; его можно будет открыть ещё раз или импортировать")
                            .clicked()
                        {
                            self.skip_external();
                        }
                        if Button::new("Остановить", Kind::Danger)
                            .icon(Icon::Stop)
                            .height(40.0)
                            .size(13.0)
                            .show(ui, p)
                            .clicked()
                        {
                            self.external = None;
                        }
                    });
                }
                None => {
                    ui.label(ui::text(
                        format!(
                            "Через браузер: {} {}",
                            files.len(),
                            plural(files.len(), ["файл", "файла", "файлов"])
                        ),
                        18.0,
                        W::Extra,
                        p.text,
                    ));
                    ui.add(
                        egui::Label::new(ui::text(
                            "Эти файлы лаунчер не может скачать сам: ModsFire и сайты с проверкой Cloudflare, а также всё, что не скачалось при «Продолжить загрузку». Лаунчер откроет их по одному в вашем браузере: скрипт для Tampermonkey нажмёт кнопки сайта, после начала загрузки закроет вкладку, и откроется следующий файл. Скачанные файлы подключатся к сборке сами. Браузер, папка загрузок и подготовка Tampermonkey — в «Настройки → Загрузка через браузер».",
                            13.0,
                            W::Regular,
                            p.muted,
                        ))
                        .wrap(),
                    );
                    // What the last queue could not get and the build still lacks.
                    let skipped = self
                        .browser_skipped
                        .iter()
                        .filter(|(url, _)| files.iter().any(|(u, _)| u == url))
                        .cloned()
                        .collect::<Vec<_>>();
                    if !skipped.is_empty() {
                        ui::inset(p)
                            .inner_margin(Margin::symmetric(16, 12))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.spacing_mut().item_spacing.y = 8.0;
                                ui.label(ui::text(
                                    format!("Не скачались: {}", skipped.len()),
                                    14.0,
                                    W::Bold,
                                    p.warn,
                                ));
                                ui.add(
                                    egui::Label::new(ui::text(
                                        "Сайт не начал загрузку за 5 минут или отказал в файле. Откройте страницу ещё раз и скачайте файл сами — он подключится из «Загрузок», либо добавьте его через «Импорт файлов…». Можно и создать профиль без них.",
                                        12.0,
                                        W::Regular,
                                        p.muted,
                                    ))
                                    .wrap(),
                                );
                                for (url, name) in &skipped {
                                    ui.horizontal(|ui| {
                                        if Button::new("Открыть", Kind::Ghost)
                                            .icon(Icon::External)
                                            .height(30.0)
                                            .size(12.0)
                                            .show(ui, p)
                                            .clicked()
                                        {
                                            // Untagged: the visitor downloads it by hand.
                                            self.watch_downloads();
                                            if let Err(e) = open_in_browser(url) {
                                                self.fail(format!("Не удалось открыть браузер: {e}"));
                                            }
                                        }
                                        ui.add(
                                            egui::Label::new(ui::text(name, 13.0, W::Semi, p.text))
                                                .truncate(),
                                        );
                                    });
                                }
                            });
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                        if Button::new("Скачать через браузер", Kind::Primary)
                            .icon(Icon::Download)
                            .height(40.0)
                            .size(13.0)
                            .enabled(idle)
                            .show(ui, p)
                            .clicked()
                        {
                            self.start_external(files.clone());
                        }
                        if Button::new(
                            if self.script_help {
                                "Скрыть инструкцию"
                            } else {
                                "Скрипт для браузера…"
                            },
                            Kind::Ghost,
                        )
                        .height(40.0)
                        .size(13.0)
                        .show(ui, p)
                        .clicked()
                        {
                            self.script_help = !self.script_help;
                        }
                    });
                }
            }
            if self.script_help {
                ui::inset(p)
                    .inner_margin(Margin::symmetric(18, 14))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 8.0;
                        ui.label(ui::text("Скрипт ставится один раз", 14.0, W::Bold, p.text));
                        for step in [
                            "1. Установите расширение Tampermonkey в браузер, который открывается по умолчанию.",
                            "2. Chrome и Edge: в настройках расширения Tampermonkey включите «Разрешить пользовательские скрипты» (в старых версиях — режим разработчика на странице расширений).",
                            "3. Нажмите «Установить скрипт» и подтвердите установку в окне Tampermonkey.",
                        ] {
                            ui.add(
                                egui::Label::new(ui::text(step, 12.0, W::Regular, p.text2)).wrap(),
                            );
                        }
                        ui.add(
                            egui::Label::new(ui::text(
                                "Скрипт работает только во вкладках, которые открыл лаунчер, и нажимает лишь бесплатные кнопки файлообменников. Проверку Cloudflare он не трогает. Без скрипта очередь тоже работает — кнопки на сайте нужно будет нажать самому.",
                                12.0,
                                W::Regular,
                                p.muted,
                            ))
                            .wrap(),
                        );
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                            if Button::new("Сайт Tampermonkey", Kind::Ghost)
                                .trailing(Icon::External)
                                .height(36.0)
                                .size(13.0)
                                .show(ui, p)
                                .clicked()
                                && let Err(e) = open_in_browser("https://www.tampermonkey.net/")
                            {
                                self.fail(format!("Не удалось открыть браузер: {e}"));
                            }
                            if Button::new("Установить скрипт", Kind::Primary)
                                .height(36.0)
                                .size(13.0)
                                .show(ui, p)
                                .clicked()
                            {
                                self.install_script(runtime);
                            }
                            if Button::new("Скопировать скрипт", Kind::Ghost)
                                .height(36.0)
                                .size(13.0)
                                .show(ui, p)
                                .on_hover_text("Для ручной установки: Tampermonkey → «Создать новый скрипт» → вставить")
                                .clicked()
                            {
                                ui.ctx().copy_text(crate::userscript::SCRIPT.to_owned());
                                self.inform("Скрипт скопирован. В Tampermonkey создайте новый скрипт, вставьте текст и сохраните.");
                            }
                        });
                    });
            }
        });
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
                        ui.label(ui::text(self.job_title, 18.0, W::Extra, p.text));
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
    ) -> RowAction {
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
        let status_width = 300.0;
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
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let import = |ui: &mut egui::Ui, kind: Kind, label: &str| {
                    Button::new(label, kind)
                        .height(36.0)
                        .size(13.0)
                        .pad(12.0)
                        .radius(8)
                        .enabled(idle)
                        .show(ui, p)
                };
                match state {
                    FileFilter::Ready => {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(ui::text("Готов", 13.0, W::Bold, p.ok));
                        ui::icon(ui, Icon::Check, 14.0, p.ok, 3.0);
                        RowAction::None
                    }
                    FileFilter::Pending => {
                        let open = Button::new("Открыть источник", Kind::Ghost)
                            .trailing(Icon::External)
                            .height(36.0)
                            .size(13.0)
                            .pad(12.0)
                            .radius(8)
                            .enabled(idle)
                            .show(ui, p)
                            .clicked();
                        let picked = import(ui, Kind::Ghost, "Импорт…")
                            .on_hover_text(
                                "Уже есть этот файл? Выберите его — скачивать не придётся",
                            )
                            .clicked();
                        if open {
                            RowAction::Open
                        } else if picked {
                            RowAction::Import
                        } else {
                            RowAction::None
                        }
                    }
                    _ => {
                        let reason = if row.excluded_boosty {
                            "Ссылка ведёт на Boosty — скачайте файл у автора и выберите его здесь"
                        } else {
                            "Нет публичной ссылки — выберите уже скачанный файл"
                        };
                        if import(ui, Kind::Warn, "Импортировать файл…")
                            .on_hover_text(reason)
                            .clicked()
                        {
                            RowAction::Import
                        } else {
                            RowAction::None
                        }
                    }
                }
            },
        )
        .inner
    }
}

/// What a file row asked for.
enum RowAction {
    None,
    Open,
    Import,
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

/// The user's Documents folder (redirected ones included). Tests and checks can
/// point it elsewhere with `ETS_LAUNCHER_DOCUMENTS_DIR`.
fn documents_dir() -> Option<PathBuf> {
    std::env::var_os("ETS_LAUNCHER_DOCUMENTS_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| directories::UserDirs::new().and_then(|d| d.document_dir().map(PathBuf::from)))
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
