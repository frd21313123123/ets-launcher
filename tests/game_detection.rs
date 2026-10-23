use ets_launcher::{
    game::{detect_from_paths, is_installation},
    settings::Settings,
};
use std::path::Path;

fn installation(path: &Path) {
    std::fs::create_dir_all(path.join("bin/win_x64")).unwrap();
    std::fs::write(
        path.join("bin/win_x64/eurotrucks2.exe"),
        b"test executable marker",
    )
    .unwrap();
}

fn manifest(library: &Path, install_dir: &str) {
    std::fs::create_dir_all(library.join("steamapps")).unwrap();
    std::fs::write(
        library.join("steamapps/appmanifest_227300.acf"),
        format!(
            "\"AppState\" {{ \"appid\" \"227300\" \"installdir\" {} }}",
            serde_json::to_string(install_dir).unwrap()
        ),
    )
    .unwrap();
}

#[test]
fn finds_custom_install_directory_in_a_second_steam_library() {
    let temp = tempfile::tempdir().unwrap();
    let steam = temp.path().join("Steam");
    let library = temp.path().join("Библиотека игр");
    let game = library.join("steamapps/common/ETS2 Custom");
    installation(&game);
    manifest(&library, "ETS2 Custom");
    std::fs::create_dir_all(steam.join("steamapps")).unwrap();
    std::fs::write(
        steam.join("steamapps/libraryfolders.vdf"),
        format!(
            "\"libraryfolders\" {{ \"0\" {{ \"path\" {} }} \"1\" {{ \"path\" {} }} }}",
            serde_json::to_string(&steam).unwrap(),
            serde_json::to_string(&library).unwrap()
        ),
    )
    .unwrap();
    assert_eq!(detect_from_paths(&[steam], &[]), Some(game));
}

#[test]
fn supports_legacy_vdf_comments_and_a_missing_library() {
    let temp = tempfile::tempdir().unwrap();
    let steam = temp.path().join("Steam");
    let library = temp.path().join("Other Library");
    let game = library.join("steamapps/common/Euro Truck Simulator 2");
    installation(&game);
    std::fs::create_dir_all(steam.join("steamapps")).unwrap();
    std::fs::write(steam.join("steamapps/libraryfolders.vdf"), format!("\u{feff}// old Steam format\n\"LibraryFolders\" {{ \"1\" \"Z:\\\\Missing\" \"2\" {} }}", serde_json::to_string(&library).unwrap())).unwrap();
    assert_eq!(detect_from_paths(&[steam], &[]), Some(game));
}

#[test]
fn ignores_stale_registry_paths_and_rejects_profile_directories() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("Documents/Euro Truck Simulator 2");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("config.cfg"), b"profile").unwrap();
    let game = temp.path().join("Games/Euro Truck Simulator 2");
    installation(&game);
    assert!(!is_installation(&profile));
    assert_eq!(
        detect_from_paths(&[], &[profile, temp.path().join("missing"), game.clone()]),
        Some(game)
    );
}

#[test]
fn ignores_a_manifest_that_escapes_the_common_directory() {
    let temp = tempfile::tempdir().unwrap();
    let steam = temp.path().join("Steam");
    installation(&steam.join("steamapps/elsewhere"));
    manifest(&steam, "../elsewhere");
    assert_eq!(detect_from_paths(&[steam], &[]), None);
}

#[test]
fn settings_from_the_previous_version_still_load() {
    let settings: Settings = serde_json::from_str(r#"{"output":"downloads"}"#).unwrap();
    assert!(settings.game_dir.is_none());
    assert_eq!(settings.output, Path::new("downloads"));
}
