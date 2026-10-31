use super::{Combo, Plan, files};
use crate::{
    archive,
    download::{self, SharedTransfer, Status},
    game::{self, Game},
    maps::profile,
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

#[derive(Clone)]
pub struct Library {
    pub root: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
    pub filename: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub packages: BTreeMap<String, Package>,
    pub completed_sources: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Problem {
    pub name: String,
    pub url: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub completed: usize,
    pub total: usize,
    pub missing: Vec<String>,
    pub problems: Vec<Problem>,
    pub home: Option<PathBuf>,
}

fn key(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))[..24].to_owned()
}
fn check(cancel: &CancellationToken) -> Result<()> {
    ensure!(!cancel.is_cancelled(), "Загрузка остановлена");
    Ok(())
}

fn directory(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir(path)?;
    }
    ensure!(
        archive::ordinary_metadata(path)?.is_dir(),
        "Нужна обычная папка: {}",
        path.display()
    );
    Ok(())
}

impl Library {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        ensure!(
            archive::ordinary_metadata(root)?.is_dir(),
            "Выберите обычную папку сборок"
        );
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    pub fn release(&self, combo: &Combo) -> Result<PathBuf> {
        combo.validate()?;
        let folder = self.root.join(&combo.id);
        directory(&folder)?;
        let release = folder.join(key(&format!("{}-{}", combo.game_version, combo.version)));
        directory(&release)?;
        for child in ["downloads", "packages", "staging"] {
            directory(&release.join(child))?;
        }
        Ok(release)
    }

