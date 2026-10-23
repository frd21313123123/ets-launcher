use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use reqwest::{Client, Response, Url};
use scraper::{Html, Selector};
use serde::Deserialize;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MAX_PAGE: usize = 4 * 1024 * 1024;
const MAX_TORRENT: usize = 8 * 1024 * 1024;
const MAX_API: usize = 12 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct FileDescriptor {
    pub post_id: u64,
    pub file_id: u64,
    pub name: String,
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
    let files: Vec<_> = article
        .select(&file_selector)
        .filter(|file| {
            !file
                .value()
                .attr("data-is-subfile")
                .is_some_and(|value| !value.is_empty() && value != "0")
        })
        .filter(|file| file.value().attr("data-deleted") != Some("1"))
        .collect();
    ensure!(
        files.len() == 1,
        "На странице должен быть один основной файл с торрентом; найдено: {}",
        files.len()
    );
    let file = files[0].value();
    let name = file.attr("data-name").context("У файла нет имени")?;
    ensure!(safe_filename(name), "Сайт вернул некорректное имя файла");
    Ok(FileDescriptor {
        post_id,
        file_id: file
            .attr("data-file-id")
            .context("Отсутствует номер файла")?
            .parse()?,
        name: name.into(),
    })
}

fn safe_filename(name: &str) -> bool {
    !name.is_empty()
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
    let page = request(client.get(page_url), cancel).await
        .context("Не удалось открыть PlayGround. Проверьте доступ к сайту или обход VPN для ets-launcher.exe")?;
    let html = bounded_body(page, MAX_PAGE, cancel).await?;
    let file = find_file(std::str::from_utf8(&html).context("Страница не в UTF-8")?)?;
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
