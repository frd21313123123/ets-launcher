use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Game {
    #[default]
    Ets2,
    Ats,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
}

impl std::str::FromStr for Game {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "ets2" => Ok(Self::Ets2),
            "ats" => Ok(Self::Ats),
            _ => anyhow::bail!("Укажите игру: ets2 или ats"),
        }
    }
}

impl std::str::FromStr for Channel {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "stable" => Ok(Self::Stable),
            "beta" => Ok(Self::Beta),
            _ => anyhow::bail!("Укажите ветку: stable или beta"),
        }
    }
}

impl Game {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ets2 => "Euro Truck Simulator 2",
            Self::Ats => "American Truck Simulator",
        }
    }
    pub fn short_name(self) -> &'static str {
        match self {
            Self::Ets2 => "ETS2",
            Self::Ats => "ATS",
        }
    }
    pub fn steam_app_id(self) -> u32 {
        match self {
            Self::Ets2 => 227_300,
            Self::Ats => 270_880,
        }
    }
    pub fn executable(self) -> &'static str {
        match self {
            Self::Ets2 => "eurotrucks2.exe",
            Self::Ats => "amtrucks.exe",
        }
    }
    pub fn source_page(self) -> &'static str {
        match self {
            Self::Ets2 => crate::SOURCE_PAGE,
            Self::Ats => crate::ATS_SOURCE_PAGE,
        }
    }
    pub fn validate_archive(self, channel: Channel, name: &str) -> anyhow::Result<()> {
        let name = name.to_ascii_lowercase();
        let prefix = match self {
            Self::Ets2 => "ets2_dlc_",
            Self::Ats => "ats_dlc_",
        };
        anyhow::ensure!(
            self == Self::Ats || channel == Channel::Stable,
            "Beta-ветка доступна только для ATS"
        );
        anyhow::ensure!(
            name.starts_with(prefix) && name.ends_with(".rar"),
            "Сайт вернул архив для другой игры: {name}"
        );
        anyhow::ensure!(
            name.contains("beta") == (channel == Channel::Beta),
            "Архив не соответствует выбранной ветке игры"
        );
        Ok(())
    }
}

/// A Documents/Euro Truck Simulator 2 profile directory is not a game installation.
pub fn is_installation(game: Game, path: &Path) -> bool {
    ["bin/win_x64", "bin/win_x86"]
        .iter()
        .any(|directory| path.join(directory).join(game.executable()).is_file())
}

pub fn detect(game: Game, saved: Option<&Path>) -> Option<PathBuf> {
    if let Some(saved) = saved.filter(|path| is_installation(game, path)) {
        return Some(saved.to_path_buf());
    }
    // Manifests and libraryfolders.vdf take priority over stale uninstall records.
    let (steam_roots, installed_games) = platform_paths(game);
    detect_from_paths(game, &steam_roots, &installed_games).map(|path| path.components().collect())
}

pub fn detect_from_paths(
    game: Game,
    steam_roots: &[PathBuf],
    game_paths: &[PathBuf],
) -> Option<PathBuf> {
    for root in steam_roots {
        if let Some(path) = find_in_steam(game, root) {
            return Some(path);
        }
        let standard = root.join("steamapps/common").join(game.name());
        if is_installation(game, &standard) {
            return Some(standard);
        }
    }
    game_paths
        .iter()
        .find(|path| is_installation(game, path))
        .cloned()
}

fn find_in_steam(game: Game, root: &Path) -> Option<PathBuf> {
    let mut libraries = vec![root.to_path_buf()];
    for file in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
        if let Some(vdf) = read_vdf(&root.join(file))
            && let Some(folders) = vdf.object("libraryfolders")
        {
            for (key, value) in &folders.0 {
                if key.parse::<u32>().is_err() {
                    continue;
                }
                let path = match value {
                    Value::Text(path) => Some(path.as_str()),
                    Value::Object(entry) => entry.text("path"),
                };
                if let Some(path) = path {
                    libraries.push(path.into());
                }
            }
        }
    }
    let app_id = game.steam_app_id().to_string();
    for library in libraries {
        if let Some(vdf) = read_vdf(&library.join(format!("steamapps/appmanifest_{app_id}.acf")))
            && let Some(app) = vdf.object("AppState")
            && app.text("appid") == Some(app_id.as_str())
            && let Some(directory) = app.text("installdir")
        {
            let relative = Path::new(directory);
            if relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            {
                let path = library.join("steamapps/common").join(relative);
                if is_installation(game, &path) {
                    return Some(path);
                }
            }
        }
        let standard = library.join("steamapps/common").join(game.name());
        if is_installation(game, &standard) {
            return Some(standard);
        }
    }
    None
}

