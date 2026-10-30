use crate::game::{Channel, Game};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
pub struct Settings {
    pub output: PathBuf,
    #[serde(default)]
    pub game_dir: Option<PathBuf>,
    #[serde(default)]
    pub ats_dir: Option<PathBuf>,
    #[serde(default)]
    pub selected_game: Game,
    #[serde(default)]
    pub ats_channel: Channel,
    #[serde(default)]
    pub ets2_channel: Channel,
    #[serde(default)]
    pub ets2_source: Option<String>,
    #[serde(default)]
    pub ats_source: Option<String>,
    #[serde(default)]
    pub mod_library: Option<PathBuf>,
    #[serde(default)]
    pub mod_catalog_source: Option<String>,
    #[serde(default)]
    pub quper_library: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            game_dir: None,
            ats_dir: None,
            selected_game: Game::Ets2,
            ats_channel: Channel::Stable,
            ets2_channel: Channel::Stable,
            ets2_source: None,
            ats_source: None,
            mod_library: None,
            mod_catalog_source: None,
            quper_library: None,
            output: UserDirs::new()
                .and_then(|dirs| dirs.download_dir().map(|dir| dir.join("ETS Launcher")))
                .unwrap_or_else(|| PathBuf::from("downloads")),
        }
    }
}

impl Settings {
    pub fn directory(&self, game: Game) -> Option<&std::path::Path> {
        match game {
            Game::Ets2 => self.game_dir.as_deref(),
            Game::Ats => self.ats_dir.as_deref(),
        }
    }

    pub fn set_directory(&mut self, game: Game, path: Option<PathBuf>) {
        match game {
            Game::Ets2 => self.game_dir = path,
            Game::Ats => self.ats_dir = path,
        }
    }

    pub fn channel(&self) -> Channel {
        match self.selected_game {
            Game::Ets2 => self.ets2_channel,
            Game::Ats => self.ats_channel,
        }
    }

    pub fn set_channel(&mut self, game: Game, channel: Channel) {
        match game {
            Game::Ets2 => self.ets2_channel = channel,
            Game::Ats => self.ats_channel = channel,
        }
    }

    pub fn source(&self, game: Game) -> &str {
        let source = match game {
            Game::Ets2 => &self.ets2_source,
            Game::Ats => &self.ats_source,
        };
        source
            .as_deref()
            .filter(|url| Self::validate_source(game, url).is_ok())
            .unwrap_or(game.source_page())
    }

    pub fn validate_source(game: Game, value: &str) -> anyhow::Result<()> {
        let url = reqwest::Url::parse(value)?;
        let category = match game {
            Game::Ets2 => "/euro_truck_simulator_2/cheat/",
            Game::Ats => "/american_truck_simulator/cheat/",
        };
        anyhow::ensure!(
            url.scheme() == "https"
                && matches!(url.host_str(), Some("www.playground.ru" | "playground.ru"))
                && url.port().is_none()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path().starts_with(category),
            "Укажите HTTPS-ссылку PlayGround на публикацию для {}",
            game.short_name()
        );
        Ok(())
    }

    pub fn set_source(&mut self, game: Game, value: &str) -> anyhow::Result<()> {
        let value = value.trim();
        let source = if value.is_empty() {
            None
        } else {
            Self::validate_source(game, value)?;
            Some(value.to_owned())
        };
        match game {
            Game::Ets2 => self.ets2_source = source,
            Game::Ats => self.ats_source = source,
        }
        Ok(())
    }

    pub fn download_directory(&self, game: Game) -> PathBuf {
        match game {
            Game::Ets2 => self.output.clone(),
            Game::Ats => self.output.join("ATS"),
        }
    }

    pub(crate) fn path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("ETS_LAUNCHER_CONFIG_DIR").map(PathBuf::from) {
            return path.is_absolute().then(|| path.join("settings.json"));
        }
        ProjectDirs::from("", "ETS Launcher", "ETS Launcher")
            .map(|dirs| dirs.config_dir().join("settings.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().ok_or_else(|| anyhow::anyhow!("Не найдена папка настроек"))?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}
