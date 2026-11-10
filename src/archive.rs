use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

mod bundled;

// Large map mods are often shipped as folders with hundreds of thousands of files.
const MAX_FILES: usize = 500_000;
const MAX_EXPANDED: u64 = 50 * 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub directory: bool,
}

pub fn helper() -> Result<PathBuf> {
    bundled::helper()
}

/// Apply Windows path rules even when validating on another platform.
pub fn safe_relative(name: &str) -> Result<PathBuf> {
    ensure!(
        !name.is_empty() && name.len() < 1024,
        "Пустой или слишком длинный путь в архиве"
    );
    let normalized = name.replace('\\', "/");
    let mut path = PathBuf::new();
    for part in normalized.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != "..",
            "Недопустимый путь в архиве: {name}"
        );
        ensure!(
            !part.ends_with(['.', ' '])
                && !part
                    .chars()
                    .any(|c| c.is_control() || "<>:\"|?*".contains(c)),
            "Недопустимое имя в архиве: {name}"
        );
        let stem = part.split('.').next().unwrap().to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|n| {
                    matches!(
                        n,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            });
        ensure!(!device, "Зарезервированное имя в архиве: {name}");
        path.push(part);
    }
    Ok(path)
}

pub fn parse_listing(text: &str) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut seen = BTreeMap::new();
    let mut expanded = 0_u64;
    for block in text
        .replace("\r\n", "\n")
        .split("\n\n")
        .filter(|s| !s.trim().is_empty())
    {
        let mut fields = BTreeMap::new();
        for line in block.lines() {
            let (key, value) = line
                .split_once(" = ")
                .context("Не удалось прочитать список файлов архива")?;
            ensure!(
                fields.insert(key, value).is_none(),
                "Неоднозначный список файлов архива"
            );
        }
        let name = fields
            .get("Path")
            .context("В архиве есть запись без имени")?;
        let path = safe_relative(name)?;
        ensure!(
            fields.get("Encrypted") != Some(&"+"),
            "Архив защищён паролем"
        );
        ensure!(
            fields.get("Anti") != Some(&"+"),
            "Архив содержит операции удаления"
        );
        ensure!(
            fields.get("Alternate Stream") != Some(&"+"),
            "Потоки NTFS в архиве не поддерживаются"
        );
        for (key, value) in &fields {
            ensure!(
                !(key.to_ascii_lowercase().contains("link") || key.contains("Reparse"))
                    || value.is_empty()
                    || *value == "-",
                "Ссылки в архиве не поддерживаются: {name}"
            );
        }
        let attributes = fields.get("Attributes").copied().unwrap_or("");
        let mode = fields.get("Mode").copied().unwrap_or("");
        ensure!(
            !attributes.split_whitespace().any(|v| v.starts_with('l')) && !mode.starts_with('l'),
            "Символическая ссылка в архиве: {name}"
        );
        let directory = fields.get("Folder") == Some(&"+")
            || attributes.starts_with('D')
            || mode.starts_with('d');
        let size: u64 = fields
            .get("Size")
            .filter(|v| !v.is_empty())
            .unwrap_or(&"0")
            .parse()?;
        expanded = expanded
            .checked_add(size)
            .context("Некорректный размер архива")?;
        ensure!(
            expanded <= MAX_EXPANDED && entries.len() < MAX_FILES,
            "Архив превышает лимит: 50 ГиБ / 500 000 записей"
        );
        let key = path.to_string_lossy().replace('\\', "/").to_lowercase();
        ensure!(
            seen.insert(key, directory).is_none(),
            "Повторяющийся путь в архиве: {name}"
        );
        entries.push(Entry {
            path,
            size,
            directory,
        });
    }
    ensure!(!entries.is_empty(), "Архив пуст");
    // A file must never also be the parent of another entry.
    for entry in &entries {
        for parent in entry
            .path
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            ensure!(
                seen.get(&parent.to_string_lossy().replace('\\', "/").to_lowercase())
                    != Some(&false),
                "В архиве файл используется как папка"
            );
        }
    }
    Ok(entries)
}

async fn bounded(mut pipe: impl AsyncRead + Unpin, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    (&mut pipe).take(limit + 1).read_to_end(&mut bytes).await?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Слишком большой вывод распаковщика"
    );
    Ok(bytes)
}

