use ets_launcher::maps::{
    self, Catalog, SourceState,
    geography::{self, Area, Coverage},
};
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn concave_coastlines_clip_and_triangulate_without_filling_bays() {
    let polygon = vec![[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [2.0, 1.0], [0.0, 4.0]];
    assert!(!geography::contains(&polygon, [2.0, 3.0]));
    assert!(geography::contains(&polygon, [0.5, 1.0]));
    let triangles = geography::triangles(&polygon);
    assert_eq!(triangles.len(), 9);
    let area: f64 = triangles
        .chunks(3)
        .map(|t| {
            let a = polygon[t[0] as usize];
            let b = polygon[t[1] as usize];
            let c = polygon[t[2] as usize];
            ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() / 2.0
        })
        .sum();
    assert!((area - 10.0).abs() < 1e-8);
    let clipped = geography::clip(&polygon, [1.0, 0.5, 3.0, 2.0]);
    assert!(
        clipped
            .iter()
            .all(|p| p[0] >= 1.0 && p[0] <= 3.0 && p[1] >= 0.5 && p[1] <= 2.0)
    );
    assert!(!geography::triangles(&clipped).is_empty());
}

#[test]
fn geographic_data_covers_every_real_mod_and_all_ten_dlcs() {
    let countries = geography::countries().unwrap();
    let layers = geography::game_layers().unwrap();
    assert_eq!(
        layers.iter().filter(|l| l.id.starts_with("dlc_")).count(),
        10
    );
    let catalog = Catalog::bundled().unwrap();
    for entry in catalog.maps.iter().filter(|m| !m.support) {
        let coverage = entry.coverage.as_ref().unwrap();
        coverage.validate().unwrap();
        if coverage.fictional {
            assert!(coverage.areas.is_empty());
        } else {
            assert!(!coverage.areas.is_empty());
        }
        for area in &coverage.areas {
            assert!(
                countries.iter().any(|c| c.id == area.country),
                "Missing {} for {}",
                area.country,
                entry.id
            );
        }
    }
    let mut vertices = 0;
    for country in countries {
        for ring in country.polygons {
            let ring = geography::clip(&ring, [-26.0, 23.0, 95.0, 73.0]);
            let triangles = geography::triangles(&ring);
            assert!(triangles.iter().all(|i| (*i as usize) < ring.len()));
            vertices += ring.len();
        }
    }
    assert!(vertices > 1000);
    let invalid = Coverage {
        summary: "Test".into(),
        source: "https://example.com".into(),
        fictional: false,
        areas: vec![Area {
            country: "RUS".into(),
            bounds: Some([30.0, 50.0, f64::NAN, 60.0]),
            rework: false,
        }],
    };
    assert!(invalid.validate().is_err());
}

#[test]
fn latest_download_button_wins_over_history_and_size_comes_from_the_site() {
    let mut entry = Catalog::bundled()
        .unwrap()
        .entry("more-european-cities")
        .unwrap()
        .clone();
    entry.source.selector = "body".into();
    let html = r#"<body><downloadbutton :size="1" :download_url="'/download/old'"></downloadbutton>
        Current Version 9.3 Compatibility 1.62 Last Updated 2026-10-24
        <downloadbutton :size="123456789" :download_url="'/maps/current/download/latest'"></downloadbutton></body>"#;
    let state = maps::source::parse(&entry, html, "https://example.com/project").unwrap();
    assert_eq!(state.version.as_deref(), Some("9.3"));
    assert_eq!(state.game_versions, vec!["1.62"]);
    assert_eq!(state.download_size, Some(123456789));
    assert_eq!(
        state.download_page.as_deref(),
        Some("https://example.com/maps/current/download/latest")
    );
}

#[test]
fn live_versions_update_individual_maps_but_do_not_inherit_old_connector_pins() {
    let catalog = Catalog::bundled().unwrap();
    let states = BTreeMap::from([(
        "rusmap".into(),
        SourceState {
            version: Some("9.0".into()),
            game_versions: vec!["1.61".into()],
            download_size: Some(456),
            ..Default::default()
        },
    )]);
    let live = catalog.with_sources(&states);
    assert_eq!(live.entry("rusmap").unwrap().release.version, "9.0");
    assert_eq!(catalog.entry("rusmap").unwrap().release.version, "2.61");
    let dlcs = live
        .maps
        .iter()
        .flat_map(|m| m.release.dlcs.clone())
        .collect();
    let mut set = maps::ModSet {
        id: "qa".into(),
        name: "QA".into(),
        game_version: "1.61".into(),
        selected: BTreeSet::from(["rusmap".into()]),
    };
    assert!(live.resolve(&set, &dlcs).is_ok());
    set.selected.insert("promods-europe".into());
    assert!(
        live.resolve(&set, &dlcs)
            .unwrap_err()
            .to_string()
            .contains("проверка стыковки")
    );
}

#[test]
fn author_steppe_priority_and_all_45_pair_plans_are_checked() {
    let catalog = Catalog::bundled().unwrap();
    let dlcs = catalog
        .maps
        .iter()
        .flat_map(|m| m.release.dlcs.clone())
        .collect();
    let set = maps::ModSet {
        id: "qa".into(),
        name: "QA".into(),
        game_version: "1.61".into(),
        selected: BTreeSet::from(["promods-great-steppe".into(), "promods-middle-east".into()]),
    };
    let plan = catalog.resolve(&set, &dlcs).unwrap();
    let index = |id| plan.ordered.iter().position(|s| s == id).unwrap();
    assert!(index("promods-great-steppe") < index("promods-europe"));
    assert!(index("promods-europe") < index("promods-middle-east"));
    let report = maps::audit::combinations(&catalog);
    assert_eq!(report.len(), 45);
    let unknown = report
        .iter()
        .find(|r| {
            r.maps.contains(&"poland-rebuilding".into())
                && r.maps.contains(&"promods-maghreb".into())
        })
        .unwrap();
    assert!(!unknown.allowed);
    let builds = maps::audit::all_builds(&catalog).unwrap();
    assert_eq!(builds.len(), 1023);
    assert!(!builds.iter().find(|b| b.maps.len() == 10).unwrap().allowed);
    assert!(
        builds
            .iter()
            .filter(|b| b.allowed && b.maps.len() > 1)
            .all(|b| !b.maps.contains(&"grand-utopia".into())
                && !b.maps.contains(&"sibirmap".into()))
    );
}

#[test]
fn generated_def_keeps_the_author_filename_and_rejects_paths() {
    let filename = maps::source::author_filename(
        Some("attachment; filename=\"z1_PL_Rebuilding_3.0.1_def_589cfaea.scs\""),
        "fallback.zip",
    )
    .unwrap();
    let catalog = Catalog::bundled().unwrap();
    let entry = catalog.entry("poland-rebuilding").unwrap();
    assert!(
        entry
            .release
            .packages
            .iter()
            .any(|p| regex::Regex::new(&p.pattern).unwrap().is_match(&filename))
    );
    assert!(
        maps::source::author_filename(
            Some("attachment; filename=\"../outside.scs\""),
            "fallback.zip"
        )
        .is_err()
    );
}

#[test]
fn connector_author_versions_can_advance_only_an_exact_matching_pair() {
    let catalog = Catalog::bundled().unwrap();
    let mut states = BTreeMap::new();
    for (id, version) in [
        ("promods-europe", "2.85"),
        ("rusmap", "2.62"),
        ("promods-rusmap-connector", "2.85-2.62-1.0"),
    ] {
        states.insert(
            id.into(),
            SourceState {
                version: Some(version.into()),
                game_versions: vec!["1.61".into()],
                ..Default::default()
            },
        );
    }
    let dlcs = catalog
        .maps
        .iter()
        .flat_map(|m| m.release.dlcs.clone())
        .collect();
    let set = maps::ModSet {
        id: "qa".into(),
        name: "QA".into(),
        game_version: "1.61".into(),
        selected: BTreeSet::from(["promods-europe".into(), "rusmap".into()]),
    };
    assert!(catalog.with_sources(&states).resolve(&set, &dlcs).is_ok());
    states.get_mut("rusmap").unwrap().version = Some("2.63".into());
    assert!(catalog.with_sources(&states).resolve(&set, &dlcs).is_err());
    states.get_mut("promods-rusmap-connector").unwrap().version = Some("unknown format".into());
    assert!(catalog.with_sources(&states).resolve(&set, &dlcs).is_err());
}

#[tokio::test]
async fn author_grand_utopia_filename_is_accepted_but_unknown_future_packages_block_import() {
    let catalog = Catalog::bundled().unwrap();
    let entry = catalog.entry("grand-utopia").unwrap();
    let root = tempfile::tempdir().unwrap();
    let inputs = tempfile::tempdir().unwrap();
    let make = |name: &str| {
        let path = inputs.path().join(name);
        let mut bytes = b"SCS#".to_vec();
        bytes.extend([1u8; 64]);
        std::fs::write(&path, bytes).unwrap();
        path
    };
    let file = make("GU - Grand Utopia (v1.20d).scs");
    let unknown = make("New_required_assets.scs");
    let library = maps::library::Library::open(root.path()).unwrap();
    let error = library
        .import(
            entry,
            &[file, unknown],
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("без правил загрузки"));
    assert!(library.record(&entry.id, &entry.release.version).is_err());
}
