use ets_launcher::{
    download::Transfer,
    game::Game,
    quper::{self, Combo, FileRow, Plan, library::Library, source},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

fn combo() -> Combo {
    source::parse_catalog(include_str!("fixtures/quper-catalog.js"))
        .unwrap()
        .into_iter()
        .find(|c| c.id == "quper-map-161")
        .unwrap()
}
fn plan() -> Plan {
    Plan {
        combo: combo(),
        order: vec!["base".into(), "addon".into()],
        files: vec![
            FileRow {
                number: 1,
                name: "Addon".into(),
                description: "".into(),
                mod_id: "addon".into(),
                urls: vec![],
                required: true,
                excluded_boosty: false,
            },
            FileRow {
                number: 2,
                name: "Base".into(),
                description: "".into(),
                mod_id: "base".into(),
                urls: vec![],
                required: true,
                excluded_boosty: false,
            },
        ],
    }
}
fn scs(root: &Path, name: &str, byte: u8) -> PathBuf {
    let path = root.join(name);
    let mut bytes = b"SCS#".to_vec();
    bytes.extend([byte; 64]);
    fs::write(&path, bytes).unwrap();
    path
}
fn transfer() -> ets_launcher::download::SharedTransfer {
    Arc::new(Mutex::new(Transfer::default()))
}

#[test]
fn real_site_objects_supply_images_versions_both_games_and_exclude_boosty() {
    let combos = source::parse_catalog(include_str!("fixtures/quper-catalog.js")).unwrap();
    assert_eq!(combos.len(), 3);
    let current = combos.iter().find(|c| c.id == "quper-map-161").unwrap();
    assert_eq!(current.version, "1.7.1");
    assert!(current.automatic_order);
    assert_eq!(
        current.preview,
        "https://qupersimulator.ru/img/combos/161/ets2-quper/quper-preview.jpg"
    );
    assert!(combos.iter().any(|c| c.game == Game::Ats));
    assert!(combos.iter().any(|c| c.game_version == "1.58"));
    assert!(!combos.iter().any(|c| c.id.contains("boosty")));
    let scripts =
        source::script_urls(r#"<script type="module" src="/assets/index-new-hash.js"></script>"#)
            .unwrap();
    assert_eq!(
        scripts,
        ["https://qupersimulator.ru/assets/index-new-hash.js"]
    );
    assert!(
        source::script_urls(r#"<script src="https://other.example/evil.js"></script>"#).is_err()
    );
}

#[test]
fn sheet_export_keeps_the_selected_tab_and_rejects_foreign_sources() {
    assert!(
        !source::sheet_url("https://docs.google.com/spreadsheets/d/public-id/edit?usp=sharing")
            .unwrap()
            .query_pairs()
            .any(|(k, _)| k == "gid")
    );
    for suffix in ["edit?gid=1593428445#gid=1593428445", "edit#gid=1593428445"] {
        let url = source::sheet_url(&format!(
            "https://docs.google.com/spreadsheets/d/public-id/{suffix}"
        ))
        .unwrap();
        assert!(
            url.query_pairs()
                .any(|(k, v)| k == "gid" && v == "1593428445")
        );
        assert!(
            url.query_pairs()
                .any(|(k, v)| k == "tqx" && v == "out:html")
        );
    }
    assert!(
        source::sheet_url("https://docs.google.com/spreadsheets/d/id/edit?gid=1%26gid%3D2")
            .is_err()
    );
    for url in [
        "https://boosty.to/quper",
        "https://a.boosty.to/post",
        "https://user:pass@qupersimulator.ru/",
        "file:///C:/secrets",
    ] {
        assert!(source::public_url(url).is_err(), "{url}");
    }
}

#[test]
fn workbook_without_gid_selects_named_combo_and_never_the_front_page_or_boosty_tab() {
    let mut combo = combo();
    combo.title = "MEGA MAP COMBO".into();
    let index = r#"items.push({name: "ССЫЛКИ НА СБОРКИ", pageUrl:"https://example", gid:"99"});items.push({name:"ETS2 MEGA BOOSTY MAP COMBO", pageUrl:"https://example", gid:"20"});items.push({name:"ETS2 MEGA MAP COMBO", pageUrl:"https://example", gid:"1631626983"});"#;
    assert_eq!(source::named_sheet(index, &combo).unwrap(), "1631626983");
    combo.title = "Unknown Combo".into();
    assert!(source::named_sheet(index, &combo).is_err());
}

#[test]
fn table_reads_real_rows_merged_archive_sources_and_excludes_paid_links() {
    let rows = source::parse_table(include_str!("fixtures/quper-files.html")).unwrap();
    let file = rows.iter().find(|r| r.number == 2).unwrap();
    assert_eq!(file.mod_id, "RealisticAddonFerryQuper");
    assert!(file.urls[0].starts_with("https://sharemods.com/"));
    let boosty = rows.iter().find(|r| r.number == 44).unwrap();
    assert!(boosty.excluded_boosty && boosty.urls.is_empty());
    let def = rows.iter().find(|r| r.number == 45).unwrap();
    let assets = rows.iter().find(|r| r.number == 46).unwrap();
    assert_eq!(def.urls, assets.urls);
    assert_ne!(def.mod_id, assets.mod_id);
    assert!(rows.iter().all(|r| !r.name.contains("Новое в сборке")));
}

#[test]
fn profile_order_is_complete_contiguous_and_preserves_author_identifiers() {
    let order = source::parse_order(" active_mods: 2\n active_mods[0]: \"base|Base title\"\n active_mods[1]: \"addon space|Title\"\n").unwrap();
    assert_eq!(order, ["base", "addon space"]);
    for bad in [
        "active_mods: 2\n active_mods[0]: \"base|Base\"",
        "active_mods: 1\n active_mods[1]: \"base|Base\"",
        "active_mods: 1\n active_mods[0]: \"../escape|Base\"",
    ] {
        assert!(source::parse_order(bad).is_err());
    }
}

#[test]
fn public_forms_keep_tokens_and_timers_and_ignore_advertising_and_login() {
    use quper::files::{Step, page_step};
    let page = reqwest::Url::parse("https://sharemods.com/id/file.scs.html").unwrap();
    let html = r#"<a href="/premium">Premium</a><form><input type="password" name="password"></form><script>countdown=15;</script><form method="POST"><input type="hidden" name="op" value="download2"><input type="hidden" name="rand" value="token"><input type="hidden" name="method_premium" value=""><input type="submit" name="method_premium" value="Premium"><input type="submit" name="method_free" value="Free Download"></form>"#;
    let Step::Form(form) = page_step(&page, html).unwrap() else {
        panic!("expected free form");
    };
    assert!(form.post);
    assert!(!form.fields.iter().any(|(key, _)| key == "method_premium"));
    assert_eq!(form.wait, 15);
    assert!(form.fields.contains(&("rand".into(), "token".into())));
    assert!(
        page_step(
            &page,
            &html.replace(
                "<form method=",
                "<form action=\"https://boosty.to/x\" method="
            )
        )
        .is_err()
    );
    assert!(page_step(&page, &html.replace("name=\"rand\"", "name=\"captcha\"")).is_err());
    let page = reqwest::Url::parse("https://truckymods.io/project").unwrap();
    let Step::Links(links) = page_step(&page, r#"<downloadbutton :download_url="'https://truckymods.io/project/download/old'"></downloadbutton><downloadbutton :download_url="'https://truckymods.io/project/download/latest'"></downloadbutton>"#).unwrap() else { panic!() };
    assert_eq!(links, ["https://truckymods.io/project/download/latest"]);
}

#[test]
fn downloads_preserve_author_filename_and_reject_html_and_unsafe_names() {
    use quper::files::{allowed_filename, filename};
    let url = reqwest::Url::parse("https://files.example/download?id=123").unwrap();
    assert_eq!(
        filename(
            Some("attachment; filename*=UTF-8''%D0%9A%D0%B0%D1%80%D1%82%D0%B0.scs"),
            &url,
            None
        )
        .unwrap(),
        "Карта.scs"
    );
    for name in [
        "../evil.scs",
        "C:\\evil.scs",
        "ads.exe",
        "error.html",
        "CON.scs",
    ] {
        assert!(!allowed_filename(name), "{name}");
    }
    assert!(
        filename(
            Some("attachment; filename=../evil.scs"),
            &url,
            Some("safe.scs")
        )
        .is_err()
    );
}

#[tokio::test]
async fn incomplete_downloads_resume_into_an_isolated_ordered_profile_without_losing_career() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let plan = plan();
    let cancel = CancellationToken::new();
    let report = library
        .import(
            &plan,
            &[scs(inputs.path(), "base.scs", 1)],
            &transfer(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(report.missing, ["addon"]);
    assert!(report.home.is_none());
    assert!(library.ready_home(&plan).unwrap().is_none());
    let report = library
        .import(
            &plan,
            &[scs(inputs.path(), "addon.scs", 2)],
            &transfer(),
            &cancel,
        )
        .await
        .unwrap();
    assert!(report.missing.is_empty());
    let home = report.home.unwrap();
    let docs = home.join("Euro Truck Simulator 2");
    assert!(docs.join("mod/base.scs").is_file());
    assert!(docs.join("mod/addon.scs").is_file());
    let profile_dir = fs::read_dir(docs.join("profiles"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let profile = fs::read_to_string(profile_dir.join("profile.sii")).unwrap();
    assert!(profile.contains("active_mods[0]: \"base|base\""));
    assert!(profile.contains("active_mods[1]: \"addon|addon\""));
    fs::write(profile_dir.join("career-save.fixture"), "keep my progress").unwrap();
    library.download(&plan, &transfer(), &cancel).await.unwrap();
    assert_eq!(
        fs::read_to_string(profile_dir.join("career-save.fixture")).unwrap(),
        "keep my progress"
    );
    assert_eq!(library.ready_home(&plan).unwrap(), Some(home));
}

#[tokio::test]
async fn ats_uses_its_own_module_home_and_profile_and_never_ets2() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let mut plan = plan();
    plan.combo.game = Game::Ats;
    let report = library
        .import(
            &plan,
            &[
                scs(inputs.path(), "base.scs", 1),
                scs(inputs.path(), "addon.scs", 2),
            ],
            &transfer(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let home = report.home.unwrap();
    assert!(!home.join("Euro Truck Simulator 2").exists());
    let profile_dir = fs::read_dir(home.join("American Truck Simulator/profiles"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let profile = fs::read_to_string(profile_dir.join("profile.sii")).unwrap();
    assert!(profile.contains("/map/usa.mbd"));
    assert!(profile.contains("brand: kenworth"));
}

#[tokio::test]
async fn cancellation_and_a_conflicting_archive_do_not_mark_a_build_ready() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let plan = plan();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        library
            .import(
                &plan,
                &[scs(inputs.path(), "base.scs", 1)],
                &transfer(),
                &cancel
            )
            .await
            .is_err()
    );
    assert_eq!(library.missing(&plan).unwrap().len(), 2);
    let cancel = CancellationToken::new();
    library
        .import(
            &plan,
            &[inputs.path().join("base.scs")],
            &transfer(),
            &cancel,
        )
        .await
        .unwrap();
    scs(inputs.path(), "base.scs", 9);
    assert!(
        library
            .import(
                &plan,
                &[inputs.path().join("base.scs")],
                &transfer(),
                &cancel
            )
            .await
            .is_err()
    );
    assert!(library.ready_home(&plan).unwrap().is_none());
    let release = library.release(&plan.combo).unwrap();
    assert_eq!(fs::read(release.join("packages/base.scs")).unwrap()[4], 1);
}

#[tokio::test]
async fn one_outer_archive_supplies_multiple_packages_and_keeps_original_filenames() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let plan = plan();
    scs(inputs.path(), "base.scs", 1);
    scs(inputs.path(), "addon.scs", 2);
    let archive = inputs.path().join("combo.zip");
    let status = std::process::Command::new(ets_launcher::archive::helper().unwrap())
        .args(["a", "-tzip", "-bso0", "-bsp0"])
        .arg(&archive)
        .args(["base.scs", "addon.scs"])
        .current_dir(inputs.path())
        .status()
        .unwrap();
    assert!(status.success());
    let report = library
        .import(&plan, &[archive], &transfer(), &CancellationToken::new())
        .await
        .unwrap();
    assert!(report.missing.is_empty() && report.home.is_some());
}

#[tokio::test]
async fn local_profile_lists_the_build_in_order_and_never_replaces_other_mods() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let documents = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let plan = plan();
    let cancel = CancellationToken::new();
    let docs = documents.path().join("Euro Truck Simulator 2");
    fs::create_dir_all(docs.join("mod")).unwrap();
    // The player already has a different mod with the same file name.
    fs::write(docs.join("mod/addon.scs"), b"someone else's mod").unwrap();
    fs::write(
        docs.join("config.cfg"),
        "uset g_lang \"ru\"\nuset r_buffer_page_size \"10\"\n",
    )
    .unwrap();
    library
        .import(
            &plan,
            &[
                scs(inputs.path(), "base.scs", 1),
                scs(inputs.path(), "addon.scs", 2),
            ],
            &transfer(),
            &cancel,
        )
        .await
        .unwrap();

    let created = library
        .create_local_profile(&plan, documents.path(), &transfer(), &cancel)
        .unwrap();
    assert!(!created.updated);
    assert_eq!(created.mods, 2);
    assert_eq!(
        fs::read(docs.join("mod/addon.scs")).unwrap(),
        b"someone else's mod"
    );
    assert!(docs.join("mod/base.scs").is_file());
    assert!(docs.join("mod/addon_quper1.scs").is_file());
    let profile = fs::read_to_string(created.profile.join("profile.sii")).unwrap();
    assert!(created.profile.starts_with(docs.join("profiles")));
    assert!(profile.contains("profile_name: \"Quper QUPER MAP COMBO\""));
    assert!(profile.contains("active_mods: 2"));
    assert!(profile.contains("active_mods[0]: \"base|base\""));
    assert!(profile.contains("active_mods[1]: \"addon_quper1|addon_quper1\""));
    let config = fs::read_to_string(docs.join("config.cfg")).unwrap();
    assert!(config.contains("uset g_lang \"ru\""));
    assert!(config.contains("uset r_buffer_page_size \"30\""));
    assert!(!config.contains("\"10\""));

    // Running it again reuses the files and keeps the career.
    fs::write(created.profile.join("career.fixture"), "keep my progress").unwrap();
    let again = library
        .create_local_profile(&plan, documents.path(), &transfer(), &cancel)
        .unwrap();
    assert!(again.updated);
    assert_eq!(again.copied, 0);
    assert!(!docs.join("mod/addon_quper2.scs").exists());
    assert_eq!(
        fs::read_to_string(again.profile.join("career.fixture")).unwrap(),
        "keep my progress"
    );
    assert!(again.profile.join("profile.sii.launcher-backup").is_file());
    let profile = fs::read_to_string(again.profile.join("profile.sii")).unwrap();
    assert!(profile.contains("active_mods[0]: \"base|base\""));
    assert!(profile.contains("active_mods[1]: \"addon_quper1|addon_quper1\""));
}

#[tokio::test]
async fn local_profile_needs_the_whole_build() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let documents = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let plan = plan();
    let cancel = CancellationToken::new();
    library
        .import(
            &plan,
            &[scs(inputs.path(), "base.scs", 1)],
            &transfer(),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        library
            .create_local_profile(&plan, documents.path(), &transfer(), &cancel)
            .is_err()
    );
    assert!(
        !documents
            .path()
            .join("Euro Truck Simulator 2/profiles")
            .exists()
    );
}

#[tokio::test]
async fn partial_profile_lists_only_the_downloaded_mods() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let documents = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let plan = plan();
    let cancel = CancellationToken::new();
    // Nothing downloaded yet: there is no profile to make.
    assert!(
        library
            .create_partial_profile(&plan, documents.path(), &transfer(), &cancel)
            .is_err()
    );
    library
        .import(
            &plan,
            &[scs(inputs.path(), "base.scs", 1)],
            &transfer(),
            &cancel,
        )
        .await
        .unwrap();
    let created = library
        .create_partial_profile(&plan, documents.path(), &transfer(), &cancel)
        .unwrap();
    assert_eq!((created.mods, created.skipped), (1, 1));
    let profile = fs::read_to_string(created.profile.join("profile.sii")).unwrap();
    assert!(profile.contains("active_mods: 1"));
    assert!(profile.contains("active_mods[0]: \"base|base\""));

    // The missing mod imported later joins the same profile, in order.
    library
        .import(
            &plan,
            &[scs(inputs.path(), "addon.scs", 2)],
            &transfer(),
            &cancel,
        )
        .await
        .unwrap();
    let full = library
        .create_local_profile(&plan, documents.path(), &transfer(), &cancel)
        .unwrap();
    assert!(full.updated);
    assert_eq!((full.mods, full.skipped), (2, 0));
    let profile = fs::read_to_string(full.profile.join("profile.sii")).unwrap();
    assert!(profile.contains("active_mods: 2"));
}

#[tokio::test]
async fn ats_local_profile_uses_the_ats_folder_and_module() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let documents = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let mut plan = plan();
    plan.combo.game = Game::Ats;
    let cancel = CancellationToken::new();
    library
        .import(
            &plan,
            &[
                scs(inputs.path(), "base.scs", 1),
                scs(inputs.path(), "addon.scs", 2),
            ],
            &transfer(),
            &cancel,
        )
        .await
        .unwrap();
    let created = library
        .create_local_profile(&plan, documents.path(), &transfer(), &cancel)
        .unwrap();
    assert!(
        created
            .profile
            .starts_with(documents.path().join("American Truck Simulator/profiles"))
    );
    assert!(!documents.path().join("Euro Truck Simulator 2").exists());
    let profile = fs::read_to_string(created.profile.join("profile.sii")).unwrap();
    assert!(profile.contains("/map/usa.mbd"));
    assert!(profile.contains("brand: kenworth"));
    assert_eq!(
        fs::read_to_string(documents.path().join("American Truck Simulator/config.cfg")).unwrap(),
        "uset r_buffer_page_size \"30\"\n"
    );
}

#[tokio::test]
async fn a_numbered_browser_copy_counts_as_the_mod_it_was_saved_from() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let plan = plan();
    let report = library
        .import(
            &plan,
            &[scs(inputs.path(), "addon(1).scs", 2)],
            &transfer(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.missing, ["base"]);
    let state = library.state(&plan.combo).unwrap();
    assert_eq!(state.packages["addon"].filename, "addon.scs");
    assert!(!state.packages.contains_key("addon(1)"));
}

#[tokio::test]
async fn a_protected_scs_package_is_used_as_it_is() {
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let library = Library::open(root.path()).unwrap();
    let plan = plan();
    // Project Russia ships its package in its own "AEM!" container.
    let path = inputs.path().join("base.scs");
    let mut bytes = b"AEM!".to_vec();
    bytes.extend([0u8; 64]);
    fs::write(&path, bytes).unwrap();
    let report = library
        .import(&plan, &[path], &transfer(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.missing, ["addon"]);
}