async fn invoke(
    tool: &Path,
    args: &[std::ffi::OsString],
    cancel: &CancellationToken,
) -> Result<String> {
    let mut command = Command::new(tool);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = command
        .spawn()
        .context("Не удалось запустить распаковщик 7-Zip")?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let task = async {
        let (status, out, err) = tokio::try_join!(
            async { Ok::<_, anyhow::Error>(child.wait().await?) },
            bounded(stdout, 16 * 1024 * 1024),
            bounded(stderr, 64 * 1024)
        )?;
        ensure!(
            status.success(),
            "Ошибка распаковки ({}): {} {}",
            status,
            String::from_utf8_lossy(&err).trim(),
            String::from_utf8_lossy(&out)
                .chars()
                .take(1500)
                .collect::<String>()
        );
        String::from_utf8(out).context("Некорректная кодировка списка файлов")
    };
    let result = tokio::select! {
        _ = cancel.cancelled() => Err(anyhow::anyhow!("Установка остановлена")),
        result = tokio::time::timeout(Duration::from_secs(3600), task) => result.context("Превышено время распаковки").and_then(|result| result),
    };
    if result.is_err() {
        // Reap the helper before staging cleanup; kill_on_drop alone does not
        // wait for Windows to release its open file handles.
        let _ = child.kill().await;
    }
    result
}

pub async fn inspect(archive: &Path, cancel: &CancellationToken) -> Result<Vec<Entry>> {
    let archive = std::fs::canonicalize(archive).context("Архив не найден")?;
    let listing = invoke(
        &helper()?,
        &[
            "l".into(),
            "-slt".into(),
            "-ba".into(),
            "-sccUTF-8".into(),
            "-p".into(),
            "--".into(),
            archive.into_os_string(),
        ],
        cancel,
    )
    .await?;
    parse_listing(&listing)
}

/// Verify CRC without extracting the mod's own resource archive.
pub async fn test(archive: &Path, cancel: &CancellationToken) -> Result<()> {
    invoke(
        &helper()?,
        &[
            "t".into(),
            "-bso0".into(),
            "-bsp0".into(),
            "-p".into(),
            "--".into(),
            archive.canonicalize()?.into_os_string(),
        ],
        cancel,
    )
    .await?;
    Ok(())
}

pub async fn extract(
    archive: &Path,
    destination: &Path,
    entries: &[Entry],
    cancel: &CancellationToken,
) -> Result<()> {
    ensure!(
        std::fs::read_dir(destination)?.next().is_none(),
        "Папка распаковки должна быть пустой"
    );
    let archive = std::fs::canonicalize(archive)?;
    let destination = std::fs::canonicalize(destination)?;
    let mut output_arg = std::ffi::OsString::from("-o");
    output_arg.push(&destination);
    invoke(
        &helper()?,
        &[
            "x".into(),
            "-y".into(),
            "-p".into(),
            "-sccUTF-8".into(),
            "-bso0".into(),
            "-bsp0".into(),
            output_arg,
            "--".into(),
            archive.into_os_string(),
        ],
        cancel,
    )
    .await?;
    verify_tree(&destination, entries)
}

pub fn ordinary_metadata(path: &Path) -> Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "Ссылки запрещены: {}",
        path.display()
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Reparse point запрещён: {}",
            path.display()
        );
    }
    Ok(metadata)
}

fn verify_tree(root: &Path, entries: &[Entry]) -> Result<()> {
    let mut expected: BTreeMap<PathBuf, u64> = entries
        .iter()
        .filter(|e| !e.directory)
        .map(|e| (e.path.clone(), e.size))
        .collect();
    let mut allowed_dirs = HashSet::new();
    for entry in entries {
        if entry.directory {
            allowed_dirs.insert(entry.path.clone());
        }
        for parent in entry.path.ancestors().skip(1) {
            allowed_dirs.insert(parent.to_path_buf());
        }
    }
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for child in std::fs::read_dir(directory)? {
            let child = child?;
            let path = child.path();
            let relative = path.strip_prefix(root)?;
            let metadata = ordinary_metadata(&path)?;
            if metadata.is_dir() {
                ensure!(
                    allowed_dirs.contains(relative),
                    "В архиве появилась неожиданная папка"
                );
                directories.push(path);
            } else {
                ensure!(metadata.is_file(), "Необычный тип файла в архиве");
                let size = expected
                    .remove(relative)
                    .context("В архиве появился неожиданный файл")?;
                ensure!(
                    metadata.len() == size,
                    "Неверный размер распакованного файла: {}",
                    relative.display()
                );
            }
        }
    }
    ensure!(expected.is_empty(), "Не все файлы архива распакованы");
    Ok(())
}
