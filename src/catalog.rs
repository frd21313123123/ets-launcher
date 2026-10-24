use crate::{game::Game, playground::Catalog, settings::Settings};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub const RETRY_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default, Serialize, Deserialize)]
pub struct CatalogCache {
    ets2: Option<Catalog>,
    ats: Option<Catalog>,
}

impl CatalogCache {
    fn path() -> Option<PathBuf> {
        Some(Settings::path()?.with_file_name("catalog.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .map(|path| Self::load_from(&path))
            .unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Self {
        if std::fs::metadata(path)
            .map(|m| m.len() > 1024 * 1024)
            .unwrap_or(true)
        {
            return Self::default();
        }
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn get(&self, game: Game, source: &str) -> Option<Catalog> {
        let value = match game {
            Game::Ets2 => &self.ets2,
            Game::Ats => &self.ats,
        };
        value
            .as_ref()
            .filter(|catalog| catalog.source_url == source && catalog.validate_for(game).is_ok())
            .cloned()
    }

    pub fn set(&mut self, game: Game, value: Catalog) {
        match game {
            Game::Ets2 => self.ets2 = Some(value),
            Game::Ats => self.ats = Some(value),
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path().context("Не найдена папка каталога")?)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path.parent().context("У каталога нет папки")?)?;
        let temporary = path.with_extension(format!("{}.part", std::process::id()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temporary, path)?;
        Ok(())
    }
}

pub struct CatalogState {
    pub catalog: Option<Catalog>,
    pub error: Option<String>,
    pub refreshing: bool,
    pub next_refresh: Instant,
    pub generation: u64,
    pub cancel: CancellationToken,
}

impl CatalogState {
    pub fn new(catalog: Option<Catalog>) -> Self {
        Self {
            catalog,
            error: None,
            refreshing: false,
            next_refresh: Instant::now(),
            generation: 0,
            cancel: CancellationToken::new(),
        }
    }
}
