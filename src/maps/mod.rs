//! ETS2 maps are independent of the DLC installer. All content and compatibility
//! rules are data loaded from the separately distributed, updateable catalog.
pub mod access;
pub mod audit;
pub mod geography;
pub mod library;
pub mod profile;
pub mod source;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub const CATALOG_URL: &str =
    "https://raw.githubusercontent.com/frd21313123123/ets-launcher/main/catalog/ets2.json";
pub const REFRESH_SECONDS: u64 = 1800;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub schema: u32,
    #[serde(default)]
    pub revision: u64,
    pub maps: Vec<MapEntry>,
    #[serde(default)]
    pub connections: Vec<Connection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MapEntry {
    pub id: String,
    pub name: String,
    pub region: String,
    pub author: String,
    pub description: String,
    pub source: SourceConfig,
    pub release: Release,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Low priority first, matching active_mods[] in profile.sii.
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub before: Vec<String>,
    #[serde(default)]
    pub require_verified_connections: bool,
    #[serde(default)]
    pub compatibility_family: Option<String>,
    #[serde(default = "europe")]
    pub module: String,
    #[serde(default)]
    pub support: bool,
    #[serde(default)]
    pub coverage: Option<geography::Coverage>,
}

fn europe() -> String {
    "/map/europe.mbd".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub dlcs: Vec<String>,
    /// Explicit required/optional slots prevent partially downloaded maps.
    pub packages: Vec<PackageRule>,
    #[serde(default)]
    pub downloads: Vec<DownloadFile>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageRule {
    pub pattern: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadFile {
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceConfig {
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub browser_url: Option<String>,
    pub selector: String,
    /// Must contain a capture group for the release version.
    pub version_pattern: String,
    #[serde(default)]
    pub stop_before: Option<String>,
    #[serde(default)]
    pub date_pattern: Option<String>,
    #[serde(default)]
    pub size_pattern: Option<String>,
    #[serde(default)]
    pub requirements_pattern: Option<String>,
    #[serde(default)]
    pub game_pattern: Option<String>,
    #[serde(default)]
    pub download_selector: Option<String>,
    #[serde(default)]
    pub download_pattern: Option<String>,
    #[serde(default)]
    pub download_api: Option<String>,
    #[serde(default)]
    pub download_wait_seconds: u64,
    #[serde(default)]
    pub follow_releases: bool,
    #[serde(default)]
    pub supplements: Vec<SourceSupplement>,
    #[serde(default)]
    pub mirrors: Vec<SourceMirror>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceMirror {
    pub page: String,
    pub selector: String,
    pub pattern: String,
    #[serde(default)]
    pub same_row: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceSupplement {
    pub page: String,
    pub selector: String,
    pub pattern: String,
    pub filename: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Connection {
    pub when: Vec<String>,
    #[serde(default)]
    pub add: Vec<String>,
    /// A positive compatibility declaration, not inferred from filenames.
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub versions: BTreeMap<String, String>,
    pub note: String,
    #[serde(default)]
    pub live_versions: Option<ConnectionVersionRule>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionVersionRule {
    pub source: String,
    pub pattern: String,
    pub bindings: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceState {
    pub version: Option<String>,
    pub page_url: String,
    pub checked_at: u64,
    pub error: Option<String>,
    #[serde(default)]
    pub links: Vec<(String, String)>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub size_label: Option<String>,
    #[serde(default)]
    pub requirements: Option<String>,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub download_page: Option<String>,
    #[serde(default)]
    pub download_size: Option<u64>,
    #[serde(default)]
    pub downloads: Vec<DownloadFile>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModSet {
    pub id: String,
    pub name: String,
    pub selected: BTreeSet<String>,
    pub game_version: String,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub ordered: Vec<String>,
    pub reasons: BTreeMap<String, String>,
    pub module: String,
}

pub fn component(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 100
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)),
        "Некорректный идентификатор: {value}"
    );
    Ok(())
}

pub fn https(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "Нужна HTTPS-ссылка без учётных данных"
    );
    Ok(url)
}

impl Catalog {
    /// Live release data may advance individual maps. Connection version pins
    /// remain reviewed data, so a new release cannot silently inherit a connector.
    pub fn with_sources(&self, sources: &BTreeMap<String, SourceState>) -> Self {
        let mut live = self.clone();
        for entry in &mut live.maps {
            if let Some(state) = sources.get(&entry.id).filter(|s| s.error.is_none()) {
                if entry.source.follow_releases
                    && !state.game_versions.is_empty()
                    && let Some(version) = &state.version
                {
                    entry.release.version = version.clone();
                    entry.release.game_versions = state.game_versions.clone();
                }
                if state.version.as_deref() == Some(&entry.release.version) {
                    entry.release.size = state.download_size.or(entry.release.size);
                }
            }
        }
        for connection in &mut live.connections {
            if let Some(rule) = &connection.live_versions
                && let Some(state) = sources
                    .get(&rule.source)
                    .filter(|s| s.error.is_none() && !s.game_versions.is_empty())
                && let Some(version) = &state.version
                && let Ok(pattern) = regex::Regex::new(&rule.pattern)
                && let Some(captures) = pattern.captures(version)
                && rule
                    .bindings
                    .values()
                    .all(|&group| captures.get(group).is_some())
            {
                for (id, &group) in &rule.bindings {
                    connection
                        .versions
                        .insert(id.clone(), captures[group].to_owned());
                }
                connection
                    .versions
                    .insert(rule.source.clone(), version.clone());
            }
        }
        live
    }
    pub fn load_from(path: &Path) -> Result<Self> {
        ensure!(
            std::fs::metadata(path)?.len() <= 4 * 1024 * 1024,
            "Каталог слишком большой"
        );
        let catalog: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn bundled() -> Result<Self> {
        let packaged = std::env::current_exe()?
            .parent()
            .context("Нет папки приложения")?
            .join("catalog/ets2.json");
        Self::load_from(&packaged).or_else(|_| {
            Self::load_from(&Path::new(env!("CARGO_MANIFEST_DIR")).join("catalog/ets2.json"))
        })
    }

    pub fn entry(&self, id: &str) -> Result<&MapEntry> {
        self.maps
            .iter()
            .find(|m| m.id == id)
            .with_context(|| format!("Нет карты {id} в каталоге"))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1 && !self.maps.is_empty() && self.maps.len() <= 500,
            "Неподдерживаемый каталог карт"
        );
        let mut ids = BTreeSet::new();
        for m in &self.maps {
            component(&m.id)?;
            ensure!(ids.insert(&m.id), "Повторный ID карты");
            https(&m.source.url)?;
            if let Some(url) = &m.source.browser_url {
                https(url)?;
            }
            if let Some(api) = &m.source.download_api {
                ensure!(
                    api.starts_with('/')
                        && !api.starts_with("//")
                        && !api.contains(['?', '#', '\\']),
                    "Некорректный путь API загрузки"
                );
            }
            ensure!(
                m.source.download_wait_seconds <= 120,
                "Слишком долгое ожидание источника"
            );
            if let Some(selector) = &m.source.download_selector {
                scraper::Selector::parse(selector)
                    .map_err(|_| anyhow::anyhow!("Некорректный селектор загрузки"))?;
            }
            if let Some(pattern) = &m.source.download_pattern {
                regex::Regex::new(pattern)?;
            }
            for rule in &m.source.supplements {
                https(&rule.page)?;
                scraper::Selector::parse(&rule.selector)
                    .map_err(|_| anyhow::anyhow!("Некорректный селектор дополнительного файла"))?;
                regex::Regex::new(
                    &rule
                        .pattern
                        .replace("{version}", &regex::escape(&m.release.version)),
                )?;
                ensure!(
                    crate::archive::safe_relative(&rule.filename)?
                        .components()
                        .count()
                        == 1,
                    "Некорректное имя дополнительного файла"
                );
            }
            for rule in &m.source.mirrors {
                https(&rule.page)?;
                scraper::Selector::parse(&rule.selector)
                    .map_err(|_| anyhow::anyhow!("Некорректный селектор зеркала"))?;
                regex::Regex::new(
                    &rule
                        .pattern
                        .replace("{version}", &regex::escape(&m.release.version))
                        .replace("{version_compact}", &m.release.version.replace('.', "")),
                )?;
            }
            if let Some(coverage) = &m.coverage {
                coverage.validate()?;
            }
            ensure!(
                matches!(
                    m.source.kind.as_str(),
                    "promods" | "scs_forum" | "truckymods" | "poland" | "roextended"
                ),
                "Неизвестный адаптер источника"
            );
            scraper::Selector::parse(&m.source.selector)
                .map_err(|_| anyhow::anyhow!("Некорректный CSS-селектор"))?;
            let re = regex::Regex::new(&m.source.version_pattern)?;
            ensure!(
                re.captures_len() > 1,
                "Нет группы версии в шаблоне источника"
            );
            for pattern in [
                &m.source.date_pattern,
                &m.source.size_pattern,
                &m.source.requirements_pattern,
                &m.source.game_pattern,
            ]
            .into_iter()
            .flatten()
            {
                ensure!(
                    regex::Regex::new(pattern)?.captures_len() > 1,
                    "Нет группы в шаблоне метаданных"
                );
            }
            ensure!(
                !m.release.version.is_empty()
                    && !m.release.game_versions.is_empty()
                    && !m.release.packages.is_empty(),
                "Неполный релиз {}",
                m.name
            );
            let module = crate::archive::safe_relative(m.module.trim_start_matches('/'))?;
            ensure!(
                m.module.starts_with("/map/") && module.extension().is_some_and(|e| e == "mbd"),
                "Некорректный модуль карты"
            );
            for p in &m.release.packages {
                regex::Regex::new(&p.pattern)?;
            }
            for d in &m.release.downloads {
                https(&d.url)?;
                let p = crate::archive::safe_relative(&d.filename)?;
                ensure!(p.components().count() == 1, "Имя загрузки содержит путь");
                if let Some(hash) = &d.sha256 {
                    ensure!(
                        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                        "Некорректный SHA256"
                    );
                }
            }
            for d in &m.release.dlcs {
                ensure!(
                    d.starts_with("dlc_")
                        && crate::archive::safe_relative(d)?.components().count() == 1,
                    "Некорректный ID DLC"
                );
            }
        }
        for m in &self.maps {
            for id in m
                .dependencies
                .iter()
                .chain(&m.conflicts)
                .chain(&m.after)
                .chain(&m.before)
            {
                self.entry(id)?;
            }
        }
        for c in &self.connections {
            ensure!(
                c.when.len() >= 2,
                "У стыковки должно быть хотя бы две карты"
            );
            for id in c.when.iter().chain(&c.add) {
                self.entry(id)?;
            }
            for id in c.versions.keys() {
                self.entry(id)?;
            }
            if let Some(rule) = &c.live_versions {
                self.entry(&rule.source)?;
                ensure!(
                    c.add.contains(&rule.source),
                    "Источник версий должен быть коннектором стыковки"
                );
                let pattern = regex::Regex::new(&rule.pattern)?;
                ensure!(
                    rule.pattern.starts_with('^') && rule.pattern.ends_with('$'),
                    "Шаблон версий коннектора должен быть полным"
                );
                ensure!(
                    c.when.iter().all(|id| rule.bindings.contains_key(id)),
                    "Версии коннектора не описывают все карты"
                );
                for (id, &group) in &rule.bindings {
                    ensure!(
                        c.when.contains(id) && group > 0 && group < pattern.captures_len(),
                        "Некорректная привязка версии коннектора"
                    );
                }
            }
        }
        Ok(())
    }

    pub fn resolve(&self, set: &ModSet, dlcs: &BTreeSet<String>) -> Result<Plan> {
        self.validate()?;
        self.resolve_validated(set, dlcs)
    }

    pub(super) fn resolve_validated(&self, set: &ModSet, dlcs: &BTreeSet<String>) -> Result<Plan> {
        ensure!(!set.selected.is_empty(), "Выберите хотя бы одну карту");
        ensure!(!set.game_version.trim().is_empty(), "Укажите версию ETS2");
        let mut selected = set.selected.clone();
        let mut reasons = BTreeMap::new();
        loop {
            let old = selected.clone();
            for id in &old {
                let m = self.entry(id)?;
                for d in &m.dependencies {
                    if selected.insert(d.clone()) {
                        reasons.insert(d.clone(), format!("Требуется для {}", m.name));
                    }
                }
            }
            for c in &self.connections {
                if c.when.iter().all(|id| selected.contains(id)) {
                    ensure!(c.verified, "Сочетание не проверено: {}", c.note);
                    for (id, version) in &c.versions {
                        ensure!(
                            self.entry(id)?.release.version == *version,
                            "Для новой версии {} нужна проверка стыковки: {}",
                            self.entry(id)?.name,
                            c.note
                        );
                    }
                    for d in &c.add {
                        if selected.insert(d.clone()) {
                            reasons.insert(d.clone(), c.note.clone());
                        }
                    }
                }
            }
            if old == selected {
                break;
            }
        }
        let mut module = europe();
        for id in &selected {
            let m = self.entry(id)?;
            ensure!(
                m.release
                    .game_versions
                    .iter()
                    .any(|v| version_matches(&set.game_version, v)),
                "{} {} не поддерживает ETS2 {}",
                m.name,
                m.release.version,
                set.game_version
            );
            for dlc in &m.release.dlcs {
                ensure!(dlcs.contains(dlc), "Для {} не найден {}.scs", m.name, dlc);
            }
            for conflict in &m.conflicts {
                ensure!(
                    !selected.contains(conflict),
                    "{} несовместим с {}",
                    m.name,
                    self.entry(conflict)?.name
                );
            }
            if m.module != europe() {
                ensure!(
                    module == europe() || module == m.module,
                    "Выбраны разные самостоятельные карты"
                );
                module = m.module.clone();
            }
        }
        if module != europe() {
            ensure!(
                selected.iter().all(|id| self
                    .entry(id)
                    .is_ok_and(|m| m.module == module || m.support)),
                "Самостоятельная карта требует отдельной сборки"
            );
        }
        let maps = selected
            .iter()
            .filter_map(|id| self.entry(id).ok())
            .filter(|m| !m.support)
            .collect::<Vec<_>>();
        for (index, a) in maps.iter().enumerate() {
            for b in &maps[index + 1..] {
                if !a.require_verified_connections && !b.require_verified_connections {
                    continue;
                }
                if a.dependencies.contains(&b.id) || b.dependencies.contains(&a.id) {
                    continue;
                }
                if a.compatibility_family.is_some()
                    && a.compatibility_family == b.compatibility_family
                {
                    continue;
                }
                ensure!(
                    self.connections.iter().any(|c| c.verified
                        && c.when.contains(&a.id)
                        && c.when.contains(&b.id)
                        && c.when.iter().all(|id| selected.contains(id))),
                    "Нет подтверждённой стыковки {} + {} для текущих версий. Выберите отдельные сборки или обновите правила коннекторов.",
                    a.name,
                    b.name
                );
            }
        }
        let mut ordered = Vec::new();
        let mut remaining = selected;
        while !remaining.is_empty() {
            let ready: Vec<_> = remaining
                .iter()
                .filter(|id| {
                    let m = self.entry(id).unwrap();
                    m.after
                        .iter()
                        .chain(m.dependencies.iter().filter(|dep| !m.before.contains(dep)))
                        .chain(
                            self.maps
                                .iter()
                                .filter(|other| other.before.contains(&m.id))
                                .map(|other| &other.id),
                        )
                        .all(|dep| !remaining.contains(dep))
                })
                .cloned()
                .collect();
            ensure!(
                !ready.is_empty(),
                "Цикл в зависимостях или порядке загрузки"
            );
            for id in ready {
                remaining.remove(&id);
                ordered.push(id);
            }
        }
        Ok(Plan {
            ordered,
            reasons,
            module,
        })
    }
}

pub fn version_matches(actual: &str, supported: &str) -> bool {
    let normalize = |s: &str| s.trim().trim_end_matches(['*', '.', 'x']).to_owned();
    let supported = normalize(supported);
    !supported.is_empty()
        && (actual == supported
            || actual
                .strip_prefix(&supported)
                .is_some_and(|rest| rest.starts_with('.')))
}

pub fn config_path(name: &str) -> Result<PathBuf> {
    Ok(crate::settings::Settings::path()
        .context("Нет папки настроек")?
        .with_file_name(name))
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("Нет родительской папки")?)?;
    let temporary = path.with_extension("json.part");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&temporary)?;
        f.write_all(&serde_json::to_vec_pretty(value)?)?;
        f.sync_all()?;
    }
    std::fs::rename(temporary, path)?;
    Ok(())
}
