use crate::game::{Channel, Game};
use crate::{
    archive::{self, Entry},
    download::{self, SharedTransfer, Status},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

pub struct Installation {
    game: Game,
    root: PathBuf,
    control: PathBuf,
    _lock: File,
}

#[derive(Clone, Debug)]
pub struct PayloadFile {
    pub source: PathBuf,
    pub relative: PathBuf,
    pub size: u64,
}

#[derive(Debug)]
pub struct Report {
    pub game: PathBuf,
    pub backup: PathBuf,
    pub files: usize,
}

pub enum Event<'a> {
    Backup(&'a Path),
    File {
        done: usize,
        total: usize,
        path: &'a Path,
    },
    Rollback,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    version: u32,
    phase: String,
    applied: usize,
    files: Vec<Original>,
    created_dirs: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Original {
    path: String,
    existed: bool,
}

#[derive(Debug)]
struct RecoveryRequired(String);
impl std::fmt::Display for RecoveryRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for RecoveryRequired {}

fn cancelled(cancel: &CancellationToken) -> Result<()> {
    ensure!(!cancel.is_cancelled(), "Установка остановлена");
    Ok(())
}

fn unique_id() -> String {
    format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id()
    )
}

pub fn ensure_game_closed(game: Game) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let system = std::env::var_os("SystemRoot").context("Не найдена папка Windows")?;
        let output =
            std::process::Command::new(PathBuf::from(system).join("System32/tasklist.exe"))
                .args([
                    "/FI",
                    &format!("IMAGENAME eq {}", game.executable()),
                    "/FO",
                    "CSV",
                    "/NH",
                ])
                .creation_flags(0x08000000)
                .output()
                .with_context(|| {
                    format!("Не удалось проверить, запущена ли {}", game.short_name())
                })?;
        ensure!(
            output.status.success(),
            "Не удалось проверить процессы игры"
        );
        ensure!(
            !String::from_utf8_lossy(&output.stdout)
                .to_ascii_lowercase()
                .contains(&format!("\"{}\"", game.executable())),
            "Закройте {} перед установкой DLC",
            game.name()
        );
    }
    Ok(())
}

fn normal_directory(path: &Path) -> Result<()> {
    ensure!(
        archive::ordinary_metadata(path)?.is_dir(),
        "Ожидалась папка: {}",
        path.display()
    );
    Ok(())
}

fn make_directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => normal_directory(path),
        Err(e) => Err(e).with_context(|| format!("Нет доступа к папке {}", path.display())),
    }
}

pub fn prepare(game: Game, directory: &Path) -> Result<Installation> {
    ensure!(
        crate::game::is_installation(game, directory),
        "Не найдена {}. Укажите папку с bin/win_x64/{}.",
        game.short_name(),
        game.executable()
    );
    ensure_game_closed(game)?;
    archive::helper()?;
    let root = fs::canonicalize(directory)?;
    let control = root.join(".ets-launcher");
    make_directory(&control)?;
    let lock_path = control.join("install.lock");
    if lock_path.try_exists()? {
        ensure!(
            archive::ordinary_metadata(&lock_path)?.is_file(),
            "Некорректный файл блокировки"
        );
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .context("Нет прав записи в папку игры")?;
    lock.try_lock()
        .context("В этой папке уже идёт установка DLC")?;
    make_directory(&control.join("backups"))?;
    make_directory(&control.join("work"))?;
    let installation = Installation {
        game,
        root,
        control,
        _lock: lock,
    };
    installation.recover()?;
    Ok(installation)
}

fn is_document(path: &Path) -> bool {
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "txt" | "md" | "pdf" | "png" | "jpg" | "jpeg" | "url" | "html"
    )
}

fn dlc_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    name.starts_with("dlc_") && name.ends_with(".scs")
}

