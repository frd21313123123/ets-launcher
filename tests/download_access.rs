use ets_launcher::maps::{
    self,
    access::{self, DownloadAccess},
    source,
};
use tokio_util::sync::CancellationToken;

#[test]
fn native_file_button_is_limited_to_the_prepared_public_host_page() {
    assert!(access::public_file_script("https://modsfire.com/download/AbC123/token").is_some());
    for url in [
        "https://modsfire.com/AbC123",
        "https://modsfire.com/login",
        "https://modsfire.com/get-premium",
        "https://modsfire.com/download/AbC123/",
        "https://modsfire.com/download/AbC123/token/extra",
        "https://modsfire.com.evil.test/download/AbC123/token",
        "http://modsfire.com/download/AbC123/token",
    ] {
        assert!(access::public_file_script(url).is_none(), "{url}");
    }
}

#[test]
fn public_download_does_not_become_members_only_due_to_login_or_premium_adverts() {
    let html = r#"<body><a href='/login'>Login</a><aside>Premium account required for one-click install</aside>
        <downloadbox :project_id="3730" :build_id="15227" download_permitted_to="all"></downloadbox></body>"#;
    let (project, build, _) = access::trucky_request(html).unwrap();
    assert_eq!((project, build), (3730, 15227));
    let help = access::browser_help("https://modsfire.com/current");
    assert!(help.contains("без регистрации"));
    assert!(!access::browser_help("https://example.com/file").contains("Войдите"));
    assert!(!access::browser_help("https://modsfire.com.evil.test/file").contains("Generate Link"));
}

#[test]
fn only_explicit_auth_is_login_while_layout_changes_and_rate_limits_remain_distinct() {
    let login = access::trucky_request("<authenticationbox></authenticationbox>").unwrap_err();
    assert_eq!(access::from_error(&login), DownloadAccess::LoginRequired);
    let login =
        access::trucky_request(r#"<downloadbox download_permitted_to="registered"></downloadbox>"#)
            .unwrap_err();
    assert_eq!(access::from_error(&login), DownloadAccess::LoginRequired);
    let changed =
        access::trucky_request("<h1>Download</h1><a href='/login'>Login</a>").unwrap_err();
    assert_eq!(access::from_error(&changed), DownloadAccess::PageChanged);
    let unknown =
        access::trucky_request(r#"<downloadbox :project_id="1" :build_id="2"></downloadbox>"#)
            .unwrap_err();
    assert_eq!(access::from_error(&unknown), DownloadAccess::PageChanged);
    for (payload, expected) in [
        (
            serde_json::json!({"success":false,"rateLimited":true}),
            DownloadAccess::RateLimited,
        ),
        (
            serde_json::json!({"success":false}),
            DownloadAccess::Unavailable,
        ),
        (
            serde_json::json!({"success":false,"requiresAuth":true}),
            DownloadAccess::LoginRequired,
        ),
    ] {
        assert_eq!(
            access::from_error(&access::trucky_payload(&payload).unwrap_err()),
            expected
        );
    }
}

#[tokio::test]
async fn forbidden_http_response_is_browser_verification_not_registration() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0; 2048];
        let _ = stream.read(&mut buf).await;
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let error = reqwest::get(format!("http://{addr}/file"))
        .await
        .unwrap()
        .error_for_status()
        .unwrap_err();
    assert_eq!(
        access::from_error(&error.into()),
        DownloadAccess::BrowserVerification
    );
    server.await.unwrap();
}

#[test]
fn mirror_selection_excludes_old_releases_and_unrelated_posts_and_deduplicates() {
    let html = r#"<article id='author'><a href='https://sharemods.com/old/PL_Rebuilding_3.0_all.7z.html'>Old</a>
      <a href='https://sharemods.com/current/PL_Rebuilding_3.0.1_all.7z.html'>Free</a>
      <a href='https://sharemods.com/current/PL_Rebuilding_3.0.1_all.7z.html'>Same</a></article>
      <aside><a href='https://sharemods.com/comment/PL_Rebuilding_3.0.1_all.7z.html'>Comment</a></aside>"#;
    let links = source::mirror_links(
        html,
        "https://polandrebuilding.pl/download",
        "#author a[href]",
        r"^https://sharemods\.com/[^/]+/PL_Rebuilding_3\.0\.1_all\.7z\.html$",
    )
    .unwrap();
    assert_eq!(links.len(), 1);
    assert!(source::is_public_host(&links[0].url));
    assert!(!source::is_public_host(
        "https://sharemods.com.evil.test/file"
    ));
}

