use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use librqbit::{AddTorrent, AddTorrentOptions, Session, SessionOptions};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{fs, io::AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    Connecting,
    Downloading,
    Extracting,
    Installing,
    RollingBack,
    Complete,
    Cancelled,
    Error,
}

#[derive(Clone, Debug)]
pub struct Transfer {
    pub status: Status,
    pub name: String,
    pub message: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed: u64,
    pub peers: usize,
    pub saved_to: Option<PathBuf>,
    pub torrent_path: Option<PathBuf>,
    pub backup_path: Option<PathBuf>,
}

impl Default for Transfer {
    fn default() -> Self {
        Self {
            status: Status::Ready,
            name: String::new(),
            message: "Готов к установке".into(),
            downloaded: 0,
            total: None,
            speed: 0,
            peers: 0,
            saved_to: None,
            torrent_path: None,
            backup_path: None,
        }
    }
}

impl Transfer {
    pub fn active(&self) -> bool {
        matches!(
            self.status,
            Status::Connecting
                | Status::Downloading
                | Status::Extracting
                | Status::Installing
                | Status::RollingBack
        )
    }
    pub fn progress(&self) -> f32 {
        self.total
            .filter(|total| *total > 0)
            .map(|total| (self.downloaded as f64 / total as f64).clamp(0.0, 1.0) as f32)
            .unwrap_or(0.0)
    }
}

pub type SharedTransfer = Arc<Mutex<Transfer>>;

pub fn set_state(state: &SharedTransfer, change: impl FnOnce(&mut Transfer)) {
    change(
        &mut state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
}

pub fn snapshot(state: &SharedTransfer) -> Transfer {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[derive(Clone)]
pub enum Source {
    TorrentBytes(Vec<u8>),
    Magnet(String),
    TestArchive,
    Playground { torrent_only: bool },
}

pub async fn run(
    source: Source,
    output: PathBuf,
    state: SharedTransfer,
    cancel: CancellationToken,
) {
    let torrent_only = matches!(&source, Source::Playground { torrent_only: true });
    let result = match source {
        Source::Playground { torrent_only } => {
            download_from_site(torrent_only, &output, &state, &cancel).await
        }
        Source::TestArchive => {
            download_http(
                crate::TEST_URL,
                &output,
                crate::TEST_FILENAME,
                &state,
                &cancel,
            )
            .await
        }
        source => download_torrent(source, output, &state, &cancel).await,
    };
    match result {
        Ok(path) => set_state(&state, |s| {
            s.status = Status::Complete;
            s.message = if torrent_only {
                "Торрент-файл скачан"
            } else {
                "Архив скачан"
            }
            .into();
            s.total = Some(s.downloaded);
            s.speed = 0;
            s.peers = 0;
            s.saved_to = Some(path);
        }),
        Err(_) if cancel.is_cancelled() => set_state(&state, |s| {
            s.status = Status::Cancelled;
            s.message = "Загрузка остановлена".into();
            s.speed = 0;
            s.peers = 0;
        }),
        Err(error) => set_state(&state, |s| {
            s.status = Status::Error;
            s.message = format!("{error:#}");
            s.speed = 0;
            s.peers = 0;
        }),
    }
}

pub async fn download_from_site(
    torrent_only: bool,
    output: &Path,
    state: &SharedTransfer,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    set_state(state, |s| s.message = "Чтение страницы PlayGround…".into());
    let fetched = crate::playground::fetch_torrent(
        &crate::playground::client()?,
        crate::SOURCE_PAGE,
        cancel,
        |remaining| {
            set_state(state, |s| {
                s.message = if remaining > 0 {
                    format!("PlayGround готовит торрент: осталось {remaining} с")
                } else {
                    "Получение торрент-файла…".into()
                }
            });
        },
    )
    .await?;
    ensure_not_cancelled(cancel)?;
    let torrent_name = format!("{}.torrent", fetched.archive_name);
    let path = save_bytes(output, &torrent_name, &fetched.bytes, cancel).await?;
    set_state(state, |s| s.torrent_path = Some(path.clone()));
    if torrent_only {
        set_state(state, |s| {
            s.name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            s.downloaded = fetched.bytes.len() as u64;
            s.total = Some(fetched.bytes.len() as u64);
        });
        return Ok(path);
    }
    let archive = output.join(&fetched.archive_name);
    let allocated = fs::metadata(&archive).await.map(|m| m.len()).unwrap_or(0);
    crate::install::ensure_space(
        output,
        fetched.archive_size.saturating_sub(allocated) + 32 * 1024 * 1024,
    )?;
    set_state(state, |s| {
        s.name = fetched.archive_name;
        s.total = Some(fetched.archive_size);
        s.message = "Торрент получен. Подключение к пирам…".into();
    });
    download_torrent(
        Source::TorrentBytes(fetched.bytes),
        output.into(),
        state,
        cancel,
    )
    .await?;
    anyhow::ensure!(
        fs::metadata(&archive).await?.len() == fetched.archive_size,
        "Размер готового архива не совпадает с торрентом"
    );
    Ok(archive)
}

fn ensure_not_cancelled(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        bail!("Загрузка остановлена");
    }
    Ok(())
}

pub async fn save_bytes(
    output: &Path,
    filename: &str,
    bytes: &[u8],
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    anyhow::ensure!(
        Path::new(filename)
            .file_name()
            .and_then(|name| name.to_str())
            == Some(filename),
        "Некорректное имя файла"
    );
    fs::create_dir_all(output).await?;
    let original = Path::new(filename);
    let mut index = 0;
    loop {
        ensure_not_cancelled(cancel)?;
        let name = if index == 0 {
            filename.to_owned()
        } else {
            format!(
                "{} ({index}).{}",
                original.file_stem().unwrap().to_string_lossy(),
                original.extension().unwrap_or_default().to_string_lossy()
            )
        };
        let destination = output.join(&name);
        let temporary = output.join(format!("{name}.part"));
        index += 1;
        if destination.exists() {
            continue;
        }
        let mut file = match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("Не удалось создать торрент-файл"),
        };
        let result: Result<()> = async {
            file.write_all(bytes).await?;
            file.sync_all().await?;
            ensure_not_cancelled(cancel)
        }
        .await;
        drop(file);
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary).await;
            return Err(error);
        }
        if let Err(error) = publish_file(&temporary, &destination).await {
            let _ = fs::remove_file(&temporary).await;
            return Err(error).context("Не удалось сохранить торрент-файл");
        }
        return Ok(destination);
    }
}

