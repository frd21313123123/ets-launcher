//! A child WebView inside the egui window. Web pages never receive a native IPC
//! bridge; only WebView2 download callbacks can submit a local file to the queue.
use anyhow::{Context, Result, ensure};
use ets_launcher::maps::{self, MapEntry};
use ets_launcher::maps::{
    access::{self, DownloadAlternative},
    source::PreparedDownload,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

#[derive(Debug)]
pub enum Event {
    Started(String),
    Complete(String, PathBuf),
    Failed(String),
    Navigate(String),
    Location(String),
    PopupRequested(String),
}

/// Cloudflare's interstitial keeps the site's address; only its title tells it apart.
pub fn is_challenge_title(title: &str) -> bool {
    let title = title.trim().to_lowercase();
    [
        "just a moment",
        "один момент",
        "attention required",
        "выполнение проверки",
        "checking your browser",
        "проверка безопасности",
    ]
    .iter()
    .any(|marker| title.starts_with(marker))
}

pub struct Browser {
    #[cfg(windows)]
    view: Option<wry::WebView>,
    #[cfg(windows)]
    context: Option<wry::WebContext>,
    #[cfg(windows)]
    challenge_state: Arc<std::sync::Mutex<Option<bool>>>,
    #[cfg(windows)]
    next_challenge_check: std::time::Instant,
    pub visible: bool,
    pub title: String,
    pub url: String,
    instructions: String,
    alternatives: Vec<DownloadAlternative>,
    pending_popup: Option<String>,
    active: Arc<AtomicUsize>,
    auto_free: bool,
    /// When a full-page check or an unfinished CAPTCHA was first observed.
    pub challenge: Option<std::time::Instant>,
    /// Set when the visitor sent the page to the system browser.
    pub external_requested: bool,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            #[cfg(windows)]
            view: None,
            #[cfg(windows)]
            context: None,
            #[cfg(windows)]
            challenge_state: Default::default(),
            #[cfg(windows)]
            next_challenge_check: std::time::Instant::now(),
            visible: false,
            title: String::new(),
            url: String::new(),
            instructions: String::new(),
            alternatives: Vec::new(),
            pending_popup: None,
            active: Arc::new(AtomicUsize::new(0)),
            auto_free: false,
            challenge: None,
            external_requested: false,
        }
    }
}

pub fn allowed_filename(name: &str) -> bool {
    ets_launcher::archive::safe_relative(name).is_ok_and(|p| p.components().count() == 1)
        && std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "scs" | "zip" | "rar" | "7z" | "torrent"
                ) || (ext.len() == 3 && ext.bytes().all(|c| c.is_ascii_digit()))
            })
}