#[test]
fn public_mirrors_come_from_the_current_release_row_only() {
    let html = r#"<table><tr><td><a href='https://modsfire.com/current'>Download</a></td><td><a href='https://sharemods.com/id/PL_Rebuilding_3.0.1_all.7z.html'>Mirror</a></td></tr>
      <tr><td><a href='https://modsfire.com/old'>Old</a></td><td><a href='https://sharemods.com/old/PL_Rebuilding_3.0_all.7z.html'>Old</a></td></tr></table>"#;
    let links = source::mirror_links_in_row(
        html,
        "https://polandrebuilding.pl/download",
        "a[href]",
        r"^https://sharemods\.com/[^/]+/PL_Rebuilding_3\.0\.1_all\.7z\.html$",
        true,
    )
    .unwrap();
    assert_eq!(links.len(), 2);
    assert!(
        links
            .iter()
            .any(|a| a.url == "https://modsfire.com/current")
    );
    assert!(!links.iter().any(|a| a.url.ends_with("/old")));
}

#[test]
fn old_forum_downloads_are_excluded_from_both_metadata_and_download_selection() {
    let catalog = maps::Catalog::bundled().unwrap();
    let mut entry = catalog.entry("rusmap").unwrap().clone();
    entry.source.selector = "main".into();
    entry.source.version_pattern = r"Version\s*(\d+\.\d+)".into();
    entry.source.stop_before = Some("Old versions:".into());
    let html = r#"<main>Version 2.61 <a class='postlink' href='https://example.com/current.scs'>Current</a><b>Old versions:</b><a class='postlink' href='https://modsfire.com/old'>Old</a></main>"#;
    let state = source::parse(&entry, html, "https://forum.scssoft.com/topic").unwrap();
    assert_eq!(state.version.as_deref(), Some("2.61"));
    assert!(state.download_page.is_none());
    assert!(!state.links.iter().any(|(_, url)| url.contains("/old")));
}

#[test]
fn popup_advert_cannot_replace_the_download_page_but_author_links_still_open() {
    let origins = vec![
        "https://modsfire.com/file".into(),
        "https://sharemods.com/mirror".into(),
    ];
    assert!(access::allowed_popup(
        "https://modsfire.com/download/second",
        &origins
    ));
    assert!(access::allowed_popup(
        "https://cdn.modsfire.com/file.scs",
        &origins
    ));
    assert!(!access::allowed_popup(
        "https://ads.example.test/offer",
        &origins
    ));
    assert!(!access::allowed_popup(
        "https://modsfire.com.evil.test/offer",
        &origins
    ));
    assert!(!access::allowed_popup(
        "http://modsfire.com/second",
        &origins
    ));
}

#[tokio::test]
async fn complete_custom_def_is_preserved_and_missing_def_is_distinct_from_other_errors() {
    let catalog = maps::Catalog::bundled().unwrap();
    let mut entry = catalog.entry("poland-rebuilding").unwrap().clone();
    entry.source.supplements[0].page = "https://127.0.0.1:1/must-not-be-requested".into();
    let root = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let library = maps::library::Library::open(root.path()).unwrap();
    let make = |name: &str| {
        let p = files.path().join(name);
        let mut bytes = b"SCS#".to_vec();
        bytes.extend([1; 32]);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let mut inputs = vec![
        make("z4_PL_Rebuilding_3.0.1_assets.scs"),
        make("z3_PL_Rebuilding_3.0.1_models.scs"),
        make("z2_PL_Rebuilding_3.0.1_map.scs"),
    ];
    let error = library
        .import(&entry, &inputs, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(maps::library::missing_supplement(&entry, &error));
    assert!(!maps::library::missing_supplement(
        &entry,
        &anyhow::anyhow!("corrupt archive")
    ));
    inputs.push(make("z1_PL_Rebuilding_3.0.1_def_personal.scs"));
    let state = std::sync::Arc::new(std::sync::Mutex::new(Default::default()));
    let record = maps::library::import_downloaded(
        &library,
        &entry,
        &inputs,
        &state,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(record.files.len(), 4);
    assert!(
        record
            .files
            .iter()
            .any(|f| f.name.ends_with("def_personal.scs"))
    );
}