/// Streams into a temporary file; an interrupted response never becomes a finished archive.
pub async fn download_http(
    url: &str,
    output: &Path,
    filename: &str,
    state: &SharedTransfer,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    if Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        != Some(filename)
    {
        bail!("Некорректное имя архива");
    }
    fs::create_dir_all(output)
        .await
        .context("Не удалось создать папку загрузки")?;
    // Reserve a unique name so another download or an existing archive is never overwritten.
    let mut index = 0;
    let (destination, temporary, mut file) = loop {
        let name = if index == 0 {
            filename.to_owned()
        } else {
            let path = Path::new(filename);
            format!(
                "{} ({index}).{}",
                path.file_stem().unwrap().to_string_lossy(),
                path.extension().unwrap_or_default().to_string_lossy()
            )
        };
        let destination = output.join(&name);
        let temporary = output.join(format!("{name}.part"));
        if !destination.exists() {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .await
            {
                Ok(file) => break (destination, temporary, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error).context("Не удалось создать файл"),
            }
        }
        index += 1;
    };
    let result: Result<()> = async {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("ETS-Launcher/0.1")
            .build()?;
        let response = tokio::select! {
            _ = cancel.cancelled() => bail!("Загрузка остановлена"),
            response = client.get(url).send() => response.context("Не удалось подключиться к серверу")?.error_for_status().context("Сервер вернул ошибку")?,
        };
        let total = response.content_length();
        set_state(state, |s| { s.status = Status::Downloading; s.total = total; s.message = "Скачивание по HTTPS".into(); });
        let mut stream = response.bytes_stream();
        let started = Instant::now();
        let mut downloaded = 0;
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => bail!("Загрузка остановлена"),
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else { break; };
            let chunk = chunk.context("Соединение прервано")?;
            file.write_all(&chunk).await.context("Не удалось записать архив")?;
            downloaded += chunk.len() as u64;
            set_state(state, |s| {
                s.downloaded = downloaded;
                s.speed = (downloaded as f64 / started.elapsed().as_secs_f64().max(0.01)) as u64;
            });
        }
        if total.is_some_and(|total| total != downloaded) { bail!("Получен неполный архив"); }
        file.flush().await?;
        file.sync_all().await?;
        Ok(())
    }.await;
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary).await;
        return Err(error);
    }
    if cancel.is_cancelled() {
        let _ = fs::remove_file(&temporary).await;
        bail!("Загрузка остановлена");
    }
    if let Err(error) = publish_file(&temporary, &destination).await {
        let _ = fs::remove_file(&temporary).await;
        return Err(error).context("Не удалось сохранить готовый архив");
    }
    Ok(destination)
}

