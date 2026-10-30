use super::{Catalog, MapEntry, ModSet, Plan, component, profile, write_json};
use crate::{
    archive,
    download::{self, SharedTransfer, Status},
    game::Game,
    install,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct Library {
    pub root: PathBuf,
}

#[derive(Debug)]
pub struct MissingPackage {
    pub pattern: String,
}
impl std::fmt::Display for MissingPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Не хватает пакета {}. Полученные файлы сохранены; добавьте остальные части или сгенерированный DEF.",
            self.pattern
        )
    }
}
impl std::error::Error for MissingPackage {}

pub fn missing_supplement(entry: &MapEntry, error: &anyhow::Error) -> bool {
    let Some(missing) = error.downcast_ref::<MissingPackage>() else {
        return false;
    };
    let Ok(pattern) = regex::Regex::new(&missing.pattern) else {
        return false;
    };
    entry
        .source
        .supplements
        .iter()
        .any(|s| pattern.is_match(&s.filename.replace("{version}", &entry.release.version)))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredFile {
    pub name: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub directory: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageRecord {
    pub id: String,
    pub version: String,
    pub files: Vec<StoredFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActiveState {
    pub set: ModSet,
    pub module: String,
    pub profile_name: String,
    pub packages: Vec<PackageRecord>,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    had_mod: bool,
    had_profile: bool,
    had_state: bool,
    profile: String,
    committed: bool,
}

pub fn digest_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 128 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn normal_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir(path)?;
    }
    ensure!(
        archive::ordinary_metadata(path)?.is_dir(),
        "Ожидалась папка: {}",
        path.display()
    );
    Ok(())
}

fn digest_directory(root: &Path) -> Result<(String, u64)> {
    let mut paths = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for item in fs::read_dir(directory)? {
            let path = item?.path();
            let meta = archive::ordinary_metadata(&path)?;
            if meta.is_dir() {
                directories.push(path);
            } else {
                ensure!(meta.is_file(), "Необычный файл в моде");
                paths.push((path, meta.len()));
            }
        }
    }
    paths.sort();
    let mut hash = Sha256::new();
    let mut size = 0u64;
    for (path, length) in paths {
        let name = path.strip_prefix(root)?.to_string_lossy();
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(length.to_le_bytes());
        hash.update(digest_file(&path)?.as_bytes());
        size = size
            .checked_add(length)
            .context("Слишком большая папка мода")?;
    }
    Ok((format!("{:x}", hash.finalize()), size))
}

fn cancelled(cancel: &CancellationToken) -> Result<()> {
    ensure!(!cancel.is_cancelled(), "Операция остановлена");
    Ok(())
}

/// Never follow a reparse point when copying or deleting a managed tree.
fn copy_tree(from: &Path, to: &Path, cancel: &CancellationToken) -> Result<()> {
    ensure!(
        archive::ordinary_metadata(from)?.is_dir(),
        "Ожидалась обычная папка"
    );
    normal_dir(to)?;
    for item in fs::read_dir(from)? {
        cancelled(cancel)?;
        let item = item?;
        let path = item.path();
        let target = to.join(item.file_name());
        let metadata = archive::ordinary_metadata(&path)?;
        if metadata.is_dir() {
            copy_tree(&path, &target, cancel)?;
        } else {
            ensure!(metadata.is_file(), "Необычный файл");
            fs::copy(path, target)?;
        }
    }
    Ok(())
}

