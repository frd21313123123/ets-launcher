use super::access::{self, DownloadAccess, DownloadAlternative};
use super::{Catalog, DownloadFile, MapEntry, SourceState, https};
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use scraper::{Html, Selector};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("Mozilla/5.0 ETS-Launcher/0.2 (map catalog)")
        .timeout(Duration::from_secs(40))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() > 5 || attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}

pub async fn get(
    client: &reqwest::Client,
    url: &str,
    cancel: &CancellationToken,
) -> Result<(String, Vec<u8>)> {
    https(url)?;
    let response = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("Проверка остановлена"),
        r = client.get(url).send() => r?.error_for_status()?,
    };
    let final_url = response.url().to_string();
    https(&final_url)?;
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("Проверка остановлена"),
        next = stream.next() => next,
    } {
        let chunk = chunk?;
        ensure!(
            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
            "Страница источника слишком большая"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok((final_url, bytes))
}

pub async fn fetch_catalog(url: &str, cancel: &CancellationToken) -> Result<Catalog> {
    let (_, bytes) = get(&client()?, url, cancel).await?;
    let catalog: Catalog =
        serde_json::from_slice(&bytes).context("Источник вернул неверный JSON каталога")?;
    catalog.validate()?;
    Ok(catalog)
}

pub fn parse(entry: &MapEntry, html: &str, page_url: &str) -> Result<SourceState> {
    let doc = Html::parse_document(html);
    let selector = Selector::parse(&entry.source.selector)
        .map_err(|_| anyhow::anyhow!("Некорректный селектор"))?;
    let element = doc
        .select(&selector)
        .next()
        .context("Изменилась разметка сайта: блок релиза не найден")?;
    let text = element.text().collect::<Vec<_>>().join(" ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = entry
        .source
        .stop_before
        .as_ref()
        .and_then(|s| text.split_once(s).map(|(a, _)| a))
        .unwrap_or(&text);
    let regex = regex::Regex::new(&entry.source.version_pattern)?;
    let version = regex
        .captures(text)
        .and_then(|c| c.get(1))
        .context(
            "Версия не распознана. Откройте сайт автора; автоматическая установка приостановлена.",
        )?
        .as_str()
        .to_owned();
    let base = https(page_url)?;
    let link_html = element.inner_html();
    let link_html = entry
        .source
        .stop_before
        .as_ref()
        .and_then(|marker| link_html.split_once(marker).map(|(current, _)| current))
        .unwrap_or(&link_html);
    let link_doc = Html::parse_fragment(link_html);
    let link_pattern = entry
        .source
        .download_pattern
        .as_ref()
        .map(|p| regex::Regex::new(p))
        .transpose()?;
    let mut links: Vec<_> = link_doc
        .select(&Selector::parse("a[href]").unwrap())
        .filter_map(|a| {
            let href = base.join(a.value().attr("href")?).ok()?;
            if href.scheme() != "https" {
                return None;
            }
            let name = a.text().collect::<Vec<_>>().join(" ").trim().to_owned();
            Some((name, href.to_string()))
        })
        .take(300)
        .collect();
    // The sidebar's /latest button carries current size and build ID. Earlier
    // buttons belong to release history and must never win by document order.
    let button = doc
        .select(&Selector::parse("downloadbutton").unwrap())
        .find(|b| {
            b.value()
                .attr(":download_url")
                .is_some_and(|v| v.trim_matches(['\'', '"']).ends_with("/download/latest"))
        });
    let mut download_size = None;
    let mut download_page = None;
    if let Some(button) = button {
        download_size = button.value().attr(":size").and_then(|v| v.parse().ok());
        if let Some(value) = button.value().attr(":download_url") {
            download_page = base
                .join(value.trim_matches(['\'', '"']))
                .ok()
                .filter(|u| u.scheme() == "https")
                .map(|u| u.to_string());
        }
    }
    if let Some(selector) = &entry.source.download_selector {
        let selector = Selector::parse(selector)
            .map_err(|_| anyhow::anyhow!("Изменился селектор загрузки"))?;
        download_page = link_doc
            .select(&selector)
            .filter_map(|a| base.join(a.value().attr("href")?).ok())
            .find(|url| {
                url.scheme() == "https"
                    && link_pattern
                        .as_ref()
                        .is_none_or(|p| p.is_match(url.as_str()))
            })
            .map(|u| u.to_string())
            .or(download_page);
    }
    if let Some(page) = &download_page {
        links.insert(0, ("Текущая загрузка".into(), page.clone()));
    }
    let game_versions = capture(&entry.source.game_pattern, text)
        .map(|s| {
            regex::Regex::new(r"1\.\d{2}")
                .unwrap()
                .find_iter(&s)
                .map(|m| m.as_str().to_owned())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        })
        .unwrap_or_default();
    Ok(SourceState {
        version: Some(version),
        page_url: page_url.into(),
        checked_at: now(),
        error: None,
        links,
        published_at: capture(&entry.source.date_pattern, text),
        size_label: capture(&entry.source.size_pattern, text),
        requirements: capture(&entry.source.requirements_pattern, text),
        game_versions,
        download_page,
        download_size,
        downloads: Vec::new(),
    })
}

#[derive(Clone, Debug)]
pub struct PreparedDownload {
    pub entry: MapEntry,
    pub page: String,
    pub message: String,
    pub access: DownloadAccess,
    pub alternatives: Vec<DownloadAlternative>,
}

/// Resolve fresh author links only at download time. Signed links and CSRF
/// cookies remain transient and are never written into the shared catalog.
pub async fn prepare(entry: &MapEntry, cancel: &CancellationToken) -> Result<PreparedDownload> {
    let state = verify(entry, cancel).await?;
    let page = state
        .download_page
        .clone()
        .or_else(|| entry.source.browser_url.clone())
        .unwrap_or_else(|| state.page_url.clone());
    let direct = !entry.release.downloads.is_empty();
    let mut prepared = PreparedDownload {
        entry: entry.clone(),
        page: page.clone(),
        message: if direct {
            DownloadAccess::Direct.message().into()
        } else {
            access::browser_help(&page)
        },
        access: if direct {
            DownloadAccess::Direct
        } else {
            DownloadAccess::PublicWebsite
        },
        alternatives: Vec::new(),
    };
    if prepared.entry.release.downloads.is_empty() && entry.source.download_api.is_some() {
        match trucky_download(entry, &prepared.page, cancel).await {
            Ok(file) => {
                prepared.entry.release.downloads.push(file);
                prepared.access = DownloadAccess::Direct;
                prepared.message = prepared.access.message().into();
            }
            Err(error) if !cancel.is_cancelled() => {
                prepared.access = access::from_error(&error);
                prepared.message = prepared.access.message().into();
            }
            Err(error) => return Err(error),
        }
    }
    if prepared.entry.release.downloads.is_empty() {
        prepared.alternatives = mirrors(entry, cancel).await.unwrap_or_default();
        ensure!(!cancel.is_cancelled(), "Подготовка загрузки остановлена");
        prepared.alternatives.retain(|a| a.url != prepared.page);
        if let Some(index) = prepared
            .alternatives
            .iter()
            .position(|a| is_public_host(&a.url))
        {
            let alternative = prepared.alternatives.remove(index);
            let old = DownloadAlternative {
                label: "Другое зеркало автора".into(),
                url: prepared.page,
            };
            prepared.page = alternative.url;
            prepared.alternatives.insert(0, old);
            prepared.access = DownloadAccess::PublicWebsite;
            prepared.message = access::browser_help(&prepared.page);
        }
    }
    if !prepared.entry.release.downloads.is_empty() {
        prepared
            .entry
            .release
            .downloads
            .extend(supplements(entry, cancel).await?);
    }
    Ok(prepared)
}

pub async fn supplements(
    entry: &MapEntry,
    cancel: &CancellationToken,
) -> Result<Vec<DownloadFile>> {
    let mut files = Vec::new();
    for rule in &entry.source.supplements {
        let (url, bytes) = get(&client()?, &rule.page, cancel).await?;
        let file = {
            let doc = Html::parse_document(&String::from_utf8_lossy(&bytes));
            let selector = Selector::parse(&rule.selector)
                .map_err(|_| anyhow::anyhow!("Изменился селектор дополнительного файла"))?;
            let pattern = regex::Regex::new(
                &rule
                    .pattern
                    .replace("{version}", &regex::escape(&entry.release.version)),
            )?;
            let base = https(&url)?;
            doc.select(&selector)
                .filter_map(|a| base.join(a.value().attr("href")?).ok())
                .find(|u| u.scheme() == "https" && pattern.is_match(u.as_str()))
                .context("На сайте нет дополнительного файла для этой версии")?
                .to_string()
        };
        let headers = tokio::select! {
            _=cancel.cancelled()=>anyhow::bail!("Скачивание остановлено"),
            result=client()?.head(&file).send()=>result?.headers().clone(),
        };
        let fallback = rule.filename.replace("{version}", &entry.release.version);
        let filename = author_filename(
            headers
                .get(reqwest::header::CONTENT_DISPOSITION)
                .and_then(|h| h.to_str().ok()),
            &fallback,
        )?;
        files.push(DownloadFile {
            url: file,
            filename,
            sha256: None,
        });
    }
    Ok(files)
}
pub fn is_public_host(url: &str) -> bool {
    https(url).ok().is_some_and(|u| {
        matches!(
            u.host_str(),
            Some("modsfire.com" | "www.modsfire.com" | "sharemods.com" | "www.sharemods.com")
        )
    })
}

pub fn mirror_links(
    html: &str,
    page: &str,
    selector: &str,
    pattern: &str,
) -> Result<Vec<DownloadAlternative>> {
    mirror_links_in_row(html, page, selector, pattern, false)
}

pub fn mirror_links_in_row(
    html: &str,
    page: &str,
    selector: &str,
    pattern: &str,
    same_row: bool,
) -> Result<Vec<DownloadAlternative>> {
    let doc = Html::parse_document(html);
    let base = https(page)?;
    let selector =
        Selector::parse(selector).map_err(|_| anyhow::anyhow!("Изменился селектор зеркал"))?;
    let pattern = regex::Regex::new(pattern)?;
    let mut links = Vec::new();
    for element in doc.select(&selector) {
        let Some(url) = element.value().attr("href").and_then(|h| base.join(h).ok()) else {
            continue;
        };
        if access::validate_alternative(url.as_str()).is_err()
            || !pattern.is_match(url.as_str())
            || links
                .iter()
                .any(|a: &DownloadAlternative| a.url == url.as_str())
        {
            continue;
        }
        links.push(DownloadAlternative {
            label: url.host_str().unwrap_or("Зеркало автора").to_owned(),
            url: url.to_string(),
        });
        if same_row {
            let row = element
                .ancestors()
                .filter_map(scraper::ElementRef::wrap)
                .find(|e| e.value().name() == "tr");
            if let Some(row) = row {
                for a in row.select(&Selector::parse("a[href]").unwrap()) {
                    if let Some(url) = a.value().attr("href").and_then(|h| base.join(h).ok())
                        && is_public_host(url.as_str())
                        && !links.iter().any(|a| a.url == url.as_str())
                    {
                        links.push(DownloadAlternative {
                            label: url.host_str().unwrap().to_owned(),
                            url: url.to_string(),
                        });
                    }
                }
            }
        }
    }
    Ok(links)
}

async fn mirrors(entry: &MapEntry, cancel: &CancellationToken) -> Result<Vec<DownloadAlternative>> {
    let mut links = Vec::new();
    for rule in &entry.source.mirrors {
        let (url, bytes) = get(&client()?, &rule.page, cancel).await?;
        let pattern = rule
            .pattern
            .replace("{version}", &regex::escape(&entry.release.version))
            .replace(
                "{version_compact}",
                &regex::escape(&entry.release.version.replace('.', "")),
            );
        links.extend(mirror_links_in_row(
            &String::from_utf8_lossy(&bytes),
            &url,
            &rule.selector,
            &pattern,
            rule.same_row,
        )?);
    }
    Ok(links)
}

pub fn author_filename(disposition: Option<&str>, fallback: &str) -> Result<String> {
    let filename = disposition
        .and_then(|h| {
            h.split(';')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("filename="))
        })
        .map(|s| s.trim_matches('"'))
        .unwrap_or(fallback);
    ensure!(
        !filename.chars().any(char::is_control)
            && crate::archive::safe_relative(filename)?
                .components()
                .count()
                == 1,
        "Опасное авторское имя файла"
    );
    Ok(filename.into())
}

async fn trucky_download(
    entry: &MapEntry,
    page: &str,
    cancel: &CancellationToken,
) -> Result<DownloadFile> {
    let client = client()?;
    let response = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("Скачивание остановлено"),
        r = client.get(https(page)?).send() => r?.error_for_status()?,
    };
    let cookies = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|s| s.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");
    let base = response.url().clone();
    let bytes = bounded(response, cancel).await?;
    let (project, build, csrf) = access::trucky_request(&String::from_utf8_lossy(&bytes))?;
    let api = base.join(
        entry
            .source
            .download_api
            .as_deref()
            .context("Нет API источника")?,
    )?;
    ensure!(
        api.origin() == base.origin(),
        "API загрузки сменил источник"
    );
    let mut request = client
        .post(api)
        .header("Referer", base.as_str())
        .header("Accept", "application/json")
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(
            &serde_json::json!({"project_id":project,"build_id":build}),
        )?);
    if !cookies.is_empty() {
        request = request.header(reqwest::header::COOKIE, cookies);
    }
    if let Some(csrf) = csrf {
        request = request.header("X-CSRF-TOKEN", csrf);
    }
    let response = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("Скачивание остановлено"),
        r = request.send() => r?.error_for_status()?,
    };
    let payload: serde_json::Value = serde_json::from_slice(&bounded(response, cancel).await?)?;
    access::trucky_payload(&payload)?;
    let filename = payload
        .get("fileName")
        .and_then(|v| v.as_str())
        .context("Нет имени файла")?
        .to_owned();
    let path = crate::archive::safe_relative(&filename)?;
    ensure!(
        path.components().count() == 1,
        "Опасное имя файла от источника"
    );
    ensure!(
        path.extension().is_some_and(|e| matches!(
            e.to_string_lossy().to_lowercase().as_str(),
            "scs" | "zip" | "7z" | "rar"
        )),
        "Неизвестный формат загрузки"
    );
    let url = payload
        .get("url")
        .and_then(|v| v.as_str())
        .context("Нет ссылки файла")?
        .to_owned();
    https(&url)?;
    tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("Скачивание остановлено"),
        _ = tokio::time::sleep(Duration::from_secs(entry.source.download_wait_seconds)) => {}
    }
    Ok(DownloadFile {
        url,
        filename,
        sha256: None,
    })
}

