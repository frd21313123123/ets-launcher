//! Reproducible author-source checks; reports never contain signed file URLs,
//! CSRF tokens or site session cookies.
use super::{
    Catalog, SourceState,
    library::{self, Library},
    source,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Serialize)]
pub struct DownloadCheck {
    pub version: Option<String>,
    pub status: String,
    pub message: String,
    pub page: String,
    pub files: Vec<String>,
    pub size: Option<u64>,
    pub access: Option<super::access::DownloadAccess>,
}
#[derive(Serialize)]
pub struct CombinationCheck {
    pub maps: Vec<String>,
    pub allowed: bool,
    pub packages: Vec<String>,
    pub message: String,
}
#[derive(Serialize)]
pub struct Report {
    pub checked_at: u64,
    pub downloads: BTreeMap<String, DownloadCheck>,
    pub combinations: Vec<CombinationCheck>,
    pub all_builds: Vec<CombinationCheck>,
    pub all_builds_error: Option<String>,
}

pub async fn inspect(catalog: &Catalog, cancel: &CancellationToken) -> Report {
    let mut downloads = BTreeMap::new();
    let mut states = BTreeMap::new();
    for entry in &catalog.maps {
        let state = source::fetch(entry, cancel).await;
        let (version, size) = state
            .as_ref()
            .map(|s| (s.version.clone(), s.download_size))
            .unwrap_or_default();
        if let Ok(state) = state {
            states.insert(entry.id.clone(), state);
        }
        let live = catalog.with_sources(&states);
        let result = source::prepare(live.entry(&entry.id).unwrap(), cancel).await;
        let check = match result {
            Ok(prepared) => DownloadCheck {
                version,
                status: if prepared.entry.release.downloads.is_empty() {
                    "browser_required"
                } else {
                    "ready"
                }
                .into(),
                message: prepared.message,
                page: prepared.page,
                files: prepared
                    .entry
                    .release
                    .downloads
                    .iter()
                    .map(|f| f.filename.clone())
                    .collect(),
                size,
                access: Some(prepared.access),
            },
            Err(error) => DownloadCheck {
                version,
                status: "error".into(),
                message: format!("{error:#}"),
                page: entry.source.url.clone(),
                files: vec![],
                size,
                access: None,
            },
        };
        downloads.insert(entry.id.clone(), check);
        if cancel.is_cancelled() {
            break;
        }
    }
    let builds = all_builds(&catalog.with_sources(&states));
    let all_builds_error = builds.as_ref().err().map(ToString::to_string);
    Report {
        checked_at: source::now(),
        downloads,
        combinations: combinations(&catalog.with_sources(&states)),
        all_builds: builds.unwrap_or_default(),
        all_builds_error,
    }
}

pub fn all_builds(catalog: &Catalog) -> Result<Vec<CombinationCheck>> {
    catalog.validate()?;
    let maps = catalog
        .maps
        .iter()
        .filter(|m| !m.support)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        maps.len() <= 12,
        "Для полного перебора поддерживается до 12 карт"
    );
    let dlcs = catalog
        .maps
        .iter()
        .flat_map(|m| m.release.dlcs.clone())
        .collect();
    let mut result = Vec::new();
    for mask in 1..(1u64 << maps.len()) {
        let selected: BTreeSet<_> = maps
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, m)| m.id.clone())
            .collect();
        let set = super::ModSet {
            id: "audit".into(),
            name: "Audit".into(),
            game_version: catalog.maps[0].release.game_versions[0].clone(),
            selected: selected.clone(),
        };
        let plan = catalog.resolve_validated(&set, &dlcs);
        result.push(match plan {
            Ok(plan) => CombinationCheck {
                maps: selected.into_iter().collect(),
                allowed: true,
                packages: plan.ordered,
                message: "План разрешён; дорожный прогон в игре не подразумевается".into(),
            },
            Err(error) => CombinationCheck {
                maps: selected.into_iter().collect(),
                allowed: false,
                packages: vec![],
                message: error.to_string(),
            },
        });
    }
    Ok(result)
}

pub fn combinations(catalog: &Catalog) -> Vec<CombinationCheck> {
    let maps = catalog
        .maps
        .iter()
        .filter(|m| !m.support)
        .collect::<Vec<_>>();
    let dlcs = catalog
        .maps
        .iter()
        .flat_map(|m| m.release.dlcs.clone())
        .collect();
    let mut combinations = Vec::new();
    for (index, a) in maps.iter().enumerate() {
        for b in &maps[index + 1..] {
            let set = super::ModSet {
                id: "audit".into(),
                name: "Source audit".into(),
                game_version: a.release.game_versions[0].clone(),
                selected: BTreeSet::from([a.id.clone(), b.id.clone()]),
            };
            let result = catalog.resolve(&set, &dlcs);
            combinations.push(match result {
                Ok(plan) => CombinationCheck {
                    maps: vec![a.id.clone(), b.id.clone()],
                    allowed: true,
                    packages: plan.ordered,
                    message:
                        "План разрешён правилами каталога; это не результат дорожного теста в игре"
                            .into(),
                },
                Err(error) => CombinationCheck {
                    maps: vec![a.id.clone(), b.id.clone()],
                    allowed: false,
                    packages: vec![],
                    message: format!("{error:#}"),
                },
            });
        }
    }
    combinations
}

pub async fn download(
    catalog: &Catalog,
    id: &str,
    root: &Path,
    cancel: &CancellationToken,
) -> Result<serde_json::Value> {
    anyhow::ensure!(
        !root.exists(),
        "Для проверки нужна новая пустая папка библиотеки"
    );
    let entry = catalog.entry(id)?;
    let state = source::fetch(entry, cancel).await?;
    let live = catalog.with_sources(&BTreeMap::<String, SourceState>::from([(id.into(), state)]));
    let entry = live.entry(id)?;
    let prepared = source::prepare(entry, cancel).await?;
    anyhow::ensure!(
        !prepared.entry.release.downloads.is_empty(),
        "Нужны действия на сайте: {} ({})",
        prepared.page,
        prepared.message
    );
    let library = Library::open(root)?;
    let transfer = Arc::new(Mutex::new(crate::download::Transfer::default()));
    let record = library::download_packages(&library, &prepared.entry, &transfer, cancel)
        .await
        .context("Проверка реальной загрузки/архива")?;
    Ok(
        serde_json::json!({"id":id,"version":record.version,"status":"downloaded_and_imported","files":record.files,"checked_at":source::now()}),
    )
}

pub fn apply_test(
    catalog: &Catalog,
    id: &str,
    root: &Path,
    game: &Path,
) -> Result<serde_json::Value> {
    let library = Library::open(root)?;
    let set = super::ModSet {
        id: format!("download-qa-{}", source::now()),
        name: "Launcher Maps QA".into(),
        game_version: library::game_version(game).context("Не определена версия ETS2")?,
        selected: BTreeSet::from([id.into()]),
    };
    let plan = catalog.resolve(&set, &library::installed_dlcs(game))?;
    anyhow::ensure!(
        !library.environment(&set.id)?.exists(),
        "Тестовое окружение уже существует"
    );
    let active = library.apply(catalog, &set, &plan, &CancellationToken::new())?;
    Ok(
        serde_json::json!({"status":"applied","homedir":library.environment(&set.id)?.join("home"),"profile":active.profile_name,"packages":active.packages}),
    )
}
