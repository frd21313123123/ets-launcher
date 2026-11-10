use base64::{Engine, engine::general_purpose::STANDARD};
use ets_launcher::{
    catalog::CatalogCache,
    game::{Channel, Game},
    playground::{self, parse_catalog},
    settings::Settings,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

fn page(game: Game, version: &str, beta: bool) -> String {
    let prefix = if game == Game::Ats { "ATS" } else { "ETS2" };
    format!(
        r#"<main id="post-item" data-post-id="100" data-download-allowed="1"><h1>{prefix} DLC {version}</h1><div class="article-content">
      <pg-file data-file-id="901" data-name="{prefix}_dlc_maps_{version}.rar" data-text="{prefix} DLC {version}" data-size="3.12 Гб" data-can-torrent-download="1"></pg-file>
      {}
      <pg-file data-file-id="903" data-is-subfile="1" data-name="Linux_1.999.rar" data-can-torrent-download="1"></pg-file>
    </div></main>"#,
        if beta {
            format!(
                r#"<pg-file data-file-id="902" data-is-subfile="1" data-name="{prefix}_dlc_next.rar" data-text="Beta 2.0.0.4s" data-size="3.45 Гб" data-can-torrent-download="1"></pg-file>"#
            )
        } else {
            String::new()
        }
    )
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

#[test]
fn parses_versions_sizes_labels_and_available_channels_for_both_games() {
    for game in Game::ALL {
        let catalog = parse_catalog(&page(game, "1.99.12.35s", true), game.source_page()).unwrap();
        catalog.validate_for(game).unwrap();
        assert_eq!(catalog.releases.len(), 2);
        let stable = catalog.select(Channel::Stable).unwrap();
        assert_eq!(stable.version.as_deref(), Some("1.99.12.35s"));
        assert_eq!(stable.size_label.as_deref(), Some("3.12 Гб"));
        assert_eq!(stable.file.file_id, 901);
        let beta = catalog.select(Channel::Beta).unwrap();
        assert_eq!(beta.version.as_deref(), Some("2.0.0.4s"));
        assert_eq!(beta.file.file_id, 902);
        assert!(!beta.file.name.to_lowercase().contains("beta"));
        assert!(catalog.title.contains("1.99.12.35s"));
        let updated = parse_catalog(&page(game, "2.1.0.0s", false), game.source_page()).unwrap();
        assert!(updated.select(Channel::Beta).is_err());
        assert_eq!(
            updated.select(Channel::Stable).unwrap().version.as_deref(),
            Some("2.1.0.0s")
        );
    }
}

#[test]
fn selects_newest_numbered_version_and_accepts_beta_as_the_main_file() {
    let html = r#"<main id="post-item" data-post-id="100"><div class="article-content">
      <pg-file data-file-id="1" data-name="ATS_dlc_1.99.0.rar" data-can-torrent-download="1"></pg-file>
      <pg-file data-file-id="2" data-name="ATS_dlc_1.100.0.rar" data-is-subfile="1" data-can-torrent-download="1"></pg-file>
      <pg-file data-file-id="3" data-name="ATS_dlc_2.0.0_BETA.rar" data-can-torrent-download="1"></pg-file>
    </div></main>"#;
    let catalog = parse_catalog(html, Game::Ats.source_page()).unwrap();
    assert_eq!(catalog.select(Channel::Stable).unwrap().file.file_id, 2);
    assert_eq!(catalog.select(Channel::Beta).unwrap().file.file_id, 3);
    assert_eq!(
        playground::parse_version("Steam DLC v1.125.10.5s build 2028"),
        Some("1.125.10.5s".into())
    );
    assert!(playground::parse_version("ATS 2028 build 45").is_none());
}

#[test]
fn cache_is_separate_by_game_and_source_and_tolerates_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.json");
    let mut cache = CatalogCache::default();
    for game in Game::ALL {
        cache.set(
            game,
            parse_catalog(&page(game, "1.99.0.0s", true), game.source_page()).unwrap(),
        );
    }
    cache.save_to(&path).unwrap();
    let cached = CatalogCache::load_from(&path);
    for game in Game::ALL {
        assert_eq!(
            cached
                .get(game, game.source_page())
                .unwrap()
                .select(Channel::Stable)
                .unwrap()
                .version
                .as_deref(),
            Some("1.99.0.0s")
        );
        assert!(
            cached
                .get(game, "https://www.playground.ru/other-page")
                .is_none()
        );
    }
    std::fs::write(&path, b"interrupted JSON").unwrap();
    assert!(
        CatalogCache::load_from(&path)
            .get(Game::Ats, Game::Ats.source_page())
            .is_none()
    );
}

#[test]
fn source_links_are_editable_without_a_rebuild_and_preserve_old_settings() {
    let mut settings: Settings =
        serde_json::from_str(r#"{"output":"cache","game_dir":"existing ETS2"}"#).unwrap();
    assert_eq!(settings.source(Game::Ats), Game::Ats.source_page());
    let new_source = "https://www.playground.ru/american_truck_simulator/cheat/new_release-9876543";
    settings.set_source(Game::Ats, new_source).unwrap();
    settings.set_channel(Game::Ets2, Channel::Beta);
    let restored: Settings =
        serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
    assert_eq!(restored.source(Game::Ats), new_source);
    assert_eq!(restored.channel(), Channel::Beta);
    for invalid in [
        "https://example.com/american_truck_simulator/cheat/file",
        "http://www.playground.ru/american_truck_simulator/cheat/file",
        Game::Ets2.source_page(),
        "https://user:password@www.playground.ru/american_truck_simulator/cheat/file",
    ] {
        assert!(settings.set_source(Game::Ats, invalid).is_err());
        assert_eq!(settings.source(Game::Ats), new_source);
    }
    settings.set_source(Game::Ats, "").unwrap();
    assert_eq!(settings.source(Game::Ats), Game::Ats.source_page());
}

#[tokio::test]
async fn observes_changed_versions_and_ids_and_downloads_fresh_metadata_instead_of_cached_data() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/post", listener.local_addr().unwrap());
    let new_name = "ATS_dlc_next.rar";
    let bytes = torrent(new_name);
    let encoded = STANDARD.encode(&bytes);
    let server = tokio::spawn(async move {
        for step in 0..4 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            let count = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            let body = if step < 3 {
                assert!(request.starts_with("GET /post "));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("cache-control: no-cache")
                );
                if step == 0 {
                    page(Game::Ats, "1.61.0.0s", false).replace("901", "44")
                } else {
                    page(Game::Ats, "1.99.0.0s", true)
                }
            } else {
                assert!(request.starts_with("GET /api/file.download?"));
                assert!(request.contains("file_id=902"));
                assert!(request.contains("post_id=100"));
                assert!(request.contains("file_name=ATS_dlc_next.rar"));
                serde_json::json!({"wait_time":null,"torrent_data":encoded}).to_string()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let client = playground::client().unwrap();
    let cancel = CancellationToken::new();
    let old = playground::fetch_catalog(&client, &url, &cancel)
        .await
        .unwrap();
    assert_eq!(old.select(Channel::Stable).unwrap().file.file_id, 44);
    let new = playground::fetch_catalog(&client, &url, &cancel)
        .await
        .unwrap();
    assert_eq!(new.select(Channel::Stable).unwrap().file.file_id, 901);
    assert_eq!(
        new.select(Channel::Stable).unwrap().version.as_deref(),
        Some("1.99.0.0s")
    );
    let fetched = playground::fetch_torrent_for(&client, &url, Channel::Beta, &cancel, |_| {})
        .await
        .unwrap();
    assert_eq!(fetched.bytes, bytes);
    server.await.unwrap();
}

#[tokio::test]
async fn follows_an_updated_source_slug_before_resolving_the_download_api() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/old-version", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for step in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            let count = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            let response = if step == 0 {
                assert!(request.starts_with("GET /old-version "));
                "HTTP/1.1 302 Found\r\nLocation: /new-version\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
            } else {
                assert!(request.starts_with("GET /new-version "));
                let body = page(Game::Ets2, "1.99.0.0s", true);
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            };
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let catalog = playground::fetch_catalog(
        &playground::client().unwrap(),
        &url,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(catalog.source_url, url);
    assert!(catalog.page_url.ends_with("/new-version"));
    assert!(catalog.select(Channel::Beta).is_ok());
    server.await.unwrap();
}

#[test]
fn only_beta_labelled_file_is_used_when_the_stable_branch_is_selected() {
    let html = r#"<main id="post-item" data-post-id="100" data-download-allowed="1"><h1>ETS2</h1><div class="article-content">
      <pg-file data-file-id="1" data-name="ETS2_dlc_maps_1.62.0.12s_BETA.rar" data-text="ETS2 DLC Unlocker 1.62.0.12s BETA (works on release 1.62.1.0s)" data-can-torrent-download="1"></pg-file>
    </div></main>"#;
    let catalog = parse_catalog(html, "").unwrap();
    assert!(catalog.select(Channel::Stable).is_err());
    assert_eq!(catalog.effective_channel(Channel::Stable), Channel::Beta);
    assert!(
        catalog
            .select(catalog.effective_channel(Channel::Stable))
            .is_ok()
    );

    let both = parse_catalog(&page(Game::Ets2, "1.0.0.1s", true), "").unwrap();
    assert_eq!(both.effective_channel(Channel::Stable), Channel::Stable);
    assert_eq!(both.effective_channel(Channel::Beta), Channel::Beta);
}
