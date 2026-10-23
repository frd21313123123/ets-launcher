use ets_launcher::download::{self, Source, Status, Transfer, transfer_in_session};
use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions};
use sha1::{Digest, Sha1};
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const ARCHIVE: &[u8] = include_bytes!("../assets/test-archive.zip");

fn torrent_bytes() -> Vec<u8> {
    torrent_for(ARCHIVE)
}

fn torrent_for(archive: &[u8]) -> Vec<u8> {
    let mut hashes = Vec::new();
    for piece in archive.chunks(16384) {
        hashes.extend_from_slice(&Sha1::digest(piece));
    }
    let mut bytes = format!(
        "d4:infod6:lengthi{}e4:name16:test-archive.zip12:piece lengthi16384e6:pieces{}:",
        archive.len(),
        hashes.len()
    )
    .into_bytes();
    bytes.extend_from_slice(&hashes);
    bytes.extend_from_slice(b"ee");
    bytes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downloads_then_extracts_and_installs_a_torrent_into_a_temporary_game() {
    use ets_launcher::install;
    let archive = include_bytes!("../assets/installation-test.zip");
    let seed_dir = tempfile::tempdir().unwrap();
    let download_dir = tempfile::tempdir().unwrap();
    let game = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(game.path().join("bin/win_x64")).unwrap();
    std::fs::write(
        game.path().join("bin/win_x64/eurotrucks2.exe"),
        b"test executable",
    )
    .unwrap();
    std::fs::write(game.path().join("dlc_mock.scs"), b"old map").unwrap();
    std::fs::write(seed_dir.path().join("test-archive.zip"), archive).unwrap();
    let seed = Session::new_with_opts(seed_dir.path().into(), options(true))
        .await
        .unwrap();
    let handle = seed
        .add_torrent(
            AddTorrent::from_bytes(torrent_for(archive)),
            Some(AddTorrentOptions {
                overwrite: true,
                disable_trackers: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), handle.wait_until_completed())
        .await
        .unwrap()
        .unwrap();
    let leecher = Session::new_with_opts(download_dir.path().into(), options(false))
        .await
        .unwrap();
    let state = Arc::new(Mutex::new(Transfer::default()));
    let cancel = CancellationToken::new();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        transfer_in_session(
            leecher.clone(),
            AddTorrent::from_bytes(torrent_for(archive)),
            &state,
            &cancel,
            Some(AddTorrentOptions {
                initial_peers: Some(vec![SocketAddr::from((
                    Ipv4Addr::LOCALHOST,
                    seed.tcp_listen_port().unwrap(),
                ))]),
                disable_trackers: true,
                ..Default::default()
            }),
        ),
    )
    .await;
    leecher.stop().await;
    seed.stop().await;
    result.expect("torrent transfer timed out").unwrap();
    let report = install::install_archive(
        install::prepare(ets_launcher::game::Game::Ets2, game.path()).unwrap(),
        download_dir.path().join("test-archive.zip"),
        state,
        cancel,
    )
    .await
    .unwrap();
    assert_eq!(report.files, 3);
    assert_eq!(
        std::fs::read(game.path().join("dlc_mock.scs")).unwrap(),
        b"new map data"
    );
    assert_eq!(
        std::fs::read(game.path().join("bin/win_x64/steam_api64.dll")).unwrap(),
        b"test library payload"
    );
    assert_eq!(
        std::fs::read(report.backup.join("original/dlc_mock.scs")).unwrap(),
        b"old map"
    );
}

fn options(listen: bool) -> SessionOptions {
    SessionOptions {
        disable_dht: true,
        disable_dht_persistence: true,
        persistence: None,
        listen_port_range: listen.then_some(40000..40100),
        enable_upnp_port_forwarding: false,
        ..Default::default()
    }
}

#[test]
fn bundled_torrent_matches_the_archive_shown_in_the_window() {
    let torrent: librqbit::TorrentMetaV1<librqbit::ByteBuf<'_>> =
        librqbit::torrent_from_bytes(ets_launcher::BUNDLED_TORRENT).unwrap();
    assert_eq!(torrent.info.length, Some(ets_launcher::ARCHIVE_SIZE));
    assert_eq!(
        torrent.info.name.as_ref().unwrap().as_ref(),
        ets_launcher::ARCHIVE_NAME.as_bytes()
    );
    assert!(torrent.info.files.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfers_an_archive_over_bittorrent_and_verifies_every_byte() {
    let seed_dir = tempfile::tempdir().unwrap();
    let download_dir = tempfile::tempdir().unwrap();
    tokio::fs::write(seed_dir.path().join("test-archive.zip"), ARCHIVE)
        .await
        .unwrap();
    let seed = Session::new_with_opts(seed_dir.path().into(), options(true))
        .await
        .unwrap();
    let handle = seed
        .add_torrent(
            AddTorrent::from_bytes(torrent_bytes()),
            Some(AddTorrentOptions {
                overwrite: true,
                disable_trackers: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), handle.wait_until_completed())
        .await
        .unwrap()
        .unwrap();
    let port = seed.tcp_listen_port().unwrap();
    let leecher = Session::new_with_opts(download_dir.path().into(), options(false))
        .await
        .unwrap();
    let state = Arc::new(Mutex::new(Transfer::default()));
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        transfer_in_session(
            leecher.clone(),
            AddTorrent::from_bytes(torrent_bytes()),
            &state,
            &CancellationToken::new(),
            Some(AddTorrentOptions {
                initial_peers: Some(vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))]),
                disable_trackers: true,
                ..Default::default()
            }),
        ),
    )
    .await;
    leecher.stop().await;
    seed.stop().await;
    result.expect("local torrent timed out").unwrap();
    assert_eq!(
        tokio::fs::read(download_dir.path().join("test-archive.zip"))
            .await
            .unwrap(),
        ARCHIVE
    );
    assert_eq!(download::snapshot(&state).downloaded, ARCHIVE.len() as u64);
}

#[tokio::test]
async fn cancels_a_torrent_without_peers() {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::new_with_opts(directory.path().into(), options(false))
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        stopper.cancel();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        transfer_in_session(
            session.clone(),
            AddTorrent::from_bytes(torrent_bytes()),
            &Arc::new(Mutex::new(Transfer::default())),
            &cancel,
            Some(AddTorrentOptions {
                disable_trackers: true,
                ..Default::default()
            }),
        ),
    )
    .await
    .unwrap();
    session.stop().await;
    assert!(result.is_err());
    assert!(cancel.is_cancelled());
}

#[tokio::test]
async fn invalid_magnet_reaches_an_error_state_without_starting_a_session() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(Transfer::default()));
    download::run(
        Source::Magnet("https://example.org".into()),
        directory.path().into(),
        state.clone(),
        CancellationToken::new(),
    )
    .await;
    assert_eq!(download::snapshot(&state).status, Status::Error);
}
