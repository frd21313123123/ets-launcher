use ets_launcher::maps::{
    self, Catalog, MapEntry, ModSet,
    library::{Library, PackageRecord},
    profile, source,
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

fn entry(id: &str) -> MapEntry {
    serde_json::from_value(serde_json::json!({
        "id":id,"name":id,"region":"Тест","author":"Fixture","description":"Fixture",
        "source":{"kind":"truckymods","url":"https://example.com/maps/test","selector":"main","version_pattern":"Current Version\\s*([0-9.]+)"},
        "release":{"version":"1.0","game_versions":["1.61"],"packages":[{"pattern":format!("^{id}.*\\.scs$")}]}
    })).unwrap()
}

fn catalog() -> Catalog {
    Catalog {
        schema: 1,
        revision: 1,
        maps: vec![entry("base"), entry("addon"), entry("connector")],
        connections: vec![],
    }
}
fn set() -> ModSet {
    ModSet {
        id: "test-build".into(),
        name: "Тестовая сборка".into(),
        game_version: "1.61".into(),
        selected: BTreeSet::from(["base".into()]),
    }
}
fn fake_scs(root: &Path, name: &str, content: u8) -> PathBuf {
    let p = root.join(name);
    let mut data = b"SCS#".to_vec();
    data.extend([content; 60]);
    fs::write(&p, data).unwrap();
    p
}
async fn import(library: &Library, entry: &MapEntry, root: &Path, content: u8) -> PackageRecord {
    let p = fake_scs(root, &format!("{}.scs", entry.id), content);
    library
        .import(entry, &[p], &CancellationToken::new())
        .await
        .unwrap()
}

#[test]
fn bundled_catalog_is_external_valid_and_contains_ten_maps_and_a_real_connector() {
    let c = Catalog::bundled().unwrap();
    c.validate().unwrap();
    assert!(c.maps.iter().filter(|m| !m.support).count() >= 10);
    let mut s = set();
    s.selected = BTreeSet::from(["promods-europe".into(), "rusmap".into()]);
    let dlcs = c.maps.iter().flat_map(|m| m.release.dlcs.clone()).collect();
    let p = c.resolve(&s, &dlcs).unwrap();
    assert_eq!(p.ordered.last().unwrap(), "promods-rusmap-connector");
    assert!(p.reasons.contains_key("promods-rusmap-connector"));
    s.selected.insert("poland-rebuilding".into());
    assert!(
        c.resolve(&s, &dlcs)
            .unwrap_err()
            .to_string()
            .contains("тройную")
    );
}

#[tokio::test]
async fn publisher_package_names_require_all_promods_models_and_reverse_roex_files() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let c = Catalog::bundled().unwrap();
    let pm = c.entry("promods-europe").unwrap();
    let names = [
        "promods-eu-assets-v284.scs",
        "promods-eu-model1-v284.scs",
        "promods-eu-model2-v284.scs",
        "promods-eu-model3-v284.scs",
        "promods-eu-media-v284.scs",
        "promods-eu-map-v284.scs",
        "promods-eu-def-v284.scs",
    ];
    let paths = names
        .iter()
        .map(|name| fake_scs(inputs.path(), name, 1))
        .collect::<Vec<_>>();
    assert!(
        library
            .import(pm, &paths[..3], &CancellationToken::new())
            .await
            .is_err()
    );
    let record = library
        .import(pm, &paths[3..], &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        record
            .files
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        names
    );
    let roex = c.entry("roextended").unwrap();
    let paths = (1..=4)
        .map(|n| fake_scs(inputs.path(), &format!("53FRX{n}.scs"), n))
        .collect::<Vec<_>>();
    let record = library
        .import(roex, &paths, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        record
            .files
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["53FRX4.scs", "53FRX3.scs", "53FRX2.scs", "53FRX1.scs"]
    );
}