async fn bounded(response: reqwest::Response, cancel: &CancellationToken) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("Запрос остановлен"),
        chunk = stream.next() => chunk,
    } {
        let chunk = chunk?;
        ensure!(
            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
            "Слишком большой ответ сайта"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn capture(pattern: &Option<String>, text: &str) -> Option<String> {
    regex::Regex::new(pattern.as_ref()?)
        .ok()?
        .captures(text)?
        .get(1)
        .map(|value| value.as_str().chars().take(600).collect())
}

pub async fn fetch(entry: &MapEntry, cancel: &CancellationToken) -> Result<SourceState> {
    let (url, bytes) = get(&client()?, &entry.source.url, cancel).await?;
    let mut state = parse(entry, &String::from_utf8_lossy(&bytes), &url)?;
    if entry.source.kind == "promods" {
        let detail = {
            let doc = Html::parse_document(&String::from_utf8_lossy(&bytes));
            let selector = Selector::parse(&entry.source.selector)
                .map_err(|_| anyhow::anyhow!("Изменился источник ProMods"))?;
            doc.select(&selector)
                .next()
                .and_then(|e| e.value().attr("href"))
                .map(str::to_owned)
        };
        if let Some(detail) = detail {
            let detail = https(&url)?.join(&detail)?.to_string();
            let (_, bytes) = get(&client()?, &detail, cancel).await?;
            let doc = Html::parse_document(&String::from_utf8_lossy(&bytes));
            let text = doc
                .root_element()
                .text()
                .collect::<Vec<_>>()
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            state.game_versions = capture(&entry.source.game_pattern, &text)
                .into_iter()
                .collect();
            state.requirements = capture(&entry.source.requirements_pattern, &text);
        }
    }
    Ok(state)
}

/// Never label a newly discovered release as compatible using rules for the old one.
pub async fn verify(entry: &MapEntry, cancel: &CancellationToken) -> Result<SourceState> {
    let state = fetch(entry, cancel).await?;
    ensure!(
        state.version.as_deref() == Some(&entry.release.version),
        "{}: на сайте версия {}, каталог проверен для {}. Обновите каталог совместимости.",
        entry.name,
        state.version.as_deref().unwrap_or("?"),
        entry.release.version
    );
    Ok(state)
}