async fn publish_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    // Windows rename refuses to replace an existing file and also works on exFAT.
    #[cfg(windows)]
    {
        fs::rename(temporary, destination).await
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(temporary, destination).await?;
        fs::remove_file(temporary).await
    }
}

async fn download_torrent(
    source: Source,
    output: PathBuf,
    state: &SharedTransfer,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    fs::create_dir_all(&output)
        .await
        .context("Не удалось создать папку загрузки")?;
    let add = match source {
        Source::TorrentBytes(bytes) => AddTorrent::from_bytes(bytes),
        Source::Magnet(magnet) => {
            if !magnet.trim().starts_with("magnet:?") {
                bail!("Вставьте magnet-ссылку, начинающуюся с magnet:?");
            }
            AddTorrent::from_url(magnet)
        }
        Source::TestArchive | Source::Playground { .. } => unreachable!(),
    };
    let opts = SessionOptions {
        persistence: None,
        listen_port_range: None,
        disable_dht_persistence: true,
        cancellation_token: Some(cancel.child_token()),
        ..Default::default()
    };
    let session = Session::new_with_opts(output.clone(), opts)
        .await
        .context("Не удалось запустить торрент-клиент")?;
    let result = transfer_in_session(
        session.clone(),
        add,
        state,
        cancel,
        Some(AddTorrentOptions {
            overwrite: true,
            ..Default::default()
        }),
    )
    .await;
    session.stop().await;
    result?;
    Ok(output)
}

/// Public to allow testing the actual peer-to-peer path against a local seed.
pub async fn transfer_in_session(
    session: Arc<Session>,
    add: AddTorrent<'static>,
    state: &SharedTransfer,
    cancel: &CancellationToken,
    options: Option<AddTorrentOptions>,
) -> Result<()> {
    let response = tokio::select! {
        _ = cancel.cancelled() => bail!("Загрузка остановлена"),
        result = session.add_torrent(add, options) => result.context("Не удалось открыть торрент")?,
    };
    let handle = response.into_handle().context("Торрент не добавлен")?;
    let mut last_bytes = 0_u64;
    let mut last_time = Instant::now();
    let mut last_progress = Instant::now();
    loop {
        let stats = handle.stats();
        if let Some(error) = stats.error {
            bail!("Ошибка торрента: {error}");
        }
        let elapsed = last_time.elapsed().as_secs_f64().max(0.01);
        let speed = (stats.progress_bytes.saturating_sub(last_bytes) as f64 / elapsed) as u64;
        if stats.progress_bytes > last_bytes {
            last_progress = Instant::now();
        }
        set_state(state, |s| {
            if let Some(name) = handle.name() {
                s.name = name;
            }
            s.status = Status::Downloading;
            s.total = Some(stats.total_bytes);
            s.downloaded = stats.progress_bytes;
            s.speed = speed;
            s.peers = stats
                .live
                .as_ref()
                .map(|live| live.snapshot.peer_stats.live)
                .unwrap_or(0);
            s.message = if last_progress.elapsed() > Duration::from_secs(15) {
                "Ожидание пиров — скорость зависит от раздачи".into()
            } else {
                "Скачивание через BitTorrent".into()
            };
        });
        if stats.finished {
            break;
        }
        last_bytes = stats.progress_bytes;
        last_time = Instant::now();
        tokio::select! {
            _ = cancel.cancelled() => bail!("Загрузка остановлена"),
            _ = tokio::time::sleep(Duration::from_millis(500)) => {},
        }
    }
    session
        .pause(&handle)
        .await
        .context("Не удалось остановить раздачу")?;
    // Pausing retains the storage handles. Remove only the session entry, keeping
    // the downloaded files, so Windows lets the external unpacker open the archive.
    session
        .delete(librqbit::api::TorrentIdOrHash::Id(handle.id()), false)
        .await
        .context("Не удалось закрыть скачанный архив")?;
    Ok(())
}
