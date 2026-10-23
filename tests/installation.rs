use ets_launcher::{
    archive,
    download::Transfer,
    install::{self, Event, PayloadFile},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn fake_game(root: &Path) {
    write(
        root.join("bin/win_x64/eurotrucks2.exe"),
        b"original executable",
    );
    write(
        root.join("bin/win_x64/steam_api64.dll"),
        b"original library",
    );
    write(root.join("dlc_mock.scs"), b"original map");
    write(root.join("base.scs"), b"base game data");
}

#[test]
fn rejects_paths_links_and_case_collisions_before_extraction() {
    for path in [
        "../escape",
        "/root",
        "C:\\escape",
        "dir/../../x",
        "file:stream",
        "bin/NUL.dll",
        "dir/COM1",
        "dir./file",
        "dir/file ",
        "dir/../x",
        "dir\nfile",
    ] {
        assert!(archive::safe_relative(path).is_err(), "accepted {path:?}");
    }
    assert!(archive::safe_relative("Архив/bin/win_x64/steam_api64.dll").is_ok());
    assert!(
        archive::parse_listing("Path = dlc_a.scs\nSize = 5\n\nPath = DLC_A.scs\nSize = 5\n")
            .is_err()
    );
    assert!(
        archive::parse_listing("Path = dlc_a.scs\nSize = 5\nSymbolic Link = outside\n").is_err()
    );
    assert!(archive::parse_listing("Path = dlc_a.scs\nSize = 5\nEncrypted = +\n").is_err());
    assert!(
        archive::parse_listing("Path = bin\nSize = 5\n\nPath = bin/file.dll\nSize = 5\n").is_err()
    );
}

#[tokio::test]
async fn extracts_a_real_rar5_with_crc_verification() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/rar5-test.rar");
    let cancel = CancellationToken::new();
    let entries = archive::inspect(&path, &cancel).await.unwrap();
    assert_eq!(entries.len(), 1);
    let staging = tempfile::tempdir().unwrap();
    archive::extract(&path, staging.path(), &entries, &cancel)
        .await
        .unwrap();
    assert_eq!(
        fs::read(staging.path().join("helloworld.txt")).unwrap(),
        b"hello libarchive test suite!\n"
    );
}

#[tokio::test]
async fn installs_wrapped_archive_and_preserves_originals_and_game_executable() {
    let game = tempfile::tempdir().unwrap();
    fake_game(game.path());
    let state = Arc::new(Mutex::new(Transfer::default()));
    let report = install::install_archive(
        install::prepare(ets_launcher::game::Game::Ets2, game.path()).unwrap(),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/installation-test.zip"),
        state,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(report.files, 3);
    assert_eq!(
        fs::read(game.path().join("dlc_mock.scs")).unwrap(),
        b"new map data"
    );
    assert_eq!(
        fs::read(game.path().join("dlc_second.scs")).unwrap(),
        b"second map data"
    );
    assert_eq!(
        fs::read(game.path().join("bin/win_x64/steam_api64.dll")).unwrap(),
        b"test library payload"
    );
    assert_eq!(
        fs::read(report.backup.join("original/dlc_mock.scs")).unwrap(),
        b"original map"
    );
    assert_eq!(
        fs::read(report.backup.join("original/bin/win_x64/steam_api64.dll")).unwrap(),
        b"original library"
    );
    assert_eq!(
        fs::read(game.path().join("bin/win_x64/eurotrucks2.exe")).unwrap(),
        b"original executable"
    );
    assert_eq!(
        fs::read(game.path().join("base.scs")).unwrap(),
        b"base game data"
    );
    assert!(!game.path().join("README.txt").exists());
    assert!(!game.path().join("Архив ETS2").exists());
    assert_eq!(
        fs::read_dir(game.path().join(".ets-launcher/work"))
            .unwrap()
            .count(),
        0
    );
    let journal: serde_json::Value =
        serde_json::from_slice(&fs::read(report.backup.join("transaction.json")).unwrap()).unwrap();
    assert_eq!(journal["phase"], "committed");
}

#[tokio::test]
async fn damaged_archive_never_changes_the_game() {
    let game = tempfile::tempdir().unwrap();
    fake_game(game.path());
    let source = tempfile::tempdir().unwrap();
    write(source.path().join("bad.rar"), b"not an archive");
    let result = install::install_archive(
        install::prepare(ets_launcher::game::Game::Ets2, game.path()).unwrap(),
        source.path().join("bad.rar"),
        Arc::new(Mutex::new(Transfer::default())),
        CancellationToken::new(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        fs::read(game.path().join("dlc_mock.scs")).unwrap(),
        b"original map"
    );
}

#[tokio::test]
async fn cancelled_unpacker_exits_before_temporary_directory_cleanup() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/installation-test.zip");
    let entries = archive::inspect(&path, &CancellationToken::new())
        .await
        .unwrap();
    let staging = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        archive::extract(&path, staging.path(), &entries, &cancel)
            .await
            .is_err()
    );
    staging
        .close()
        .expect("unpacker retained a temporary file lock");
}