impl Browser {
    /// Diagnostic metadata only: no cookies, field values, tokens or file URLs.
    pub fn inspect_public_controls(&self, output: PathBuf) {
        #[cfg(windows)]
        if let Some(view) = &self.view {
            let _ = view.evaluate_script_with_callback(r#"(() => ({
                ready: document.readyState,
                forms: [...document.forms].slice(0, 15).map(f => ({
                    op: f.querySelector('input[name="op"]')?.value,
                    fields: [...f.querySelectorAll('input,button')].map(e => ({tag:e.tagName, name:e.name, type:e.type, id:e.id, disabled:e.disabled, text:e.tagName==='BUTTON'?e.textContent.trim().slice(0,100):undefined}))
                })),
                buttons: [...document.querySelectorAll('button,a[id],input[type="submit"]')].slice(0,100).map(e=>({tag:e.tagName,id:e.id,name:e.name,text:(e.textContent||e.value||'').trim().slice(0,100),disabled:e.disabled})),
                captcha: !!document.querySelector('iframe[src*="captcha"], .h-captcha, .g-recaptcha'),
                turnstile: !!document.querySelector('.cf-turnstile, iframe[src*="challenges.cloudflare.com"]'),
                turnstileSolved: [...document.querySelectorAll('input[name="cf-turnstile-response"]')].some(t => t.value.length > 20),
                url: location.origin + location.pathname
            }))()"#, move |data| {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) { let _ = maps::write_json(&output, &value); }
            });
        }
        #[cfg(not(windows))]
        let _ = output;
    }

    /// Quper queue entry. Only the normal public buttons of supported hosts
    /// are automated; account checks and captchas remain in the visible page.
    pub fn open_file(
        &mut self,
        frame: &eframe::Frame,
        id: &str,
        title: &str,
        url: &str,
        output: PathBuf,
        sender: mpsc::Sender<Event>,
    ) -> Result<()> {
        ets_launcher::quper::source::public_url(url)?;
        let entry: MapEntry = serde_json::from_value(serde_json::json!({
            "id": id, "name": title, "region": "", "author": "Quper", "description": "",
            "source": {"kind":"quper", "url":url, "selector":"main", "version_pattern":"(.*)"},
            "release":{"version":"queue", "game_versions":[], "packages":[]}
        }))?;
        self.auto_free = true;
        let result = self.open(frame, &entry, output, sender, None);
        self.auto_free = false;
        result
    }

    pub fn downloading(&self) -> bool {
        self.active.load(Ordering::Relaxed) != 0
    }

    pub fn open(
        &mut self,
        frame: &eframe::Frame,
        entry: &MapEntry,
        output: PathBuf,
        sender: mpsc::Sender<Event>,
        prepared: Option<&PreparedDownload>,
    ) -> Result<()> {
        ensure!(
            !self.downloading(),
            "Дождитесь окончания текущей браузерной загрузки"
        );
        let url = prepared.map(|p| p.page.as_str()).unwrap_or_else(|| {
            entry
                .source
                .browser_url
                .as_deref()
                .unwrap_or(&entry.source.url)
        });
        maps::https(url)?;
        self.title = entry.name.clone();
        self.url = url.into();
        self.challenge = None;
        self.external_requested = false;
        self.pending_popup = None;
        self.instructions = prepared
            .map(|p| p.message.clone())
            .unwrap_or_else(|| access::browser_help(url));
        self.alternatives = prepared.map(|p| p.alternatives.clone()).unwrap_or_default();
        std::fs::create_dir_all(&output)?;
        #[cfg(windows)]
        {
            self.view = None;
            ensure!(
                wry::webview_version().is_ok(),
                "Не найден WebView2 Runtime. Нажмите «Установить WebView2» либо откройте сайт во внешнем браузере и импортируйте файлы."
            );
            self.context = Some(wry::WebContext::new(Some(maps::config_path(
                "mod-browser",
            )?)));
            let context = self.context.as_mut().unwrap();
            let id = entry.id.clone();
            let completed_id = id.clone();
            let started_sender = sender.clone();
            let navigation_sender = sender.clone();
            let popup_sender = sender.clone();
            let mut popup_origins = vec![url.to_owned(), entry.source.url.clone()];
            popup_origins.extend(self.alternatives.iter().map(|a| a.url.clone()));
            let completed_sender = sender;
            let auto_free = self.auto_free;
            let active = self.active.clone();
            let completed_active = active.clone();
            self.challenge_state = Default::default();
            self.next_challenge_check = std::time::Instant::now();
            let initialization = if auto_free {
                // WebView2 also injects initialization scripts into subframes.
                // Keep automation outside the CAPTCHA provider's own frames.
                format!(
                    "(() => {{ if (window.top !== window) return; const captchaPending = {CAPTCHA_PENDING}; {QUPER_FREE_DOWNLOAD} }})()"
                )
            } else {
                String::new()
            };
            let webview = wry::WebViewBuilder::new_with_web_context(context)
                .with_url(url)
                .with_initialization_script(initialization)
                .with_bounds(wry::Rect { position: wry::dpi::LogicalPosition::new(20.0, 170.0).into(), size: wry::dpi::LogicalSize::new(800.0, 600.0).into() })
                .with_navigation_handler(move |url| {
                    let allowed = (url.starts_with("https://") || url == "about:blank")
                        && (!auto_free || url == "about:blank" || ets_launcher::quper::source::public_url(&url).is_ok());
                    if allowed { let _ = navigation_sender.send(Event::Location(url)); }
                    allowed
                })
                .with_new_window_req_handler(move |url, _| {
                    if auto_free && ets_launcher::quper::source::public_url(&url).is_err() { return wry::NewWindowResponse::Deny; }
                    if access::allowed_popup(&url,&popup_origins) { let _ = popup_sender.send(Event::Navigate(url)); }
                    else if maps::https(&url).is_ok() {let _=popup_sender.send(Event::PopupRequested(url));}
                    wry::NewWindowResponse::Deny
                })
                .with_download_started_handler(move |_, path| {
                    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false; };
                    if !allowed_filename(name) { let _ = started_sender.send(Event::Failed("Сайт предложил неподдерживаемый файл. Ожидается архив или пакет карты.".into())); return false; }
                    let target = output.join(name);
                    let partial = output.join(format!("{name}.browser-part"));
                    if target.exists() || std::fs::OpenOptions::new().create_new(true).write(true).open(&partial).is_err() { let _ = started_sender.send(Event::Failed(format!("Файл {name} уже загружен или скачивается. Импортируйте его или выберите другой файл."))); return false; }
                    *path = partial;
                    active.fetch_add(1, Ordering::Relaxed);
                    let _ = started_sender.send(Event::Started(id.clone()));
                    true
                })
                .with_download_completed_handler(move |_, path, success| {
                    let _ = completed_active.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_sub(1)));
                    let event = if success { path.map(|p| {
                        let target = p.with_file_name(p.file_stem().unwrap_or_default());
                        if target.exists() { return Event::Failed("Такой файл уже есть в очереди импорта".into()); }
                        match std::fs::rename(&p, &target) { Ok(()) => Event::Complete(completed_id.clone(), target), Err(e) => Event::Failed(format!("Не удалось сохранить браузерную загрузку: {e}")) }
                    }).unwrap_or_else(|| Event::Failed("Браузер не вернул файл загрузки".into())) }
                        else { if let Some(path)=path.filter(|p|p.extension().is_some_and(|ext|ext=="browser-part")) {let _=std::fs::remove_file(path);} Event::Failed("Загрузка на сайте прервана. Повторите её или импортируйте файл.".into()) };
                    let _ = completed_sender.send(event);
                })
                .build_as_child(frame).context("Не удалось открыть встроенный браузер")?;
            self.view = Some(webview);
            self.visible = true;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = (frame, sender);
            anyhow::bail!(
                "Встроенный браузер карт доступен в Windows. Используйте внешний браузер и импорт."
            )
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        #[cfg(windows)]
        if let Some(view) = &self.view {
            let _ = view.set_visible(false);
        }
    }

    pub fn suspend(&self) {
        #[cfg(windows)]
        if let Some(view) = &self.view {
            let _ = view.set_visible(false);
        }
    }

    pub fn navigate(&mut self, url: &str) {
        if maps::https(url).is_err() {
            return;
        }
        #[cfg(windows)]
        if let Some(view) = &self.view {
            // Keep the browser's normal referrer when a site's download link
            // targets a new window. Native load_url starts a fresh navigation
            // without it, which some public file hosts redirect to step one.
            if let Ok(url) = serde_json::to_string(url) {
                let _ = view.evaluate_script(format!("location.assign({url})").as_str());
            }
        }
    }

    pub fn popup_requested(&mut self, url: String) {
        self.pending_popup = Some(url);
    }

    pub fn cancel(&mut self) {
        self.hide();
        self.challenge = None;
        self.external_requested = false;
        #[cfg(windows)]
        {
            self.view = None;
            self.challenge_state = Default::default();
        }
        self.active.store(0, Ordering::Relaxed);
    }

    /// Use the configured browser and retain the queue's userscript marker.
    /// Only a successful launch starts the Downloads-folder watcher.
    pub fn open_external(&mut self) -> Result<()> {
        maps::https(&self.url)?;
        crate::browsers::open_url(&crate::userscript::tagged(&self.url))?;
        self.external_requested = true;
        Ok(())
    }

    /// Read only public check state; no native IPC bridge or response tokens
    /// are exposed. Replacing the state on open/cancel ignores late callbacks
    /// from the previous page's WebView.
    fn check_challenge(&mut self) {
        #[cfg(windows)]
        {
            use std::time::{Duration, Instant};
            let taken = self
                .challenge_state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take();
            if let Some(pending) = taken {
                if pending {
                    self.challenge.get_or_insert_with(Instant::now);
                } else {
                    self.challenge = None;
                }
            }
            if Instant::now() < self.next_challenge_check {
                return;
            }
            self.next_challenge_check = Instant::now() + Duration::from_secs(1);
            if let Some(view) = &self.view {
                let state = self.challenge_state.clone();
                let script = format!(
                    "(() => {{ if (document.readyState === 'loading') return null; return {{title: document.title, pending: ({CAPTCHA_PENDING})()}}; }})()"
                );
                let _ = view.evaluate_script_with_callback(&script, move |data| {
                    #[derive(serde::Deserialize)]
                    struct Check {
                        title: String,
                        pending: bool,
                    }
                    if let Ok(check) = serde_json::from_str::<Check>(&data) {
                        *state.lock().unwrap_or_else(|p| p.into_inner()) =
                            Some(check.pending || is_challenge_title(&check.title));
                    }
                });
            }
        }
    }

    pub fn show(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui;
        self.check_challenge();
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
        ui.horizontal_wrapped(|ui| {
            ui.strong(&self.title);
            if ui.button("← Назад").clicked() {
                #[cfg(windows)]
                if let Some(view) = &self.view {
                    let _ = view.evaluate_script("history.back()");
                }
            }
            if ui.button("Обновить страницу").clicked() {
                #[cfg(windows)]
                if let Some(view) = &self.view {
                    let _ = view.evaluate_script("location.reload()");
                }
            }
            if let Some(script) = access::public_file_script(&self.url)
                && ui
                    .button("Скачать файл со страницы")
                    .on_hover_text(
                        "Открыть бесплатную ссылку файла, опубликованную на этой странице",
                    )
                    .clicked()
            {
                #[cfg(windows)]
                if let Some(view) = &self.view {
                    let _ = view.evaluate_script(script);
                }
            }
            if ui.button("Внешний браузер ↗").clicked()
                && let Err(error) = self.open_external()
            {
                self.instructions = format!("Не удалось открыть браузер: {error}");
            }
            if ui.button("Вернуться к сборкам").clicked() {
                self.hide();
            }
        });
        ui.label(egui::RichText::new(&self.instructions).size(12.0));
        if let Some(url) = self.pending_popup.clone() {
            let host = maps::https(&url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default();
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Сайт запросил отдельное окно: {host}"));
                if ui.button("Открыть").clicked() {
                    self.navigate(&url);
                    self.pending_popup = None;
                }
                if ui.button("×").clicked() {
                    self.pending_popup = None;
                }
            });
        }
        let mut mirror = None;
        if !self.alternatives.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label("Другие ссылки автора:");
                for alternative in &self.alternatives {
                    if ui.button(&alternative.label).clicked() {
                        mirror = Some(alternative.url.clone());
                    }
                }
            });
        }
        if let Some(url) = mirror {
            self.instructions = access::browser_help(&url);
            self.navigate(&url);
        }
        let rect = ui.available_rect_before_wrap().intersect(ui.clip_rect());
        #[cfg(windows)]
        if let Some(view) = &self.view {
            let _ = view.set_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(rect.min.x as f64, rect.min.y as f64)
                    .into(),
                size: wry::dpi::LogicalSize::new(
                    rect.width().max(1.0) as f64,
                    rect.height().max(1.0) as f64,
                )
                .into(),
            });
            let _ = view.set_visible(self.visible);
        }
        ui.allocate_rect(rect, egui::Sense::hover());
    }
}

