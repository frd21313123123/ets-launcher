//! Browsers registered in Windows, and which Tampermonkey build each one needs.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

/// The browser and Downloads folder chosen in the settings; `None` means the
/// system default. Set by the window whenever the settings change.
static PREFERENCES: Mutex<(Option<PathBuf>, Option<PathBuf>)> = Mutex::new((None, None));

pub fn set_preferences(browser: Option<PathBuf>, downloads: Option<PathBuf>) {
    *PREFERENCES.lock().unwrap_or_else(|p| p.into_inner()) = (browser, downloads);
}

fn preferences() -> (Option<PathBuf>, Option<PathBuf>) {
    PREFERENCES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Opens an address in the browser chosen in the settings. Debug builds can
/// log it instead (`ETS_LAUNCHER_OPEN_LOG`) so queues are testable without a browser.
pub fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(debug_assertions)]
    if let Some(log) = std::env::var_os("ETS_LAUNCHER_OPEN_LOG") {
        use std::io::Write;
        return std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .and_then(|mut f| writeln!(f, "{url}"));
    }
    open(preferences().0.as_deref(), url)
}

/// The system Downloads folder.
pub fn system_downloads() -> Option<PathBuf> {
    directories::UserDirs::new().and_then(|d| d.download_dir().map(PathBuf::from))
}

/// The folder the browser saves to: the one chosen in the settings, else the
/// system Downloads folder. Debug builds can override it (`ETS_LAUNCHER_DOWNLOADS_DIR`).
pub fn downloads_folder() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(folder) = std::env::var_os("ETS_LAUNCHER_DOWNLOADS_DIR") {
        return Some(folder.into());
    }
    preferences().1.or_else(system_downloads)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub name: String,
    pub exe: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Family {
    /// Firefox and its forks: Zen, LibreWolf, Floorp, Waterfox.
    Firefox,
    Edge,
    /// Chrome and other Chromium browsers (Brave, Opera, Vivaldi, Helium…).
    Chromium,
}

/// Browsers from the Start menu registry ("default apps" list), Internet Explorer excluded.
#[cfg(windows)]
pub fn detect() -> Vec<Installed> {
    use windows_registry::{CURRENT_USER, LOCAL_MACHINE};
    let mut found: Vec<Installed> = Vec::new();
    for (hive, root) in [
        (CURRENT_USER, "SOFTWARE\\Clients\\StartMenuInternet"),
        (LOCAL_MACHINE, "SOFTWARE\\Clients\\StartMenuInternet"),
        (
            LOCAL_MACHINE,
            "SOFTWARE\\WOW6432Node\\Clients\\StartMenuInternet",
        ),
    ] {
        let Ok(clients) = hive.open(root) else {
            continue;
        };
        let Ok(names) = clients.keys() else { continue };
        for key in names {
            if key.eq_ignore_ascii_case("IEXPLORE.EXE") {
                continue;
            }
            let Ok(entry) = clients.open(&key) else {
                continue;
            };
            let name = entry.get_string("").unwrap_or_else(|_| key.clone());
            let Some(exe) = entry
                .open("shell\\open\\command")
                .and_then(|c| c.get_string(""))
                .ok()
                .and_then(|command| executable(&command))
            else {
                continue;
            };
            if exe.is_file() && !found.iter().any(|b| b.exe == exe) {
                found.push(Installed { name, exe });
            }
        }
    }
    found.sort_by_key(|b| b.name.to_lowercase());
    found
}

#[cfg(not(windows))]
pub fn detect() -> Vec<Installed> {
    Vec::new()
}

/// The program Windows opens https links with (the default browser).
#[cfg(windows)]
pub fn system_default() -> Option<PathBuf> {
    use windows_registry::{CLASSES_ROOT, CURRENT_USER};
    let prog = CURRENT_USER
        .open(
            "Software\\Microsoft\\Windows\\Shell\\Associations\\UrlAssociations\\https\\UserChoice",
        )
        .and_then(|k| k.get_string("ProgId"))
        .ok()?;
    let command = CLASSES_ROOT
        .open(format!("{prog}\\shell\\open\\command"))
        .and_then(|k| k.get_string(""))
        .ok()?;
    executable(&command).filter(|exe| exe.is_file())
}

#[cfg(not(windows))]
pub fn system_default() -> Option<PathBuf> {
    None
}

/// The program path of a registry command: `"C:\…\zen.exe" -osint -url "%1"`.
fn executable(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    let path = if let Some(rest) = command.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        command.split(" -").next()?.trim()
    };
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub fn family(exe: &Path) -> Family {
    let file = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match file.as_str() {
        "firefox" | "zen" | "librewolf" | "floorp" | "waterfox" | "mullvadbrowser" => {
            Family::Firefox
        }
        "msedge" => Family::Edge,
        _ => Family::Chromium,
    }
}

/// Tampermonkey's page in the store this browser installs extensions from.
pub fn tampermonkey_page(family: Family) -> &'static str {
    match family {
        Family::Firefox => "https://addons.mozilla.org/firefox/addon/tampermonkey/",
        Family::Edge => {
            "https://microsoftedge.microsoft.com/addons/detail/tampermonkey/iikmkjmpaadaobahmlepeloendndfphd"
        }
        Family::Chromium => {
            "https://chromewebstore.google.com/detail/tampermonkey/dhdgffkkebhmkfjojejmpbldmpobfkfo"
        }
    }
}

/// Opens an address in the chosen browser, or in the system default one.
pub fn open(browser: Option<&Path>, url: &str) -> std::io::Result<()> {
    match browser {
        Some(exe) => std::process::Command::new(exe).arg(url).spawn().map(|_| ()),
        None => open::that(url),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_commands_and_families_are_read() {
        assert_eq!(
            executable(r#""C:\Program Files\Zen Browser\zen.exe""#),
            Some(PathBuf::from(r"C:\Program Files\Zen Browser\zen.exe"))
        );
        assert_eq!(
            executable(r#""C:\Edge\msedge.exe" --single-argument %1"#),
            Some(PathBuf::from(r"C:\Edge\msedge.exe"))
        );
        assert_eq!(
            executable(r#""C:\Program Files\Zen Browser\zen.exe" -osint -url "%1""#),
            Some(PathBuf::from(r"C:\Program Files\Zen Browser\zen.exe"))
        );
        assert_eq!(
            executable(r"C:\Browser\browser.exe -url %1"),
            Some(PathBuf::from(r"C:\Browser\browser.exe"))
        );
        assert_eq!(
            family(Path::new(r"C:\Program Files\Zen Browser\zen.exe")),
            Family::Firefox
        );
        assert_eq!(family(Path::new(r"C:\Edge\msedge.exe")), Family::Edge);
        assert_eq!(family(Path::new(r"C:\Helium\chrome.exe")), Family::Chromium);
        assert!(tampermonkey_page(Family::Firefox).contains("addons.mozilla.org"));
    }
}