#[derive(Debug)]
enum Value {
    Text(String),
    Object(Vdf),
}

#[derive(Debug)]
struct Vdf(Vec<(String, Value)>);

impl Vdf {
    fn object(&self, key: &str) -> Option<&Self> {
        self.0.iter().find_map(|(name, value)| match value {
            Value::Object(object) if name.eq_ignore_ascii_case(key) => Some(object),
            _ => None,
        })
    }
    fn text(&self, key: &str) -> Option<&str> {
        self.0.iter().find_map(|(name, value)| match value {
            Value::Text(text) if name.eq_ignore_ascii_case(key) => Some(text.as_str()),
            _ => None,
        })
    }
}

fn read_vdf(path: &Path) -> Option<Vdf> {
    if std::fs::metadata(path).ok()?.len() > 2 * 1024 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let tokens = tokenize(&text)?;
    let mut position = 0;
    parse_entries(&tokens, &mut position, 0)
}

#[derive(Debug)]
enum Token {
    Text(String),
    Open,
    Close,
}

fn tokenize(text: &str) -> Option<Vec<Token>> {
    let mut input = text.trim_start_matches('\u{feff}').chars().peekable();
    let mut tokens = Vec::new();
    while let Some(ch) = input.next() {
        match ch {
            ch if ch.is_whitespace() => {}
            '/' if input.peek() == Some(&'/') => {
                for ch in input.by_ref() {
                    if ch == '\n' {
                        break;
                    }
                }
            }
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
            '"' => {
                let mut text = String::new();
                loop {
                    match input.next()? {
                        '"' => break,
                        '\\' => {
                            let next = input.next()?;
                            if next != '\\' && next != '"' {
                                text.push('\\');
                            }
                            text.push(next);
                        }
                        ch => text.push(ch),
                    }
                }
                tokens.push(Token::Text(text));
            }
            ch => {
                let mut text = String::from(ch);
                while input
                    .peek()
                    .is_some_and(|ch| !ch.is_whitespace() && *ch != '{' && *ch != '}')
                {
                    text.push(input.next()?);
                }
                tokens.push(Token::Text(text));
            }
        }
    }
    Some(tokens)
}

fn parse_entries(tokens: &[Token], position: &mut usize, depth: usize) -> Option<Vdf> {
    if depth > 32 {
        return None;
    }
    let mut entries = Vec::new();
    while let Some(token) = tokens.get(*position) {
        if matches!(token, Token::Close) {
            if depth == 0 {
                return None;
            }
            *position += 1;
            return Some(Vdf(entries));
        }
        let Token::Text(key) = token else {
            return None;
        };
        *position += 1;
        let value = match tokens.get(*position)? {
            Token::Text(value) => {
                *position += 1;
                Value::Text(value.clone())
            }
            Token::Open => {
                *position += 1;
                Value::Object(parse_entries(tokens, position, depth + 1)?)
            }
            Token::Close => return None,
        };
        entries.push((key.clone(), value));
    }
    (depth == 0).then_some(Vdf(entries))
}

fn platform_paths(game: Game) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut steam = Vec::new();
    let mut games = Vec::new();
    #[cfg(windows)]
    {
        use windows_registry::{CURRENT_USER, LOCAL_MACHINE};
        for (hive, key, value) in [
            (CURRENT_USER, "Software\\Valve\\Steam", "SteamPath"),
            (LOCAL_MACHINE, "SOFTWARE\\Valve\\Steam", "InstallPath"),
            (
                LOCAL_MACHINE,
                "SOFTWARE\\WOW6432Node\\Valve\\Steam",
                "InstallPath",
            ),
        ] {
            if let Ok(value) = hive.open(key).and_then(|key| key.get_string(value)) {
                steam.push(value.into());
            }
        }
        for prefix in [
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
            "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
        ] {
            if let Ok(value) = LOCAL_MACHINE
                .open(format!("{prefix}\\Steam App {}", game.steam_app_id()))
                .and_then(|key| key.get_string("InstallLocation"))
            {
                games.push(value.into());
            }
        }
        for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(path) = std::env::var_os(variable) {
                steam.push(PathBuf::from(path).join("Steam"));
            }
        }
        // Bounded checks of common roots, without recursively scanning every disk.
        for drive in 'C'..='Z' {
            for suffix in ["Steam", "SteamLibrary", "SteamLibrary/steam", "Games/Steam"] {
                steam.push(PathBuf::from(format!("{drive}:/{suffix}")));
            }
            for suffix in [format!("Games/{}", game.name()), game.name().to_owned()] {
                games.push(PathBuf::from(format!("{drive}:/{suffix}")));
            }
        }
    }
    (steam, games)
}