/// Find the game contents even when the archive has one or more enclosing folders.
pub fn payload(entries: &[Entry]) -> Result<Vec<PayloadFile>> {
    let roots: BTreeSet<PathBuf> = entries
        .iter()
        .filter(|e| !e.directory && dlc_name(&e.path))
        .map(|e| e.path.parent().unwrap_or(Path::new("")).to_path_buf())
        .collect();
    ensure!(
        roots.len() == 1,
        "Не удалось определить папку DLC в архиве: ожидалась одна папка с dlc_*.scs"
    );
    let root = roots.first().unwrap();
    let mut files = Vec::new();
    for entry in entries.iter().filter(|e| !e.directory) {
        let Ok(relative) = entry.path.strip_prefix(root) else {
            ensure!(
                is_document(&entry.path),
                "Файл вне папки DLC: {}",
                entry.path.display()
            );
            continue;
        };
        let components: Vec<_> = relative
            .iter()
            .map(|p| p.to_string_lossy().to_ascii_lowercase())
            .collect();
        let bin = components.len() >= 3
            && components[0] == "bin"
            && matches!(components[1].as_str(), "win_x64" | "win_x86");
        let extension = relative
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        if components.len() == 1 && dlc_name(relative)
            || bin && matches!(extension.as_str(), "dll" | "ini" | "json" | "cfg" | "txt")
        {
            ensure!(entry.size > 0, "Пустой файл DLC: {}", relative.display());
            files.push(PayloadFile {
                source: entry.path.clone(),
                relative: relative.to_path_buf(),
                size: entry.size,
            });
        } else {
            ensure!(
                is_document(relative),
                "Неожиданный файл в архиве DLC: {}",
                relative.display()
            );
        }
    }
    ensure!(!files.is_empty(), "В архиве нет файлов DLC");
    Ok(files)
}

fn checked_target(root: &Path, relative: &Path) -> Result<PathBuf> {
    let safe = archive::safe_relative(&relative.to_string_lossy())?;
    let mut target = root.to_path_buf();
    for (index, part) in safe.iter().enumerate() {
        target.push(part);
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                let metadata = archive::ordinary_metadata(&target)?;
                if index + 1 < safe.components().count() {
                    ensure!(metadata.is_dir(), "Файл вместо папки: {}", target.display());
                } else {
                    ensure!(
                        metadata.is_file(),
                        "Папка вместо файла: {}",
                        target.display()
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(target)
}

pub fn ensure_space(path: &Path, required: u64) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(
                path: *const u16,
                available: *mut u64,
                total: *mut u64,
                free: *mut u64,
            ) -> i32;
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0;
        // All pointers are valid for the duration of the synchronous OS call.
        let success = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        ensure!(
            success != 0,
            "Не удалось проверить свободное место: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            available >= required,
            "Недостаточно места на диске: нужно {}, доступно {}",
            crate::format_bytes(required),
            crate::format_bytes(available)
        );
    }
    #[cfg(not(windows))]
    let _ = (path, required);
    Ok(())
}

fn copy_sync(source: &Path, target: &Path, cancel: &CancellationToken) -> Result<()> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)?;
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        cancelled(cancel)?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
    }
    output.sync_all()?;
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> Result<bool> {
    if !b.try_exists()? || fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(false);
    }
    let mut a = File::open(a)?;
    let mut b = File::open(b)?;
    let mut left = [0; 64 * 1024];
    let mut right = [0; 64 * 1024];
    loop {
        let count = a.read(&mut left)?;
        if count == 0 {
            return Ok(true);
        }
        b.read_exact(&mut right[..count])?;
        if left[..count] != right[..count] {
            return Ok(false);
        }
    }
}

fn write_journal(backup: &Path, journal: &Journal) -> Result<()> {
    let temporary = backup.join("transaction.part");
    let mut file = File::create(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(journal)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, backup.join("transaction.json"))?;
    Ok(())
}

impl Installation {
    fn recover(&self) -> Result<()> {
        for child in fs::read_dir(self.control.join("backups"))? {
            let backup = child?.path();
            normal_directory(&backup)?;
            let path = backup.join("transaction.json");
            if !path.try_exists()? {
                continue;
            }
            ensure!(
                archive::ordinary_metadata(&path)?.is_file()
                    && fs::metadata(&path)?.len() < 8 * 1024 * 1024,
                "Некорректный журнал установки"
            );
            let mut journal: Journal = serde_json::from_slice(&fs::read(path)?)?;
            ensure!(
                journal.version == 1 && journal.applied <= journal.files.len(),
                "Неизвестный формат журнала установки"
            );
            if journal.phase == "installing" {
                self.rollback(&backup, &mut journal).map_err(|e| {
                    RecoveryRequired(format!(
                        "Не удалось восстановить прерванную установку: {e:#}. Резервная копия: {}",
                        backup.display()
                    ))
                })?;
            }
        }
        Ok(())
    }

