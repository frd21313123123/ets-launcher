use std::path::{Component, Path, PathBuf};

pub const STEAM_APP_ID: u32 = 227_300;

/// A Documents/Euro Truck Simulator 2 profile directory is not a game installation.
pub fn is_installation(path: &Path) -> bool {
    [
        "bin/win_x64/eurotrucks2.exe",
        "bin/win_x86/eurotrucks2.exe",
        "bin/linux_x64/eurotrucks2",
    ]
    .iter()
    .any(|exe| path.join(exe).is_file())
}

pub fn detect(saved: Option<&Path>) -> Option<PathBuf> {
    if let Some(saved) = saved.filter(|path| is_installation(path)) {
        return Some(saved.to_path_buf());
    }
    // Manifests and libraryfolders.vdf take priority over stale uninstall records.
    let (steam_roots, installed_games) = platform_paths();
    detect_from_paths(&steam_roots, &installed_games).map(|path| path.components().collect())
}

pub fn detect_from_paths(steam_roots: &[PathBuf], game_paths: &[PathBuf]) -> Option<PathBuf> {
    for root in steam_roots {
        if let Some(path) = find_in_steam(root) {
            return Some(path);
        }
        let standard = root.join("steamapps/common/Euro Truck Simulator 2");
        if is_installation(&standard) {
            return Some(standard);
        }
    }
    game_paths
        .iter()
        .find(|path| is_installation(path))
        .cloned()
}

fn find_in_steam(root: &Path) -> Option<PathBuf> {
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
    for library in libraries {
        if let Some(vdf) =
            read_vdf(&library.join(format!("steamapps/appmanifest_{STEAM_APP_ID}.acf")))
            && let Some(app) = vdf.object("AppState")
            && app.text("appid") == Some("227300")
            && let Some(directory) = app.text("installdir")
        {
            let relative = Path::new(directory);
            if relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            {
                let path = library.join("steamapps/common").join(relative);
                if is_installation(&path) {
                    return Some(path);
                }
            }
        }
        let standard = library.join("steamapps/common/Euro Truck Simulator 2");
        if is_installation(&standard) {
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

fn platform_paths() -> (Vec<PathBuf>, Vec<PathBuf>) {
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
                .open(format!("{prefix}\\Steam App {STEAM_APP_ID}"))
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
            for suffix in ["Games/Euro Truck Simulator 2", "Euro Truck Simulator 2"] {
                games.push(PathBuf::from(format!("{drive}:/{suffix}")));
            }
        }
    }
    (steam, games)
}