const CAPTCHA_PENDING: &str = include_str!("../assets/browser/captcha-pending.js");

const QUPER_FREE_DOWNLOAD: &str = r#"
(() => {
    const hosts = ['sharemods.com', 'www.sharemods.com', 'modsfire.com', 'www.modsfire.com', 'mods.to', 'www.mods.to'];
    if (!hosts.includes(location.hostname)) {
        // An author's own page: follow its first "Download" link to a file host
        // (older versions further down are ignored). Only the top page acts.
        if (window.top !== window) return;
        const fileHosts = /(^|\.)(sharemods\.com|modsfire\.com|mods\.to|wdfiles\.ru|drive\.google\.com|truckymods\.io)$/;
        let followed = false;
        const follow = setInterval(() => {
            if (followed || document.readyState === 'loading') return;
            if (captchaPending()) return;
            const link = [...document.querySelectorAll('a[href]')].find(a => {
                if (!/^\s*(download|скачать)\b/i.test(a.textContent)) return false;
                try {
                    const url = new URL(a.href, location.href);
                    return url.protocol === 'https:' && fileHosts.test(url.hostname);
                } catch { return false; }
            });
            if (link) { followed = true; clearInterval(follow); location.assign(link.href); }
        }, 1500);
        setTimeout(() => clearInterval(follow), 60000);
        return;
    }
    let acted = false;
    let readyAt = null;
    let consentDeclined = false;
    let checkShown = false;
    let generated = false;
    const timer = setInterval(() => {
        if (acted || document.readyState === 'loading') return;
        if (readyAt === null) readyAt = Date.now();
        const visible = e => e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden';
        // Cookie walls cover the page and the Cloudflare widget. They are always
        // answered with the most private choice.
        if (!consentDeclined) {
            const reject = [...document.querySelectorAll('button, a[role="button"]')].find(b => visible(b) && /^(reject all|decline all|refuse all|отклонить все|отклонить)$/i.test(b.textContent.trim()));
            if (reject) { consentDeclined = true; reject.click(); }
        }
        if (captchaPending()) {
            // Keep the check in sight in case Cloudflare asks the visitor to click it.
            const widget = document.querySelector('.cf-turnstile, .g-recaptcha, .h-captcha, iframe[src*="challenges.cloudflare.com"], iframe[src*="/recaptcha/"], iframe[src*="hcaptcha.com"]');
            if (!checkShown && widget) { checkShown = true; widget.scrollIntoView({ block: 'center' }); }
            return;
        }
        const direct = document.querySelector('a#downloadnow[href], a#downloadbtn[href], a#download_link[href]') || [...document.querySelectorAll('a[href]')].find(a => /^start download(?:\s|$)/i.test(a.textContent.trim()));
        if (direct) {
            const url = new URL(direct.href, location.href);
            if (url.protocol === 'https:' && !url.username && !url.password && !url.hostname.replace(/\.$/, '').match(/(^|\.)boosty\.to$/) && url.href !== location.href) {
                acted = true; clearInterval(timer); location.assign(url.href); return;
            }
        }
        // Wait for the host's normal timer; a visually enabled button can
        // still be rejected by its server when clicked immediately.
        if (Date.now() - readyAt < 15000) return;
        const countdown = document.querySelector('#countdown, #countdown_str, .countdown');
        if (countdown && /\d+/.test(countdown.textContent) && Number(countdown.textContent.match(/\d+/)[0]) > 0) return;
        if (location.hostname.includes('sharemods.com')) {
            const form = [...document.forms].find(f => ['download1', 'download2'].includes(f.querySelector('input[name="op"]')?.value));
            if (!form || form.querySelector('input[type="password"], input[name="captcha"]')) return;
            const target = new URL(form.getAttribute('action') || location.href, location.href);
            if (target.origin !== location.origin) return;
            // `method_free` is often a hidden field: clicking it does nothing, so
            // only real submit controls are considered.
            const button = form.querySelector('button[name="method_free"], input[type="submit"][name="method_free"]') || [...form.querySelectorAll('button, input[type="submit"]')].find(b => {
                const label = (b.value || b.textContent).trim().replace(/\s+/g, ' ');
                return !/premium|paid/i.test(label) && /^(create download link|free download|start download|download)(?:\s|$)/i.test(label);
            });
            if (button && !button.disabled && button.getAttribute('aria-disabled') !== 'true') {
                acted = true; clearInterval(timer); button.click();
            }
        } else if (location.hostname.includes('mods.to')) {
            const button = [...document.querySelectorAll('button, a[href]')].find(b => /^start now$/i.test(b.textContent.trim()));
            if (button && !button.disabled && button.getAttribute('aria-disabled') !== 'true') {
                acted = true; clearInterval(timer); button.click();
            }
        } else {
            // ModsFire removes the button's href, "generates" for a few seconds and
            // then turns it into a "Download Link" leading to the next step.
            const ready = [...document.querySelectorAll('a.download-button[href]')].find(a => /^download link$/i.test(a.textContent.trim()));
            if (ready) {
                const url = new URL(ready.getAttribute('href'), location.href);
                if (url.origin === location.origin) { acted = true; clearInterval(timer); location.assign(url.href); }
                return;
            }
            if (!generated) {
                const button = [...document.querySelectorAll('a, button')].find(b => /^generate\s+(download\s+)?link$/i.test(b.textContent.trim()));
                if (button && !button.disabled) { generated = true; button.click(); }
            }
        }
    }, 1500);
    setTimeout(() => clearInterval(timer), 600000);
})();
"#;

