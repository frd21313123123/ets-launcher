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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            game_dir: None,
            ats_dir: None,
            selected_game: Game::Ets2,
            ats_channel: Channel::Stable,
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
            Game::Ets2 => Channel::Stable,
            Game::Ats => self.ats_channel,
        }
    }

    pub fn download_directory(&self, game: Game) -> PathBuf {
        match game {
            Game::Ets2 => self.output.clone(),
            Game::Ats => self.output.join("ATS"),
        }
    }

    fn path() -> Option<PathBuf> {
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