fn staged_files(staging: &Path) -> Vec<PayloadFile> {
    let names = ["dlc_new.scs", "dlc_mock.scs", "bin/win_x86/steam_api.dll"];
    names
        .iter()
        .map(|name| {
            write(staging.join(name), b"replacement");
            PayloadFile {
                source: PathBuf::from(name),
                relative: PathBuf::from(name),
                size: 11,
            }
        })
        .collect()
}

#[test]
fn cancellation_after_replacement_rolls_back_and_removes_created_directories() {
    let game = tempfile::tempdir().unwrap();
    fake_game(game.path());
    let staging = tempfile::tempdir().unwrap();
    let files = staged_files(staging.path());
    let cancel = CancellationToken::new();
    let result = install::prepare(ets_launcher::game::Game::Ets2, game.path())
        .unwrap()
        .install_files(staging.path(), &files, &cancel, |event| {
            if let Event::File { done: 2, .. } = event {
                cancel.cancel();
            }
        });
    assert!(result.is_err());
    assert_eq!(
        fs::read(game.path().join("dlc_mock.scs")).unwrap(),
        b"original map"
    );
    assert!(!game.path().join("dlc_new.scs").exists());
    assert!(!game.path().join("bin/win_x86").exists());
}

#[test]
fn file_copy_failure_rolls_back_completed_replacements() {
    let game = tempfile::tempdir().unwrap();
    fake_game(game.path());
    let staging = tempfile::tempdir().unwrap();
    let files = staged_files(staging.path());
    let result = install::prepare(ets_launcher::game::Game::Ets2, game.path())
        .unwrap()
        .install_files(staging.path(), &files, &CancellationToken::new(), |event| {
            if let Event::File { done: 2, .. } = event {
                fs::remove_file(staging.path().join("bin/win_x86/steam_api.dll")).unwrap();
            }
        });
    assert!(result.is_err());
    assert_eq!(
        fs::read(game.path().join("dlc_mock.scs")).unwrap(),
        b"original map"
    );
    assert!(!game.path().join("dlc_new.scs").exists());
    assert!(!game.path().join("bin/win_x86").exists());
}

#[test]
fn recovers_a_transaction_interrupted_by_process_exit_and_prevents_concurrent_install() {
    let game = tempfile::tempdir().unwrap();
    fake_game(game.path());
    let installation = install::prepare(ets_launcher::game::Game::Ets2, game.path()).unwrap();
    assert!(install::prepare(ets_launcher::game::Game::Ets2, game.path()).is_err());
    drop(installation);
    let backup = game.path().join(".ets-launcher/backups/interrupted");
    write(backup.join("original/dlc_mock.scs"), b"original map");
    write(
        game.path().join("dlc_mock.scs"),
        b"replaced before power loss",
    );
    write(game.path().join("dlc_new.scs"), b"new before power loss");
    let journal = serde_json::json!({"version":1,"phase":"installing","applied":2,
        "files":[{"path":"dlc_mock.scs","existed":true},{"path":"dlc_new.scs","existed":false}],"created_dirs":[]});
    write(
        backup.join("transaction.json"),
        serde_json::to_vec(&journal).unwrap(),
    );
    let _installation = install::prepare(ets_launcher::game::Game::Ets2, game.path()).unwrap();
    assert_eq!(
        fs::read(game.path().join("dlc_mock.scs")).unwrap(),
        b"original map"
    );
    assert!(!game.path().join("dlc_new.scs").exists());
    let journal: serde_json::Value =
        serde_json::from_slice(&fs::read(backup.join("transaction.json")).unwrap()).unwrap();
    assert_eq!(journal["phase"], "rolled_back");
}

#[test]
fn rejects_unknown_layout_or_game_executable_replacement() {
    let listing =
        "Path = dlc_test.scs\nSize = 20\n\nPath = bin/win_x64/eurotrucks2.exe\nSize = 20\n";
    assert!(install::payload(&archive::parse_listing(listing).unwrap()).is_err());
    assert!(
        install::payload(&archive::parse_listing("Path = unrelated.txt\nSize = 5\n").unwrap())
            .is_err()
    );
}
