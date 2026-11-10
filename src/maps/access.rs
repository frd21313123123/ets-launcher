//! Access requirements are evidence, not guesses based on a failed request.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadAccess {
    Direct,
    PublicWebsite,
    LoginRequired,
    BrowserVerification,
    RateLimited,
    PageChanged,
    Unavailable,
}

impl DownloadAccess {
    pub fn message(self) -> &'static str {
        match self {
            Self::Direct => "Файл доступен для автоматического скачивания",
            Self::PublicWebsite => {
                "Откройте бесплатную загрузку на сайте. Готовые файлы установятся автоматически."
            }
            Self::LoginRequired => {
                "Это зеркало явно запросило вход. Откройте его в браузере или выберите другое зеркало."
            }
            Self::BrowserVerification => {
                "Сайт не разрешил автоматический запрос. Откройте страницу в браузере; это не означает, что нужен аккаунт."
            }
            Self::RateLimited => {
                "Сайт ограничил частоту загрузок. Подождите или выберите другое зеркало."
            }
            Self::PageChanged => {
                "Форма загрузки изменилась. Продолжите скачивание на странице сайта."
            }
            Self::Unavailable => {
                "Не удалось получить файл автоматически. Откройте страницу сайта или выберите другое зеркало."
            }
        }
    }
}

#[derive(Debug)]
pub struct DownloadIssue {
    pub access: DownloadAccess,
    pub detail: String,
}
impl std::fmt::Display for DownloadIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.access.message(), self.detail)
    }
}
impl std::error::Error for DownloadIssue {}

pub fn issue(access: DownloadAccess, detail: impl Into<String>) -> anyhow::Error {
    DownloadIssue {
        access,
        detail: detail.into(),
    }
    .into()
}

pub fn from_error(error: &anyhow::Error) -> DownloadAccess {
    if let Some(issue) = error.downcast_ref::<DownloadIssue>() {
        return issue.access;
    }
    if let Some(http) = error.downcast_ref::<reqwest::Error>() {
        return match http.status().map(|s| s.as_u16()) {
            Some(401) => DownloadAccess::LoginRequired,
            Some(403) => DownloadAccess::BrowserVerification,
            Some(429) => DownloadAccess::RateLimited,
            _ => DownloadAccess::Unavailable,
        };
    }
    DownloadAccess::PageChanged
}

pub fn browser_help(url: &str) -> String {
    let host = super::https(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    match host.as_str() {
        "modsfire.com"|"www.modsfire.com"=>"Бесплатно без регистрации: нажмите Generate Link, затем Download. Install Mod / Premium — отдельный платный вариант; он не нужен для скачивания файла.".into(),
        "sharemods.com"|"www.sharemods.com"=>"Лаунчер сам дождётся таймера и проверки Cloudflare и нажмёт Download. Если Cloudflare попросит отметить «Я человек», отметьте — дальше загрузка продолжится автоматически, и файл попадёт в установщик.".into(),
        "mods.to"|"www.mods.to"=>"Лаунчер сам дождётся проверки Cloudflare и нажмёт Start now. Если Cloudflare попросит отметить «Я человек», отметьте — остальное произойдёт автоматически.".into(),
        _=>DownloadAccess::PublicWebsite.message().into(),
    }
}

/// Open only the public file anchor already rendered by the author host. Its
/// advertising overlay can consume every click when popups are declined.
/// This does not generate URLs, skip the host's preparation step, or read cookies.
pub fn public_file_script(page: &str) -> Option<&'static str> {
    let page = super::https(page).ok()?;
    if !matches!(page.host_str(), Some("modsfire.com" | "www.modsfire.com")) {
        return None;
    }
    let segments = page.path_segments()?.collect::<Vec<_>>();
    if segments.len() != 3
        || segments[0] != "download"
        || !segments[1..]
            .iter()
            .all(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric()))
    {
        return None;
    }
    Some(
        r#"(() => {
            const page = new URL(location.href);
            if (page.protocol !== 'https:' || !['modsfire.com','www.modsfire.com'].includes(page.hostname)) return false;
            const match = page.pathname.match(/^\/download\/([A-Za-z0-9]+)\/[A-Za-z0-9]+$/);
            const anchor = document.querySelector('a#downloadnow[href]');
            if (!match || !anchor) return false;
            const file = new URL(anchor.href, page);
            if (file.origin !== page.origin || file.pathname !== '/d/' + match[1] || file.search || file.hash) return false;
            location.assign(file.href);
            return true;
        })()"#,
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadAlternative {
    pub label: String,
    pub url: String,
}

/// Parse only the download form. Login links and Premium advertising elsewhere
/// on the page are not proof that a free download requires an account.
pub fn trucky_request(html: &str) -> Result<(u64, u64, Option<String>)> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let Some(form) = doc.select(&Selector::parse("downloadbox").unwrap()).next() else {
        let access = if doc
            .select(&Selector::parse("authenticationbox").unwrap())
            .next()
            .is_some()
        {
            DownloadAccess::LoginRequired
        } else {
            DownloadAccess::PageChanged
        };
        return Err(issue(access, "Форма файла не найдена."));
    };
    match form.value().attr("download_permitted_to") {
        Some("all") => {}
        Some("registered") => {
            return Err(issue(
                DownloadAccess::LoginRequired,
                "Автор ограничил это зеркало зарегистрированными пользователями.",
            ));
        }
        _ => {
            return Err(issue(
                DownloadAccess::PageChanged,
                "Неизвестный режим доступа к файлу.",
            ));
        }
    }
    let number = |name| -> Result<u64> {
        let value = form
            .value()
            .attr(name)
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&v| v > 0)
            .ok_or_else(|| {
                issue(
                    DownloadAccess::PageChanged,
                    "Изменились идентификаторы формы.",
                )
            })?;
        Ok(value)
    };
    let csrf = doc
        .select(&Selector::parse("meta[name=csrf-token]").unwrap())
        .next()
        .and_then(|e| e.value().attr("content"))
        .map(str::to_owned);
    Ok((number(":project_id")?, number(":build_id")?, csrf))
}

pub fn trucky_payload(payload: &serde_json::Value) -> Result<()> {
    if payload.get("success").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(());
    }
    let flag = |key| payload.get(key).and_then(|v| v.as_bool()) == Some(true);
    let access = if flag("rateLimited") {
        DownloadAccess::RateLimited
    } else if flag("loginRequired") || flag("requiresAuth") {
        DownloadAccess::LoginRequired
    } else {
        DownloadAccess::Unavailable
    };
    Err(issue(access, "Сервис не выдал ссылку на файл."))
}

pub fn validate_alternative(url: &str) -> Result<()> {
    let url = super::https(url)?;
    ensure!(url.fragment().is_none(), "Ссылка зеркала содержит фрагмент");
    Ok(())
}

pub fn allowed_popup(url: &str, origins: &[String]) -> bool {
    let Ok(url) = super::https(url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    origins.iter().any(|origin| {
        let Ok(origin) = super::https(origin) else {
            return false;
        };
        origin.host_str().is_some_and(|expected| {
            host == expected
                || host
                    .strip_suffix(expected)
                    .is_some_and(|prefix| prefix.ends_with('.'))
        })
    })
}
