use crate::game::{Channel, Game};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use reqwest::{Client, Response, Url};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

const MAX_PAGE: usize = 4 * 1024 * 1024;
const MAX_TORRENT: usize = 8 * 1024 * 1024;
const MAX_API: usize = 12 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDescriptor {
    pub post_id: u64,
    pub file_id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub file: FileDescriptor,
    pub channel: Channel,
    pub label: String,
    pub version: Option<String>,
    pub size_label: Option<String>,
    pub main: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub source_url: String,
    pub page_url: String,
    pub title: String,
    pub checked_at: u64,
    pub releases: Vec<Release>,
}

impl Catalog {
    pub fn select(&self, channel: Channel) -> Result<&Release> {
        let mut candidates: Vec<_> = self
            .releases
            .iter()
            .filter(|file| file.channel == channel)
            .collect();
        ensure!(
            !candidates.is_empty(),
            "На странице нет торрент-файла для выбранной ветки"
        );
        if candidates.len() > 1
            && candidates
                .iter()
                .all(|r| r.version.as_deref().and_then(version_key).is_some())
        {
            let newest = candidates
                .iter()
                .filter_map(|r| r.version.as_deref().and_then(version_key))
                .max()
                .unwrap();
            candidates
                .retain(|r| r.version.as_deref().and_then(version_key).as_ref() == Some(&newest));
        }
        if candidates.len() > 1 {
            let main: Vec<_> = candidates.iter().copied().filter(|r| r.main).collect();
            if main.len() == 1 {
                return Ok(main[0]);
            }
        }
        ensure!(
            candidates.len() == 1,
            "На странице несколько равнозначных торрентов для выбранной ветки: {}",
            candidates.len()
        );
        Ok(candidates[0])
    }

    /// The preferred branch when the page has an archive for it, otherwise the
    /// other branch if only that one exists. Pages often publish a single file
    /// labelled "BETA" that the author says also works on the release branch.
    pub fn effective_channel(&self, preferred: Channel) -> Channel {
        if self.select(preferred).is_ok() {
            return preferred;
        }
        let other = match preferred {
            Channel::Stable => Channel::Beta,
            Channel::Beta => Channel::Stable,
        };
        if self.select(other).is_ok() {
            other
        } else {
            preferred
        }
    }

    pub fn validate_for(&self, game: Game) -> Result<()> {
        ensure!(
            self.releases.len() <= 100,
            "Слишком много файлов на странице"
        );
        for release in &self.releases {
            ensure!(safe_filename(&release.file.name), "Некорректное имя архива");
            game.validate_archive(&release.file.name)?;
        }
        Ok(())
    }
}

pub struct FetchedTorrent {
    pub bytes: Vec<u8>,
    pub archive_name: String,
    pub archive_size: u64,
}

pub fn client() -> Result<Client> {
    Client::builder()
        .user_agent("Mozilla/5.0 ETS-Launcher/0.1")
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .context("Не удалось создать HTTP-клиент")
}

pub fn find_file(html: &str) -> Result<FileDescriptor> {
    find_file_for(html, Channel::Stable)
}

pub fn find_file_for(html: &str, channel: Channel) -> Result<FileDescriptor> {
    Ok(parse_catalog(html, "")?.select(channel)?.file.clone())
}

fn bounded_text(value: &str, maximum: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(maximum)
        .collect()
}

fn beta_label(value: &str) -> bool {
    let value = value.to_lowercase();
    value.contains("beta") || value.contains("бета") || value.contains('β')
}

pub fn parse_version(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    for (start, byte) in bytes.iter().enumerate() {
        if !byte.is_ascii_digit() || start > 0 && bytes[start - 1].is_ascii_digit() {
            continue;
        }
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            end += 1;
        }
        let number = value[start..end].trim_end_matches('.');
        let parts: Vec<_> = number.split('.').collect();
        if (2..=8).contains(&parts.len())
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.parse::<u32>().is_ok())
        {
            return Some(if bytes.get(end) == Some(&b's') {
                format!("{number}s")
            } else {
                number.to_owned()
            });
        }
    }
    None
}

fn version_key(value: &str) -> Option<[u32; 8]> {
    let mut result = [0; 8];
    for (index, part) in value.trim_end_matches('s').split('.').enumerate() {
        *result.get_mut(index)? = part.parse().ok()?;
    }
    Some(result)
}