#[test]
fn dependencies_connections_priorities_and_version_pins_are_resolved_together() {
    let mut c = catalog();
    c.maps[1].dependencies = vec!["base".into()];
    c.maps[2].after = vec!["addon".into()];
    c.connections=serde_json::from_value(serde_json::json!([{"when":["base","addon"],"add":["connector"],"verified":true,"versions":{"base":"1.0"},"note":"road connector"}])).unwrap();
    let mut s = set();
    s.selected = BTreeSet::from(["addon".into()]);
    let p = c.resolve(&s, &BTreeSet::new()).unwrap();
    assert_eq!(p.ordered, vec!["base", "addon", "connector"]);
    c.maps[0].release.version = "2.0".into();
    assert!(c.resolve(&s, &BTreeSet::new()).is_err());
}

#[test]
fn missing_dlcs_conflicts_wrong_game_standalone_and_cycles_block_plans() {
    let mut c = catalog();
    let s = set();
    c.maps[0].release.dlcs = vec!["dlc_east".into()];
    assert!(c.resolve(&s, &BTreeSet::new()).is_err());
    c.maps[0].release.dlcs.clear();
    c.maps[0].release.game_versions = vec!["1.60".into()];
    assert!(c.resolve(&s, &BTreeSet::new()).is_err());
    c.maps[0].release.game_versions = vec!["1.61".into()];
    c.maps[0].dependencies = vec!["addon".into()];
    c.maps[1].dependencies = vec!["base".into()];
    assert!(
        c.resolve(&s, &BTreeSet::new())
            .unwrap_err()
            .to_string()
            .contains("Цикл")
    );
    c.maps[1].dependencies.clear();
    c.maps[0].conflicts = vec!["addon".into()];
    assert!(c.resolve(&s, &BTreeSet::new()).is_err());
    c.maps[0].conflicts.clear();
    c.maps[1].module = "/map/island.mbd".into();
    assert!(c.resolve(&s, &BTreeSet::new()).is_err());
    assert!(maps::version_matches("1.61.1.9", "1.61"));
    assert!(!maps::version_matches("1.610", "1.61"));
}

#[test]
fn catalog_rejects_paths_invalid_patterns_duplicate_ids_and_untrusted_schemes() {
    let mut c = catalog();
    c.maps[0].id = "../escape".into();
    assert!(c.validate().is_err());
    c = catalog();
    c.maps[0].source.url = "file:///C:/secret".into();
    assert!(c.validate().is_err());
    c = catalog();
    c.maps[0].release.packages[0].pattern = "[".into();
    assert!(c.validate().is_err());
    c = catalog();
    c.maps[1].id = "base".into();
    assert!(c.validate().is_err());
    c = catalog();
    c.maps[0].module = "/map/../../escape.mbd".into();
    assert!(c.validate().is_err());
}

#[test]
fn source_parses_release_region_only_and_detects_markup_changes() {
    let m = entry("base");
    let page = "<aside>Current Version 99.0</aside><main>Current Version 1.2 <a href='/file.scs'>Download</a></main>";
    let state = source::parse(&m, page, &m.source.url).unwrap();
    assert_eq!(state.version.as_deref(), Some("1.2"));
    assert_eq!(state.links[0].1, "https://example.com/file.scs");
    assert!(source::parse(&m, "<h1>login required</h1>", &m.source.url).is_err());
}

#[test]
fn profile_order_encoding_and_updates_preserve_existing_progress_fields() {
    let bytes = profile::create(
        "Тест \"один\"",
        "/map/europe.mbd",
        &["base".into(), "addon".into()],
    )
    .unwrap();
    let text = profile::decode(&bytes).unwrap();
    assert!(text.contains("active_mods[0]: \"base|base\""));
    assert!(text.contains("version: 6\n"));
    assert!(text.contains("active_mods[1]: \"addon|addon\""));
    assert_eq!(profile::folder_name("А"), "D090");
    let text = text.replace("cached_experience: 0", "cached_experience: 9876");
    let changed =
        profile::update(text.as_bytes(), "/map/europe.mbd", &["connector".into()]).unwrap();
    let changed = String::from_utf8(changed).unwrap();
    assert!(changed.contains("cached_experience: 9876"));
    assert!(changed.contains("active_mods: 1"));
    assert!(!changed.contains("active_mods[1]"));
    assert!(profile::update(text.as_bytes(), "/map/island.mbd", &[]).is_err());
    assert!(profile::decode(b"ScsCbad").is_err());
    assert!(profile::decode(b"BSIIbad").is_err());
}

