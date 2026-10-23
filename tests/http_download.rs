use ets_launcher::download::{SharedTransfer, Transfer, download_http, snapshot};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

const ARCHIVE: &[u8] = include_bytes!("../assets/test-archive.zip");
fn state() -> SharedTransfer {
    Arc::new(Mutex::new(Transfer::default()))
}

async fn server(status: u16, body: &'static [u8], extra_length: usize, stall: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 8192];
        let _ = stream.read(&mut request).await;
        let headers = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len() + extra_length
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        if stall {
            stream.write_all(&body[..128]).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        } else {
            stream.write_all(body).await.unwrap();
        }
    });
    format!("http://{address}/test.zip")
}

#[tokio::test]
async fn downloads_the_exact_archive_without_overwriting_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(dir.path().join("test.zip"), b"keep this")
        .await
        .unwrap();
    let state = state();
    let path = download_http(
        &server(200, ARCHIVE, 0, false).await,
        dir.path(),
        "test.zip",
        &state,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(tokio::fs::read(&path).await.unwrap(), ARCHIVE);
    assert_eq!(
        tokio::fs::read(dir.path().join("test.zip")).await.unwrap(),
        b"keep this"
    );
    assert_eq!(path.file_name().unwrap(), "test (1).zip");
    assert_eq!(snapshot(&state).downloaded, ARCHIVE.len() as u64);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[tokio::test]
async fn rejects_http_errors_and_removes_partial_files() {
    let dir = tempfile::tempdir().unwrap();
    let error = download_http(
        &server(404, b"missing", 0, false).await,
        dir.path(),
        "test.zip",
        &state(),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("404"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn truncated_response_never_looks_like_a_finished_archive() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        download_http(
            &server(200, ARCHIVE, 100, false).await,
            dir.path(),
            "test.zip",
            &state(),
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn cancellation_stops_a_stalled_download_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let state = state();
    let cancel = CancellationToken::new();
    let watcher = state.clone();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        for _ in 0..100 {
            if snapshot(&watcher).downloaded > 0 {
                stopper.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stopper.cancel();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        download_http(
            &server(200, ARCHIVE, 0, true).await,
            dir.path(),
            "test.zip",
            &state,
            &cancel,
        ),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    assert!(cancel.is_cancelled());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