pub fn parse_catalog(html: &str, source_url: &str) -> Result<Catalog> {
    let html = Html::parse_document(html);
    let article_selector = Selector::parse("main#post-item[data-post-id]").unwrap();
    let file_selector =
        Selector::parse(".article-content pg-file[data-can-torrent-download='1']").unwrap();
    let article = html.select(&article_selector).next()
        .context("На странице PlayGround не найден блок скачивания. Возможно, сайт изменился или требует входа.")?;
    ensure!(
        article.value().attr("data-download-allowed") != Some("0"),
        "PlayGround запретил скачивание этого файла"
    );
    let post_id = article
        .value()
        .attr("data-post-id")
        .context("Отсутствует номер публикации")?
        .parse()?;
    let title_selector = Selector::parse("h1").unwrap();
    let title = html
        .select(&title_selector)
        .next()
        .map(|node| bounded_text(&node.text().collect::<String>(), 240))
        .unwrap_or_default();
    let mut releases: Vec<Release> = Vec::new();
    for node in article.select(&file_selector) {
        let file = node.value();
        if file.attr("data-deleted") == Some("1") {
            continue;
        }
        let name = file.attr("data-name").context("У файла нет имени")?;
        let label = bounded_text(file.attr("data-text").unwrap_or(name), 240);
        let platform = format!("{name} {label}").to_ascii_lowercase();
        if platform.contains("linux")
            || platform.contains("steam deck")
            || platform.contains("steam_deck")
            || platform
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|p| matches!(p, "mac" | "macos"))
        {
            continue;
        }
        ensure!(safe_filename(name), "Сайт вернул некорректное имя файла");
        let descriptor = FileDescriptor {
            post_id,
            file_id: file
                .attr("data-file-id")
                .context("Отсутствует номер файла")?
                .parse()?,
            name: name.into(),
        };
        if let Some(previous) = releases
            .iter()
            .find(|r| r.file.file_id == descriptor.file_id)
        {
            ensure!(
                previous.file.name == descriptor.name,
                "Неоднозначные имена одного файла на странице"
            );
            continue;
        }
        let main = !file
            .attr("data-is-subfile")
            .is_some_and(|v| !v.is_empty() && v != "0");
        let channel = if beta_label(name) || beta_label(&label) {
            Channel::Beta
        } else {
            Channel::Stable
        };
        releases.push(Release {
            file: descriptor,
            channel,
            version: parse_version(&label).or_else(|| parse_version(name)),
            label,
            size_label: file
                .attr("data-size")
                .map(|v| bounded_text(v, 48))
                .filter(|v| !v.is_empty()),
            main,
        });
        ensure!(releases.len() <= 100, "Слишком много файлов на странице");
    }
    Ok(Catalog {
        source_url: source_url.into(),
        page_url: source_url.into(),
        title,
        checked_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        releases,
    })
}

pub async fn fetch_catalog(
    client: &Client,
    page_url: &str,
    cancel: &CancellationToken,
) -> Result<Catalog> {
    let response = request(client.get(page_url)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .header(reqwest::header::PRAGMA, "no-cache"), cancel).await
        .context("Не удалось открыть PlayGround. Проверьте доступ к сайту или обход VPN для ets-launcher.exe")?;
    let resolved_url = response.url().to_string();
    let body = bounded_body(response, MAX_PAGE, cancel).await?;
    let mut catalog = parse_catalog(
        std::str::from_utf8(&body).context("Страница не в UTF-8")?,
        page_url,
    )?;
    catalog.page_url = resolved_url;
    Ok(catalog)
}

fn safe_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 240
        && name != "."
        && name != ".."
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|ch| ch.is_control() || "<>:\"/\\|?*".contains(ch))
}

#[derive(Deserialize)]
struct DownloadReply {
    #[serde(default)]
    wait_time: Option<u64>,
    lock_hash: Option<String>,
    torrent_data: Option<String>,
    error: Option<serde_json::Value>,
    message: Option<String>,
}

/// Uses the same anonymous file.download endpoint as the site's download button.
/// The server's wait_time is honored before the lock_hash request is sent.
pub async fn fetch_torrent(
    client: &Client,
    page_url: &str,
    cancel: &CancellationToken,
    on_wait: impl Fn(u64),
) -> Result<FetchedTorrent> {
    fetch_torrent_for(client, page_url, Channel::Stable, cancel, on_wait).await
}

