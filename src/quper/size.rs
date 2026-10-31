//! Download size of a build, estimated before anything is downloaded.
//!
//! Quper's tables list no sizes, so each public source is asked once: file hosts
//! print the size on their page, Google Drive on its confirmation page, and
//! TruckyMods through the file link its own button requests. Nothing is
//! downloaded; sources that publish no size are counted separately.

use super::{Plan, files, source};
use anyhow::Result;
use futures_util::{StreamExt, stream};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Estimate {
    /// Release the estimate belongs to; a new version or date needs a new one.
    pub version: String,
    pub date: String,
    /// Sum of the sizes that are known.
    pub bytes: u64,
    /// Distinct sources (one archive can supply several mods).
    pub sources: usize,
    /// Sources whose size the host does not publish, plus rows without a public link.
    pub unknown: usize,
}

impl Estimate {
    pub fn matches(&self, plan_version: &str, plan_date: &str) -> bool {
        self.version == plan_version && self.date == plan_date
    }
}

/// "48 KB", "4.94 MB", "1,2 ГБ", "1.1G" → bytes (binary multiples, as hosts use).
pub fn parse_size(text: &str) -> Option<u64> {
    let pattern = regex::Regex::new(
        r"(?i)^\s*(\d+(?:[.,]\d+)?)\s*(b|k|kb|kib|m|mb|mib|g|gb|gib|t|tb|кб|мб|гб|тб)?\s*$",
    )
    .ok()?;
    let found = pattern.captures(text)?;
    let value: f64 = found[1].replace(',', ".").parse().ok()?;
    let unit = found
        .get(2)
        .map(|m| m.as_str().to_lowercase())
        .unwrap_or_default();
    let power = match unit.chars().next() {
        None | Some('b') => 0,
        Some('k' | 'к') => 1,
        Some('m' | 'м') => 2,
        Some('g' | 'г') => 3,
        Some('t' | 'т') => 4,
        _ => return None,
    };
    Some((value * 1024f64.powi(power)).round() as u64)
}

/// The size a host prints on its file page, for hosts with a known layout.
pub fn page_size(host: &str, html: &str) -> Option<u64> {
    let host = host.trim_start_matches("www.");
    let pattern = match host {
        "sharemods.com" => r"</strong>\s*<span>\(([^)<]{1,20})\)</span>",
        "modsfire.com" => r#"content="Download file [^"]*? - ([\d.,]+\s*[KMGT]?B)"#,
        "mods.to" => r#"<span class="text-1">\s*([\d.,]+\s*[KMGT]?B)\s*</span>"#,
        "drive.usercontent.google.com" => r"\((\d+(?:\.\d+)?[KMGT])\)",
        _ => return None,
    };
    let found = regex::Regex::new(pattern).ok()?.captures(html)?;
    parse_size(&found[1])
}

async fn probe(client: &Client, url: &str, cancel: &CancellationToken) -> Result<Option<u64>> {
    let mut url = source::public_url(url)?;
    if url.host_str() == Some("drive.google.com") {
        url = files::google_download(&url)?;
    }
    let host = url.host_str().unwrap_or_default().to_owned();
    match host.trim_start_matches("www.") {
        "truckymods.io" | "beta.truckymods.io" => {
            let (mut page, mut html, mut cookies) = page_with_cookies(client, url, cancel).await?;
            // A project page links to its download page, which holds the file form.
            // The first download button is the newest build.
            if !html.contains("<downloadbox") {
                let button = regex::Regex::new(
                    r#":download_url="'(https://[^']+/download/(?:\d+|latest))'""#,
                )?;
                let Some(link) = button.captures(&html).map(|c| c[1].to_owned()) else {
                    return Ok(None);
                };
                (page, html, cookies) =
                    page_with_cookies(client, source::public_url(&link)?, cancel).await?;
            }
            // The form is tied to the page's session, as in the site's own button.
            let (link, _) = files::trucky_link(client, &page, &html, &cookies, cancel).await?;
            file_length(client, &link, cancel).await
        }
        "sharemods.com" | "modsfire.com" | "mods.to" => {
            let (_, html) = source::get(client, url.as_str(), cancel).await?;
            Ok(page_size(&host, &String::from_utf8_lossy(&html)))
        }
        "drive.usercontent.google.com" => {
            let response = tokio::select! {
                _ = cancel.cancelled() => anyhow::bail!("Остановлено"),
                r = client.get(url.clone()).send() => r?.error_for_status()?,
            };
            let html = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|t| t.contains("html"));
            if !html {
                // A small file is served at once; its length is enough, the body is dropped.
                return Ok(response.content_length());
            }
            let (_, page) = source::get(client, url.as_str(), cancel).await?;
            Ok(page_size(&host, &String::from_utf8_lossy(&page)))
        }
        _ => Ok(None),
    }
}