    fn lock(&self, release: &Path) -> Result<fs::File> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(release.join("library.lock"))?;
        file.try_lock()
            .context("Эта сборка уже обрабатывается другой копией лаунчера")?;
        Ok(file)
    }

    pub fn state(&self, combo: &Combo) -> Result<State> {
        let release = self.release(combo)?;
        let path = release.join("state.json");
        let state: State = if path.exists() {
            serde_json::from_slice(&fs::read(path)?)?
        } else {
            State::default()
        };
        for (id, package) in &state.packages {
            super::valid_mod_id(id)?;
            ensure!(
                files::allowed_filename(&package.filename),
                "Повреждено имя пакета в библиотеке"
            );
        }
        Ok(state)
    }

    /// Progress of a release that was already started, without creating any folders:
    /// `(ready mods, environment ready)`, or `None` when nothing was downloaded yet.
    pub fn progress(&self, plan: &Plan) -> Result<Option<(usize, bool)>> {
        let combo = &plan.combo;
        combo.validate()?;
        let release = self
            .root
            .join(&combo.id)
            .join(key(&format!("{}-{}", combo.game_version, combo.version)));
        if !release.join("packages").is_dir() {
            return Ok(None);
        }
        let missing = self.missing(plan)?.len();
        // Opening a build creates its folders; that alone is not a download.
        if missing == plan.order.len() {
            return Ok(None);
        }
        Ok(Some((
            plan.order.len().saturating_sub(missing),
            missing == 0 && self.ready_home(plan)?.is_some(),
        )))
    }

    pub fn missing(&self, plan: &Plan) -> Result<Vec<String>> {
        let release = self.release(&plan.combo)?;
        let state = self.state(&plan.combo)?;
        Ok(plan
            .order
            .iter()
            .filter(|id| {
                state.packages.get(&id.to_lowercase()).is_none_or(|p| {
                    archive::ordinary_metadata(&release.join("packages").join(&p.filename))
                        .map_or(true, |m| !m.is_file() || m.len() != p.size)
                })
            })
            .cloned()
            .collect())
    }

    pub fn ready_home(&self, plan: &Plan) -> Result<Option<PathBuf>> {
        let release = self.release(&plan.combo)?;
        let installed = release.join("home").join(".quper-plan.json");
        if !installed.exists() || !self.missing(plan)?.is_empty() {
            return Ok(None);
        }
        let previous: Plan = serde_json::from_slice(&fs::read(installed)?)?;
        if previous.order != plan.order || previous.combo.game != plan.combo.game {
            return Ok(None);
        }
        let home = release.join("home");
        let docs = home.join(plan.combo.game.name());
        let state = self.state(&plan.combo)?;
        for id in &plan.order {
            let p = &state.packages[&id.to_lowercase()];
            if !docs.join("mod").join(&p.filename).is_file() {
                return Ok(None);
            }
        }
        let profile_path = docs
            .join("profiles")
            .join(profile::folder_name(&profile_name(&plan.combo)))
            .join("profile.sii");
        Ok(profile_path.is_file().then_some(home))
    }

    /// Queue public sources once per archive, even when it supplies many rows.
    /// Individual failures are retained and do not prevent other downloads.
    pub async fn download(
        &self,
        plan: &Plan,
        transfer: &SharedTransfer,
        cancel: &CancellationToken,
    ) -> Result<Report> {
        plan.validate()?;
        let release = self.release(&plan.combo)?;
        let _lock = self.lock(&release)?;
        crate::maps::write_json(&release.join("plan.json"), plan)?;
        let mut state = self.state(&plan.combo)?;
        let mut problems = Vec::new();
        let mut sources = BTreeMap::<String, String>::new();
        let missing: BTreeSet<_> = self
            .missing(plan)?
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        for row in &plan.files {
            if !row.required || !missing.contains(&row.mod_id.to_lowercase()) {
                continue;
            }
            if row.urls.is_empty() {
                problems.push(Problem {
                    name: row.name.clone(),
                    url: None,
                    message: if row.excluded_boosty {
                        "Источник Boosty исключён. Можно импортировать уже имеющийся файл."
                    } else {
                        "В таблице нет публичной ссылки. Можно импортировать готовый файл."
                    }
                    .into(),
                });
            }
            for url in &row.urls {
                sources
                    .entry(url.clone())
                    .or_insert_with(|| row.name.clone());
            }
        }
        let total = sources.len();
        let mut completed = 0;
        for (url, name) in sources {
            check(cancel)?;
            let source_key = key(&url);
            // A source is reused only if its required packages still exist.
            let row_ids = plan
                .files
                .iter()
                .filter(|r| r.required && r.urls.contains(&url))
                .map(|r| r.mod_id.to_lowercase())
                .collect::<Vec<_>>();
            let current_missing = self.missing(plan)?;
            if row_ids
                .iter()
                .all(|id| !current_missing.iter().any(|m| m.eq_ignore_ascii_case(id)))
            {
                completed += 1;
                continue;
            }
            let output = release.join("downloads").join(&source_key);
            directory(&output)?;
            download::set_state(transfer, |s| {
                s.status = Status::Connecting;
                s.name = name.clone();
                s.message = format!("Источник {} из {total}: {name}", completed + 1);
                s.downloaded = 0;
                s.total = None;
            });
            let result = files::download_source(&url, &output, transfer, cancel).await;
            check(cancel)?;
            let mut inputs = Vec::new();
            for entry in fs::read_dir(&output)? {
                let path = entry?.path();
                if path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(files::allowed_filename)
                    && archive::ordinary_metadata(&path)?.is_file()
                {
                    inputs.push(path);
                }
            }
            let imported = if !inputs.is_empty() {
                self.import_inner(plan, &release, &mut state, &inputs, transfer, cancel)
                    .await
            } else {
                Ok(0)
            };
            match (result, imported) {
                (Ok(_), Ok(_)) => {
                    state.completed_sources.insert(source_key);
                }
                (Err(error), _) | (_, Err(error)) => problems.push(Problem {
                    name: name.clone(),
                    url: Some(url),
                    message: format!("{error:#}"),
                }),
            }
            crate::maps::write_json(&release.join("state.json"), &state)?;
            completed += 1;
        }
        let missing = self.missing(plan)?;
        let home = if missing.is_empty() {
            Some(self.prepare_inner(plan, &release, &state, cancel)?)
        } else {
            None
        };
        Ok(Report {
            completed,
            total,
            missing,
            problems,
            home,
        })
    }

    pub async fn import(
        &self,
        plan: &Plan,
        inputs: &[PathBuf],
        transfer: &SharedTransfer,
        cancel: &CancellationToken,
    ) -> Result<Report> {
        plan.validate()?;
        let release = self.release(&plan.combo)?;
        let _lock = self.lock(&release)?;
        crate::maps::write_json(&release.join("plan.json"), plan)?;
        let mut state = self.state(&plan.combo)?;
        self.import_inner(plan, &release, &mut state, inputs, transfer, cancel)
            .await?;
        let missing = self.missing(plan)?;
        let home = if missing.is_empty() {
            Some(self.prepare_inner(plan, &release, &state, cancel)?)
        } else {
            None
        };
        Ok(Report {
            completed: inputs.len(),
            total: inputs.len(),
            missing,
            problems: vec![],
            home,
        })
    }

    async fn import_inner(
        &self,
        plan: &Plan,
        release: &Path,
        state: &mut State,
        inputs: &[PathBuf],
        transfer: &SharedTransfer,
        cancel: &CancellationToken,
    ) -> Result<usize> {
        let staging = release.join("staging").join(format!(
            "import-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir(&staging)?;
        let result = Self::unpack(plan, release, state, inputs, &staging, transfer, cancel).await;
        // Only the unique directory created immediately above is removed.
        let _ = fs::remove_dir_all(&staging);
        // Already verified packages remain resumable even if a later input fails.
        crate::maps::write_json(&release.join("state.json"), state)?;
        result
    }

    async fn unpack(
        plan: &Plan,
        release: &Path,
        state: &mut State,
        inputs: &[PathBuf],
        staging: &Path,
        transfer: &SharedTransfer,
        cancel: &CancellationToken,
    ) -> Result<usize> {
        let expected: BTreeSet<_> = plan.order.iter().map(|s| s.to_lowercase()).collect();
        let mut queue = inputs.iter().cloned().map(|p| (p, 0u8)).collect::<Vec<_>>();
        let mut imported = 0;
        let mut sequence = 0;
        let rar_part = regex::Regex::new(r"(?i)\.part(\d+)\.rar$")?;
        while let Some((path, depth)) = queue.pop() {
            check(cancel)?;
            ensure!(
                depth <= 4 && sequence <= 2000,
                "Слишком много вложенных архивов"
            );
            ensure!(
                archive::ordinary_metadata(&path)?.is_file(),
                "Ожидается обычный файл"
            );
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .context("Неизвестное имя архива")?;
            ensure!(files::allowed_filename(name), "Неизвестный формат: {name}");
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_lowercase();
            if ext.len() == 3 && ext.bytes().all(|b| b.is_ascii_digit()) && ext != "001" {
                continue;
            }
            if rar_part
                .captures(name)
                .is_some_and(|c| c[1].parse::<u32>().is_ok_and(|n| n > 1))
            {
                continue;
            }
            let mut id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .context("Нет имени мода")?
                .to_owned();
            // A browser saving a file again names it "mod(1).scs"; the build
            // knows it as "mod", so the copy is stored under its original name.
            let mut stored = name.to_owned();
            if !expected.contains(&id.to_lowercase())
                && let Some(base) = files::browser_copy_base(&id)
                && expected.contains(&base.to_lowercase())
            {
                let base = base.to_owned();
                stored = match path.extension().and_then(|e| e.to_str()) {
                    Some(extension) => format!("{base}.{extension}"),
                    None => base.clone(),
                };
                id = base;
            }
            let name = stored.as_str();
            let mut magic = [0u8; 4];
            fs::File::open(&path)?
                .read_exact(&mut magic)
                .context("Файл пуст или повреждён")?;
            download::set_state(transfer, |s| {
                s.status = Status::Extracting;
                s.name = name.into();
            });
            let entries = if ext == "scs" && &magic == b"SCS#" {
                None
            } else {
                Some(archive::inspect(&path, cancel).await?)
            };
            let resource_zip = ext == "zip" && entries.as_ref().is_some_and(|entries| {
                !entries.iter().any(|e| e.path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("scs")))
                && entries.iter().any(|e| matches!(e.path.components().next(), Some(std::path::Component::Normal(s)) if matches!(s.to_string_lossy().to_ascii_lowercase().as_str(), "manifest.sii" | "manifest.sui" | "def" | "map" | "material")))
            });
            let nested_packages = entries.as_ref().is_some_and(|entries| {
                entries.iter().any(|e| {
                    e.path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(files::allowed_filename)
                })
            });
            if ext == "scs"
                || resource_zip
                || (ext == "zip" && expected.contains(&id.to_lowercase()) && !nested_packages)
            {
                super::valid_mod_id(&id)?;
                if entries.is_some() {
                    archive::test(&path, cancel).await?;
                }
                let sha256 = crate::maps::library::digest_file(&path)?;
                let size = fs::metadata(&path)?.len();
                let destination = release.join("packages").join(name);
                if destination.exists() {
                    archive::ordinary_metadata(&destination)?;
                    ensure!(
                        crate::maps::library::digest_file(&destination)? == sha256,
                        "Для {name} уже есть файл с другим содержимым. Обновите версию сборки или выберите другую папку."
                    );
                } else {
                    fs::copy(&path, &destination)?;
                }
                if let Some(previous) = state.packages.get(&id.to_lowercase()) {
                    ensure!(previous.sha256 == sha256, "Неоднозначный пакет {id}");
                }
                state.packages.insert(
                    id.to_lowercase(),
                    Package {
                        filename: name.into(),
                        size,
                        sha256,
                    },
                );
                imported += 1;
            } else {
                let output = staging.join(sequence.to_string());
                sequence += 1;
                fs::create_dir(&output)?;
                archive::extract(
                    &path,
                    &output,
                    entries.as_ref().context("Нет списка архива")?,
                    cancel,
                )
                .await?;
                let mut directories = vec![output];
                while let Some(dir) = directories.pop() {
                    for entry in fs::read_dir(dir)? {
                        let child = entry?.path();
                        let meta = archive::ordinary_metadata(&child)?;
                        if meta.is_dir() {
                            directories.push(child);
                        } else if child
                            .file_name()
                            .and_then(|s| s.to_str())
                            .is_some_and(files::allowed_filename)
                        {
                            queue.push((child, depth + 1));
                        }
                    }
                }
            }
        }
        ensure!(imported > 0, "В архивах не найдено пакетов SCS/ZIP");
        Ok(imported)
    }

    fn prepare_inner(
        &self,
        plan: &Plan,
        release: &Path,
        state: &State,
        cancel: &CancellationToken,
    ) -> Result<PathBuf> {
        check(cancel)?;
        ensure!(
            self.missing(plan)?.is_empty(),
            "Сборка загружена не полностью"
        );
        for id in &plan.order {
            check(cancel)?;
            let package = &state.packages[&id.to_lowercase()];
            let path = release.join("packages").join(&package.filename);
            archive::ordinary_metadata(&path)?;
            ensure!(
                crate::maps::library::digest_file(&path)? == package.sha256,
                "Повреждён пакет {}",
                package.filename
            );
        }
        if let Some(home) = self.ready_home(plan)? {
            return Ok(home);
        }
        let home = release.join("home");
        // Preserve an existing career rather than replacing its profile/save files.
        ensure!(
            !home.exists(),
            "Окружение этой версии уже существует. Выберите другую папку сборок для изменённого состава."
        );
        let staging = release.join("staging").join("home");
        if staging.exists() {
            archive::ordinary_metadata(&staging)?;
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir(&staging)?;
        let result: Result<()> = (|| {
            let docs = staging.join(plan.combo.game.name());
            fs::create_dir(&docs)?;
            fs::create_dir(docs.join("mod"))?;
            for id in &plan.order {
                check(cancel)?;
                let package = &state.packages[&id.to_lowercase()];
                let path = release.join("packages").join(&package.filename);
                archive::ordinary_metadata(&path)?;
                let target = docs.join("mod").join(&package.filename);
                if fs::hard_link(&path, &target).is_err() {
                    fs::copy(&path, &target)?;
                }
            }
            let name = profile_name(&plan.combo);
            let dir = docs.join("profiles").join(profile::folder_name(&name));
            fs::create_dir_all(&dir)?;
            let bytes = new_profile(plan.combo.game, &name, &plan.order)?;
            fs::write(dir.join("profile.sii"), bytes)?;
            fs::write(docs.join("config.cfg"), "uset r_buffer_page_size \"30\"\n")?;
            crate::maps::write_json(&staging.join(".quper-plan.json"), plan)?;
            check(cancel)?;
            fs::rename(&staging, &home)?;
            Ok(())
        })();
        if result.is_err() && staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        result?;
        Ok(home)
    }

    pub fn launch(&self, plan: &Plan, installation: &Path) -> Result<()> {
        let release = self.release(&plan.combo)?;
        let _lock = self.lock(&release)?;
        let game = plan.combo.game;
        crate::install::ensure_game_closed(game)?;
        ensure!(
            game::is_installation(game, installation),
            "Укажите папку {} в разделе DLC и настроек",
            game.short_name()
        );
        let home = self
            .ready_home(plan)?
            .context("Сначала скачайте все файлы сборки")?;
        if let Some(version) = crate::maps::library::game_version_for(game, installation) {
            ensure!(
                crate::maps::version_matches(&version, &plan.combo.game_version),
                "Установленная {} {} не соответствует сборке для {}",
                game.short_name(),
                version,
                plan.combo.game_version
            );
        }
        #[cfg(windows)]
        let home = PathBuf::from(
            home.to_string_lossy()
                .strip_prefix("\\\\?\\")
                .unwrap_or(&home.to_string_lossy()),
        );
        let mut command = if let Some(steam) = game::steam_client(game, installation) {
            let mut command = std::process::Command::new(steam);
            command
                .arg("-applaunch")
                .arg(game.steam_app_id().to_string());
            command
        } else {
            std::process::Command::new(installation.join("bin/win_x64").join(game.executable()))
        };
        command
            .arg("-homedir")
            .arg(home)
            .arg("-nointro")
            .arg("-unlimitedlog")
            .current_dir(installation)
            .spawn()?;
        Ok(())
    }
}

/// A build added to the game's ordinary documents folder by
/// [`Library::create_local_profile`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalProfile {
    pub name: String,
    pub profile: PathBuf,
    pub mods: usize,
    /// Bytes copied because a hard link to the library was not possible.
    pub copied: u64,
    /// The profile already existed: its career was kept, only the mod list changed.
    pub updated: bool,
}