#[tokio::test]
async fn imports_are_immutable_and_shared_by_hard_links_between_environments() {
    let temp = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let l = Library::open(temp.path()).unwrap();
    let c = catalog();
    let record = import(&l, &c.maps[0], inputs.path(), 1).await;
    let s = set();
    let p = c.resolve(&s, &BTreeSet::new()).unwrap();
    let a = l.apply(&c, &s, &p, &CancellationToken::new()).unwrap();
    assert_eq!(a.packages[0].files[0].sha256, record.files[0].sha256);
    let mut other = s.clone();
    other.id = "second".into();
    l.apply(&c, &other, &p, &CancellationToken::new()).unwrap();
    let from = l.package_path("base", "1.0").unwrap().join("base.scs");
    let mounted = l
        .environment(&s.id)
        .unwrap()
        .join("home/Euro Truck Simulator 2/mod/base.scs");
    assert_eq!(fs::read(&from).unwrap(), fs::read(&mounted).unwrap());
    // A write via a hard link must appear at both environment paths. Copies fail this check.
    fs::write(&mounted, b"same underlying file").unwrap();
    assert_eq!(fs::read(from).unwrap(), b"same underlying file");
    assert_eq!(
        fs::read(
            l.environment("second")
                .unwrap()
                .join("home/Euro Truck Simulator 2/mod/base.scs")
        )
        .unwrap(),
        b"same underlying file"
    );
    assert!(l.apply(&c, &s, &p, &CancellationToken::new()).is_err()); // hash corruption
}