    fn required_space(&self, entries: &[Entry], files: &[PayloadFile]) -> Result<u64> {
        let mut size: u64 = entries.iter().map(|e| e.size).sum();
        size += files.iter().map(|f| f.size).max().unwrap_or(0) + 128 * 1024 * 1024;
        for file in files {
            let target = checked_target(&self.root, &file.relative)?;
            if target.try_exists()? {
                let metadata = fs::metadata(&target)?;
                ensure!(
                    !metadata.permissions().readonly(),
                    "Файл защищён от записи: {}",
                    target.display()
                );
                size += metadata.len();
            }
        }
        Ok(size)
    }

    fn rollback(&self, backup: &Path, journal: &mut Journal) -> Result<()> {
        let original = backup.join("original");
        normal_directory(&original)?;
        let cancel = CancellationToken::new();
        for item in journal.files.iter().take(journal.applied).rev() {
            let relative = archive::safe_relative(&item.path)?;
            let target = checked_target(&self.root, &relative)?;
            if item.existed {
                let source = checked_target(&original, &relative)?;
                ensure!(
                    source.is_file(),
                    "Отсутствует резервная копия {}",
                    source.display()
                );
                if same_file(&source, &target)? {
                    continue;
                }
                let temporary = backup.join("restore.part");
                if temporary.try_exists()? {
                    ensure!(
                        archive::ordinary_metadata(&temporary)?.is_file(),
                        "Некорректный временный файл восстановления"
                    );
                    fs::remove_file(&temporary)?;
                }
                copy_sync(&source, &temporary, &cancel)?;
                fs::rename(temporary, &target)?;
            } else if target.try_exists()? {
                fs::remove_file(target)?;
            }
        }
        for dir in journal.created_dirs.iter().rev() {
            let path = self.root.join(archive::safe_relative(dir)?);
            if path.try_exists()? {
                normal_directory(&path)?;
                match fs::remove_dir(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        journal.phase = "rolled_back".into();
        write_journal(backup, journal)?;
        Ok(())
    }

    pub fn install_files(
        &self,
        staging: &Path,
        files: &[PayloadFile],
        cancel: &CancellationToken,
        mut progress: impl FnMut(Event<'_>),
    ) -> Result<Report> {
        cancelled(cancel)?;
        ensure_game_closed(self.game)?;
        let backup = self.control.join("backups").join(unique_id());
        fs::create_dir(&backup)?;
        fs::create_dir(backup.join("original"))?;
        let mut journal = Journal {
            version: 1,
            phase: "installing".into(),
            applied: 0,
            files: Vec::new(),
            created_dirs: Vec::new(),
        };
        let mut directories = BTreeSet::new();
        progress(Event::Backup(&backup));
        for file in files {
            cancelled(cancel)?;
            let source = checked_target(staging, &file.source)?;
            ensure!(
                archive::ordinary_metadata(&source)?.len() == file.size,
                "Повреждён распакованный файл"
            );
            let target = checked_target(&self.root, &file.relative)?;
            let existed = target.try_exists()?;
            if existed {
                let original = backup.join("original").join(&file.relative);
                fs::create_dir_all(original.parent().unwrap())?;
                copy_sync(&target, &original, cancel)?;
            }
            for parent in file
                .relative
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
            {
                if !self.root.join(parent).try_exists()? {
                    directories.insert(parent.to_path_buf());
                }
            }
            journal.files.push(Original {
                path: file.relative.to_string_lossy().into_owned(),
                existed,
            });
        }
        let mut directories: Vec<_> = directories.into_iter().collect();
        directories.sort_by_key(|p| p.components().count());
        journal.created_dirs = directories
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        write_journal(&backup, &journal)?;
        let result: Result<()> = (|| {
            for directory in &directories {
                cancelled(cancel)?;
                make_directory(&self.root.join(directory))?;
            }
            for (index, file) in files.iter().enumerate() {
                cancelled(cancel)?;
                let target = checked_target(&self.root, &file.relative)?;
                ensure!(
                    target.try_exists()? == journal.files[index].existed,
                    "Содержимое игры изменилось во время установки"
                );
                journal.applied = index + 1;
                write_journal(&backup, &journal)?;
                let temporary = backup.join("install.part");
                copy_sync(&staging.join(&file.source), &temporary, cancel)?;
                cancelled(cancel)?;
                fs::rename(temporary, target)
                    .with_context(|| format!("Не удалось заменить {}", file.relative.display()))?;
                progress(Event::File {
                    done: index + 1,
                    total: files.len(),
                    path: &file.relative,
                });
            }
            cancelled(cancel)?;
            journal.phase = "committed".into();
            write_journal(&backup, &journal)?;
            Ok(())
        })();
        if let Err(error) = result {
            progress(Event::Rollback);
            self.rollback(&backup, &mut journal).map_err(|rollback| {
                RecoveryRequired(format!(
                    "{error:#}. Не удалось завершить откат: {rollback:#}. Резервная копия: {}",
                    backup.display()
                ))
            })?;
            return Err(error.context("Установка прервана, исходные файлы восстановлены"));
        }
        Ok(Report {
            game: self.root.clone(),
            backup,
            files: files.len(),
        })
    }
}

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub async fn install_archive(
    installation: Installation,
    archive: PathBuf,
    state: SharedTransfer,
    cancel: CancellationToken,
) -> Result<Report> {
    cancelled(&cancel)?;
    download::set_state(&state, |s| {
        s.status = Status::Extracting;
        s.message = "Проверка структуры архива…".into();
        s.downloaded = 0;
        s.total = None;
        s.speed = 0;
        s.peers = 0;
    });
    let entries = archive::inspect(&archive, &cancel).await?;
    let files = payload(&entries)?;
    ensure_space(
        &installation.root,
        installation.required_space(&entries, &files)?,
    )?;
    let staging = Staging(installation.control.join("work").join(unique_id()));
    fs::create_dir(&staging.0)?;
    download::set_state(&state, |s| {
        s.message = "Распаковка архива и проверка CRC…".into()
    });
    archive::extract(&archive, &staging.0, &entries, &cancel).await?;
    tokio::task::spawn_blocking(move || {
        // Keep the staging guard and the exclusive installation lock alive until commit/rollback.
        installation.install_files(&staging.0, &files, &cancel, |event| {
            download::set_state(&state, |s| match event {
                Event::Backup(path) => {
                    s.status = Status::Installing;
                    s.message = "Создание резервной копии…".into();
                    s.downloaded = 0;
                    s.total = Some(files.len() as u64);
                    s.speed = 0;
                    s.peers = 0;
                    s.backup_path = Some(path.into());
                }
                Event::File { done, total, path } => {
                    s.downloaded = done as u64;
                    s.message = format!("Установка {done}/{total}: {}", path.display());
                }
                Event::Rollback => {
                    s.status = Status::RollingBack;
                    s.message = "Восстановление исходных файлов…".into();
                }
            })
        })
    })
    .await
    .context("Не удалось завершить установку")?
}

pub async fn run(
    game: Game,
    channel: Channel,
    page_url: String,
    output: PathBuf,
    directory: PathBuf,
    state: SharedTransfer,
    cancel: CancellationToken,
) {
    let result: Result<Report> = async {
        download::set_state(&state, |s| {
            s.message = "Проверка игры и предыдущей установки…".into()
        });
        let installation = tokio::task::spawn_blocking(move || prepare(game, &directory)).await??;
        cancelled(&cancel)?;
        let archive =
            download::download_from_site(game, channel, &page_url, false, &output, &state, &cancel)
                .await?;
        install_archive(installation, archive, state.clone(), cancel.clone()).await
    }
    .await;
    match result {
        Ok(report) => download::set_state(&state, |s| {
            s.status = Status::Complete;
            s.message = format!(
                "DLC установлены. Файлов: {}. Резервная копия: {}",
                report.files,
                report.backup.display()
            );
            s.saved_to = Some(report.game);
            s.backup_path = Some(report.backup);
            s.speed = 0;
            s.peers = 0;
        }),
        Err(error) => download::set_state(&state, |s| {
            let safely_cancelled =
                cancel.is_cancelled() && !error.chain().any(|e| e.is::<RecoveryRequired>());
            s.status = if safely_cancelled {
                Status::Cancelled
            } else {
                Status::Error
            };
            s.message = format!("{error:#}");
            s.speed = 0;
            s.peers = 0;
        }),
    }
}