impl Library {
    /// Adds the build to the regular game folder `documents/<game>`: packages go to
    /// `mod/` (hard links when possible) and a local profile lists them in the
    /// author's order. A same-named profile keeps its career and gets a backup.
    /// The caller checks that the game is closed: it rewrites profiles on exit.
    pub fn create_local_profile(
        &self,
        plan: &Plan,
        documents: &Path,
        transfer: &SharedTransfer,
        cancel: &CancellationToken,
    ) -> Result<LocalProfile> {
        plan.validate()?;
        let release = self.release(&plan.combo)?;
        let _lock = self.lock(&release)?;
        ensure!(
            self.missing(plan)?.is_empty(),
            "Сначала скачайте все файлы сборки"
        );
        let state = self.state(&plan.combo)?;
        let game = plan.combo.game;
        let docs = documents.join(game.name());
        let mods = docs.join("mod");
        fs::create_dir_all(&mods)?;
        archive::ordinary_metadata(&mods)?;
        let total = plan.order.len();
        let mut names = Vec::with_capacity(total);
        let mut copied = 0;
        for (index, id) in plan.order.iter().enumerate() {
            check(cancel)?;
            let package = &state.packages[&id.to_lowercase()];
            download::set_state(transfer, |s| {
                s.status = Status::Installing;
                s.name = package.filename.clone();
                s.message = format!("Подключение модов к профилю: {} из {total}", index + 1);
                s.downloaded = index as u64;
                s.total = Some(total as u64);
                s.speed = 0;
            });
            let source = release.join("packages").join(&package.filename);
            let (target, stem) = local_target(&mods, package)?;
            if !target.exists() && fs::hard_link(&source, &target).is_err() {
                crate::install::ensure_space(&mods, package.size)?;
                let partial = mods.join(format!("{}.launcher-part", package.filename));
                let result =
                    fs::copy(&source, &partial).and_then(|_| fs::rename(&partial, &target));
                if result.is_err() {
                    let _ = fs::remove_file(&partial);
                }
                result?;
                copied += package.size;
            }
            // The author's identifier is kept unless the file had to be renamed.
            names.push(if stem.eq_ignore_ascii_case(id) {
                id.clone()
            } else {
                stem
            });
        }
        check(cancel)?;
        let name = profile_name(&plan.combo);
        let dir = docs.join("profiles").join(profile::folder_name(&name));
        let file = dir.join("profile.sii");
        let updated = file.is_file();
        let bytes = if updated {
            let previous = fs::read(&file)?;
            fs::write(dir.join("profile.sii.launcher-backup"), &previous)?;
            profile::update(&previous, module(game), &names)?
        } else {
            fs::create_dir_all(&dir)?;
            new_profile(game, &name, &names)?
        };
        let temporary = dir.join("profile.sii.launcher-new");
        fs::write(&temporary, bytes)?;
        fs::rename(&temporary, &file)?;
        raise_buffer(&docs.join("config.cfg"))?;
        download::set_state(transfer, |s| {
            s.status = Status::Complete;
            s.downloaded = total as u64;
        });
        Ok(LocalProfile {
            name,
            profile: dir,
            mods: names.len(),
            copied,
            updated,
        })
    }
}

