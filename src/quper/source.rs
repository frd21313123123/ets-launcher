use super::{Catalog, Combo, FileRow, Plan, SITE, valid_mod_id};
use crate::game::Game;
use anyhow::{Context, Result, ensure};
use regex::Regex;
use reqwest::{Client, Url};
use scraper::{Html, Selector};
use std::{collections::BTreeSet, time::Duration};
use tokio_util::sync::CancellationToken;

pub fn is_boosty(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|h| h == "boosty.to" || h.ends_with(".boosty.to"))
}

pub fn public_url(value: &str) -> Result<Url> {
    let url = crate::maps::https(value)?;
    ensure!(!is_boosty(&url), "Boosty исключён из загрузок");
    Ok(url)
}

pub fn site_url(value: &str) -> Result<Url> {
    let url = public_url(value)?;
    ensure!(
        matches!(
            url.host_str(),
            Some("qupersimulator.ru" | "www.qupersimulator.ru")
        ),
        "Неизвестный адрес сайта Quper"
    );
    Ok(url)
}

/// Preserve the sheet's gid: different combos share one workbook.
pub fn sheet_url(value: &str) -> Result<Url> {
    let url = public_url(value)?;
    ensure!(
        url.host_str() == Some("docs.google.com"),
        "Неизвестный формат списка файлов Quper"
    );
    let parts: Vec<_> = url.path_segments().context("Нет адреса таблицы")?.collect();
    ensure!(
        parts.len() >= 3 && parts[0] == "spreadsheets" && parts[1] == "d",
        "Не найдена таблица файлов"
    );
    ensure!(
        !parts[2].is_empty()
            && parts[2]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "Некорректный ID таблицы"
    );
    let gid = url
        .query_pairs()
        .find(|(k, _)| k == "gid")
        .map(|(_, v)| v.into_owned())
        .or_else(|| {
            url.fragment().and_then(|f| {
                let mut fragment = url.clone();
                fragment.set_query(Some(f));
                fragment
                    .query_pairs()
                    .find(|(k, _)| k == "gid")
                    .map(|(_, v)| v.into_owned())
            })
        });
    if let Some(gid) = &gid {
        ensure!(
            !gid.is_empty() && gid.bytes().all(|b| b.is_ascii_digit()),
            "Некорректный лист таблицы"
        );
    }
    let mut export = Url::parse(&format!(
        "https://docs.google.com/spreadsheets/d/{}/gviz/tq",
        parts[2]
    ))?;
    export.query_pairs_mut().append_pair("tqx", "out:html");
    if let Some(gid) = &gid {
        export.query_pairs_mut().append_pair("gid", gid);
    }
    Ok(export)
}

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent("Mozilla/5.0 ETS-Launcher/0.3 (Quper combos)")
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 8 || public_url(attempt.url().as_str()).is_err() {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}

pub async fn get(
    client: &Client,
    url: &str,
    cancel: &CancellationToken,
) -> Result<(String, Vec<u8>)> {
    public_url(url)?;
    // The shared reader bounds metadata to 8 MiB and supports cancellation.
    let (url, bytes) = crate::maps::source::get(client, url, cancel).await?;
    public_url(&url)?;
    Ok((url, bytes))
}

pub fn script_urls(html: &str) -> Result<Vec<String>> {
    let doc = Html::parse_document(html);
    let base = Url::parse(SITE)?;
    let mut urls = Vec::new();
    for script in doc.select(&Selector::parse("script[src]").unwrap()) {
        let url = base.join(script.value().attr("src").unwrap())?;
        site_url(url.as_str())?;
        if url.path().ends_with(".js") {
            urls.push(url.to_string());
        }
    }
    ensure!(
        !urls.is_empty() && urls.len() <= 20,
        "Сайт Quper изменил формат каталога"
    );
    Ok(urls)
}

fn field(object: &str, name: &str) -> Result<Option<String>> {
    let pattern = format!(
        r#"(?:^|[{{,])\s*"?{}"?\s*:\s*("(?:\\.|[^"\\])*")"#,
        regex::escape(name)
    );
    Regex::new(&pattern)?
        .captures(object)
        .map(|c| serde_json::from_str(&c[1]).context("Не удалось прочитать поле Quper"))
        .transpose()
}

// Read just data objects, never evaluate the site's JavaScript.
fn object_end(bytes: &[u8], start: usize) -> Option<usize> {
    let (mut depth, mut quote, mut escape) = (0usize, None, false);
    for (offset, &b) in bytes[start..].iter().enumerate() {
        if let Some(q) = quote {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == q {
                quote = None;
            }
        } else if matches!(b, b'"' | b'\'' | b'`') {
            quote = Some(b);
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(start + offset + 1);
            }
        }
    }
    None
}

