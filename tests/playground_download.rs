#[path = "fixtures/ets2.rs"]
mod ets2_fixture;
use base64::{Engine, engine::general_purpose::STANDARD};
use ets_launcher::{
    download::save_bytes,
    playground::{client, fetch_torrent, find_file, validate_torrent},
};
use ets2_fixture::{ARCHIVE_NAME, ARCHIVE_SIZE, BUNDLED_TORRENT};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

fn page() -> String {
    format!(
        r#"<main id="post-item" data-post-id="91" data-download-allowed="1"><div class="article-content">
      <pg-file data-is-subfile="1" data-can-torrent-download="1" data-file-id="1" data-name="linux.rar"></pg-file>
      <pg-file data-can-torrent-download="1" data-file-id="777" data-name="{ARCHIVE_NAME}"></pg-file>
      <pg-file data-is-subfile="1" data-can-torrent-download="0" data-file-id="2" data-name="mac.rar"></pg-file>
    </div></main><div class="comments"><pg-file data-can-torrent-download="1" data-file-id="999" data-name="wrong.rar"></pg-file></div>"#
    )
}

#[test]
fn extracts_current_file_id_from_the_article_and_ignores_other_downloads() {
    let file = find_file(&page()).unwrap();
    assert_eq!(file.post_id, 91);
    assert_eq!(file.file_id, 777);
    assert_eq!(file.name, ARCHIVE_NAME);
}

#[test]
fn rejects_missing_or_disabled_download_blocks() {
    assert!(find_file("<html>Please sign in</html>").is_err());
    assert!(
        find_file(&page().replace("data-download-allowed=\"1\"", "data-download-allowed=\"0\""))
            .is_err()
    );
}

#[test]
fn rejects_ambiguous_files_and_unsafe_names() {
    let ambiguous = page().replace("</div></main>", r#"<pg-file data-can-torrent-download="1" data-file-id="778" data-name="another.rar"></pg-file></div></main>"#);
    assert!(find_file(&ambiguous).is_err());
    assert!(find_file(&page().replace(ARCHIVE_NAME, "../archive.rar")).is_err());
}

#[test]
fn verifies_torrent_data_instead_of_saving_an_html_error_as_a_torrent() {
    assert!(validate_torrent(b"<html>Access denied</html>".to_vec(), ARCHIVE_NAME).is_err());
    assert!(validate_torrent(BUNDLED_TORRENT.to_vec(), "different.rar").is_err());
    let result = validate_torrent(BUNDLED_TORRENT.to_vec(), ARCHIVE_NAME).unwrap();
    assert_eq!(result.archive_size, ARCHIVE_SIZE);
}

async fn mock_site(wait: u64, cancel_after_wait: bool) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/post", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut waiting_since = None;
        for step in 0..if cancel_after_wait { 2 } else { 3 } {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 8192];
            let length = stream.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..length]);
            let body = if step == 0 {
                assert!(request.starts_with("GET /post "));
                page()
            } else {
                assert!(request.starts_with("GET /api/file.download?"));
                assert!(request.contains("file_id=777"));
                assert!(request.contains("post_id=91"));
                assert!(request.contains("file_name=ETS2_dlc_maps_1.61.1.9s.rar"));
                if step == 1 {
                    waiting_since = Some(Instant::now());
                    serde_json::json!({"wait_time":wait,"lock_hash":"test-lock","torrent_data":null}).to_string()
                } else {
                    assert!(waiting_since.unwrap().elapsed() >= Duration::from_millis(950));
                    assert!(request.contains("lock_hash=test-lock"));
                    assert!(request.contains("log=0"));
                    serde_json::json!({"wait_time":null,"torrent_data":STANDARD.encode(BUNDLED_TORRENT)}).to_string()
                }
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (url, task)
}

#[tokio::test]
async fn downloads_the_torrent_using_server_wait_and_lock_hash() {
    let (url, server) = mock_site(1, false).await;
    let ticks = Arc::new(Mutex::new(Vec::new()));
    let observed = ticks.clone();
    let result = fetch_torrent(
        &client().unwrap(),
        &url,
        &CancellationToken::new(),
        move |seconds| observed.lock().unwrap().push(seconds),
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert_eq!(result.bytes, BUNDLED_TORRENT);
    assert_eq!(*ticks.lock().unwrap(), vec![1, 0]);
}

#[tokio::test]
async fn cancels_while_the_website_is_preparing_the_download() {
    let (url, server) = mock_site(30, true).await;
    let cancel = CancellationToken::new();
    let stopper = cancel.clone();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        fetch_torrent(&client().unwrap(), &url, &cancel, move |_| stopper.cancel()),
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert!(result.is_err());
    assert!(cancel.is_cancelled());
}

#[tokio::test]
async fn saves_the_validated_torrent_without_overwriting_a_previous_copy() {
    let output = tempfile::tempdir().unwrap();
    let name = format!("{ARCHIVE_NAME}.torrent");
    std::fs::write(output.path().join(&name), b"existing file").unwrap();
    let path = save_bytes(
        output.path(),
        &name,
        BUNDLED_TORRENT,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), BUNDLED_TORRENT);
    assert_eq!(
        std::fs::read(output.path().join(name)).unwrap(),
        b"existing file"
    );
    assert_eq!(std::fs::read_dir(output.path()).unwrap().count(), 2);
}
