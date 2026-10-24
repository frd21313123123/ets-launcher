use anyhow::{Context, Result, ensure};
use directories::ProjectDirs;
use std::{
    fs::{self, File, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};

// Keep the official helper and its licenses unmodified inside the launcher.
// It runs as a separate process after extraction; no installer or network is needed.
const FILES: &[(&str, &[u8])] = &[
    ("7z.exe", include_bytes!("../../assets/7zip/7z.exe")),
    ("7z.dll", include_bytes!("../../assets/7zip/7z.dll")),
    (
        "License.txt",
        include_bytes!("../../assets/7zip/License.txt"),
    ),
    (
        "LGPL-2.1.txt",
        include_bytes!("../../assets/7zip/LGPL-2.1.txt"),
    ),
    ("ORIGIN.md", include_bytes!("../../assets/7zip/ORIGIN.md")),
    ("Launcher-LICENSE.txt", include_bytes!("../../LICENSE")),
];

fn bundle_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        // The hash only names the versioned cache. File integrity is checked
        // against the complete embedded bytes before returning the helper.
        let mut hash = DefaultHasher::new();
        FILES.hash(&mut hash);
        format!("{:016x}", hash.finish())
    })
}

pub(super) fn helper() -> Result<PathBuf> {
    let cache = match std::env::var_os("ETS_LAUNCHER_TOOL_CACHE") {
        Some(path) => PathBuf::from(path),
        None => ProjectDirs::from("", "ETS Launcher", "ETS Launcher")
            .context("Не найдена папка кэша распаковщика")?
            .cache_dir()
            .join("tools"),
    };
    ensure!(
        cache.is_absolute(),
        "Папка кэша распаковщика должна иметь абсолютный путь"
    );
    prepare(&cache).context("Не удалось подготовить встроенный распаковщик 7-Zip")
}

fn prepare(cache: &Path) -> Result<PathBuf> {
    let directory = cache.join(format!("7zip-{}", bundle_id()));
    fs::create_dir_all(&directory)?;
    ensure!(
        super::ordinary_metadata(&directory)?.is_dir(),
        "Некорректная папка распаковщика"
    );
    // An OS lock serializes first use and repair across threads and processes.
    // Closing the handle releases it even after a failed extraction.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(".prepare.lock"))?;
    lock.lock()?;
    for (name, bytes) in FILES {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let metadata = super::ordinary_metadata(&path)?;
                ensure!(metadata.is_file(), "Некорректный файл распаковщика: {name}");
                if metadata.len() == bytes.len() as u64 && fs::read(&path)? == *bytes {
                    continue;
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        // Publish each complete file atomically. Interrupted writes are repaired
        // on the next call; a different bundled version gets its own directory.
        let temporary = directory.join(format!("{name}.tmp"));
        if temporary.exists() {
            ensure!(
                super::ordinary_metadata(&temporary)?.is_file(),
                "Некорректный временный файл распаковщика"
            );
        }
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
    }
    Ok(directory.join("7z.exe"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_missing_and_corrupt_cached_files_without_rewriting_valid_files() {
        let cache = tempfile::tempdir().unwrap();
        let helper = prepare(cache.path()).unwrap();
        let directory = helper.parent().unwrap();
        let modified = fs::metadata(&helper).unwrap().modified().unwrap();
        fs::write(directory.join("7z.dll"), b"incomplete DLL").unwrap();
        fs::remove_file(directory.join("License.txt")).unwrap();
        fs::write(directory.join("7z.dll.tmp"), b"interrupted write").unwrap();

        assert_eq!(prepare(cache.path()).unwrap(), helper);
        for (name, bytes) in FILES {
            assert_eq!(fs::read(directory.join(name)).unwrap(), *bytes);
        }
        assert_eq!(fs::metadata(&helper).unwrap().modified().unwrap(), modified);
        assert!(!directory.join("7z.dll.tmp").exists());
    }

    #[test]
    fn concurrent_first_use_publishes_one_complete_bundle() {
        let cache = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        prepare(cache.path()).unwrap()
                    })
                })
                .collect();
            let expected = cache
                .path()
                .join(format!("7zip-{}", bundle_id()))
                .join("7z.exe");
            for job in jobs {
                assert_eq!(job.join().unwrap(), expected);
            }
            for (name, bytes) in FILES {
                assert_eq!(
                    fs::read(expected.parent().unwrap().join(name)).unwrap(),
                    *bytes
                );
            }
        });
    }
}
