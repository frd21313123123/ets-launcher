use base64::{Engine, engine::general_purpose::STANDARD};
use ets_launcher::{
    download::Transfer,
    game::{self, Channel, Game},
    install, playground,
    settings::Settings,
};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

const ATS_PAGE: &str = include_str!("fixtures/ats-page.html");

fn installation(root: &Path, game: Game) {
    fs::create_dir_all(root.join("bin/win_x64")).unwrap();
    fs::write(
        root.join("bin/win_x64").join(game.executable()),
        b"original game executable",
    )
    .unwrap();
}

fn manifest(library: &Path, game: Game, directory: &str) {
    fs::create_dir_all(library.join("steamapps")).unwrap();
    fs::write(
        library.join(format!("steamapps/appmanifest_{}.acf", game.steam_app_id())),
        format!(
            "\"AppState\" {{ \"appid\" \"{}\" \"installdir\" {} }}",
            game.steam_app_id(),
            serde_json::to_string(directory).unwrap()
        ),
    )
    .unwrap();
}

#[test]
fn detects_both_games_from_their_own_manifests_and_never_confuses_executables() {
    let directory = tempfile::tempdir().unwrap();
    let steam = directory.path().join("Steam");
    let library = directory.path().join("Библиотека игр");
    let ets = library.join("steamapps/common/Custom ETS");
    let ats = library.join("steamapps/common/Custom ATS");
    installation(&ets, Game::Ets2);
    installation(&ats, Game::Ats);
    manifest(&library, Game::Ets2, "Custom ETS");
    manifest(&library, Game::Ats, "Custom ATS");
    fs::create_dir_all(steam.join("steamapps")).unwrap();
    fs::write(
        steam.join("steamapps/libraryfolders.vdf"),
        format!(
            "\"libraryfolders\" {{ \"1\" {{ \"path\" {} }} }}",
            serde_json::to_string(&library).unwrap()
        ),
    )
    .unwrap();
    assert_eq!(
        game::detect_from_paths(
            Game::Ats,
            std::slice::from_ref(&steam),
            std::slice::from_ref(&ets)
        ),
        Some(ats.clone())
    );
    assert_eq!(
        game::detect_from_paths(Game::Ets2, &[steam], std::slice::from_ref(&ats)),
        Some(ets.clone())
    );
    assert!(!game::is_installation(Game::Ats, &ets));
    assert!(!game::is_installation(Game::Ets2, &ats));
    assert!(install::prepare(Game::Ats, &ets).is_err());
    assert!(!ets.join(".ets-launcher").exists());
}

#[test]
fn detects_standard_ats_directory_without_a_manifest() {
    let directory = tempfile::tempdir().unwrap();
    let ats = directory
        .path()
        .join("steamapps/common/American Truck Simulator");
    installation(&ats, Game::Ats);
    assert_eq!(
        game::detect_from_paths(Game::Ats, &[directory.path().into()], &[]),
        Some(ats)
    );
}

#[test]
fn old_settings_migrate_and_game_paths_and_beta_choice_are_independent() {
    let mut settings: Settings =
        serde_json::from_str(r#"{"output":"cache","game_dir":"existing-ets"}"#).unwrap();
    assert_eq!(settings.selected_game, Game::Ets2);
    assert_eq!(
        settings.directory(Game::Ets2),
        Some(Path::new("existing-ets"))
    );
    assert!(settings.directory(Game::Ats).is_none());
    settings.set_directory(Game::Ats, Some("new-ats".into()));
    settings.ats_channel = Channel::Beta;
    assert_eq!(settings.channel(), Channel::Stable);
    settings.selected_game = Game::Ats;
    assert_eq!(settings.channel(), Channel::Beta);
    assert_ne!(
        settings.download_directory(Game::Ats),
        settings.download_directory(Game::Ets2)
    );
    let restored: Settings =
        serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
    assert_eq!(
        restored.directory(Game::Ets2),
        Some(Path::new("existing-ets"))
    );
    assert_eq!(restored.directory(Game::Ats), Some(Path::new("new-ats")));
    assert_eq!(restored.selected_game, Game::Ats);
    assert_eq!(restored.channel(), Channel::Beta);
}

#[test]
fn chooses_stable_or_beta_ats_download_and_rejects_cross_game_archives() {
    let stable = playground::find_file_for(ATS_PAGE, Channel::Stable).unwrap();
    let beta = playground::find_file_for(ATS_PAGE, Channel::Beta).unwrap();
    assert_eq!(stable.post_id, 1660567);
    assert_eq!(stable.file_id, 442139);
    assert_eq!(beta.file_id, 443254);
    assert!(Game::Ats.validate_archive(&stable.name).is_ok());
    assert!(Game::Ats.validate_archive(&beta.name).is_ok());
    assert!(Game::Ets2.validate_archive(&stable.name).is_err());
    assert!(
        playground::find_file_for(
            &ATS_PAGE.replace("ATS_dlc_maps_1.62.0.4s_BETA.rar", "other.rar"),
            Channel::Beta
        )
        .is_err()
    );
    assert_ne!(Game::Ats.source_page(), Game::Ets2.source_page());
}

fn torrent(name: &str) -> Vec<u8> {
    let mut bytes = format!(
        "d4:infod6:lengthi1e4:name{}:{}12:piece lengthi16384e6:pieces20:",
        name.len(),
        name
    )
    .into_bytes();
    bytes.extend_from_slice(&[0; 20]);
    bytes.extend_from_slice(b"ee");
    bytes
}

#[tokio::test]
async fn requests_the_matching_ats_torrent_from_the_website_api() {
    for channel in [Channel::Stable, Channel::Beta] {
        let descriptor = playground::find_file_for(ATS_PAGE, channel).unwrap();
        let bytes = torrent(&descriptor.name);
        let encoded = STANDARD.encode(&bytes);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/ats", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for step in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 8192];
                let count = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                let body = if step == 0 {
                    assert!(request.starts_with("GET /ats "));
                    ATS_PAGE.to_owned()
                } else {
                    assert!(request.starts_with("GET /api/file.download?"));
                    assert!(request.contains(&format!("file_id={}", descriptor.file_id)));
                    assert!(request.contains("post_id=1660567"));
                    assert!(request.contains(&descriptor.name));
                    serde_json::json!({"wait_time":null,"torrent_data":encoded}).to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let fetched = playground::fetch_torrent_for(
            &playground::client().unwrap(),
            &url,
            channel,
            &CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(fetched.bytes, bytes);
        Game::Ats.validate_archive(&fetched.archive_name).unwrap();
    }
}

#[tokio::test]
async fn installs_into_ats_and_preserves_its_executable_and_original_files() {
    let directory = tempfile::tempdir().unwrap();
    installation(directory.path(), Game::Ats);
    fs::write(directory.path().join("dlc_mock.scs"), b"old ATS map").unwrap();
    let report = install::install_archive(
        install::prepare(Game::Ats, directory.path()).unwrap(),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/installation-test.zip"),
        Arc::new(Mutex::new(Transfer::default())),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.files, 3);
    assert_eq!(
        fs::read(directory.path().join("dlc_mock.scs")).unwrap(),
        b"new map data"
    );
    assert_eq!(
        fs::read(report.backup.join("original/dlc_mock.scs")).unwrap(),
        b"old ATS map"
    );
    assert_eq!(
        fs::read(directory.path().join("bin/win_x64/amtrucks.exe")).unwrap(),
        b"original game executable"
    );
    assert!(
        !directory
            .path()
            .join("bin/win_x64/eurotrucks2.exe")
            .exists()
    );
}