pub fn parse_catalog(script: &str) -> Result<Vec<Combo>> {
    let start = Regex::new(r#"\{\s*"?id"?\s*:\s*"[A-Za-z0-9_-]+""#)?;
    let tags_pattern = Regex::new(r#"(?:^|[,\{])\s*"?tags"?\s*:\s*(\[[^\]]*\])"#)?;
    let automods = Regex::new(r#"(?:^|[,\{])\s*"?automods"?\s*:\s*(?:!0|true)"#)?;
    let base = Url::parse(SITE)?;
    let mut combos = Vec::new();
    for found in start.find_iter(script) {
        let end = object_end(script.as_bytes(), found.start())
            .context("Повреждены данные каталога Quper")?;
        let object = &script[found.start()..end];
        let Some(game_version) = field(object, "version_game")? else {
            continue;
        };
        let tags: Vec<String> = tags_pattern
            .captures(object)
            .map(|c| serde_json::from_str(&c[1]))
            .transpose()?
            .unwrap_or_default();
        if tags.iter().any(|t| t == "boosty") {
            continue;
        }
        let Some(table) = field(object, "table")?.filter(|s| !s.is_empty()) else {
            continue;
        };
        if Url::parse(&table).is_ok_and(|u| is_boosty(&u)) {
            continue;
        }
        let game = if tags.iter().any(|t| t == "ets2") {
            Game::Ets2
        } else if tags.iter().any(|t| t == "ats") {
            Game::Ats
        } else {
            continue;
        };
        let id = field(object, "id")?.context("Нет ID сборки")?;
        let combo = Combo {
            page: base.join(&id)?.to_string(),
            id,
            title: field(object, "title")?.context("Нет названия сборки")?,
            preview: base
                .join(
                    &field(object, "preview")?
                        .or(field(object, "image")?)
                        .context("Нет обложки сборки")?,
                )?
                .to_string(),
            version: field(object, "version")?.context("Нет версии сборки")?,
            date: field(object, "date")?.unwrap_or_default(),
            game_version,
            game,
            table,
            automatic_order: automods.is_match(object),
            convoy: tags.iter().any(|t| t == "convoy"),
        };
        combo.validate()?;
        combos.push(combo);
    }
    Ok(combos)
}

pub async fn fetch_catalog(cancel: &CancellationToken) -> Result<Catalog> {
    let client = client()?;
    let (_, html) = get(&client, SITE, cancel).await?;
    let mut combos = Vec::new();
    for url in script_urls(&String::from_utf8(html)?)? {
        let (_, script) = get(&client, &url, cancel).await?;
        combos.extend(parse_catalog(&String::from_utf8(script)?)?);
    }
    let catalog = Catalog {
        fetched_at: crate::maps::source::now(),
        combos,
    };
    catalog.validate()?;
    Ok(catalog)
}

fn text(cell: scraper::ElementRef<'_>) -> String {
    cell.text()
        .collect::<String>()
        .trim()
        .replace('\u{a0}', " ")
        .trim()
        .to_owned()
}

fn links(cell: scraper::ElementRef<'_>) -> Result<(Vec<String>, bool)> {
    let pattern = Regex::new(r#"https?://[^\s<>"']+"#)?;
    let mut links = Vec::new();
    let mut boosty = false;
    let mut candidates = cell
        .select(&Selector::parse("a[href]").unwrap())
        .filter_map(|a| a.value().attr("href").map(str::to_owned))
        .collect::<Vec<_>>();
    candidates.extend(
        pattern
            .find_iter(&text(cell))
            .map(|m| m.as_str().trim_end_matches([',', ';', ')']).to_owned()),
    );
    for value in candidates {
        let Ok(mut url) = Url::parse(&value) else {
            continue;
        };
        if url.host_str() == Some("www.google.com")
            && url.path() == "/url"
            && let Some(value) = url
                .query_pairs()
                .find(|(k, _)| k == "q" || k == "url")
                .map(|(_, v)| v.into_owned())
        {
            url = Url::parse(&value)?;
        }
        if is_boosty(&url) {
            boosty = true;
            continue;
        }
        // Some old author tables contain plain HTTP links to HTTPS-capable hosts.
        if url.scheme() == "http" {
            let _ = url.set_scheme("https");
        }
        if public_url(url.as_str()).is_ok() && !links.contains(&url.to_string()) {
            links.push(url.to_string());
        }
    }
    Ok((links, boosty))
}

pub fn parse_table(html: &str) -> Result<Vec<FileRow>> {
    let doc = Html::parse_document(html);
    let rows = Selector::parse("tr").unwrap();
    let cells = Selector::parse("td").unwrap();
    let mut result = Vec::new();
    let mut inherited = (Vec::new(), false);
    for row in doc.select(&rows) {
        let cells = row.select(&cells).collect::<Vec<_>>();
        if cells.len() < 4 {
            continue;
        }
        // Exclude headings, change logs, profiles, ads and unrelated footer links.
        let Ok(number) = text(cells[0]).parse::<u32>() else {
            continue;
        };
        let name = text(cells[1]);
        if name.is_empty() {
            continue;
        }
        let mod_id = cells
            .get(5)
            .map(|c| text(*c))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| name.clone());
        valid_mod_id(&mod_id)?;
        let raw = text(cells[3]);
        let (urls, excluded_boosty) = if raw.trim().is_empty()
            && cells[3]
                .select(&Selector::parse("a[href]").unwrap())
                .next()
                .is_none()
        {
            inherited.clone() // Merged cells: several packages inside the same archive.
        } else {
            let current = links(cells[3])?;
            inherited = current.clone();
            current
        };
        result.push(FileRow {
            number,
            name,
            description: text(cells[2]),
            mod_id,
            urls,
            excluded_boosty,
            required: true,
        });
    }
    ensure!(
        !result.is_empty() && result.len() <= 2000,
        "Не удалось прочитать строки таблицы Quper"
    );
    Ok(result)
}

pub fn parse_order(text: &str) -> Result<Vec<String>> {
    let count = Regex::new(r"(?m)^\s*active_mods:\s*(\d+)\s*$")?;
    let expected: usize = count.captures(text).context("Нет списка модов Quper")?[1].parse()?;
    ensure!(
        expected > 0 && expected <= 2000,
        "Некорректное количество модов"
    );
    let pattern = Regex::new(r#"(?m)^\s*active_mods\[(\d+)\]:\s*"([^"|]+)\|[^"\r\n]*"\s*$"#)?;
    let mut order = Vec::new();
    for row in pattern.captures_iter(text) {
        ensure!(
            row[1].parse::<usize>()? == order.len(),
            "Нарушен порядок модов Quper"
        );
        valid_mod_id(&row[2])?;
        order.push(row[2].to_owned());
    }
    ensure!(
        order.len() == expected,
        "Получен неполный порядок модов Quper"
    );
    Ok(order)
}

/// A workbook URL can open a navigation sheet rather than the combo itself.
/// Resolve the author's named tab instead of assuming gid=0 or the first tab.
pub fn named_sheet(html: &str, combo: &Combo) -> Result<String> {
    fn title(value: &str) -> String {
        value
            .to_lowercase()
            .split_whitespace()
            .filter(|s| !matches!(*s, "ets2" | "ats" | "for" | "beta" | "final"))
            .collect::<String>()
    }
    let target = title(&combo.title);
    let pattern =
        Regex::new(r#"items\.push\(\{\s*name:\s*("(?:\\.|[^"\\])*")\s*,[^}]*?\bgid:\s*"(\d+)""#)?;
    let mut matches = Vec::new();
    for item in pattern.captures_iter(html) {
        let name: String = serde_json::from_str(&item[1])?;
        if !name.to_lowercase().contains("boosty") && title(&name) == target {
            matches.push(item[2].to_owned());
        }
    }
    ensure!(
        matches.len() == 1,
        "В книге Quper не найден однозначный лист «{}»",
        combo.title
    );
    Ok(matches.remove(0))
}

async fn resolve_sheet(client: &Client, combo: &Combo, cancel: &CancellationToken) -> Result<Url> {
    let mut export = sheet_url(&combo.table)?;
    if !export.query_pairs().any(|(k, _)| k == "gid") {
        let mut index = export.clone();
        index.set_path(&format!(
            "{}/htmlview",
            export.path().trim_end_matches("/gviz/tq")
        ));
        index.set_query(None);
        let (_, html) = get(client, index.as_str(), cancel).await?;
        let gid = named_sheet(&String::from_utf8(html)?, combo)?;
        export.query_pairs_mut().append_pair("gid", &gid);
    }
    Ok(export)
}

pub async fn fetch_plan(combo: &Combo, cancel: &CancellationToken) -> Result<Plan> {
    combo.validate()?;
    let client = client()?;
    let (_, html) = get(
        &client,
        resolve_sheet(&client, combo, cancel).await?.as_str(),
        cancel,
    )
    .await?;
    let mut files = parse_table(&String::from_utf8(html)?)?;
    let order = if combo.automatic_order {
        let url = format!("{SITE}profiles/{}/{}.txt", combo.game_version, combo.id);
        let (_, bytes) = get(&client, &url, cancel)
            .await
            .context("Не удалось получить авторский порядок модов")?;
        let order = parse_order(&String::from_utf8(bytes)?)?;
        let required: BTreeSet<_> = order.iter().map(|id| id.to_lowercase()).collect();
        for file in &mut files {
            file.required = required.contains(&file.mod_id.to_lowercase());
        }
        order
    } else {
        let mut ids = BTreeSet::new();
        files
            .iter()
            .rev()
            .map(|f| f.mod_id.clone())
            .filter(|s| ids.insert(s.to_lowercase()))
            .collect()
    };
    let plan = Plan {
        combo: combo.clone(),
        files,
        order,
    };
    plan.validate()?;
    Ok(plan)
}