/// A public page with the session cookies it sets.
async fn page_with_cookies(
    client: &Client,
    url: Url,
    cancel: &CancellationToken,
) -> Result<(Url, String, String)> {
    let response = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("Остановлено"),
        r = client.get(url).send() => r?.error_for_status()?,
    };
    let page = source::public_url(response.url().as_str())?;
    let cookies = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");
    anyhow::ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= 8 * 1024 * 1024),
        "Страница слишком большая"
    );
    let html = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("Остановлено"),
        t = response.text() => t?,
    };
    Ok((page, html, cookies))
}

/// Length of a signed file link. Such links are valid for GET only, so a single
/// byte is requested and the total is read from `Content-Range`.
async fn file_length(
    client: &Client,
    url: &str,
    cancel: &CancellationToken,
) -> Result<Option<u64>> {
    let url: Url = source::public_url(url)?;
    let response = tokio::select! {
        _ = cancel.cancelled() => anyhow::bail!("Остановлено"),
        r = client.get(url).header("Range", "bytes=0-0").send() => r?.error_for_status()?,
    };
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let total = header("content-range")
        .and_then(|range| range.rsplit('/').next().and_then(|v| v.trim().parse().ok()))
        .or_else(|| {
            // A server that ignores Range answers with the whole file; only its length is used.
            (response.status() == reqwest::StatusCode::OK)
                .then(|| header("content-length").and_then(|v| v.parse().ok()))
                .flatten()
        });
    Ok(total.filter(|length: &u64| *length > 0))
}

/// Asks every distinct source of the required rows for its size. `done` counts
/// finished sources so the window can show progress.
pub async fn estimate(
    plan: &Plan,
    done: Arc<AtomicUsize>,
    cancel: &CancellationToken,
) -> Result<Estimate> {
    let client = source::client()?;
    let mut seen = BTreeSet::new();
    let mut urls = Vec::new();
    let mut unknown = 0;
    for row in plan.files.iter().filter(|r| r.required) {
        match row.urls.first() {
            Some(url) if seen.insert(url.clone()) => urls.push(url.clone()),
            Some(_) => {}
            None => unknown += 1,
        }
    }
    let sources = urls.len();
    let sizes = stream::iter(urls)
        .map(|url| {
            let (client, done) = (&client, done.clone());
            async move {
                let size = probe(client, &url, cancel).await.ok().flatten();
                done.fetch_add(1, Ordering::Relaxed);
                size
            }
        })
        .buffer_unordered(6)
        .collect::<Vec<_>>()
        .await;
    anyhow::ensure!(!cancel.is_cancelled(), "Остановлено");
    unknown += sizes.iter().filter(|s| s.is_none()).count();
    Ok(Estimate {
        version: plan.combo.version.clone(),
        date: plan.combo.date.clone(),
        bytes: sizes.into_iter().flatten().sum(),
        sources,
        unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_printed_by_hosts_are_read() {
        assert_eq!(parse_size("48 KB"), Some(48 * 1024));
        assert_eq!(parse_size("4.94 MB"), Some(5_179_965));
        assert_eq!(parse_size("1,5 ГБ"), Some(1_610_612_736));
        assert_eq!(parse_size("1.1G"), Some(1_181_116_006));
        assert_eq!(parse_size("много"), None);
        assert_eq!(
            page_size(
                "sharemods.com",
                "<strong>\n  RealisticAddonFerryQuper.scs\n</strong>\n<span>(48 KB)</span>"
            ),
            Some(48 * 1024)
        );
        assert_eq!(
            page_size(
                "modsfire.com",
                r#"<meta name="description" content="Download file gmc.fullscreen.maps.ets2.scs - 91.23 KB MB from ModsFire.com">"#
            ),
            Some(93_420)
        );
        assert_eq!(
            page_size(
                "mods.to",
                r#"<h1 class="title-5">BXP.scs <span class="text-1">
                4.94 MB </span></h1>"#
            ),
            Some(5_179_965)
        );
        assert_eq!(
            page_size(
                "drive.usercontent.google.com",
                "<p>map.rar (1.1G) is too large for Google to scan</p>"
            ),
            Some(1_181_116_006)
        );
        assert_eq!(page_size("wdfiles.ru", "<span>(48 KB)</span>"), None);
    }
}
