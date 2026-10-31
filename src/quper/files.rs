//! Public host download flows. HTML never becomes a completed mod file.
use super::source::{self, public_url};
use crate::{
    archive,
    download::{self, SharedTransfer, Status},
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use reqwest::{Client, Response, Url};
use scraper::{Html, Selector};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

pub fn allowed_filename(name: &str) -> bool {
    archive::safe_relative(name).is_ok_and(|p| p.components().count() == 1)
        && Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "scs" | "zip" | "rar" | "7z"
                ) || (ext.len() == 3 && ext.bytes().all(|b| b.is_ascii_digit()))
            })
}

fn percent_decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])?;
            decoded.push(u8::from_str_radix(hex, 16)?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    Ok(String::from_utf8(decoded)?)
}

pub fn filename(disposition: Option<&str>, url: &Url, fallback: Option<&str>) -> Result<String> {
    let extended = disposition
        .and_then(|h| {
            h.split(';')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("filename*="))
        })
        .map(|s| s.trim_matches('"'))
        .and_then(|s| s.split_once("''").map(|(_, s)| s))
        .map(percent_decode)
        .transpose()?;
    let simple = disposition
        .and_then(|h| {
            h.split(';')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("filename="))
        })
        .map(|s| s.trim_matches('"').to_owned());
    let from_path = url
        .path_segments()
        .and_then(|mut p| p.next_back())
        .map(percent_decode)
        .transpose()?;
    let name = extended
        .or(simple)
        .or_else(|| from_path.filter(|s| allowed_filename(s)))
        .or_else(|| fallback.map(str::to_owned))
        .context("Сайт не сообщил имя файла; откройте его во встроенном браузере")?;
    ensure!(
        allowed_filename(&name),
        "Сайт предложил неподдерживаемый или опасный файл: {name}"
    );
    Ok(name)
}

#[derive(Clone, Debug)]
pub struct Form {
    pub url: String,
    pub fields: Vec<(String, String)>,
    pub post: bool,
    pub wait: u64,
}

#[derive(Debug)]
pub enum Step {
    Links(Vec<String>),
    Form(Form),
    Trucky,
}

/// A part of a split archive: "pack.zip.001" is part 1 of "pack.zip",
/// "pack.part2.rar" is part 2 of "pack.rar". Whole files give `None`.
pub fn split_part(name: &str) -> Option<(String, u32)> {
    let lower = name.to_ascii_lowercase();
    if let Some((base, number)) = lower.rsplit_once('.')
        && number.len() == 3
        && number.bytes().all(|b| b.is_ascii_digit())
        && !base.is_empty()
    {
        return Some((base.to_owned(), number.parse().ok()?));
    }
    let stem = lower.strip_suffix(".rar")?;
    let (base, number) = stem.rsplit_once(".part")?;
    if base.is_empty() || number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((format!("{base}.rar"), number.parse().ok()?))
}