pub async fn fetch_torrent_for(
    client: &Client,
    page_url: &str,
    channel: Channel,
    cancel: &CancellationToken,
    on_wait: impl Fn(u64),
) -> Result<FetchedTorrent> {
    let catalog = fetch_catalog(client, page_url, cancel).await?;
    let file = &catalog.select(channel)?.file;
    let page_url = &catalog.page_url;
    let api_url = Url::parse(page_url)?.join("/api/file.download")?;
    let mut lock_hash: Option<String> = None;
    for _ in 0..3 {
        let mut params = vec![
            ("file_id", file.file_id.to_string()),
            ("post_id", file.post_id.to_string()),
            ("file_name", file.name.clone()),
        ];
        if let Some(lock) = &lock_hash {
            params.push(("lock_hash", lock.clone()));
            params.push(("log", "0".into()));
        }
        let response = request(
            client
                .get(api_url.clone())
                .query(&params)
                .header(reqwest::header::ACCEPT, "application/json")
                .header(reqwest::header::REFERER, page_url),
            cancel,
        )
        .await?;
        let body = bounded_body(response, MAX_API, cancel).await?;
        let reply: DownloadReply = serde_json::from_slice(&body)
            .context("Не удалось разобрать ответ PlayGround. Возможно, формат сайта изменился или требуется проверка в браузере")?;
        if reply
            .error
            .as_ref()
            .is_some_and(|error| !error.is_null() && error != false)
        {
            bail!(
                "PlayGround: {}",
                reply.message.as_deref().unwrap_or("скачивание недоступно")
            );
        }
        let wait_time = reply.wait_time.unwrap_or(0);
        if wait_time > 0 {
            ensure!(
                wait_time <= 300,
                "PlayGround просит ждать более пяти минут. Попробуйте позже"
            );
            lock_hash = Some(reply.lock_hash.context("Сайт не передал ключ ожидания")?);
            for remaining in (1..=wait_time).rev() {
                on_wait(remaining);
                tokio::select! {
                    _ = cancel.cancelled() => bail!("Загрузка остановлена"),
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
            }
            on_wait(0);
            continue;
        }
        let encoded = reply
            .torrent_data
            .context("PlayGround не вернул torrent_data")?;
        ensure!(
            encoded.len() <= MAX_TORRENT.div_ceil(3) * 4,
            "Торрент-файл слишком большой"
        );
        let bytes = STANDARD
            .decode(encoded.trim())
            .context("Повреждены данные torrent_data")?;
        return validate_torrent(bytes, &file.name);
    }
    bail!("Сайт несколько раз продлил ожидание. Повторите загрузку позже")
}

pub fn validate_torrent(bytes: Vec<u8>, expected_name: &str) -> Result<FetchedTorrent> {
    ensure!(bytes.len() <= MAX_TORRENT, "Торрент-файл слишком большой");
    let torrent: librqbit::TorrentMetaV1<librqbit::ByteBuf<'_>> =
        librqbit::torrent_from_bytes(&bytes).context("Ответ сайта не является торрент-файлом")?;
    let name = std::str::from_utf8(
        torrent
            .info
            .name
            .as_ref()
            .context("У торрента нет имени")?
            .as_ref(),
    )?;
    ensure!(
        name == expected_name && safe_filename(name),
        "Имя архива в торренте не совпадает с файлом на странице"
    );
    ensure!(
        torrent.info.files.is_none(),
        "Ожидался торрент с одним архивом"
    );
    let size = torrent
        .info
        .length
        .context("В торренте не указан размер архива")?;
    ensure!(
        size > 0 && torrent.info.piece_length > 0,
        "Некорректный размер архива или части торрента"
    );
    let expected_hash_bytes = size
        .div_ceil(torrent.info.piece_length as u64)
        .checked_mul(20)
        .context("Слишком большой размер торрента")?;
    ensure!(
        expected_hash_bytes == torrent.info.pieces.as_ref().len() as u64,
        "Некорректная таблица хешей торрента"
    );
    let archive_name = name.to_owned();
    Ok(FetchedTorrent {
        bytes,
        archive_name,
        archive_size: size,
    })
}

async fn request(builder: reqwest::RequestBuilder, cancel: &CancellationToken) -> Result<Response> {
    tokio::select! {
        _ = cancel.cancelled() => bail!("Загрузка остановлена"),
        result = builder.send() => result.context("Не удалось получить ответ сайта")?.error_for_status().context("Сайт вернул ошибку"),
    }
}

async fn bounded_body(
    response: Response,
    limit: usize,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= limit as u64),
        "Ответ сайта слишком большой"
    );
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => bail!("Загрузка остановлена"),
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else {
            return Ok(bytes);
        };
        let chunk = chunk.context("Ответ сайта был прерван")?;
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "Ответ сайта слишком большой"
        );
        bytes.extend_from_slice(&chunk);
    }
}