#[cfg(test)]
mod tests {
    use super::{Browser, is_challenge_title};

    #[test]
    fn cloudflare_check_pages_are_recognised_by_title() {
        assert!(is_challenge_title("Just a moment..."));
        assert!(is_challenge_title("Один момент…"));
        assert!(is_challenge_title("Attention Required! | Cloudflare"));
        assert!(!is_challenge_title(
            "Download file Koromyslovo Add-on for Project Russia 1.5.scs - ModsFire.com"
        ));
        assert!(!is_challenge_title(""));
    }

    #[test]
    fn invalid_handoff_does_not_start_watching_downloads() {
        let mut browser = Browser {
            url: "about:blank".into(),
            ..Default::default()
        };
        assert!(browser.open_external().is_err());
        assert!(!browser.external_requested);
    }

    #[cfg(windows)]
    #[test]
    fn check_lifecycle_ignores_late_callbacks_from_a_cancelled_view() {
        use std::time::{Duration, Instant};
        let mut browser = Browser {
            next_challenge_check: Instant::now() + Duration::from_secs(60),
            ..Default::default()
        };
        *browser.challenge_state.lock().unwrap() = Some(true);
        browser.check_challenge();
        let since = browser.challenge.unwrap();
        *browser.challenge_state.lock().unwrap() = Some(true);
        browser.check_challenge();
        assert_eq!(browser.challenge, Some(since));
        *browser.challenge_state.lock().unwrap() = Some(false);
        browser.check_challenge();
        assert!(browser.challenge.is_none());

        let old_check = browser.challenge_state.clone();
        browser.external_requested = true;
        browser.cancel();
        *old_check.lock().unwrap() = Some(true);
        browser.check_challenge();
        assert!(browser.challenge.is_none());
        assert!(!browser.external_requested);
    }
}