fn remove_tree(path: &Path) -> Result<()> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            junction::delete(path).or_else(|_| fs::remove_dir(path))?;
            return Ok(());
        }
    }
    if meta.file_type().is_symlink() {
        fs::remove_file(path)?;
    } else if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            remove_tree(&entry?.path())?;
        }
        fs::remove_dir(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn version_dir(version: &str) -> String {
    format!("{:x}", Sha256::digest(version.as_bytes()))[..24].to_owned()
}

impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        fs::create_dir_all(path)?;
        let root = fs::canonicalize(path)?;
        ensure!(
            archive::ordinary_metadata(&root)?.is_dir(),
            "Выберите обычную папку библиотеки"
        );
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetVolumePathNameW(file: *const u16, volume: *mut u16, length: u32) -> i32;
                fn GetVolumeInformationW(
                    root: *const u16,
                    label: *mut u16,
                    label_len: u32,
                    serial: *mut u32,
                    max_component: *mut u32,
                    flags: *mut u32,
                    system: *mut u16,
                    system_len: u32,
                ) -> i32;
            }
            let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut volume = [0u16; 1024];
            let mut system = [0u16; 32];
            // SAFETY: all UTF-16 buffers are valid, writable and sized as passed.
            unsafe {
                ensure!(
                    GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32)
                        != 0,
                    "Не удалось определить диск библиотеки"
                );
                ensure!(
                    GetVolumeInformationW(
                        volume.as_ptr(),
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        system.as_mut_ptr(),
                        system.len() as u32
                    ) != 0,
                    "Не удалось проверить файловую систему"
                );
            }
            let name = String::from_utf16_lossy(
                &system[..system.iter().position(|c| *c == 0).unwrap_or(system.len())],
            );
            ensure!(
                name == "NTFS" && !root.to_string_lossy().starts_with("\\\\?\\UNC\\"),
                "Для библиотеки нужен локальный NTFS-диск"
            );
        }
        for folder in ["packages", "environments", "incoming", "staging"] {
            normal_dir(&root.join(folder))?;
        }
        Ok(Self { root })
    }

    pub fn lock(&self) -> Result<fs::File> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("library.lock"))?;
        file.try_lock()
            .context("Библиотека занята другой операцией или копией лаунчера")?;
        Ok(file)
    }

    pub fn environment(&self, id: &str) -> Result<PathBuf> {
        component(id)?;
        Ok(self.root.join("environments").join(id))
    }

    pub fn package_path(&self, id: &str, version: &str) -> Result<PathBuf> {
        component(id)?;
        Ok(self
            .root
            .join("packages")
            .join(id)
            .join(version_dir(version)))
    }

    pub fn record(&self, id: &str, version: &str) -> Result<PackageRecord> {
        let path = self.package_path(id, version)?;
        let record: PackageRecord = serde_json::from_slice(&fs::read(path.join("record.json"))?)?;
        ensure!(
            record.id == id && record.version == version,
            "Повреждён индекс пакетов"
        );
        for f in &record.files {
            ensure!(
                archive::safe_relative(&f.name)?.components().count() == 1,
                "Небезопасное имя пакета"
            );
            let meta = archive::ordinary_metadata(&path.join(&f.name))?;
            ensure!(
                if f.directory {
                    meta.is_dir()
                } else {
                    meta.is_file() && meta.len() == f.size
                },
                "Файл библиотеки отсутствует или повреждён: {}",
                f.name
            );
        }
        Ok(record)
    }

    pub fn incoming(&self, entry: &MapEntry) -> Result<PathBuf> {
        component(&entry.id)?;
        let parent = self.root.join("incoming").join(&entry.id);
        normal_dir(&parent)?;
        let path = parent.join(version_dir(&entry.release.version));
        normal_dir(&path)?;
        let raw = path.join("raw");
        normal_dir(&raw)?;
        Ok(raw)
    }

    /// Preserve partial multipart downloads until every required slot is present.
    pub async fn import(
        &self,
        entry: &MapEntry,
        inputs: &[PathBuf],
        cancel: &CancellationToken,
    ) -> Result<PackageRecord> {
        let _lock = self.lock()?;
        let raw = self.incoming(entry)?;
        for input in inputs {
            cancelled(cancel)?;
            let meta = archive::ordinary_metadata(input)?;
            ensure!(
                meta.is_file() || meta.is_dir(),
                "Выберите файл или папку мода"
            );
            let name = input
                .file_name()
                .and_then(|n| n.to_str())
                .context("Нет имени файла")?;
            ensure!(
                archive::safe_relative(name)?.components().count() == 1,
                "Некорректное имя"
            );
            let target = raw.join(name);
            if fs::canonicalize(input)? == fs::canonicalize(&target).unwrap_or_default() {
                continue;
            }
            if meta.is_dir() {
                ensure!(!target.exists(), "Папка уже импортирована");
                copy_tree(input, &target, cancel)?;
            } else if target.exists() {
                ensure!(
                    digest_file(input)? == digest_file(&target)?,
                    "Уже есть другой файл с именем {name}. Удалите незавершённую загрузку перед повторным импортом."
                );
            } else {
                install::ensure_space(&raw, meta.len())?;
                if let Some(expected) = entry
                    .release
                    .downloads
                    .iter()
                    .find(|d| d.filename == name)
                    .and_then(|d| d.sha256.as_ref())
                {
                    ensure!(
                        digest_file(input)?.eq_ignore_ascii_case(expected),
                        "Не совпадает SHA256 импортируемого файла {name}"
                    );
                }
                fs::copy(input, target)?;
            }
        }
        let work = raw.parent().unwrap().join("work");
        remove_tree(&work)?;
        normal_dir(&work)?;
        let mut candidates = Vec::new();
        for item in fs::read_dir(&raw)? {
            let path = item?.path();
            let meta = archive::ordinary_metadata(&path)?;
            if meta.is_dir() {
                candidates.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_lowercase();
            if name.ends_with(".scs") {
                validate_scs(&path)?;
                if is_zip(&path)? {
                    archive::test(&path, cancel).await?;
                }
                candidates.push(path);
                continue;
            }
            if !(name.ends_with(".zip")
                || name.ends_with(".7z")
                || name.ends_with(".rar")
                || name.ends_with(".001"))
            {
                continue;
            }
            if let Some(part) = name
                .rsplit_once(".part")
                .and_then(|(_, n)| n.strip_suffix(".rar"))
                && part.parse::<u32>().ok() != Some(1)
            {
                continue;
            }
            let entries = archive::inspect(&path, cancel).await?;
            let is_mod = entries.iter().any(|e| {
                e.path == Path::new("manifest.sii")
                    || e.path.starts_with("map")
                    || e.path.starts_with("def")
            });
            if is_mod && name.ends_with(".zip") {
                archive::test(&path, cancel).await?;
                candidates.push(path);
                continue;
            }
            let extract = work.join(format!("archive-{}", version_dir(&name)));
            normal_dir(&extract)?;
            install::ensure_space(&work, entries.iter().map(|e| e.size).sum())?;
            archive::extract(&path, &extract, &entries, cancel).await?;
            for e in entries {
                if e.directory {
                    continue;
                }
                let p = extract.join(e.path);
                if p.extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("scs") || ext.eq_ignore_ascii_case("zip")
                }) {
                    validate_scs(&p)?;
                    if is_zip(&p)? {
                        archive::test(&p, cancel).await?;
                    }
                    candidates.push(p);
                }
            }
        }
        candidates.sort();
        let mut files = Vec::new();
        let mut used = BTreeSet::new();
        let stage = work.join("package");
        normal_dir(&stage)?;
        for rule in &entry.release.packages {
            let re = regex::Regex::new(&rule.pattern)?;
            let matched: Vec<_> = candidates
                .iter()
                .filter(|p| re.is_match(&p.file_name().unwrap().to_string_lossy()))
                .collect();
            if matched.is_empty() && !rule.optional {
                return Err(MissingPackage {
                    pattern: rule.pattern.clone(),
                }
                .into());
            }
            for p in matched {
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                ensure!(
                    used.insert(name.to_lowercase()),
                    "Пакет {name} неоднозначен или подходит нескольким правилам"
                );
                let meta = archive::ordinary_metadata(p)?;
                let destination = stage.join(&name);
                let (hash, size) = if meta.is_dir() {
                    copy_tree(p, &destination, cancel)?;
                    digest_directory(&destination)?
                } else {
                    let hash = digest_file(p)?;
                    fs::hard_link(p, &destination)
                        .context("NTFS не поддерживает жёсткие ссылки в выбранной библиотеке")?;
                    (hash, meta.len())
                };
                files.push(StoredFile {
                    name,
                    sha256: hash,
                    size,
                    directory: meta.is_dir(),
                });
            }
        }
        ensure!(!files.is_empty(), "Не найдены пакеты карты");
        let unknown = candidates
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy())
            .filter(|name| !used.contains(&name.to_lowercase()))
            .collect::<Vec<_>>();
        ensure!(
            unknown.is_empty(),
            "У релиза появились пакеты без правил загрузки: {}. Обновите каталог; установка неполного набора остановлена.",
            unknown
                .iter()
                .map(|s| s.as_ref())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let record = PackageRecord {
            id: entry.id.clone(),
            version: entry.release.version.clone(),
            files,
        };
        write_json(&stage.join("record.json"), &record)?;
        let destination = self.package_path(&entry.id, &entry.release.version)?;
        normal_dir(destination.parent().unwrap())?;
        if destination.exists() {
            let existing = self.record(&entry.id, &entry.release.version)?;
            ensure!(
                serde_json::to_vec(&existing)? == serde_json::to_vec(&record)?,
                "Эта версия уже установлена с другим содержимым. Создайте отдельную сборку для других DEF-настроек."
            );
        } else {
            fs::rename(&stage, &destination)?;
        }
        remove_tree(raw.parent().unwrap())?;
        Ok(record)
    }

    pub fn active(&self, id: &str) -> Result<ActiveState> {
        Ok(serde_json::from_slice(&fs::read(
            self.environment(id)?.join("active.json"),
        )?)?)
    }

    pub fn apply(
        &self,
        catalog: &Catalog,
        set: &ModSet,
        plan: &Plan,
        cancel: &CancellationToken,
    ) -> Result<ActiveState> {
        let _lock = self.lock()?;
        install::ensure_game_closed(Game::Ets2)?;
        let env = self.environment(&set.id)?;
        normal_dir(&env)?;
        self.recover_inner(&env)?;
        let previous = self.active(&set.id).ok();
        if let Some(previous) = &previous {
            ensure!(
                previous.module == plan.module,
                "Для другого модуля создайте новую сборку"
            );
        }
        let packages = plan
            .ordered
            .iter()
            .map(|id| {
                let m = catalog.entry(id)?;
                self.record(id, &m.release.version)
            })
            .collect::<Result<Vec<_>>>()?;
        let next = ActiveState {
            set: set.clone(),
            module: plan.module.clone(),
            profile_name: previous
                .as_ref()
                .map(|s| s.profile_name.clone())
                .unwrap_or_else(|| set.name.clone()),
            packages,
        };
        self.commit(&env, &next, None, cancel)?;
        let _ = self.prune_unused_versions(&next.packages);
        Ok(next)
    }

    fn commit(
        &self,
        env: &Path,
        next: &ActiveState,
        restore_profile: Option<&Path>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        normal_dir(&env.join("home"))?;
        let home = env.join("home/Euro Truck Simulator 2");
        normal_dir(&home)?;
        let profiles = home.join("profiles");
        normal_dir(&profiles)?;
        let profile_id = profile::folder_name(&next.profile_name);
        ensure!(
            !profile_id.is_empty() && profile_id.len() <= 160,
            "Слишком длинное имя профиля"
        );
        let profile_dir = profiles.join(&profile_id);
        let transaction = env.join("transaction");
        remove_tree(&transaction)?;
        normal_dir(&transaction)?;
        let next_mod = transaction.join("mod.next");
        normal_dir(&next_mod)?;
        let mut names = Vec::new();
        let mut seen = BTreeSet::new();
        for record in &next.packages {
            self.record(&record.id, &record.version)?;
            for f in &record.files {
                cancelled(cancel)?;
                ensure!(
                    seen.insert(f.name.to_lowercase()),
                    "Совпадают имена пакетов: {}",
                    f.name
                );
                let source = self
                    .package_path(&record.id, &record.version)?
                    .join(&f.name);
                let target = next_mod.join(&f.name);
                if f.directory {
                    ensure!(
                        digest_directory(&source)?.0 == f.sha256,
                        "Изменилось содержимое папки мода {}",
                        f.name
                    );
                    #[cfg(windows)]
                    junction::create(&source, &target)?;
                    #[cfg(not(windows))]
                    std::os::unix::fs::symlink(&source, &target)?;
                } else {
                    ensure!(
                        digest_file(&source)? == f.sha256,
                        "Повреждён пакет {}",
                        f.name
                    );
                    fs::hard_link(source, target)?;
                }
                names.push(if f.directory {
                    f.name.clone()
                } else {
                    Path::new(&f.name)
                        .file_stem()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                });
            }
        }
        let bytes = if let Some(from) = restore_profile {
            fs::read(from.join("profile.sii"))?
        } else if profile_dir.join("profile.sii").is_file() {
            profile::update(
                &fs::read(profile_dir.join("profile.sii"))?,
                &next.module,
                &names,
            )?
        } else {
            profile::create(&next.profile_name, &next.module, &names)?
        };
        let journal = Journal {
            had_mod: home.join("mod").exists(),
            had_profile: profile_dir.exists(),
            had_state: env.join("active.json").exists(),
            profile: profile_id,
            committed: false,
        };
        if journal.had_profile {
            copy_tree(&profile_dir, &transaction.join("profile.previous"), cancel)?;
        }
        if journal.had_state {
            fs::copy(
                env.join("active.json"),
                transaction.join("active.previous.json"),
            )?;
        }
        if let Some(from) = restore_profile {
            copy_tree(from, &transaction.join("profile.restore"), cancel)?;
        }
        {
            use std::io::Write;
            let mut f = fs::File::create(transaction.join("profile.next.sii"))?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        cancelled(cancel)?;
        write_json(&env.join("journal.json"), &journal)?;
        let result: Result<()> = (|| {
            install::ensure_game_closed(Game::Ets2)?;
            if journal.had_mod {
                fs::rename(home.join("mod"), transaction.join("mod.previous"))?;
            }
            fs::rename(&next_mod, home.join("mod"))?;
            if restore_profile.is_some() {
                remove_tree(&profile_dir)?;
                fs::rename(transaction.join("profile.restore"), &profile_dir)?;
            }
            normal_dir(&profile_dir)?;
            fs::copy(
                transaction.join("profile.next.sii"),
                profile_dir.join("profile.sii"),
            )?;
            write_json(&env.join("active.json"), next)?;
            write_json(
                &env.join("journal.json"),
                &Journal {
                    committed: true,
                    ..journal
                },
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            self.recover_inner(env)?;
            return Err(error);
        }
        self.recover_inner(env)?;
        Ok(())
    }

    fn recover_inner(&self, env: &Path) -> Result<()> {
        let journal_path = env.join("journal.json");
        if !journal_path.exists() {
            return Ok(());
        }
        let journal: Journal = serde_json::from_slice(&fs::read(&journal_path)?)?;
        ensure!(
            journal.profile.bytes().all(|b| b.is_ascii_hexdigit()) && !journal.profile.is_empty(),
            "Повреждён журнал профиля"
        );
        let transaction = env.join("transaction");
        if journal.committed {
            let rollback = env.join("rollback");
            if transaction.exists() {
                remove_tree(&rollback)?;
                fs::rename(&transaction, rollback)?;
            }
        } else {
            let mods = env.join("home/Euro Truck Simulator 2/mod");
            if transaction.join("mod.previous").exists() {
                remove_tree(&mods)?;
                fs::rename(transaction.join("mod.previous"), &mods)?;
            } else if !journal.had_mod {
                remove_tree(&mods)?;
            }
            let target = env
                .join("home/Euro Truck Simulator 2/profiles")
                .join(&journal.profile);
            if journal.had_profile {
                remove_tree(&target)?;
                copy_tree(
                    &transaction.join("profile.previous"),
                    &target,
                    &CancellationToken::new(),
                )?;
            } else {
                remove_tree(&target)?;
            }
            if journal.had_state {
                fs::copy(
                    transaction.join("active.previous.json"),
                    env.join("active.json"),
                )?;
            } else if env.join("active.json").exists() {
                fs::remove_file(env.join("active.json"))?;
            }
            remove_tree(&transaction)?;
        }
        fs::remove_file(journal_path)?;
        Ok(())
    }

    pub fn recover(&self, id: &str) -> Result<()> {
        let _lock = self.lock()?;
        install::ensure_game_closed(Game::Ets2)?;
        self.recover_inner(&self.environment(id)?)
    }

    pub fn clear_incoming(&self, entry: &MapEntry) -> Result<()> {
        let _lock = self.lock()?;
        let raw = self.incoming(entry)?;
        remove_tree(raw.parent().context("Нет папки загрузки")?)
    }

    fn prune_unused_versions(&self, changed: &[PackageRecord]) -> Result<()> {
        let used = used_versions(self)?;
        for package in changed {
            let parent = self
                .package_path(&package.id, &package.version)?
                .parent()
                .unwrap()
                .to_owned();
            for dir in fs::read_dir(parent)? {
                let dir = dir?.path();
                let record_path = dir.join("record.json");
                if let Ok(bytes) = fs::read(record_path)
                    && let Ok(record) = serde_json::from_slice::<PackageRecord>(&bytes)
                    && record.id == package.id
                    && !used
                        .get(&record.id)
                        .is_some_and(|versions| versions.contains(&record.version))
                {
                    remove_tree(&dir)?;
                }
            }
        }
        Ok(())
    }

    pub fn rollback(&self, id: &str, cancel: &CancellationToken) -> Result<ActiveState> {
        let _lock = self.lock()?;
        install::ensure_game_closed(Game::Ets2)?;
        let env = self.environment(id)?;
        self.recover_inner(&env)?;
        let rollback = env.join("rollback");
        let previous: ActiveState = serde_json::from_slice(
            &fs::read(rollback.join("active.previous.json"))
                .context("Нет предыдущей версии сборки")?,
        )?;
        self.commit(
            &env,
            &previous,
            Some(&rollback.join("profile.previous")),
            cancel,
        )?;
        Ok(previous)
    }

    pub fn launch(&self, id: &str, game: &Path) -> Result<()> {
        let _lock = self.lock()?;
        install::ensure_game_closed(Game::Ets2)?;
        let env = self.environment(id)?;
        self.recover_inner(&env)?;
        self.active(id)?;
        let active = self.active(id)?;
        if let Some(version) = game_version(game) {
            ensure!(
                super::version_matches(&version, &active.set.game_version),
                "Установленная ETS2 {version} не соответствует версии сборки {}",
                active.set.game_version
            );
        }
        let executable = game.join("bin/win_x64/eurotrucks2.exe");
        ensure!(executable.is_file(), "Не найден eurotrucks2.exe");
        let home = env.join("home");
        // SCS parses -homedir itself. Pass an ordinary local drive path instead
        // of the extended-length prefix returned by Windows canonicalize().
        #[cfg(windows)]
        let home = PathBuf::from(
            home.to_string_lossy()
                .strip_prefix("\\\\?\\")
                .unwrap_or(&home.to_string_lossy()),
        );
        let mut command = if let Some(steam) = crate::game::steam_client(Game::Ets2, game) {
            let mut command = std::process::Command::new(steam);
            command
                .arg("-applaunch")
                .arg(Game::Ets2.steam_app_id().to_string());
            command
        } else {
            std::process::Command::new(executable)
        };
        command
            .arg("-homedir")
            .arg(home)
            .current_dir(game)
            .spawn()?;
        Ok(())
    }
}

pub fn game_version(game: &Path) -> Option<String> {
    game_version_for(Game::Ets2, game)
}

pub fn game_version_for(game_type: Game, game: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "version.dll", kind = "raw-dylib", modifiers = "+verbatim")]
        unsafe extern "system" {
            fn GetFileVersionInfoSizeW(path: *const u16, handle: *mut u32) -> u32;
            fn GetFileVersionInfoW(
                path: *const u16,
                handle: u32,
                size: u32,
                data: *mut std::ffi::c_void,
            ) -> i32;
            fn VerQueryValueW(
                data: *const std::ffi::c_void,
                block: *const u16,
                value: *mut *mut std::ffi::c_void,
                length: *mut u32,
            ) -> i32;
        }
        let path: Vec<u16> = game
            .join("bin/win_x64")
            .join(game_type.executable())
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let block = [b'\\' as u16, 0];
        // SAFETY: Win32 reads terminated UTF-16 and writes only to sized buffers;
        // the returned fixed-file-info pointer is borrowed from `data` below.
        unsafe {
            let mut unused = 0;
            let size = GetFileVersionInfoSizeW(path.as_ptr(), &mut unused);
            if size == 0 || size > 1024 * 1024 {
                return None;
            }
            let mut data = vec![0u8; size as usize];
            if GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 {
                return None;
            }
            let mut value = std::ptr::null_mut();
            let mut length = 0;
            if VerQueryValueW(
                data.as_ptr().cast(),
                block.as_ptr(),
                &mut value,
                &mut length,
            ) == 0
                || length < 24
                || value.is_null()
            {
                return None;
            }
            let words = std::slice::from_raw_parts(value.cast::<u32>(), 6);
            if words[0] != 0xfeef04bd {
                return None;
            }
            let ms = words[4];
            let ls = words[5];
            Some(format!(
                "{}.{}.{}.{}",
                ms >> 16,
                ms & 0xffff,
                ls >> 16,
                ls & 0xffff
            ))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (game_type, game);
        None
    }
}