#[tokio::test]
async fn partial_import_keeps_def_filename_and_rejects_html() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let l = Library::open(root.path()).unwrap();
    let mut m = entry("base");
    m.release.packages = serde_json::from_value(
        serde_json::json!([{"pattern":"^base-map\\.scs$"},{"pattern":"^base-def.*\\.scs$"}]),
    )
    .unwrap();
    let map = fake_scs(inputs.path(), "base-map.scs", 1);
    assert!(
        l.import(&m, &[map], &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(l.incoming(&m).unwrap().join("base-map.scs").is_file());
    let def = fake_scs(inputs.path(), "base-def-personal-12345.scs", 2);
    let record = l
        .import(&m, &[def], &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(record.files[1].name, "base-def-personal-12345.scs");
    let mut bad = entry("bad");
    bad.release.packages[0].pattern = ".*".into();
    let path = inputs.path().join("bad.scs");
    fs::write(&path, b"<html>Not a mod</html>").unwrap();
    assert!(
        l.import(&bad, &[path], &CancellationToken::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn update_and_rollback_preserve_profile_and_save_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let l = Library::open(root.path()).unwrap();
    let mut c = catalog();
    import(&l, &c.maps[0], inputs.path(), 1).await;
    let s = set();
    let p = c.resolve(&s, &BTreeSet::new()).unwrap();
    l.apply(&c, &s, &p, &CancellationToken::new()).unwrap();
    let env = l.environment(&s.id).unwrap();
    let profile = env
        .join("home/Euro Truck Simulator 2/profiles")
        .join(profile::folder_name(&s.name));
    fs::create_dir(profile.join("save")).unwrap();
    fs::write(profile.join("save/career"), b"checkpoint-before-update").unwrap();
    c.maps[0].release.version = "2.0".into();
    import(&l, &c.maps[0], inputs.path(), 2).await;
    l.apply(&c, &s, &p, &CancellationToken::new()).unwrap();
    assert_eq!(
        fs::read(profile.join("save/career")).unwrap(),
        b"checkpoint-before-update"
    );
    fs::write(profile.join("save/career"), b"after-update").unwrap();
    let old = l.rollback(&s.id, &CancellationToken::new()).unwrap();
    assert_eq!(old.packages[0].version, "1.0");
    assert_eq!(
        fs::read(profile.join("save/career")).unwrap(),
        b"checkpoint-before-update"
    );
    assert_eq!(
        fs::read(env.join("home/Euro Truck Simulator 2/mod/base.scs")).unwrap()[4],
        1
    );
}

#[tokio::test]
async fn cancellation_name_collisions_and_parallel_operations_do_not_replace_active_build() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let l = Library::open(root.path()).unwrap();
    let c = catalog();
    import(&l, &c.maps[0], inputs.path(), 1).await;
    let s = set();
    let p = c.resolve(&s, &BTreeSet::new()).unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(l.apply(&c, &s, &p, &cancel).is_err());
    assert!(l.active(&s.id).is_err());
    let lock = l.lock().unwrap();
    assert!(l.lock().is_err());
    drop(lock);
    l.apply(&c, &s, &p, &CancellationToken::new()).unwrap();
    let before = fs::read(l.environment(&s.id).unwrap().join("active.json")).unwrap();
    let mut invalid = s.clone();
    invalid.name = String::new(); // existing profile identity remains stable
    let mut bad_plan = p.clone();
    bad_plan.ordered.push("base".into());
    assert!(
        l.apply(&c, &invalid, &bad_plan, &CancellationToken::new())
            .is_err()
    );
    assert_eq!(
        fs::read(l.environment(&s.id).unwrap().join("active.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn recovery_restores_an_interrupted_commit() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let l = Library::open(root.path()).unwrap();
    let c = catalog();
    let s = set();
    import(&l, &c.maps[0], inputs.path(), 1).await;
    let p = c.resolve(&s, &BTreeSet::new()).unwrap();
    l.apply(&c, &s, &p, &CancellationToken::new()).unwrap();
    let env = l.environment(&s.id).unwrap();
    let tx = env.join("transaction");
    fs::create_dir(&tx).unwrap();
    let profile_id = profile::folder_name(&s.name);
    let profile = env
        .join("home/Euro Truck Simulator 2/profiles")
        .join(&profile_id);
    fs::create_dir(tx.join("profile.previous")).unwrap();
    fs::copy(
        profile.join("profile.sii"),
        tx.join("profile.previous/profile.sii"),
    )
    .unwrap();
    fs::copy(env.join("active.json"), tx.join("active.previous.json")).unwrap();
    fs::rename(
        env.join("home/Euro Truck Simulator 2/mod"),
        tx.join("mod.previous"),
    )
    .unwrap();
    fs::create_dir(env.join("home/Euro Truck Simulator 2/mod")).unwrap();
    fs::write(profile.join("profile.sii"), b"interrupted write").unwrap();
    maps::write_json(&env.join("journal.json"),&serde_json::json!({"had_mod":true,"had_profile":true,"had_state":true,"profile":profile_id,"committed":false})).unwrap();
    l.recover(&s.id).unwrap();
    assert!(
        env.join("home/Euro Truck Simulator 2/mod/base.scs")
            .is_file()
    );
    assert!(
        fs::read_to_string(profile.join("profile.sii"))
            .unwrap()
            .starts_with("SiiNunit")
    );
    assert!(!env.join("journal.json").exists());
}

#[tokio::test]
async fn imports_nested_archives_and_reports_missing_multivolume_parts() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let l = Library::open(root.path()).unwrap();
    let m = entry("base");
    let data = inputs.path().join("data");
    fs::create_dir(&data).unwrap();
    let mut payload = b"SCS#".to_vec();
    let mut random = 0x1234abcd_u32;
    for _ in 0..8192 {
        random ^= random << 13;
        random ^= random >> 17;
        random ^= random << 5;
        payload.push(random as u8);
    }
    fs::write(data.join("base.scs"), &payload).unwrap();
    let archive = inputs.path().join("map.7z");
    let result = std::process::Command::new(ets_launcher::archive::helper().unwrap())
        .args(["a", "-t7z", "-v1k"])
        .arg(&archive)
        .arg(data.join("base.scs"))
        .output()
        .unwrap();
    assert!(result.status.success());
    let first = archive.with_extension("7z.001");
    assert!(
        l.import(&m, &[first], &CancellationToken::new())
            .await
            .is_err()
    );
    let remaining = fs::read_dir(inputs.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e != "001"))
        .collect::<Vec<_>>();
    let record = l
        .import(&m, &remaining, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(record.files[0].name, "base.scs");
    assert_eq!(
        fs::read(l.package_path("base", "1.0").unwrap().join("base.scs")).unwrap(),
        payload
    );
}

#[test]
fn encrypted_and_binary_profile_containers_are_decoded_without_a_helper_executable() {
    use cbc::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    use std::io::Write;
    let plain = profile::create("Codec test", "/map/europe.mbd", &["base".into()]).unwrap();
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&plain).unwrap();
    let compressed = encoder.finish().unwrap();
    // Public SCS save-format key, used only for this synthetic test fixture.
    let key = [
        0x2a, 0x5f, 0xcb, 0x17, 0x91, 0xd2, 0x2f, 0xb6, 0x02, 0x45, 0xb3, 0xd8, 0x36, 0x9e, 0xd0,
        0xb2, 0xc2, 0x73, 0x71, 0x56, 0x3f, 0xbf, 0x1f, 0x3c, 0x9e, 0xdf, 0x6b, 0x11, 0x82, 0x5a,
        0x5d, 0x0a,
    ];
    let iv = [7u8; 16];
    let mut buffer = vec![0; compressed.len() + 16];
    buffer[..compressed.len()].copy_from_slice(&compressed);
    let encrypted = cbc::Encryptor::<aes::Aes256>::new(&key.into(), &iv.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, compressed.len())
        .unwrap();
    let mut container = b"ScsC".to_vec();
    container.extend([0; 32]);
    container.extend(iv);
    container.extend((plain.len() as u32).to_le_bytes());
    container.extend(encrypted);
    assert_eq!(
        profile::decode(&container).unwrap(),
        String::from_utf8(plain).unwrap()
    );
    let changed = profile::update(&container, "/map/europe.mbd", &["updated".into()]).unwrap();
    assert!(
        String::from_utf8(changed)
            .unwrap()
            .contains("updated|updated")
    );
    // An empty, valid BSII v1 schema stream. It exercises the binary decoder.
    let mut binary = b"BSII".to_vec();
    binary.extend(1u32.to_le_bytes());
    binary.extend(0u32.to_le_bytes());
    binary.push(0);
    binary.extend(1u32.to_le_bytes());
    assert!(profile::decode(&binary).unwrap().starts_with("SiiNunit"));
}

#[tokio::test]
async fn unpacked_folder_is_linked_and_uses_its_complete_directory_name_as_mod_id() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let l = Library::open(root.path()).unwrap();
    let mut c = catalog();
    c.maps[0].release.packages[0].pattern = "^base[.]folder$".into();
    let folder = inputs.path().join("base.folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("manifest.sii"), b"fixture").unwrap();
    let record = l
        .import(&c.maps[0], &[folder], &CancellationToken::new())
        .await
        .unwrap();
    assert!(record.files[0].directory);
    assert!(!record.files[0].sha256.is_empty());
    let s = set();
    let p = c.resolve(&s, &BTreeSet::new()).unwrap();
    l.apply(&c, &s, &p, &CancellationToken::new()).unwrap();
    let home = l
        .environment(&s.id)
        .unwrap()
        .join("home/Euro Truck Simulator 2");
    assert_eq!(
        fs::read(home.join("mod/base.folder/manifest.sii")).unwrap(),
        b"fixture"
    );
    let text = fs::read_to_string(
        home.join("profiles")
            .join(profile::folder_name(&s.name))
            .join("profile.sii"),
    )
    .unwrap();
    assert!(text.contains("base.folder|base.folder"));
    fs::write(
        l.package_path("base", "1.0")
            .unwrap()
            .join("base.folder/manifest.sii"),
        b"modified",
    )
    .unwrap();
    assert!(l.apply(&c, &s, &p, &CancellationToken::new()).is_err());
}