/// Where a package goes in the shared `mod` folder and the identifier the
/// profile must use. Another mod with the same file name is never replaced.
fn local_target(mods: &Path, package: &Package) -> Result<(PathBuf, String)> {
    let path = Path::new(&package.filename);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .context("Некорректное имя пакета")?;
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("scs");
    for attempt in 0..100 {
        let candidate = if attempt == 0 {
            stem.to_owned()
        } else {
            format!("{stem}_quper{attempt}")
        };
        let target = mods.join(format!("{candidate}.{extension}"));
        let Ok(metadata) = fs::symlink_metadata(&target) else {
            return Ok((target, candidate));
        };
        if archive::ordinary_metadata(&target).is_ok()
            && metadata.is_file()
            && metadata.len() == package.size
            && crate::maps::library::digest_file(&target)? == package.sha256
        {
            return Ok((target, candidate));
        }
    }
    anyhow::bail!("Слишком много модов с именем {}", package.filename)
}

fn module(game: Game) -> &'static str {
    match game {
        Game::Ets2 => "/map/europe.mbd",
        Game::Ats => "/map/usa.mbd",
    }
}

fn new_profile(game: Game, name: &str, mods: &[String]) -> Result<Vec<u8>> {
    let bytes = profile::create(name, module(game), mods)?;
    Ok(if game == Game::Ats {
        String::from_utf8(bytes)?
            .replace("brand: scania_r", "brand: kenworth")
            .into_bytes()
    } else {
        bytes
    })
}

/// Large map combos need a bigger render buffer; an already larger value is kept.
fn raise_buffer(config: &Path) -> Result<()> {
    const WANTED: u32 = 30;
    let text = match fs::read(config) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    let line = regex::Regex::new(r#"(?m)^uset r_buffer_page_size "(\d+)"\s*$"#)?;
    let updated = match line.captures(&text) {
        Some(found) if found[1].parse::<u32>().is_ok_and(|v| v >= WANTED) => return Ok(()),
        Some(_) => line
            .replace(&text, format!("uset r_buffer_page_size \"{WANTED}\""))
            .into_owned(),
        None => format!(
            "{text}{}uset r_buffer_page_size \"{WANTED}\"\n",
            if text.is_empty() || text.ends_with('\n') {
                ""
            } else {
                "\n"
            }
        ),
    };
    fs::write(config, updated)?;
    Ok(())
}

fn profile_name(combo: &Combo) -> String {
    let name = format!("Quper {}", combo.title);
    let mut result = String::new();
    for c in name.chars() {
        if result.len() + c.len_utf8() > 78 {
            break;
        }
        result.push(c);
    }
    result
}