fn validate_scs(path: &Path) -> Result<()> {
    let mut bytes = [0; 16];
    let n = fs::File::open(path)?.read(&mut bytes)?;
    ensure!(
        n >= 16 && (bytes.starts_with(b"SCS#") || bytes.starts_with(b"PK\x03\x04")),
        "Файл {} не является пакетом SCS/ZIP (возможно, скачалась HTML-страница)",
        path.display()
    );
    Ok(())
}

fn is_zip(path: &Path) -> Result<bool> {
    let mut magic = [0; 4];
    fs::File::open(path)?.read_exact(&mut magic)?;
    Ok(magic == *b"PK\x03\x04")
}

pub fn installed_dlcs(game: &Path) -> BTreeSet<String> {
    fs::read_dir(game)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_str()?.to_owned();
            (name.starts_with("dlc_")
                && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("scs")))
            .then_some(name)
        })
        .collect()
}

pub async fn download_packages(
    library: &Library,
    entry: &MapEntry,
    state: &SharedTransfer,
    cancel: &CancellationToken,
) -> Result<PackageRecord> {
    super::source::verify(entry, cancel).await?;
    ensure!(
        !entry.release.downloads.is_empty(),
        "Продолжите бесплатную загрузку на странице сайта, затем загрузите все пакеты карты."
    );
    let output = library.incoming(entry)?;
    let mut paths = Vec::new();
    for file in &entry.release.downloads {
        cancelled(cancel)?;
        download::set_state(state, |s| {
            s.name = format!("{} · {}", entry.name, file.filename);
            s.downloaded = 0;
            s.total = None;
        });
        let cached = output.join(&file.filename);
        let path = if cached.exists() {
            ensure!(
                archive::ordinary_metadata(&cached)?.is_file(),
                "В кэше загрузки обнаружена ссылка или каталог"
            );
            cached
        } else {
            download::download_http(&file.url, &output, &file.filename, state, cancel).await?
        };
        if let Some(hash) = &file.sha256 {
            ensure!(
                digest_file(&path)?.eq_ignore_ascii_case(hash),
                "Не совпадает SHA256 файла {}",
                file.filename
            );
        }
        paths.push(path);
    }
    download::set_state(state, |s| {
        s.status = Status::Extracting;
        s.message = "Проверка пакетов карты".into();
    });
    library.import(entry, &paths, cancel).await
}