/// A browser's unfinished download (Chrome, Edge, Firefox, Zen, Opera, Safari).
pub fn unfinished_download(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".crdownload",
        ".part",
        ".partial",
        ".download",
        ".opdownload",
        ".tmp",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

/// An unfinished download that will become a mod or archive: Firefox's
/// "mod.Ab12.scs.part", Chrome's "mod.zip.crdownload" or its nameless
/// "Unconfirmed 123.crdownload". Videos and other downloads give `false`.
pub fn unfinished_mod(name: &str) -> bool {
    if !unfinished_download(name) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    let Some((target, _)) = lower.rsplit_once('.') else {
        return false;
    };
    allowed_filename(target) || target.starts_with("unconfirmed ")
}

/// The original name of a file a browser saved again under a numbered name:
/// "mod(1)" or "mod (2)" gives "mod". Other names give `None`.
pub fn browser_copy_base(stem: &str) -> Option<&str> {
    let open = stem.strip_suffix(')')?.rfind('(')?;
    let number = &stem[open + 1..stem.len() - 1];
    if number.is_empty() || number.len() > 3 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let base = stem[..open].trim_end();
    (!base.is_empty()).then_some(base)
}

/// File hosts the launcher knows how to download from; any other site is an
/// author's own page that links to one of them.
pub fn is_file_host(url: &Url) -> bool {
    matches!(
        url.host_str().map(|h| h.trim_start_matches("www.")),
        Some(
            "sharemods.com"
                | "modsfire.com"
                | "mods.to"
                | "wdfiles.ru"
                | "drive.google.com"
                | "drive.usercontent.google.com"
                | "truckymods.io"
                | "beta.truckymods.io"
        )
    )
}

/// The current download of an author's page: the first "Download"/"Скачать"
/// link that leads to a file host. Older versions listed further down the page
/// are ignored.
pub fn primary_download(base: &Url, html: &str) -> Option<Url> {
    let doc = Html::parse_document(html);
    let label = regex::Regex::new(r"(?i)^\s*(download|скачать)\b").ok()?;
    doc.select(&Selector::parse("a[href]").unwrap())
        .filter(|a| label.is_match(&a.text().collect::<String>()))
        .filter_map(|a| base.join(a.value().attr("href")?).ok())
        .find(|url| public_url(url.as_str()).is_ok() && is_file_host(url))
}

/// Select the host's actual free form, ignoring login forms and advertising.
pub fn page_step(base: &Url, html: &str) -> Result<Step> {
    let doc = Html::parse_document(html);
    let host = base.host_str().unwrap_or_default();
    if matches!(host, "truckymods.io" | "beta.truckymods.io") {
        if doc
            .select(&Selector::parse("downloadbox").unwrap())
            .next()
            .is_some()
        {
            return Ok(Step::Trucky);
        }
        for node in doc.select(&Selector::parse("downloadbutton").unwrap()) {
            if let Some(value) = node.value().attr(":download_url") {
                let url = base.join(value.trim_matches('\''))?;
                if url.path().ends_with("/download/latest") {
                    public_url(url.as_str())?;
                    return Ok(Step::Links(vec![url.to_string()]));
                }
            }
        }
    }
    for selector in [
        "a#downloadnow[href]",
        "a#downloadbtn[href]",
        "a#download_link[href]",
        "a.download-link[href]",
        "a.btn-download[href]",
    ] {
        for a in doc.select(&Selector::parse(selector).unwrap()) {
            let url = base.join(a.value().attr("href").unwrap())?;
            if public_url(url.as_str()).is_ok() && url != *base {
                return Ok(Step::Links(vec![url.to_string()]));
            }
        }
    }
    let free_form_host = matches!(
        host,
        "sharemods.com"
            | "www.sharemods.com"
            | "modsfire.com"
            | "www.modsfire.com"
            | "wdfiles.ru"
            | "www.wdfiles.ru"
            | "mods.to"
            | "drive.usercontent.google.com"
    );
    if free_form_host {
        let wait_pattern = regex::Regex::new(
            r#"(?i)(?:countdown_str|countdown|wait_time|seconds)\s*[=:>(]\s*['"]?(\d{1,3})"#,
        )?;
        for form in doc.select(&Selector::parse("form").unwrap()) {
            let mut fields = Vec::new();
            for input in form.select(&Selector::parse("input[name]").unwrap()) {
                let name = input.value().attr("name").unwrap();
                if name == "method_premium" {
                    continue;
                }
                let kind = input.value().attr("type").unwrap_or("text");
                if matches!(kind, "password" | "email") {
                    fields.clear();
                    break;
                }
                if kind == "hidden" || (kind == "submit" && name == "method_free") {
                    fields.push((
                        name.to_owned(),
                        input.value().attr("value").unwrap_or_default().to_owned(),
                    ));
                }
            }
            let download = fields.iter().any(|(k, v)| {
                (k == "op" && matches!(v.as_str(), "download1" | "download2"))
                    || (host == "drive.usercontent.google.com" && k == "id")
            });
            if !download {
                continue;
            }
            // Captchas and account checks are left to the normal browser flow.
            if form
                .select(
                    &Selector::parse(
                        ".g-recaptcha, .h-captcha, .cf-turnstile, [name=g-recaptcha-response], [name=cf-turnstile-response], [name=captcha]",
                    )
                    .unwrap(),
                )
                .next()
                .is_some()
            {
                bail!("Сайт требует проверку в браузере");
            }
            let target = base.join(
                form.value()
                    .attr("action")
                    .filter(|s| !s.is_empty())
                    .unwrap_or(base.as_str()),
            )?;
            public_url(target.as_str())?;
            ensure!(
                target.origin() == base.origin(),
                "Форма загрузки сменила сайт"
            );
            let wait = wait_pattern
                .captures(html)
                .and_then(|c| c[1].parse::<u64>().ok())
                .unwrap_or(10)
                .min(120);
            return Ok(Step::Form(Form {
                url: target.to_string(),
                fields,
                post: form
                    .value()
                    .attr("method")
                    .unwrap_or("get")
                    .eq_ignore_ascii_case("post"),
                wait,
            }));
        }
    }
    // An author's project page may publish several archive parts. Only explicit
    // file/mirror links inside its content are followed, never navigation/ads.
    let mut links = Vec::new();
    let selector = Selector::parse("main a[href], article a[href], .download a[href], a[download], a[href$='.scs'], a[href$='.zip'], a[href$='.7z'], a[href$='.rar']").unwrap();
    for a in doc.select(&selector) {
        let Ok(url) = base.join(a.value().attr("href").unwrap()) else {
            continue;
        };
        if public_url(url.as_str()).is_err() || url == *base {
            continue;
        }
        let path = url.path().to_lowercase();
        let is_file = Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(allowed_filename);
        let mirror = matches!(
            url.host_str(),
            Some("sharemods.com" | "modsfire.com" | "wdfiles.ru")
        ) && !matches!(path.as_str(), "/" | "/login" | "/premium");
        if (is_file || mirror) && !links.contains(&url.to_string()) {
            links.push(url.to_string());
        }
    }
    ensure!(
        !links.is_empty() && links.len() <= 30,
        "Нет публичной ссылки файла. Продолжите во встроенном браузере или импортируйте архив"
    );
    Ok(Step::Links(links))
}

async fn body(response: Response, cancel: &CancellationToken) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = tokio::select! { _ = cancel.cancelled() => bail!("Загрузка остановлена"), c = stream.next() => c }
    {
        let chunk = chunk?;
        ensure!(
            bytes.len() + chunk.len() <= 8 * 1024 * 1024,
            "Страница загрузки слишком большая"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn save_response(
    response: Response,
    output: &Path,
    name: &str,
    state: &SharedTransfer,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    ensure!(allowed_filename(name), "Недопустимый файл");
    tokio::fs::create_dir_all(output).await?;
    let target = output.join(name);
    if target.exists() {
        let meta = archive::ordinary_metadata(&target)?;
        ensure!(
            meta.is_file() && meta.len() > 0,
            "Повреждён ранее загруженный файл: {name}"
        );
        return Ok(target);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let partial = output.join(format!("{name}.{}-{stamp}.part", std::process::id()));
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&partial)
        .await?;
    let result: Result<()> = async {
        let total = response.content_length();
        let mut stream = response.bytes_stream();
        let mut downloaded = 0u64;
        let started = Instant::now();
        download::set_state(state, |s| { s.status = Status::Downloading; s.name = name.into(); s.downloaded = 0; s.total = total; s.speed = 0; });
        while let Some(chunk) = tokio::select! { _ = cancel.cancelled() => bail!("Загрузка остановлена"), c = stream.next() => c } {
            let chunk = chunk?;
            if downloaded == 0 {
                let prefix = String::from_utf8_lossy(&chunk[..chunk.len().min(200)]).trim_start().to_ascii_lowercase();
                ensure!(!prefix.starts_with('<') && !prefix.starts_with("{\"error"), "Сервер вернул страницу вместо файла");
            }
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            download::set_state(state, |s| { s.downloaded = downloaded; s.speed = (downloaded as f64 / started.elapsed().as_secs_f64().max(0.01)) as u64; });
        }
        ensure!(downloaded > 0 && total.is_none_or(|t| t == downloaded), "Получен неполный файл");
        ensure!(!cancel.is_cancelled(), "Загрузка остановлена");
        file.sync_all().await?;
        Ok(())
    }.await;
    drop(file);
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(error);
    }
    if let Err(error) = tokio::fs::rename(&partial, &target).await {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(error.into());
    }
    Ok(target)
}

/// The direct download address of a Google Drive file page.
pub fn google_download(page: &Url) -> Result<Url> {
    let id = page
        .path_segments()
        .and_then(|s| {
            let p = s.collect::<Vec<_>>();
            (p.len() >= 3 && p[0] == "file" && p[1] == "d").then(|| p[2].to_owned())
        })
        .or_else(|| {
            page.query_pairs()
                .find(|(k, _)| k == "id")
                .map(|(_, v)| v.into_owned())
        })
        .context("Нет ID файла Google Drive")?;
    let mut url = Url::parse("https://drive.usercontent.google.com/download")?;
    url.query_pairs_mut()
        .append_pair("id", &id)
        .append_pair("export", "download");
    Ok(url)
}

pub async fn download_source(
    url: &str,
    output: &Path,
    state: &SharedTransfer,
    cancel: &CancellationToken,
) -> Result<Vec<PathBuf>> {
    let client = source::client()?;
    let mut first = public_url(url)?;
    if first.host_str() == Some("drive.google.com") {
        first = google_download(&first)?;
    }
    let mut queue = vec![(
        first.to_string(),
        None::<Form>,
        None::<String>,
        String::new(),
        None::<String>,
    )];
    let mut visited = BTreeSet::new();
    let mut files = Vec::new();
    let mut steps = 0;
    while let Some((url, form, referer, cookies, fallback)) = queue.pop() {
        ensure!(!cancel.is_cancelled(), "Загрузка остановлена");
        steps += 1;
        ensure!(steps <= 80, "Слишком много переходов на сайте файла");
        let key = format!(
            "{url}:{}",
            form.as_ref()
                .map(|f| format!("{:?}", f.fields))
                .unwrap_or_default()
        );
        if !visited.insert(key) {
            continue;
        }
        let mut request = client.get(public_url(&url)?);
        if let Some(form) = form {
            tokio::select! { _ = cancel.cancelled() => bail!("Загрузка остановлена"), _ = tokio::time::sleep(Duration::from_secs(form.wait)) => {} }
            request = if form.post {
                client.post(public_url(&form.url)?).form(&form.fields)
            } else {
                client.get(public_url(&form.url)?).query(&form.fields)
            };
        }
        if let Some(referer) = &referer {
            request = request.header("Referer", referer);
        }
        if !cookies.is_empty() {
            request = request.header("Cookie", &cookies);
        }
        let response = tokio::select! { _ = cancel.cancelled() => bail!("Загрузка остановлена"), r = request.send() => r?.error_for_status()? };
        let base = public_url(response.url().as_str())?;
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        let disposition = response
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let cookie_values = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|s| s.split(';').next())
            .collect::<Vec<_>>()
            .join("; ");
        let cookie_values = if cookie_values.is_empty() {
            cookies
        } else {
            cookie_values
        };
        if !content_type.contains("html")
            && !content_type.contains("json")
            && (disposition.is_some() || filename(None, &base, fallback.as_deref()).is_ok())
        {
            let name = filename(disposition.as_deref(), &base, fallback.as_deref())?;
            files.push(save_response(response, output, &name, state, cancel).await?);
            continue;
        }
        let html = String::from_utf8(body(response, cancel).await?)?;
        match page_step(&base, &html)? {
            Step::Links(links) => {
                for link in links.into_iter().rev() {
                    queue.push((link, None, Some(base.to_string()), String::new(), None));
                }
            }
            Step::Form(form) => queue.push((
                form.url.clone(),
                Some(form),
                Some(base.to_string()),
                cookie_values,
                None,
            )),
            Step::Trucky => {
                let file = trucky_file(&client, &base, &html, &cookie_values, cancel).await?;
                queue.push((
                    file.0,
                    None,
                    Some(base.to_string()),
                    String::new(),
                    Some(file.1),
                ));
            }
        }
    }
    ensure!(!files.is_empty(), "Сайт не вернул файлы");
    Ok(files)
}

async fn trucky_file(
    client: &Client,
    base: &Url,
    html: &str,
    cookies: &str,
    cancel: &CancellationToken,
) -> Result<(String, String)> {
    let file = trucky_link(client, base, html, cookies, cancel).await?;
    tokio::select! { _ = cancel.cancelled() => bail!("Загрузка остановлена"), _ = tokio::time::sleep(Duration::from_secs(10)) => {} }
    Ok(file)
}

/// The file address and name TruckyMods' own download button requests.
pub(super) async fn trucky_link(
    client: &Client,
    base: &Url,
    html: &str,
    cookies: &str,
    cancel: &CancellationToken,
) -> Result<(String, String)> {
    let (project, build, csrf) = crate::maps::access::trucky_request(html)?;
    let mut request = client
        .post(base.join("/projects/createDownloadUrl")?)
        .header("Referer", base.as_str())
        .header("Accept", "application/json")
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(
            &serde_json::json!({"project_id":project,"build_id":build}),
        )?);
    if !cookies.is_empty() {
        request = request.header("Cookie", cookies);
    }
    if let Some(csrf) = csrf {
        request = request.header("X-CSRF-TOKEN", csrf);
    }
    let response = tokio::select! { _ = cancel.cancelled() => bail!("Загрузка остановлена"), r = request.send() => r?.error_for_status()? };
    let payload: serde_json::Value = serde_json::from_slice(&body(response, cancel).await?)?;
    crate::maps::access::trucky_payload(&payload)?;
    let url = payload["url"]
        .as_str()
        .context("Нет ссылки файла TruckyMods")?
        .to_owned();
    public_url(&url)?;
    let name = payload["fileName"]
        .as_str()
        .context("Нет имени файла TruckyMods")?
        .to_owned();
    ensure!(allowed_filename(&name), "Некорректный файл TruckyMods");
    Ok((url, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_archive_parts_are_recognised() {
        assert_eq!(
            split_part("ProMods.zip.001"),
            Some(("promods.zip".into(), 1))
        );
        assert_eq!(
            split_part("promods.7z.012"),
            Some(("promods.7z".into(), 12))
        );
        assert_eq!(split_part("map.part2.rar"), Some(("map.rar".into(), 2)));
        assert_eq!(split_part("map.rar"), None);
        assert_eq!(split_part("promods-eu-map-v284.scs"), None);
        assert!(unfinished_download("promods.zip.001.crdownload"));
        assert!(unfinished_download("promods.zip.002.part"));
        assert!(!unfinished_download("map.part2.rar"));
        assert!(unfinished_mod("BXPrcRussiaPoland161.Xy9z.scs.part"));
        assert!(unfinished_mod("promods.zip.002.part"));
        assert!(unfinished_mod("Unconfirmed 812345.crdownload"));
        assert!(!unfinished_mod("video.mp4.part"));
        assert!(!unfinished_mod("map.part2.rar"));
    }

    #[test]
    fn numbered_browser_copies_give_the_original_name() {
        assert_eq!(
            browser_copy_base("Koromyslovo Add-on for Project Russia 1.5(1)"),
            Some("Koromyslovo Add-on for Project Russia 1.5")
        );
        assert_eq!(browser_copy_base("ulmap10 (12)"), Some("ulmap10"));
        assert_eq!(browser_copy_base("ulmap10"), None);
        assert_eq!(browser_copy_base("map (beta)"), None);
        assert_eq!(browser_copy_base("(1)"), None);
    }

    #[test]
    fn an_author_page_resolves_to_its_current_download() {
        let base =
            Url::parse("https://kieqw.github.io/kieqwmods.github.io/koromyslovo.html").unwrap();
        let html = r#"<nav><a href="index.html">News</a></nav>
            <a href="https://truckymods.io/euro-truck-simulator-2/maps/project-russia">Project Russia</a>
            <a href="https://modsfire.com/f69b7lEnUu3pT4y" class="btn btn-download">Download (1.76 MB)</a>
            <div class="version"><a href="https://modsfire.com/C683GWhsCv2qlR4" class="btn btn-version-download">Download</a></div>"#;
        assert_eq!(
            primary_download(&base, html).unwrap().as_str(),
            "https://modsfire.com/f69b7lEnUu3pT4y"
        );
        assert!(
            primary_download(&base, r#"<a href="https://example.com/x">Download</a>"#).is_none()
        );
        assert!(primary_download(&base, r#"<a href="https://boosty.to/a">Скачать</a>"#).is_none());
        assert!(is_file_host(
            &Url::parse("https://www.modsfire.com/a").unwrap()
        ));
        assert!(!is_file_host(&base));
    }
    use std::sync::{Arc, Mutex};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn response(bytes: &[u8], length: usize) -> Response {
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let bytes = bytes.to_vec();
        tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            stream.write_all(&bytes).await.unwrap();
        });
        Client::new()
            .get(format!("http://{address}/file.scs"))
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn http_stream_commits_exact_bytes_but_discards_html_truncation_and_cancellation() {
        let bytes = b"SCS#test package";
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(Mutex::new(download::Transfer::default()));
        let cancel = CancellationToken::new();
        let path = save_response(
            response(bytes, bytes.len()).await,
            root.path(),
            "good.scs",
            &state,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        let bad = b"<!DOCTYPE html><html>Login required</html>";
        assert!(
            save_response(
                response(bad, bad.len()).await,
                root.path(),
                "html.scs",
                &state,
                &cancel
            )
            .await
            .is_err()
        );
        assert!(
            save_response(
                response(bytes, bytes.len() + 100).await,
                root.path(),
                "short.scs",
                &state,
                &cancel
            )
            .await
            .is_err()
        );
        cancel.cancel();
        assert!(
            save_response(
                response(bytes, bytes.len()).await,
                root.path(),
                "cancelled.scs",
                &state,
                &cancel
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