/// A public archive obtained in the browser may still need a separately
/// generated DEF. Retry only that missing slot; never replace a supplied DEF.
pub async fn import_downloaded(
    library: &Library,
    entry: &MapEntry,
    inputs: &[PathBuf],
    state: &SharedTransfer,
    cancel: &CancellationToken,
) -> Result<PackageRecord> {
    match library.import(entry, inputs, cancel).await {
        Err(error) if missing_supplement(entry, &error) => {
            let mut supplement = entry.clone();
            download::set_state(state, |s| {
                s.message = "Получение дополнительного файла с сайта автора".into()
            });
            supplement.release.downloads = super::source::supplements(entry, cancel).await?;
            download_packages(library, &supplement, state, cancel).await
        }
        result => result,
    }
}

/// Valid versions used by current builds or the one-step rollback are retained.
pub fn used_versions(library: &Library) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut used = BTreeMap::<String, BTreeSet<String>>::new();
    for env in fs::read_dir(library.root.join("environments"))? {
        let env = env?.path();
        for name in ["active.json", "rollback/active.previous.json"] {
            if let Ok(bytes) = fs::read(env.join(name))
                && let Ok(state) = serde_json::from_slice::<ActiveState>(&bytes)
            {
                for p in state.packages {
                    used.entry(p.id).or_default().insert(p.version);
                }
            }
        }
    }
    Ok(used)
}
