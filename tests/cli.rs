use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env(
            "REPODB_SEMGREP_CORE",
            semgrep_corpus::required_semgrep_core(),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run(repo: &Path, args: &[&str]) -> (bool, String) {
    run_with_env(repo, args, &[])
}

fn test_config_home(repo: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let config_home = repo.join(".test-xdg-config");
    let config_dir = config_home.join("repodb");
    fs::create_dir_all(&config_dir).unwrap();
    let embedder = repo.join(".git/repodb-test-embedder");
    fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[tools]\nembedding_command = {:?}\n",
            embedder.to_string_lossy()
        ),
    )
    .unwrap();
    config_home
}

fn run_with_env(repo: &Path, args: &[&str], vars: &[(&str, &str)]) -> (bool, String) {
    let config_home = test_config_home(repo);
    let mut command = Command::new(env!("CARGO_BIN_EXE_repodb"));
    command
        .args(args)
        .current_dir(repo)
        .env(
            "REPODB_SEMGREP_CORE",
            semgrep_corpus::required_semgrep_core(),
        )
        .env("XDG_CONFIG_HOME", config_home);
    for (key, value) in vars {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    if !output.status.success() {
        eprintln!(
            "repodb {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

fn http_server(
    body: Vec<u8>,
    request_count: usize,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::{Duration, Instant},
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::with_capacity(request_count);
        for _ in 0..request_count {
            let deadline = Instant::now() + Duration::from_secs(5);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("HTTP preview request was not received: {error}"),
                }
            };
            let mut request = [0; 2048];
            let bytes_read = stream.read(&mut request).unwrap();
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
            requests.push(String::from_utf8_lossy(&request[..bytes_read]).into_owned());
        }
        requests
    });
    (format!("http://{address}"), server)
}

fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    fs::write(
        dir.path().join("notes.txt"),
        "First line\nExact [literal] TARGET\nlast line\n",
    )
    .unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() { println!(\"Rust phrase\"); }\n",
    )
    .unwrap();
    fs::write(dir.path().join("ignored.bin"), [0, 1, 2, 3]).unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "initial"]);
    dir
}

#[test]
fn git_import_accepts_one_optional_revision_and_pins_it_to_the_resolved_commit() {
    let repo = repository();
    let root = repo.path();
    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(external.path().join("notes.txt"), "first snapshot marker\n").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "first"]);
    let first = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(external.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    git(external.path(), &["tag", "v1"]);
    fs::write(
        external.path().join("notes.txt"),
        "second snapshot marker\n",
    )
    .unwrap();
    git(external.path(), &["commit", "-qam", "second"]);
    let latest = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(external.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();

    let locator = external.path().to_str().unwrap();
    assert!(run(root, &["init"]).0);
    assert!(run(root, &["import", "--revision", "v1", locator]).0);
    assert!(run(root, &["refresh"]).0);
    assert_eq!(search_json(root, &["first snapshot marker"]).len(), 1);
    assert!(search_json(root, &["second snapshot marker"]).is_empty());
    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let (selected, count): (String, i64) = database.query_row(
        "SELECT pinned_commit,(SELECT count(*) FROM imported_versions WHERE source_id=imported_sources.id) FROM imported_sources WHERE locator=?1",
        [locator], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(selected, first);
    assert_eq!(count, 1);

    assert!(!run(root, &["import", "--revision", "not-a-revision", locator]).0);
    assert!(!run(root, &["import", locator, "another-source"]).0);
    assert_ne!(first, latest);
}

#[test]
fn import_preview_reports_selected_files_sizes_and_exclusions_without_db_side_effects() {
    use std::os::unix::fs::symlink;

    let repo = repository();
    let root = repo.path();
    let selected = root.join("selected");
    fs::create_dir_all(selected.join("nested/.git")).unwrap();
    for (name, body) in [
        ("a.md", "markdown"),
        ("b.markdown", "mark"),
        ("c.txt", "plain"),
        ("d.pdf", "pdf"),
        ("e.html", "html"),
        ("f.org", "org"),
        ("g.rs", "rust"),
        ("extensionless", "text"),
        ("nested/h.htm", "htm"),
        ("unknown.bin", "unsupported"),
    ] {
        fs::write(selected.join(name), body).unwrap();
    }
    fs::write(selected.join("nested/.git/hidden.md"), "hidden").unwrap();
    let outside = root.join("outside.md");
    fs::write(&outside, "outside").unwrap();
    symlink(&outside, selected.join("linked.md")).unwrap();

    let output = root.join("preview-output");
    assert!(!root.join(".git/repodb.sqlite").exists());
    let (ok, json) = run(root, &["import", "--preview", selected.to_str().unwrap()]);
    assert!(ok);
    let preview: serde_json::Value = serde_json::from_str(&json).unwrap();
    let files = preview["selected_supported_local_files"]
        .as_array()
        .unwrap();
    assert_eq!(files.len(), 9);
    assert!(
        files
            .windows(2)
            .all(|pair| pair[0].as_str() < pair[1].as_str())
    );
    assert_eq!(
        preview["unsupported_regular_files"][0]["path"],
        selected.join("unknown.bin").to_string_lossy().as_ref()
    );
    assert!(
        preview["unsupported_regular_files"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("unsupported")
    );
    assert_eq!(
        preview["total_selected_source_bytes"],
        8 + 4 + 5 + 3 + 4 + 3 + 4 + 4 + 3
    );
    assert_eq!(preview["current_db_bytes"], serde_json::Value::Null);
    assert_eq!(
        preview["estimated_net_db_growth_bytes"],
        serde_json::Value::Null
    );
    assert_eq!(
        preview["warning_basis_bytes"],
        preview["total_selected_source_bytes"]
    );
    assert_eq!(preview["warn_large"], false);
    assert!(!root.join(".git/repodb.sqlite").exists());
    let (single_ok, single) = run(
        root,
        &[
            "import",
            "--preview",
            selected.join("a.md").to_str().unwrap(),
        ],
    );
    assert!(single_ok);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&single).unwrap()["selected_supported_local_files"].as_array().unwrap().len(), 1);
    let (bad_ok, _) = run(root, &["import", "--preview", output.to_str().unwrap()]);
    assert!(!bad_ok);
    let (dir_import_ok, _) = run(root, &["import", selected.to_str().unwrap()]);
    assert!(!dir_import_ok);
    let large = root.join("large.txt");
    fs::write(&large, vec![0; 50_000_000]).unwrap();
    let (large_ok, large_json) = run(root, &["import", "--preview", large.to_str().unwrap()]);
    assert!(large_ok);
    let large_preview: serde_json::Value = serde_json::from_str(&large_json).unwrap();
    assert_eq!(large_preview["warn_large"], true);
    assert_eq!(large_preview["warning_basis_bytes"], 50_000_000);
}

#[test]
fn import_preview_absolute_warning_threshold_uses_decimal_megabytes() {
    let repo = repository();
    let root = repo.path();
    let source = root.join("threshold.txt");

    fs::write(&source, vec![b'x'; 49_999_999]).unwrap();
    let (below_ok, below_json) = run(root, &["import", "--preview", source.to_str().unwrap()]);
    assert!(below_ok, "{below_json}");
    let below: serde_json::Value = serde_json::from_str(&below_json).unwrap();
    assert_eq!(below["total_selected_source_bytes"], 49_999_999);
    assert_eq!(below["current_db_bytes"], serde_json::Value::Null);
    assert_eq!(
        below["estimated_net_db_growth_bytes"],
        serde_json::Value::Null
    );
    assert_eq!(below["warning_basis_bytes"], 49_999_999);
    assert_eq!(below["warn_large"], false);

    fs::write(&source, vec![b'x'; 50_000_000]).unwrap();
    let (at_ok, at_json) = run(root, &["import", "--preview", source.to_str().unwrap()]);
    assert!(at_ok, "{at_json}");
    let at: serde_json::Value = serde_json::from_str(&at_json).unwrap();
    assert_eq!(at["total_selected_source_bytes"], 50_000_000);
    assert_eq!(at["current_db_bytes"], serde_json::Value::Null);
    assert_eq!(at["estimated_net_db_growth_bytes"], serde_json::Value::Null);
    assert_eq!(at["warning_basis_bytes"], 50_000_000);
    assert_eq!(at["warn_large"], true);
}

#[test]
fn import_preview_warns_when_source_exceeds_twice_existing_database_size() {
    let repo = repository();
    let root = repo.path();
    let source = root.join("threshold.txt");
    fs::write(&source, vec![b'x'; 100]).unwrap();
    let database = root.join(".git/repodb.sqlite");

    fs::write(&database, vec![b'd'; 50]).unwrap();
    let (equal_ok, equal_json) = run(root, &["import", "--preview", source.to_str().unwrap()]);
    assert!(equal_ok, "{equal_json}");
    let equal_preview: serde_json::Value = serde_json::from_str(&equal_json).unwrap();
    assert_eq!(equal_preview["total_selected_source_bytes"], 100);
    assert_eq!(equal_preview["current_db_bytes"], 50);
    assert_eq!(equal_preview["warn_large"], false);

    fs::write(&database, vec![b'd'; 49]).unwrap();
    let (greater_ok, greater_json) = run(root, &["import", "--preview", source.to_str().unwrap()]);
    assert!(greater_ok, "{greater_json}");
    let greater_preview: serde_json::Value = serde_json::from_str(&greater_json).unwrap();
    assert_eq!(greater_preview["total_selected_source_bytes"], 100);
    assert_eq!(greater_preview["current_db_bytes"], 49);
    assert_eq!(greater_preview["warn_large"], true);
}

#[test]
fn url_import_preview_reports_body_size_without_import_side_effects() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let database_path = root.join(".git/repodb.sqlite");
    let body = b"<a href='http://127.0.0.1:1/other'>link</a> preview text".to_vec();
    let body_size = body.len() as u64;
    let (origin, server) = http_server(body, 2);
    let url = format!("{origin}/preview.html");
    assert!(run(root, &["import", &url]).0);
    assert!(run(root, &["refresh", "--imports"]).0);
    let before = search_json(root, &["preview text"])
        .into_iter()
        .find(|result| result["path"] == url)
        .unwrap();
    assert!(before["first_retrieved_at"].is_string());
    assert!(before["last_retrieved_at"].is_string());
    let database_size = fs::metadata(&database_path).unwrap().len();
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let (ok, output) = run(root, &["import", "--preview", &url]);
    assert!(ok, "{output}");
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| { request.starts_with("GET /preview.html HTTP/1.1") }),
        "{requests:?}"
    );
    let preview: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(preview["selected_url"], url);
    assert_eq!(preview["total_selected_source_bytes"], body_size);
    assert_eq!(preview["current_db_bytes"], database_size);
    assert_eq!(
        preview["estimated_net_db_growth_bytes"],
        serde_json::Value::Null
    );
    assert_eq!(preview["warning_basis_bytes"], body_size);
    assert_eq!(preview["warn_large"], false);
    assert_eq!(fs::metadata(&database_path).unwrap().len(), database_size);
    let after = search_json(root, &["preview text"])
        .into_iter()
        .find(|result| result["path"] == url)
        .unwrap();
    assert_eq!(
        after, before,
        "preview must not alter indexed content or provenance"
    );

    let database = rusqlite::Connection::open(database_path).unwrap();
    let sources: i64 = database
        .query_row("SELECT count(*) FROM imported_sources", [], |row| {
            row.get(0)
        })
        .unwrap();
    let versions: i64 = database
        .query_row("SELECT count(*) FROM imported_versions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!((sources, versions), (1, 1));
}

#[test]
fn url_import_preview_warns_using_source_bytes_when_growth_is_unknown() {
    let repo = repository();
    let root = repo.path();
    let database_path = root.join(".git/repodb.sqlite");
    fs::write(&database_path, b"x").unwrap();
    let body = b"abc".to_vec();
    let body_size = body.len() as u64;
    let (origin, server) = http_server(body, 1);
    let url = format!("{origin}/small.txt");

    let (ok, output) = run(root, &["import", "--preview", &url]);
    assert!(ok, "{output}");
    server.join().unwrap();
    let preview: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(preview["selected_url"], url);
    assert_eq!(preview["total_selected_source_bytes"], body_size);
    assert_eq!(preview["current_db_bytes"], 1);
    assert_eq!(
        preview["estimated_net_db_growth_bytes"],
        serde_json::Value::Null
    );
    assert_eq!(preview["warning_basis_bytes"], body_size);
    assert_eq!(preview["warn_large"], true);
}

#[test]
fn refresh_size_limit_modes_report_fallback_and_control_local_imports() {
    fn large_local_import(size_limit: Option<&str>, imports_only: bool) -> (bool, String, i64) {
        let repo = repository();
        let root = repo.path();
        let source = root.join("large-import.txt");
        fs::write(&source, "seed").unwrap();
        assert!(run(root, &["init"]).0);
        assert!(run(root, &["import", source.to_str().unwrap()]).0);
        let database_size = fs::metadata(root.join(".git/repodb.sqlite")).unwrap().len();
        let source_size = database_size.saturating_mul(2).saturating_add(1);
        fs::write(&source, vec![b'x'; source_size as usize]).unwrap();

        let mut args = vec!["refresh"];
        if imports_only {
            args.push("--imports");
        }
        if let Some(size_limit) = size_limit {
            args.push(size_limit);
        }
        let (ok, output) = run(root, &args);
        let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
        let published_versions: i64 = database
            .query_row("SELECT count(*) FROM imported_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        (ok, output, published_versions)
    }

    let (default_ok, _, default_versions) = large_local_import(None, false);
    assert!(
        !default_ok,
        "the default size limit must reject large imports"
    );
    assert_eq!(
        default_versions, 0,
        "error must not publish imported versions"
    );

    let (warn_ok, warning, warn_versions) = large_local_import(Some("--size-limit=warn"), false);
    assert!(warn_ok, "warn must continue import: {warning}");
    assert_eq!(warn_versions, 1);
    assert!(warning.contains("source bytes="), "{warning}");
    assert!(
        warning.contains("estimated net database growth="),
        "{warning}"
    );
    assert!(warning.contains("source_bytes_fallback"), "{warning}");

    let (ignore_ok, ignored, ignore_versions) =
        large_local_import(Some("--size-limit=ignore"), true);
    assert!(ignore_ok, "ignore must continue import: {ignored}");
    assert_eq!(ignore_versions, 1);
    assert!(
        !ignored.contains("warning"),
        "ignore must be silent: {ignored}"
    );
}

#[test]
fn url_refresh_size_limit_warn_continues_with_source_byte_fallback() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let database_size = fs::metadata(root.join(".git/repodb.sqlite")).unwrap().len();
    let body = vec![b'x'; database_size.saturating_mul(2).saturating_add(1) as usize];
    let (origin, server) = http_server(body.clone(), 1);
    let url = format!("{origin}/large.txt");
    assert!(run(root, &["import", &url]).0);

    let (ok, output) = run(root, &["refresh", "--size-limit=warn", "--imports"]);
    assert!(ok, "warn must continue URL import: {output}");
    assert_eq!(server.join().unwrap().len(), 1);
    assert!(
        output.contains(&format!("source bytes={}", body.len())),
        "{output}"
    );
    assert!(
        output.contains("estimated net database growth=unknown"),
        "{output}"
    );
    assert!(output.contains("source_bytes_fallback"), "{output}");
}

#[test]
fn git_repository_refresh_is_not_restricted_by_size_limit() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let database_size = fs::metadata(root.join(".git/repodb.sqlite")).unwrap().len();
    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(
        external.path().join("large.txt"),
        vec![b'x'; database_size.saturating_mul(2).saturating_add(1) as usize],
    )
    .unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "large Git content"]);

    assert!(run(root, &["import", external.path().to_str().unwrap()]).0);
    let (ok, output) = run(root, &["refresh", "--imports", "--size-limit=error"]);
    assert!(
        ok,
        "Git registration and refresh remain unrestricted: {output}"
    );
    assert!(
        !output.contains("warning"),
        "Git content must not be gated: {output}"
    );
}

#[test]
fn search_scope_filters_repo_and_document_sources_for_all_modes() {
    let repo = repository();
    let root = repo.path();
    fs::create_dir(root.join("docs")).unwrap();
    fs::write(
        root.join("src/local_scope.rs"),
        "SCOPE_MARKER local source\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/local_scope.md"),
        "SCOPE_MARKER local docs\n",
    )
    .unwrap();
    fs::write(root.join("opaque.dat"), "SCOPE_MARKER unknown type\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "add scoped results"]);
    let local_commit = git_text(root, &["rev-parse", "HEAD"]);

    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::create_dir(external.path().join("src")).unwrap();
    fs::write(
        external.path().join("src/external_scope.rs"),
        "SCOPE_MARKER imported source\n",
    )
    .unwrap();
    fs::write(
        external.path().join("README.md"),
        "SCOPE_MARKER imported docs\n",
    )
    .unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "external sources"]);

    let imported_docs = root.join("handbook.txt");
    fs::write(&imported_docs, "SCOPE_MARKER imported local document\n").unwrap();
    assert!(run(root, &["init"]).0);
    assert!(run(root, &["import", external.path().to_str().unwrap()]).0);
    assert!(run(root, &["import", imported_docs.to_str().unwrap()]).0);
    assert!(run(root, &["refresh"]).0);

    let imported_doc_path = imported_docs.to_str().unwrap();
    let expected = vec![
        (
            "source",
            vec!["src/local_scope.rs", "src/external_scope.rs"],
        ),
        ("repo-local-source", vec!["src/local_scope.rs"]),
        ("repo-local-docs", vec!["docs/local_scope.md"]),
        (
            "all-docs",
            vec!["docs/local_scope.md", "README.md", imported_doc_path],
        ),
        (
            "all",
            vec![
                "src/local_scope.rs",
                "docs/local_scope.md",
                "opaque.dat",
                "src/external_scope.rs",
                "README.md",
                imported_doc_path,
            ],
        ),
    ];
    for mode in ["exact", "--semantic", "--combined"] {
        for (scope, paths) in &expected {
            let mut args = vec!["search", "--json"];
            if mode != "exact" {
                args.extend([mode, "--semantic-limit", "20"]);
            }
            args.extend(["SCOPE_MARKER", "--scope", scope]);
            let (ok, output) = run(root, &args);
            assert!(ok, "{mode} scope {scope}: {output}");
            let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
            let results = results
                .into_iter()
                .filter(|result| result["component"] == "content")
                .collect::<Vec<_>>();
            let mut actual: Vec<_> = results
                .iter()
                .map(|result| result["path"].as_str().unwrap())
                .collect();
            let is_doc = |path: &str| {
                let path = path.split(['?', '#']).next().unwrap_or(path);
                matches!(
                    path.rsplit('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .as_str(),
                    "md" | "markdown" | "txt" | "pdf" | "html" | "htm" | "org"
                )
            };
            match *scope {
                "source" => assert!(
                    results.iter().all(|result| {
                        (result["commit_id"].is_string() || result["repo_id"].is_string())
                            && !is_doc(result["path"].as_str().unwrap())
                    }),
                    "{mode} scope {scope}: {results:?}"
                ),
                "repo-local-source" => assert!(
                    results.iter().all(|result| {
                        result["commit_id"].is_string()
                            && result["repo_id"].is_null()
                            && result["path"].as_str().unwrap().ends_with(".rs")
                    }),
                    "{mode} scope {scope}: {results:?}"
                ),
                "repo-local-docs" => assert!(
                    results.iter().all(|result| {
                        result["commit_id"].is_string()
                            && result["repo_id"].is_null()
                            && is_doc(result["path"].as_str().unwrap())
                    }),
                    "{mode} scope {scope}: {results:?}"
                ),
                "all-docs" => assert!(
                    results
                        .iter()
                        .all(|result| is_doc(result["path"].as_str().unwrap())),
                    "{mode} scope {scope}: {results:?}"
                ),
                "all" => {}
                _ => unreachable!(),
            }
            if mode == "exact" {
                actual.sort_unstable();
                let mut expected = paths.to_vec();
                expected.sort_unstable();
                assert_eq!(actual, expected, "{mode} scope {scope}");
            } else {
                for path in paths {
                    assert!(
                        actual.contains(path),
                        "{mode} scope {scope} omits {path}: {actual:?}"
                    );
                }
            }
        }
    }

    let default_results = search_json(root, &["SCOPE_MARKER"]);
    let all_results = search_json(root, &["SCOPE_MARKER", "--scope", "all"]);
    assert_eq!(default_results.len(), all_results.len());
    assert!(!run(root, &["search", "SCOPE_MARKER", "--scope", "unknown"]).0);
    assert!(
        !run(
            root,
            &[
                "search",
                "SCOPE_MARKER",
                "--scope",
                "all",
                "--scope",
                "source"
            ]
        )
        .0
    );

    let scoped_commit_results = search_json(
        root,
        &[
            "SCOPE_MARKER",
            "--scope",
            "repo-local-source",
            "--commit",
            &local_commit,
        ],
    );
    assert_eq!(scoped_commit_results.len(), 1);
    assert_eq!(scoped_commit_results[0]["path"], "src/local_scope.rs");
}

#[test]
fn explicit_database_paths_keep_independent_history_and_preserve_default() {
    let repo = repository();
    let root = repo.path();
    let databases = tempfile::tempdir().unwrap();
    let first = databases.path().join("first.sqlite");
    let second = databases.path().join("second.sqlite");
    let first_path = first.to_str().unwrap();
    let second_path = second.to_str().unwrap();
    let search = |path: &str, phrase: &str| -> Vec<serde_json::Value> {
        let (ok, output) = run(root, &["--database", path, "search", "--json", phrase]);
        assert!(ok, "{output}");
        serde_json::from_str::<Vec<serde_json::Value>>(&output)
            .unwrap()
            .into_iter()
            .filter(|result| result["component"] == "content")
            .collect()
    };

    assert!(run(root, &["--database", first_path, "init"]).0);
    assert!(first.is_file());
    assert!(!second.exists());
    assert!(!root.join(".git/repodb.sqlite").exists());
    assert_eq!(search(first_path, "Exact [literal] TARGET").len(), 1);

    fs::write(root.join("notes.txt"), "Second unique marker\n").unwrap();
    git(root, &["add", "notes.txt"]);
    git(root, &["commit", "-qm", "change marker"]);
    assert!(run(root, &["--database", second_path, "init"]).0);
    assert_eq!(search(second_path, "Second unique marker").len(), 1);
    assert!(search(first_path, "Second unique marker").is_empty());
    assert!(!root.join(".git/repodb.sqlite").exists());

    assert!(run(root, &["--database", second_path, "retention", "latest"]).0);
    let import_path = root.join("external.txt");
    fs::write(&import_path, "Imported isolated marker\n").unwrap();
    assert!(
        run(
            root,
            &[
                "--database",
                second_path,
                "import",
                import_path.to_str().unwrap()
            ]
        )
        .0
    );
    assert!(run(root, &["--database", second_path, "refresh"]).0);
    assert!(run(root, &["--database", second_path, "reembed"]).0);
    assert_eq!(search(second_path, "Imported isolated marker").len(), 1);
    for mode in ["--semantic", "--combined"] {
        let (ok, output) = run(
            root,
            &[
                "--database",
                second_path,
                "search",
                "--json",
                mode,
                "Second unique marker",
            ],
        );
        assert!(ok, "{mode}: {output}");
        assert!(
            !serde_json::from_str::<Vec<serde_json::Value>>(&output)
                .unwrap()
                .is_empty()
        );
    }
    assert!(search(first_path, "Imported isolated marker").is_empty());

    assert!(run(root, &["--database", first_path, "refresh", "--changed"]).0);
    assert_eq!(search(first_path, "Second unique marker").len(), 1);
    assert!(run(root, &["init"]).0);
    assert!(root.join(".git/repodb.sqlite").is_file());
}

#[test]
fn database_option_resolves_relative_to_cwd_and_rejects_missing_paths() {
    let repo = repository();
    let root = repo.path();
    let config_home = test_config_home(root);
    let in_subdirectory = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_repodb"))
            .args(args)
            .current_dir(root.join("src"))
            .env("XDG_CONFIG_HOME", &config_home)
            .env(
                "REPODB_SEMGREP_CORE",
                semgrep_corpus::required_semgrep_core(),
            )
            .output()
            .unwrap()
    };
    let output = in_subdirectory(&["--database", "isolated.sqlite", "init"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join("src/isolated.sqlite").is_file());
    assert!(!root.join(".git/repodb.sqlite").exists());
    let output = in_subdirectory(&[
        "--database",
        "isolated.sqlite",
        "search",
        "--json",
        "Rust phrase",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout)
            .unwrap()
            .into_iter()
            .filter(|result| result["component"] == "content")
            .count(),
        1
    );
    for args in [
        vec!["--database"],
        vec!["--database", ""],
        vec!["--database", "", "init"],
    ] {
        assert!(!run(root, &args).0, "{args:?} must fail");
    }
    assert!(!root.join(".git/repodb.sqlite").exists());
}

#[test]
fn history_search_reports_non_utf8_paths_losslessly_without_changing_utf8_paths() {
    use std::os::unix::ffi::OsStrExt;

    let repo = repository();
    let root = repo.path();
    let invalid_path = std::ffi::OsStr::from_bytes(b"invalid-\xff.txt");
    fs::write(root.join(invalid_path), "nonutf marker\n").unwrap();
    fs::write(root.join("bytes:ff"), "utf8 collision marker\n").unwrap();
    fs::write(root.join("valid.txt"), "valid history marker\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "non-UTF-8 path"]);
    assert!(run(root, &["init"]).0);

    let results = search_json(root, &["nonutf marker"]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["path"], "bytes:696e76616c69642dff2e747874");
    assert_eq!(results[0]["path_bytes_hex"], "696e76616c69642dff2e747874");

    let utf8_collision = search_json(root, &["utf8 collision marker"]);
    assert_eq!(utf8_collision.len(), 1);
    assert_eq!(utf8_collision[0]["path"], "bytes:ff");
    assert!(utf8_collision[0].get("path_bytes_hex").is_none());

    let ordinary = search_json(root, &["valid history marker"]);
    assert_eq!(ordinary.len(), 1);
    assert_eq!(ordinary[0]["path"], "valid.txt");
    assert!(ordinary[0].get("path_bytes_hex").is_none());

    let (_, readable) = run(root, &["search", "nonutf marker"]);
    assert!(
        readable.contains("bytes:696e76616c69642dff2e747874 (non-UTF-8):"),
        "{readable}"
    );

    let (_, literal_readable) = run(root, &["search", "utf8 collision marker"]);
    assert!(literal_readable.contains("bytes:ff:"), "{literal_readable}");
    assert!(
        !literal_readable.contains("non-UTF-8"),
        "{literal_readable}"
    );
}

#[test]
fn git_snapshot_stores_ranges_not_verbatim_text_and_hydrates_search_results() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);

    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let indexed_columns = database
        .prepare("PRAGMA table_info(indexed_units)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!indexed_columns.iter().any(|column| column == "text"));
    assert!(indexed_columns.iter().any(|column| column == "byte_start"));
    assert!(indexed_columns.iter().any(|column| column == "byte_end"));
    let blob_columns = database
        .prepare("PRAGMA table_info(blob_contents)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!blob_columns.iter().any(|column| column == "content"));
    drop(database);

    let results = search_json(root, &["Exact [literal] TARGET"]);
    assert_eq!(
        results[0]["text"],
        "First line\nExact [literal] TARGET\nlast line"
    );

    let blob_id = results[0]["blob_id"].as_str().unwrap();
    let object = root
        .join(".git/objects")
        .join(&blob_id[..2])
        .join(&blob_id[2..]);
    fs::remove_file(object).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .args(["search", "Exact [literal] TARGET"])
        .current_dir(root)
        .env(
            "REPODB_SEMGREP_CORE",
            semgrep_corpus::required_semgrep_core(),
        )
        .env("XDG_CONFIG_HOME", test_config_home(root))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "must not fabricate partial results"
    );
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains(&format!(
            "cannot hydrate Git blob {blob_id}: object is missing"
        )),
        "missing-object error must include the actionable blob ID: {error}"
    );
}

#[test]
fn exact_search_hydrates_multiple_units_from_one_git_blob_in_source_order() {
    let repo = repository();
    let root = repo.path();
    fs::write(
        root.join("many.md"),
        "First paragraph has shared marker.\n\nSecond paragraph has shared marker.\n",
    )
    .unwrap();
    git(root, &["add", "many.md"]);
    git(root, &["commit", "-qm", "add multiple searchable units"]);
    assert!(run(root, &["init"]).0);

    let results = search_json(root, &["shared marker"]);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["text"], "First paragraph has shared marker.");
    assert_eq!(results[1]["text"], "Second paragraph has shared marker.");
}

#[test]
fn default_embedding_command_is_used_on_first_scan_and_saved_for_search() {
    let repo = repository();
    let root = repo.path();
    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let embedder = bin.join("generate_embeddings");
    fs::write(
        &embedder,
        "#!/usr/bin/env python3\nimport sys\nfor _ in sys.stdin.buffer.read().split(b'\\0'):\n    print('[1,0]')\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    let config_dir = home.path().join(".config/repodb");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[tools]\nsemgrep_core = {:?}\n",
            semgrep_corpus::required_semgrep_core().to_string_lossy()
        ),
    )
    .unwrap();
    let vars = [
        ("HOME", home.path().to_str().unwrap()),
        ("XDG_CONFIG_HOME", ""),
    ];
    let (ok, output) = run_with_env(root, &["init"], &vars);
    assert!(ok, "{output}");
    let (ok, output) = run_with_env(root, &["search", "Rust phrase", "--semantic"], &vars);
    assert!(ok && output.contains("src/main.rs"), "{output}");
}

#[test]
fn default_embedding_batch_handles_ten_thousand_vectors() {
    let repo = repository();
    let root = repo.path();
    let content = (0..10_000)
        .map(|index| format!("large-batch-unit-{index}\n"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(root.join("batch.txt"), content).unwrap();
    git(root, &["add", "batch.txt"]);
    git(root, &["commit", "-qm", "large embedding batch"]);

    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let embedder = bin.join("generate_embeddings");
    fs::write(
        &embedder,
        "#!/usr/bin/env python3\nimport sys\nfor _ in sys.stdin.buffer.read().split(b'\\0'):\n    print('[1,0]')\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    let config_dir = home.path().join(".config/repodb");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[tools]\nsemgrep_core = {:?}\n",
            semgrep_corpus::required_semgrep_core().to_string_lossy()
        ),
    )
    .unwrap();
    let vars = [
        ("HOME", home.path().to_str().unwrap()),
        ("XDG_CONFIG_HOME", ""),
    ];
    let (ok, output) = run_with_env(root, &["init"], &vars);
    assert!(ok, "{output}");
    let db = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let (units, vectors): (i64, i64) = db
        .query_row(
            "SELECT (SELECT count(*) FROM indexed_units),(SELECT count(*) FROM unit_embeddings)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(units >= 10_000);
    assert_eq!(units, vectors);
}

#[test]
fn init_and_refresh_populate_semantic_vectors_automatically() {
    let repo = repository();
    let root = repo.path();
    let core = semgrep_corpus::required_semgrep_core();
    let embedder = fake_embedder(root, "printf '[1,0]'");
    let xdg = tempfile::tempdir().unwrap();
    let config_dir = xdg.path().join("repodb");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[tools]\nsemgrep_core = {:?}\nembedding_command = {:?}\n",
            core.to_string_lossy(),
            embedder.to_string_lossy()
        ),
    )
    .unwrap();

    let run_configured = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_repodb"))
            .args(args)
            .current_dir(root)
            .env_remove("REPODB_SEMGREP_CORE")
            .env("XDG_CONFIG_HOME", xdg.path())
            .output()
            .unwrap()
    };
    let output = run_configured(&["init"]);
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run_configured(&["search", "Rust phrase", "--semantic", "--json"]);
    assert!(
        output.status.success(),
        "semantic search failed after init: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let results: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(results.iter().any(|result| result["path"] == "src/main.rs"));

    fs::write(root.join("notes.txt"), "Added during rescan\n").unwrap();
    git(root, &["add", "notes.txt"]);
    git(root, &["commit", "-qm", "add rescan content"]);
    let output = run_configured(&["refresh"]);
    assert!(
        output.status.success(),
        "refresh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run_configured(&["search", "Added during rescan", "--semantic", "--json"]);
    assert!(
        output.status.success(),
        "semantic search failed after refresh: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let results: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(results.iter().any(|result| result["path"] == "notes.txt"));
}

#[test]
fn xdg_config_supplies_semgrep_core_and_environment_override_wins() {
    let repo = repository();
    let root = repo.path();
    let xdg = tempfile::tempdir().unwrap();
    let config_dir = xdg.path().join("repodb");
    fs::create_dir_all(&config_dir).unwrap();
    let core = semgrep_corpus::required_semgrep_core();
    let embedder = fake_embedder(root, "printf '[1,0]'");
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[tools]\nsemgrep_core = {:?}\nembedding_command = {:?}\n",
            core.display().to_string(),
            embedder.to_string_lossy()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .arg("init")
        .current_dir(root)
        .env_remove("REPODB_SEMGREP_CORE")
        .env("XDG_CONFIG_HOME", xdg.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "XDG config should supply the core path: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .arg("refresh")
        .current_dir(root)
        .env("REPODB_SEMGREP_CORE", "/no/such/semgrep-core")
        .env("XDG_CONFIG_HOME", xdg.path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "environment override must win");
}

#[test]
fn default_xdg_config_path_uses_home_dot_config() {
    let repo = repository();
    let root = repo.path();
    let home = tempfile::tempdir().unwrap();
    let config_dir = home.path().join(".config/repodb");
    fs::create_dir_all(&config_dir).unwrap();
    let core = semgrep_corpus::required_semgrep_core();
    let embedder = fake_embedder(root, "printf '[1,0]'");
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[tools]\nsemgrep_core = {:?}\nembedding_command = {:?}\n",
            core.display().to_string(),
            embedder.to_string_lossy()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .arg("init")
        .current_dir(root)
        .env_remove("REPODB_SEMGREP_CORE")
        .env_remove("XDG_CONFIG_HOME")
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "default XDG config should supply the core path: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn optional_segmenter_uses_byte_partition_and_falls_back_on_invalid_output() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let embedder = root.join("embedder");
    fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);
    let segmenter = root.join("segmenter");
    fs::write(
        &segmenter,
        "#!/bin/sh\ncat >/dev/null\n[ \"$1\" = rust ] || exit 2\nprintf '[[0,12],[12,39]]'\n",
    )
    .unwrap();
    fs::set_permissions(&segmenter, fs::Permissions::from_mode(0o755)).unwrap();
    let xdg = tempfile::tempdir().unwrap();
    let config_dir = xdg.path().join("repodb");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[tools]\nsegmenter = {:?}\nembedding_command = {:?}\n",
            segmenter.to_string_lossy(),
            embedder.to_string_lossy()
        ),
    )
    .unwrap();
    let xdg_config_home = xdg.path().to_string_lossy().into_owned();
    assert!(run_with_env(root, &["refresh"], &[("XDG_CONFIG_HOME", &xdg_config_home)]).0);
    let all = search_json(root, &["Rust phrase", "--all-history"]);
    assert_eq!(all[0]["text"], "println!(\"Rust phrase\"); }\n");
    fs::write(&segmenter, "#!/bin/sh\nprintf '[ [0,1], [0,2] ]'\n").unwrap();
    let (success, _) = run_with_env(
        root,
        &["refresh"],
        &[
            ("REPODB_SEGMENTER", segmenter.to_str().unwrap()),
            ("XDG_CONFIG_HOME", &xdg_config_home),
        ],
    );
    assert!(
        success,
        "malformed ranges must fall back instead of failing refresh"
    );
    assert_eq!(
        search_json(root, &["Rust phrase", "--all-history"])[0]["text"],
        "fn main() { println!(\"Rust phrase\"); }"
    );
    for unavailable in [root.join("missing-segmenter"), {
        let failing = root.join("failing-segmenter");
        fs::write(&failing, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&failing, fs::Permissions::from_mode(0o755)).unwrap();
        failing
    }] {
        assert!(
            run_with_env(
                root,
                &["refresh"],
                &[
                    ("REPODB_SEGMENTER", unavailable.to_str().unwrap()),
                    ("XDG_CONFIG_HOME", &xdg_config_home),
                ]
            )
            .0
        );
        assert_eq!(
            search_json(root, &["Rust phrase", "--all-history"])[0]["text"],
            "fn main() { println!(\"Rust phrase\"); }"
        );
    }
}

#[test]
fn semgrep_core_is_required_and_uses_real_ast() {
    use std::os::unix::fs::PermissionsExt;

    let core = semgrep_corpus::required_semgrep_core();
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let embedder = root.join("embedder");
    fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);

    let source = "// leading gap\nfunc Hello() { println(\"AST_UNIT\"); }\n// trailing café gap\n";
    let source_path = root.join("src/main.go");
    fs::write(&source_path, source).unwrap();
    git(root, &["add", "src/main.go"]);
    git(root, &["commit", "-qm", "add Go source"]);
    assert!(
        run_with_env(
            root,
            &["refresh"],
            &[("REPODB_SEMGREP_CORE", core.to_str().unwrap())],
        )
        .0
    );
    let syntax = search_json(root, &["AST_UNIT", "--all-history"]);
    assert_eq!(syntax[0]["text"], "func Hello() { println(\"AST_UNIT\"); }");
    assert_eq!(syntax[0]["line_start"], 2);
    assert_eq!(syntax[0]["line_end"], 2);
    let gap = search_json(root, &["leading gap", "--all-history"]);
    assert_eq!(gap[0]["text"], "// leading gap\n");
    let trailing = search_json(root, &["trailing café gap", "--all-history"]);
    assert_eq!(trailing[0]["text"], "\n// trailing café gap\n");
    assert_eq!(
        format!(
            "{}{}{}",
            gap[0]["text"].as_str().unwrap(),
            syntax[0]["text"].as_str().unwrap(),
            trailing[0]["text"].as_str().unwrap()
        ),
        source
    );
    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let ranges = database
        .prepare("SELECT u.byte_start,u.byte_end FROM indexed_units u JOIN git_blob_units bu USING(unit_id) JOIN git_occurrences o USING(blob_id) WHERE CAST(o.path AS TEXT)='src/main.go' ORDER BY u.byte_start")
        .unwrap()
        .query_map([], |row| Ok((row.get::<_, usize>(0)?, row.get::<_, usize>(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(ranges.len(), 3);
    let mut stored_text = Vec::new();
    for (start, end) in ranges {
        stored_text.push(&source[start..end]);
    }
    assert_eq!(stored_text.concat(), source);
    for result in [&gap[0], &syntax[0], &trailing[0]] {
        assert!(stored_text.contains(&result["text"].as_str().unwrap()));
    }

    // Missing core is fatal for source refresh and must preserve the prior snapshot.
    let unavailable_path = root.join("src/unavailable.go");
    fs::write(&unavailable_path, source.replace("AST_UNIT", "UNAVAILABLE")).unwrap();
    git(root, &["add", "src/unavailable.go"]);
    git(root, &["commit", "-qm", "add unavailable core fixture"]);
    assert!(
        !run_with_env(
            root,
            &["refresh"],
            &[("REPODB_SEMGREP_CORE", "/no/such/semgrep-core")],
        )
        .0
    );
    assert!(
        !run_with_env(root, &["refresh"], &[("REPODB_SEMGREP_CORE", "/bin/echo")]).0,
        "wrong-version core must fail refresh"
    );
    let missing_core = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .args(["refresh"])
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root.join(".test-xdg-config"))
        .env_remove("REPODB_SEMGREP_CORE")
        .output()
        .unwrap();
    assert!(!missing_core.status.success());
    assert!(search_json(root, &["UNAVAILABLE", "--all-history"]).is_empty());
    assert!(!search_json(root, &["AST_UNIT", "--all-history"]).is_empty());
}

#[test]
fn optional_segmenter_rejects_utf8_split_and_falls_back() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let embedder = root.join("embedder");
    fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);

    let source = root.join("unicode.rs");
    fs::write(&source, "fn unicode() { println!(\"é marker\"); }\n").unwrap();
    let locator = source.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    let segmenter = root.join("segmenter");
    fs::write(
        &segmenter,
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *'é marker'*) printf '[[0,26],[26,40]]' ;; *) printf '[[0,1],[1,2]]' ;; esac\n",
    )
    .unwrap();
    fs::set_permissions(&segmenter, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        run_with_env(
            root,
            &["refresh"],
            &[("REPODB_SEGMENTER", segmenter.to_str().unwrap())]
        )
        .0
    );

    let results = search_json(root, &["é marker", "--all-history"]);
    assert!(
        results
            .iter()
            .any(|result| result["text"] == "fn unicode() { println!(\"é marker\"); }")
    );
}

#[test]
fn imported_text_and_git_blobs_use_document_and_source_units_with_line_ranges() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let fake = root.join("fake-embedder");
    fs::write(&fake, "#!/bin/sh\ncat >/dev/null\nprintf '[1.0, 0.0]'\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);

    let document = root.join("external.md");
    fs::write(
        &document,
        "first paragraph line one\nline two\n\nsecond paragraph marker\n",
    )
    .unwrap();
    let document_locator = document.to_string_lossy().into_owned();
    assert!(run(root, &["import", &document_locator]).0);
    assert!(run(root, &["refresh"]).0);
    let first = search_json(root, &["first paragraph"]);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["text"], "first paragraph line one\nline two");
    assert_eq!(
        (
            first[0]["line_start"].as_u64(),
            first[0]["line_end"].as_u64()
        ),
        (Some(1), Some(2))
    );
    let second = search_json(root, &["second paragraph marker"]);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["line_start"], 4);

    let rust = root.join("external.rs");
    fs::write(
        &rust,
        "pub fn local_alpha() {\n    println!(\"local alpha marker\");\n}\n\npub struct LocalBeta;\n",
    )
    .unwrap();
    let rust_locator = rust.to_string_lossy().into_owned();
    assert!(run(root, &["import", &rust_locator]).0);
    assert!(run(root, &["refresh"]).0);
    let local_alpha = search_json(root, &["local alpha marker"]);
    assert_eq!(local_alpha.len(), 1);
    assert_eq!(
        local_alpha[0]["text"],
        "fn local_alpha() {\n    println!(\"local alpha marker\");\n}"
    );
    assert_eq!(
        (
            local_alpha[0]["line_start"].as_u64(),
            local_alpha[0]["line_end"].as_u64()
        ),
        (Some(1), Some(3))
    );
    let local_beta = search_json(root, &["LocalBeta"]);
    assert_eq!(local_beta.len(), 1);
    assert_eq!(local_beta[0]["text"], "struct LocalBeta");
    assert_eq!(
        (
            local_beta[0]["line_start"].as_u64(),
            local_beta[0]["line_end"].as_u64()
        ),
        (Some(5), Some(5))
    );

    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(external.path().join("lib.rs"), "fn alpha() {\n    println!(\"first git marker\");\n}\n\nfn beta() {\n    println!(\"second git marker\");\n}\n").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "source"]);
    let git_locator = external.path().to_string_lossy().into_owned();
    assert!(run(root, &["import", &git_locator]).0);
    assert!(run(root, &["refresh"]).0);
    let unit = search_json(root, &["second git marker"]);
    assert_eq!(unit.len(), 1);
    assert_eq!(unit[0]["path"], "lib.rs");
    assert_eq!(
        unit[0]["text"],
        "fn beta() {\n    println!(\"second git marker\");\n}"
    );
    assert_eq!(
        (unit[0]["line_start"].as_u64(), unit[0]["line_end"].as_u64()),
        (Some(5), Some(7))
    );
}

#[test]
fn local_document_formats_import_searchable_text_and_reject_unsupported_files() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    for (name, contents, query) in [
        (
            "guide.md",
            "Markdown searchable marker",
            "Markdown searchable marker",
        ),
        (
            "guide.markdown",
            "Markdown extension marker",
            "Markdown extension marker",
        ),
        (
            "guide.txt",
            "Plain text searchable marker",
            "Plain text searchable marker",
        ),
        ("source.rs", "pub fn code_marker() {}", "code_marker"),
        (
            "guide.org",
            "Org searchable marker",
            "Org searchable marker",
        ),
        (
            "guide.html",
            "<html><body><p>HTML searchable marker</p></body></html>",
            "HTML searchable marker",
        ),
        (
            "guide.htm",
            "<html><body><p>HTM searchable marker</p></body></html>",
            "HTM searchable marker",
        ),
    ] {
        let path = root.join(name);
        fs::write(&path, contents).unwrap();
        let path = path.to_string_lossy().into_owned();
        assert!(run(root, &["import", &path]).0, "import {name}");
        assert!(run(root, &["refresh"]).0, "refresh {name}");
        let results = search_json(root, &[query]);
        assert!(
            results.iter().any(|result| result["path"] == path),
            "{name}: {results:?}"
        );
    }

    let unsupported = root.join("notes.csv");
    fs::write(&unsupported, "unsupported format").unwrap();
    let unsupported = unsupported.to_string_lossy().into_owned();
    assert!(!run(root, &["import", &unsupported]).0);
}

#[test]
fn html_import_preserves_heading_link_target_and_table_context_in_searchable_text() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/context.html");
    let locator = fixture.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh", "--imports"]).0);

    for query in ["Deployment guide", "rollback", "Gold"] {
        let results = search_json(root, &[query]);
        assert!(!results.is_empty(), "missing HTML context for {query}");
        assert!(
            results.iter().any(|result| result["path"] == locator),
            "missing HTML source reference for {query}: {results:?}"
        );
        let text = results
            .iter()
            .map(|result| result["text"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\\n");
        assert!(text.contains(query), "{query:?} missing from {text:?}");
        if query == "rollback" {
            assert!(
                text.contains("https://example.test/rollback"),
                "link target missing from {text:?}"
            );
        }
        if query == "Gold" {
            for table_context in ["Tier", "Retention", "Thirty days"] {
                assert!(
                    text.contains(table_context),
                    "{table_context:?} missing from {text:?}"
                );
            }
        }
    }
}

#[test]
fn html_import_excludes_script_and_style_contents_from_search() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let path = root.join("page.html");
    fs::write(
        &path,
        "<html><head><style>STYLE_SECRET_MARKER { color: red; }</style></head><body><p>VISIBLE_PAGE_MARKER</p><script>const SCRIPT_SECRET_MARKER = 'hidden';</script></body></html>",
    )
    .unwrap();
    let path = path.to_string_lossy().into_owned();
    assert!(run(root, &["import", &path]).0);
    assert!(run(root, &["refresh"]).0);

    let visible = search_json(root, &["VISIBLE_PAGE_MARKER"]);
    assert!(
        visible.iter().any(|result| result["path"] == path),
        "{visible:?}"
    );
    assert!(search_json(root, &["SCRIPT_SECRET_MARKER"]).is_empty());
    assert!(search_json(root, &["STYLE_SECRET_MARKER"]).is_empty());
}

#[test]
fn html_import_discards_unclosed_script_and_style_contents() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let sources = [
        (
            "unclosed-script.html",
            "VISIBLE_BEFORE_SCRIPT_MARKER<script>SECRET_UNCLOSED_SCRIPT_MARKER",
            "VISIBLE_BEFORE_SCRIPT_MARKER",
            "SECRET_UNCLOSED_SCRIPT_MARKER",
        ),
        (
            "unclosed-style.html",
            "VISIBLE_BEFORE_STYLE_MARKER<style>SECRET_UNCLOSED_STYLE_MARKER",
            "VISIBLE_BEFORE_STYLE_MARKER",
            "SECRET_UNCLOSED_STYLE_MARKER",
        ),
    ];
    for (name, contents, _, _) in sources {
        let path = root.join(name);
        fs::write(&path, contents).unwrap();
        let path = path.to_string_lossy().into_owned();
        assert!(run(root, &["import", &path]).0, "import {name}");
    }
    assert!(run(root, &["refresh"]).0);

    for (name, _, visible_marker, secret_marker) in sources {
        let path = root.join(name).to_string_lossy().into_owned();
        let visible = search_json(root, &[visible_marker]);
        assert!(
            visible.iter().any(|result| result["path"] == path),
            "{visible:?}"
        );
        assert!(search_json(root, &[secret_marker]).is_empty());
    }
}

#[test]
fn html_import_versions_hash_original_bytes_and_preserve_distinct_sources() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let sources = [
        (
            "first.html",
            "<html><body><p>Shared searchable text</p></body></html>",
            "aad0141fa5cf4b490b4c2762d7595bb4fe2187dbee6b877a11789911268e22fb",
        ),
        (
            "second.html",
            "<html><body><section>Shared searchable text</section></body></html>",
            "c01235d091024dd1034788d1185e5e5b9ccac21912510db6a6f00c388146569f",
        ),
    ];
    for (name, contents, _) in sources {
        let path = root.join(name);
        fs::write(&path, contents).unwrap();
        let path = path.to_string_lossy().into_owned();
        assert!(run(root, &["import", &path]).0, "import {name}");
    }
    assert!(run(root, &["refresh"]).0);

    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let mut versions = database
        .prepare(
            "SELECT s.locator,v.version,v.content,v.representation_sha256 \
             FROM imported_sources s JOIN imported_versions v ON v.source_id=s.id \
             ORDER BY s.locator",
        )
        .unwrap();
    let rows: Vec<(String, String, String, String)> = versions
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), sources.len());
    for ((_, version, content, representation_hash), (_, _, source_hash)) in
        rows.iter().zip(sources)
    {
        assert_eq!(version, source_hash);
        assert_eq!(content, "   Shared searchable text   ");
        assert_eq!(
            representation_hash,
            "61f66ecf5088e4d2cad86e9d3d5af2b7cb8d2a689f84b3c30c21ac742e0294e7"
        );
    }
    assert_ne!(rows[0].1, rows[1].1);
    assert_eq!(rows[0].2, rows[1].2);
    assert!(!search_json(root, &["Shared searchable text"]).is_empty());
}

#[test]
fn imported_files_refresh_with_references_and_both_retention_policies() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let file = root.join("external.txt");
    fs::write(&file, "imported first marker\n").unwrap();
    let file_string = file.to_string_lossy().into_owned();
    assert!(run(root, &["import", &file_string]).0);
    assert!(run(root, &["refresh"]).0);
    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let (version, content, representation_hash): (String, String, String) = database
        .query_row(
            "SELECT version,content,representation_sha256 FROM imported_versions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        version,
        "f525d245d3da65c919757490cd56b442540a5c5f03e4ee3d6c9a662e8c1a2ea7"
    );
    assert_eq!(content, "imported first marker\n");
    assert_eq!(representation_hash, version);
    drop(database);
    let first = search_json(root, &["marker"]);
    assert!(
        first
            .iter()
            .any(|x| x["path"] == file_string && x["line_start"] == 1),
        "{first:?}"
    );
    fs::write(&file, "imported second marker\n").unwrap();
    assert!(
        search_json(root, &["second"]).is_empty(),
        "search must not fetch sources"
    );
    assert!(run(root, &["retention", "all"]).0);
    assert!(run(root, &["refresh"]).0);
    assert!(!search_json(root, &["first"]).is_empty());
    assert!(!search_json(root, &["second"]).is_empty());
    fs::write(&file, "imported third marker\n").unwrap();
    assert!(run(root, &["refresh"]).0);
    assert!(run(root, &["retention", "latest"]).0);
    assert!(
        !search_json(root, &["first"])
            .iter()
            .any(|x| x["path"] == file_string)
    );
    assert!(
        !search_json(root, &["second"])
            .iter()
            .any(|x| x["path"] == file_string)
    );
    assert!(!search_json(root, &["third"]).is_empty());
    let directory = root.to_string_lossy().into_owned();
    assert!(run(root, &["import", &directory]).0);
    assert!(run(root, &["refresh"]).0);
    assert!(!search_json(root, &["Rust phrase"]).is_empty());
}

#[test]
fn imports_only_refresh_indexes_files_without_mutating_history_snapshot() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let database_path = root.join(".git/repodb.sqlite");
    let before = rusqlite::Connection::open(&database_path).unwrap();
    let before_commit: String = before
        .query_row("SELECT commit_id FROM git_commits LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let before_vectors: i64 = before
        .query_row("SELECT count(*) FROM unit_embeddings", [], |row| row.get(0))
        .unwrap();
    drop(before);

    let file = root.join("isolated-import.txt");
    fs::write(&file, "distinct import-only searchable marker\n").unwrap();
    let locator = file.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh", "--imports"]).0);
    let exact = search_json(root, &["distinct import-only searchable"]);
    assert!(
        exact.iter().any(|result| result["path"] == locator),
        "{exact:?}"
    );
    let semantic = search_json(root, &["distinct import-only searchable", "--semantic"]);
    assert!(
        semantic.iter().any(|result| result["path"] == locator),
        "{semantic:?}"
    );
    let combined = search_json(root, &["distinct import-only searchable", "--combined"]);
    assert!(
        combined.iter().any(|result| result["path"] == locator),
        "{combined:?}"
    );

    let after = rusqlite::Connection::open(&database_path).unwrap();
    let after_commit: String = after
        .query_row("SELECT commit_id FROM git_commits LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let after_vectors: i64 = after
        .query_row("SELECT count(*) FROM unit_embeddings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after_commit, before_commit);
    assert_eq!(after_vectors, before_vectors);
    assert!(run(root, &["refresh"]).0);
    assert!(
        search_json(root, &["distinct import-only searchable"])
            .iter()
            .any(|result| result["path"] == locator)
    );
}

#[test]
fn failed_imports_only_refresh_preserves_published_imports_and_history() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let file = root.join("retry-import.txt");
    fs::write(&file, "previously published import marker\n").unwrap();
    let locator = file.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh", "--imports"]).0);
    let database_path = root.join(".git/repodb.sqlite");
    let before = rusqlite::Connection::open(&database_path).unwrap();
    let history_commits: Vec<String> = before
        .prepare("SELECT commit_id FROM git_commits ORDER BY commit_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let history_vectors: i64 = before
        .query_row("SELECT count(*) FROM unit_embeddings", [], |row| row.get(0))
        .unwrap();
    drop(before);

    fs::write(&file, "unpublished replacement import marker\n").unwrap();
    let unavailable_url = "http://127.0.0.1:1/unreachable";
    assert!(run(root, &["import", unavailable_url]).0);
    assert!(!run(root, &["refresh", "--imports"]).0);
    assert!(!search_json(root, &["previously published import marker"]).is_empty());
    assert!(search_json(root, &["unpublished replacement import marker"]).is_empty());

    let after = rusqlite::Connection::open(&database_path).unwrap();
    let after_commits: Vec<String> = after
        .prepare("SELECT commit_id FROM git_commits ORDER BY commit_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let after_vectors: i64 = after
        .query_row("SELECT count(*) FROM unit_embeddings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after_commits, history_commits);
    assert_eq!(after_vectors, history_vectors);
}

#[test]
fn imports_only_dimension_mismatch_preserves_published_imports_and_history() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let file = root.join("dimension-import.txt");
    fs::write(&file, "published dimension marker\n").unwrap();
    let locator = file.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh", "--imports"]).0);

    let embedder = root.join(".git/repodb-test-embedder");
    fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0,0]'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&file, "rejected dimension marker\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .args(["refresh", "--imports"])
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root.join(".test-xdg-config"))
        .env(
            "REPODB_SEMGREP_CORE",
            semgrep_corpus::required_semgrep_core(),
        )
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!search_json(root, &["published dimension marker"]).is_empty());
    assert!(search_json(root, &["rejected dimension marker"]).is_empty());

    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let dimension: String = database
        .query_row(
            "SELECT value FROM embedding_config WHERE key='dimension'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dimension, "2");
    let vector_dimension: usize = database
        .query_row(
            "SELECT length(vector)/4 FROM unit_embeddings LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(vector_dimension, 2);
}

#[test]
fn imported_files_keep_line_references_for_paragraph_and_bounded_units() {
    let repo = repository();
    let root = repo.path();
    let file = root.join("many-lines.txt");
    let input = (1..=256)
        .map(|line| format!("stream marker line {line}\n"))
        .collect::<String>();
    fs::write(&file, input).unwrap();
    let locator = file.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh"]).0);

    let results = search_json(root, &["marker"]);
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|result| result["line_start"] == 1));
    assert!(results.iter().any(|result| result["line_end"] == 256));
}

#[test]
fn local_file_larger_than_previous_limit_refreshes() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let file = root.join("large.txt");
    fs::write(&file, "previous searchable marker\n").unwrap();
    let locator = file.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh"]).0);

    // Sparse padding exceeds the former 10 MiB input ceiling while keeping the
    // fixture fast and leaving the searchable marker easy to assert.
    fs::write(
        &file,
        format!("larger file marker\\n{}", "x".repeat(10 * 1024 * 1024)),
    )
    .unwrap();
    let (success, _) = run(root, &["refresh", "--size-limit=ignore"]);
    assert!(
        success,
        "valid files above the former ceiling should refresh"
    );
    assert_eq!(search_json(root, &["larger file marker"]).len(), 1);
}

#[test]
fn pdf_conversion_larger_than_previous_limit_refreshes() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let text = root.join("version.txt");
    fs::write(&text, "previous atomic version marker\n").unwrap();
    let text_locator = text.to_string_lossy().into_owned();
    let pdf = root.join("oversized.pdf");
    fs::write(&pdf, b"%PDF-test").unwrap();
    let pdf_locator = pdf.to_string_lossy().into_owned();
    assert!(run(root, &["import", &text_locator]).0);
    assert!(run(root, &["import", &pdf_locator]).0);

    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let converter = bin.join("pdftotext");
    fs::write(
        &converter,
        "#!/bin/sh\ncat >/dev/null\nif [ \"$PDF_BIG\" = 1 ]; then dd if=/dev/zero bs=1048576 count=11 2>/dev/null; else printf 'cached page'; fi\n",
    )
    .unwrap();
    fs::set_permissions(&converter, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    assert!(run_with_env(root, &["refresh"], &[("PATH", path.as_str())]).0);
    fs::write(&pdf, b"%PDF-changed input").unwrap();
    fs::write(&text, "uncommitted replacement marker\n").unwrap();

    let (success, _) = run_with_env(
        root,
        &["refresh"],
        &[("PATH", path.as_str()), ("PDF_BIG", "1")],
    );
    assert!(
        success,
        "valid PDF output above the former ceiling should refresh"
    );
    assert!(search_json(root, &["previous atomic version marker"]).is_empty());
    assert_eq!(
        search_json(root, &["uncommitted replacement marker"]).len(),
        1
    );
}

#[test]
fn noisy_pdf_converter_stderr_is_drained_without_rejecting_output() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let preserved = root.join("preserved.txt");
    fs::write(&preserved, "previous searchable snapshot marker\n").unwrap();
    let pdf = root.join("manual.pdf");
    fs::write(&pdf, b"%PDF-test").unwrap();
    let preserved_locator = preserved.to_string_lossy().into_owned();
    let pdf_locator = pdf.to_string_lossy().into_owned();
    assert!(run(root, &["import", &preserved_locator]).0);
    assert!(run(root, &["import", &pdf_locator]).0);
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let converter = bin.join("pdftotext");
    fs::write(
        &converter,
        "#!/bin/sh\ncat >/dev/null\nprintf 'old pdf text'\n",
    )
    .unwrap();
    fs::set_permissions(&converter, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    assert!(run_with_env(root, &["refresh"], &[("PATH", path.as_str())]).0);

    fs::write(
        &converter,
        "#!/bin/sh\ncat >/dev/null\nhead -c 1048576 /dev/zero >&2\nprintf 'replacement text'\n",
    )
    .unwrap();
    fs::write(&pdf, b"%PDF-changed source").unwrap();
    fs::write(&preserved, "replacement searchable snapshot marker\n").unwrap();
    let (success, _) = run_with_env(root, &["refresh"], &[("PATH", path.as_str())]);
    assert!(success, "stderr volume should not reject valid PDF output");
    assert!(search_json(root, &["previous searchable snapshot marker"]).is_empty());
    assert_eq!(
        search_json(root, &["replacement searchable snapshot marker"]).len(),
        1
    );
}

#[test]
fn oversized_import_embedding_output_does_not_replace_imported_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let file = root.join("source.txt");
    fs::write(&file, "previous imported marker\n").unwrap();
    let locator = file.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh"]).0);

    let command = root.join("import-embedder");
    fs::write(&command, "#!/bin/sh\ncat >/dev/null\nprintf '[1.0, 0.0]'\n").unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", command.to_str().unwrap()]).0);
    fs::write(
        &command,
        "#!/bin/sh\ncat >/dev/null\ndd if=/dev/zero bs=1048576 count=2 2>/dev/null\n",
    )
    .unwrap();
    fs::write(&file, "replacement imported marker\n").unwrap();

    let (success, _) = run(root, &["refresh"]);
    assert!(
        !success,
        "oversized imported embedding output must fail refresh"
    );
    assert_eq!(search_json(root, &["previous imported marker"]).len(), 1);
    assert!(search_json(root, &["replacement imported marker"]).is_empty());
}

#[test]
fn imported_git_retention_all_keeps_only_the_imported_commit_pin() {
    let repo = repository();
    let root = repo.path();
    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(external.path().join("marker.md"), "pinned import marker\n").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "first"]);
    let first = git_text(external.path(), &["rev-parse", "HEAD"]);
    let locator = external.path().to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["retention", "all"]).0);
    fs::write(external.path().join("marker.md"), "later ancestor marker\n").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "second"]);
    assert!(run(root, &["refresh"]).0);
    let pinned = search_json(root, &["pinned import marker"]);
    assert_eq!(pinned.len(), 1, "{pinned:?}");
    assert_eq!(pinned[0]["commit_id"], first);
    assert!(search_json(root, &["later ancestor marker"]).is_empty());
    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let count: i64 = database.query_row(
        "SELECT count(*) FROM imported_git_units WHERE source_id=(SELECT id FROM imported_sources WHERE locator=?1)",
        [&locator], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn populated_legacy_imported_git_migrates_under_latest_retention_and_preserves_vector() {
    let repo = repository();
    let root = repo.path();
    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(
        external.path().join("latest.md"),
        "latest legacy imported marker\n",
    )
    .unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "latest source commit"]);

    let locator = external.path().to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh"]).0);
    assert!(run(root, &["retention", "latest"]).0);
    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let vector = vec![0, 0, 0, 64, 0, 0, 64, 64];
    database.execute_batch(
        "CREATE TABLE imported_git_units_legacy(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,line,text));",
    ).unwrap();
    let (source_id, commit): (String, String) = database
        .query_row(
            "SELECT source_id,commit_id FROM imported_git_units LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    database.execute(
        "INSERT INTO imported_git_units_legacy VALUES(?1,?2,'latest.md',1,1,'latest legacy imported marker',?3)",
        rusqlite::params![source_id, commit, vector],
    ).unwrap();
    database.execute_batch(
        "DROP TABLE imported_git_units; ALTER TABLE imported_git_units_legacy RENAME TO imported_git_units;",
    ).unwrap();

    assert!(run(root, &["refresh"]).0);
    let results = search_json(root, &["latest legacy imported marker"]);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["commit_id"], commit);
    assert_eq!(results[0]["path"], "latest.md");
    assert_eq!(results[0]["text"], "latest legacy imported marker");
    let migrated_vector: Option<Vec<u8>> = database
        .query_row(
            "SELECT vector FROM imported_git_units WHERE commit_id=?1",
            [&commit],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(migrated_vector, Some(vector));
    let columns = database
        .prepare("PRAGMA table_info(imported_git_units)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!columns.iter().any(|column| column == "text"));
}

#[test]
fn latest_retention_skips_unreachable_legacy_git_rows_with_missing_blobs() {
    let repo = repository();
    let root = repo.path();
    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(external.path().join("marker.md"), "old legacy marker\n").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "old"]);
    let old_commit = git_text(external.path(), &["rev-parse", "HEAD"]);
    let old_blob = git_text(external.path(), &["rev-parse", "HEAD:marker.md"]);
    git(external.path(), &["checkout", "--orphan", "latest"]);
    git(external.path(), &["rm", "-rf", "."]);
    fs::write(external.path().join("marker.md"), "new legacy marker\n").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "latest"]);
    let latest_commit = git_text(external.path(), &["rev-parse", "HEAD"]);
    let locator = external.path().to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["retention", "latest"]).0);

    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let source_id: String = database
        .query_row("SELECT id FROM imported_sources", [], |row| row.get(0))
        .unwrap();
    database.execute_batch(
        "CREATE TABLE imported_git_units_legacy(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,line,text));",
    ).unwrap();
    database.execute(
        "INSERT INTO imported_git_units_legacy VALUES(?1,?2,'marker.md',1,1,'old legacy marker',NULL)",
        rusqlite::params![source_id, old_commit],
    ).unwrap();
    database.execute(
        "INSERT INTO imported_git_units_legacy VALUES(?1,?2,'marker.md',1,1,'new legacy marker',NULL)",
        rusqlite::params![source_id, latest_commit],
    ).unwrap();
    database.execute_batch(
        "DROP TABLE imported_git_units; ALTER TABLE imported_git_units_legacy RENAME TO imported_git_units;",
    ).unwrap();
    let old_object = external
        .path()
        .join(".git/objects")
        .join(&old_blob[..2])
        .join(&old_blob[2..]);
    fs::remove_file(old_object).unwrap();

    assert!(run(root, &["refresh"]).0);
    let results = search_json(root, &["new legacy marker"]);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["commit_id"], latest_commit);
    assert!(search_json(root, &["old legacy marker"]).is_empty());
    let count: i64 = database
        .query_row("SELECT count(*) FROM imported_git_units", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn refresh_preserves_legacy_snapshot_when_pinned_commit_blob_is_missing() {
    let repo = repository();
    let root = repo.path();
    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(external.path().join("marker.md"), "legacy marker\n").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "source"]);
    let commit = git_text(external.path(), &["rev-parse", "HEAD"]);
    let blob = git_text(external.path(), &["rev-parse", "HEAD:marker.md"]);
    let branch = git_text(external.path(), &["branch", "--show-current"]);
    let locator = external.path().to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["retention", "latest"]).0);
    assert!(run(root, &["refresh"]).0);
    let embedder = fake_embedder(root, "printf '[1,0]'");
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);

    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let source_id: String = database
        .query_row("SELECT id FROM imported_sources", [], |row| row.get(0))
        .unwrap();
    let vector: Option<Vec<u8>> = database
        .query_row(
            "SELECT vector FROM imported_git_units WHERE source_id=?1 AND commit_id=?2",
            rusqlite::params![source_id, commit],
            |row| row.get(0),
        )
        .unwrap();
    assert!(vector.is_some(), "published fixture must have a vector");
    database.execute_batch(
        "CREATE TABLE imported_git_units_legacy(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,line,text));",
    ).unwrap();
    database.execute(
        "INSERT INTO imported_git_units_legacy SELECT source_id,commit_id,path,line,line_end,'legacy marker',vector FROM imported_git_units",
        [],
    ).unwrap();
    database.execute_batch(
        "DROP TABLE imported_git_units; ALTER TABLE imported_git_units_legacy RENAME TO imported_git_units;",
    ).unwrap();

    git(
        external.path(),
        &["update-ref", "-d", &format!("refs/heads/{branch}")],
    );
    assert!(git_text(external.path(), &["rev-list", "--all"]).is_empty());
    let old_object = external
        .path()
        .join(".git/objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    fs::remove_file(old_object).unwrap();

    let (ok, output) = run(root, &["refresh"]);
    assert!(
        !ok,
        "refresh must fail when the pinned commit's blob is missing: {output}"
    );
    let count: i64 = database
        .query_row(
            "SELECT count(*) FROM imported_git_units WHERE source_id=?1",
            [&source_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let versions: i64 = database
        .query_row(
            "SELECT count(*) FROM imported_versions WHERE source_id=?1",
            [&source_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(versions, 1);
    assert_eq!(search_json(root, &["legacy marker"]).len(), 1);
    let columns = database
        .prepare("PRAGMA table_info(imported_git_units)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(columns.contains(&"text".to_owned()), "{columns:?}");
    assert_eq!(
        columns,
        [
            "source_id",
            "commit_id",
            "path",
            "line",
            "line_end",
            "text",
            "vector"
        ]
    );
}

#[test]
fn imported_git_semantic_search_hydrates_distinct_rust_units_on_one_line() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repository();
    let root = repo.path();
    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(
        external.path().join("units.rs"),
        "fn alpha() {} fn beta() {}\n",
    )
    .unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "two units on one line"]);

    let locator = external.path().to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh"]).0);
    let embedder = root.join("git-unit-embedder");
    fs::write(
        &embedder,
        "#!/bin/sh\ncat >/dev/null\nprintf '[1.0, 0.0]'\n",
    )
    .unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);

    let results = search_json(root, &["unrelated phrase", "--semantic"]);
    let units = results
        .iter()
        .filter(|result| result["repo_id"] == locator && result["path"] == "units.rs")
        .map(|result| result["text"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(units.len() >= 2, "{results:?}");
    assert!(units.iter().all(|text| !text.is_empty()), "{results:?}");
    let joined = units.join(" ");
    assert!(
        joined.contains("alpha") && joined.contains("beta"),
        "{results:?}"
    );
}

#[test]
fn imported_git_semantic_top_n_does_not_hydrate_nonselected_blob() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repository();
    let root = repo.path();
    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(external.path().join("selected.txt"), "selected candidate\n").unwrap();
    fs::write(
        external.path().join("unselected.txt"),
        "unselected candidate\n",
    )
    .unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "semantic candidates"]);

    let config_home = root.join("semantic-config");
    let config_dir = config_home.join("repodb");
    fs::create_dir_all(&config_dir).unwrap();
    let embedder = root.join("semantic-embedder");
    fs::write(
        &embedder,
        "#!/usr/bin/env python3\nimport sys\nfor text in sys.stdin.buffer.read().split(b'\\0'):\n    if b'semantic query' in text or b'selected candidate' in text:\n        print('[1.0, 0.0]')\n    else:\n        print('[0.0, 1.0]')\n",
    )
    .unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[tools]\nembedding_command = {:?}\n",
            embedder.to_string_lossy()
        ),
    )
    .unwrap();
    let invoke = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
            .args(args)
            .current_dir(root)
            .env("XDG_CONFIG_HOME", &config_home)
            .env(
                "REPODB_SEMGREP_CORE",
                semgrep_corpus::required_semgrep_core(),
            )
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap(),
        )
    };

    let locator = external.path().to_string_lossy().into_owned();
    assert!(invoke(&["import", &locator]).0);
    assert!(invoke(&["refresh"]).0);

    let blob_id = git_text(external.path(), &["rev-parse", "HEAD:unselected.txt"]);
    let object = external
        .path()
        .join(".git/objects")
        .join(&blob_id[..2])
        .join(&blob_id[2..]);
    fs::remove_file(object).unwrap();

    let (success, output) = invoke(&[
        "search",
        "semantic query",
        "--semantic",
        "--semantic-limit",
        "1",
        "--type=content",
        "--json",
    ]);
    assert!(success, "{output}");
    let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["path"], "selected.txt", "{results:?}");
}

#[test]
fn imported_git_repository_refreshes_with_identity_commit_path_and_line_refs() {
    let repo = repository();
    let root = repo.path();
    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(external.path().join("guide.md"), "first git marker\n").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "first"]);
    let first = git_text(external.path(), &["rev-parse", "HEAD"]);
    let locator = external.path().to_string_lossy().into_owned();
    assert!(run(root, &["import", "--revision", &first, &locator]).0);
    assert!(run(root, &["refresh", "--imports"]).0);
    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let columns = database
        .prepare("PRAGMA table_info(imported_git_units)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!columns.iter().any(|column| column == "text"));
    assert!(columns.iter().any(|column| column == "blob_id"));
    assert!(columns.iter().any(|column| column == "byte_start"));
    drop(database);
    let results = search_json(root, &["first git marker"]);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["path"], "guide.md");
    assert_eq!(results[0]["commit_id"], first);
    assert_eq!(results[0]["line_start"], 1);
    assert_eq!(results[0]["line_end"], 1);
    assert!(results[0]["repo_id"].as_str().unwrap().contains(&locator));
    let embedder = root.join("git-embedder");
    fs::write(
        &embedder,
        "#!/bin/sh\ncat >/dev/null\nprintf '[1.0, 0.0]'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);
    let semantic = search_json(root, &["unrelated phrase", "--semantic"]);
    assert!(
        semantic
            .iter()
            .any(|x| x["repo_id"] == locator && x["commit_id"] == first)
    );

    fs::write(external.path().join("guide.md"), "second git marker\n").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "second"]);
    let second = git_text(external.path(), &["rev-parse", "HEAD"]);
    assert!(search_json(root, &["second git marker"]).is_empty());
    assert!(run(root, &["retention", "all"]).0);
    assert!(run(root, &["refresh"]).0);
    assert!(search_json(root, &["second git marker"]).is_empty());
    let still_pinned = search_json(root, &["first git marker"]);
    assert_eq!(still_pinned.len(), 1);
    assert_eq!(still_pinned[0]["commit_id"], first);
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh"]).0);
    let newest = search_json(root, &["second git marker"]);
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0]["commit_id"], second);
    assert!(!search_json(root, &["first git marker"]).is_empty());
    assert!(run(root, &["retention", "latest"]).0);
    assert!(search_json(root, &["first git marker"]).is_empty());

    let bare = external.path().join("remote.git");
    let clone = Command::new("git")
        .args(["clone", "--bare"])
        .arg(external.path())
        .arg(&bare)
        .output()
        .unwrap();
    assert!(clone.status.success());
    let remote = format!("file://{}", bare.display());
    assert!(run(root, &["import", "--revision", "HEAD", &remote]).0);
    fs::write(external.path().join("guide.md"), "third remote marker\\n").unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "third"]);
    let third = git_text(external.path(), &["rev-parse", "HEAD"]);
    let remote_branch = git_text(external.path(), &["branch", "--show-current"]);
    git(&bare, &["fetch", external.path().to_str().unwrap(), &third]);
    git(
        &bare,
        &["update-ref", &format!("refs/heads/{remote_branch}"), &third],
    );
    assert!(run(root, &["refresh"]).0);
    assert!(search_json(root, &["third remote marker"]).is_empty());
    let remote_result = search_json(root, &["second git marker"]);
    assert!(
        remote_result
            .iter()
            .any(|x| x["repo_id"].as_str().unwrap().contains(&remote)),
        "{remote_result:?}"
    );
}

#[test]
fn long_file_git_locator_uses_bounded_mirror_directory_name() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);

    let source = repository();
    let parent = tempfile::tempdir().unwrap();
    let nested = parent.path().join("a".repeat(100)).join("b".repeat(100));
    fs::create_dir_all(&nested).unwrap();
    let bare = nested.join("remote.git");
    let clone = Command::new("git")
        .args(["clone", "--bare"])
        .arg(source.path())
        .arg(&bare)
        .output()
        .unwrap();
    assert!(clone.status.success());
    let locator = format!("file://{}", bare.display());
    assert!(locator.len() > 128);
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh"]).0);
    assert!(!search_json(root, &["Rust phrase"]).is_empty());

    let cache = root.join(".git").join("repodb-cache");
    let mirrors = fs::read_dir(cache)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("git-"))
        .collect::<Vec<_>>();
    assert_eq!(mirrors.len(), 1, "{mirrors:?}");
    assert!(mirrors[0].len() <= 68, "{:?}", mirrors[0]);
}

#[test]
fn imported_pdfs_preserve_pages_cache_conversions_and_refresh_versions() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let calls = root.join("converter-calls");
    let converter = bin.join("pdftotext");
    fs::write(
        &converter,
        "#!/bin/sh\nprintf called >> \"$PDF_CALLS\"\nprintf '%s' \"$(cat \"$PDF_TEXT\")\"\n",
    )
    .unwrap();
    fs::set_permissions(&converter, fs::Permissions::from_mode(0o755)).unwrap();
    let pdf = root.join("manual.pdf");
    fs::write(&pdf, b"%PDF-test first source").unwrap();
    let pdf_string = pdf.to_string_lossy().into_owned();
    assert!(run(root, &["import", &pdf_string]).0);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let text = root.join("converter-text");
    fs::write(&text, "first page text\x0csecond page marker\x0c").unwrap();
    let env = [
        ("PATH", path.as_str()),
        ("PDF_CALLS", calls.to_str().unwrap()),
        ("PDF_TEXT", text.to_str().unwrap()),
    ];
    assert!(run_with_env(root, &["refresh"], &env).0);
    assert!(run_with_env(root, &["refresh"], &env).0);
    assert_eq!(fs::read_to_string(&calls).unwrap(), "called");
    let cache_dir = root.join(".git/repodb-cache");
    assert!(cache_dir.is_dir());
    let cache = fs::read_dir(&cache_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(&cache, b"{partial").unwrap();
    assert!(run_with_env(root, &["refresh"], &env).0);
    assert_eq!(fs::read_to_string(&calls).unwrap(), "calledcalled");
    assert!(run_with_env(root, &["refresh"], &env).0);
    assert_eq!(fs::read_to_string(&calls).unwrap(), "calledcalled");
    assert!(serde_json::from_slice::<Vec<String>>(&fs::read(&cache).unwrap()).is_ok());
    let database = root.join(".git/repodb.sqlite");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let (durable, representation_hash): (String, String) = connection
        .query_row(
            "SELECT content,representation_sha256 FROM imported_versions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let durable: serde_json::Value = serde_json::from_str(&durable).unwrap();
    assert_eq!(
        durable["pages"],
        serde_json::json!(["first page text", "second page marker", ""])
    );
    assert_eq!(representation_hash.len(), 64);
    fs::remove_file(&cache).unwrap();
    fs::remove_file(&pdf).unwrap();
    let embedder = fake_embedder(root, "printf '[1,0]'");
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);
    let page_two = search_json(root, &["second page marker"]);
    assert_eq!(page_two[0]["path"], pdf_string);
    assert_eq!(page_two[0]["page"], 2);
    assert_eq!(page_two[0]["line_start"], 2);
    let (human_ok, human_page) = run(root, &["search", "second page marker"]);
    assert!(human_ok, "{human_page}");
    assert!(
        human_page.contains(&format!("{pdf_string}:page 2")),
        "human output must expose the page reference: {human_page}"
    );

    assert!(run(root, &["retention", "all"]).0);
    fs::write(&pdf, b"%PDF-test changed source").unwrap();
    fs::write(&text, "updated page marker\x0c").unwrap();
    assert!(run_with_env(root, &["refresh"], &env).0);
    assert_eq!(fs::read_to_string(&calls).unwrap(), "calledcalledcalled");
    assert!(!search_json(root, &["first page text"]).is_empty());
    assert!(!search_json(root, &["updated page marker"]).is_empty());
    assert!(run(root, &["retention", "latest"]).0);
    assert!(search_json(root, &["first page text"]).is_empty());
    assert!(!search_json(root, &["updated page marker"]).is_empty());

    // Verify large page sets and final-page references; cached arrays remain
    // reusable without rerunning pdftotext.
    let many_pages = (0..1_001).map(|i| format!("page-{i}"));
    fs::write(&text, many_pages.collect::<Vec<_>>().join("\x0c")).unwrap();
    fs::write(&pdf, b"%PDF-test many pages").unwrap();
    assert!(run_with_env(root, &["refresh"], &env).0);
    let calls_after_conversion = fs::read_to_string(&calls).unwrap();
    assert!(run_with_env(root, &["refresh"], &env).0);
    assert_eq!(fs::read_to_string(&calls).unwrap(), calls_after_conversion);
    let last_page = search_json(root, &["page-1000"]);
    assert!(last_page.iter().any(|result| result["page"] == 1_001));
}

#[test]
fn imported_pdf_splits_paragraphs_with_shared_page_metadata() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let converter = bin.join("pdftotext");
    fs::write(&converter, "#!/bin/sh\ncat \"$PDF_TEXT\"\n").unwrap();
    fs::set_permissions(&converter, fs::Permissions::from_mode(0o755)).unwrap();
    let pdf = root.join("paragraphs.pdf");
    fs::write(&pdf, b"%PDF-test paragraphs").unwrap();
    let pdf_string = pdf.to_string_lossy().into_owned();
    assert!(run(root, &["import", &pdf_string]).0);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let text = root.join("converter-text");
    let long_paragraph = "x".repeat(5_000);
    fs::write(
        &text,
        format!(
            "cross-page duplicate marker\n\nrepeated paragraph marker\n\nrepeated paragraph marker\n\nwrapped line one\r\nwrapped line two\r\n \t\r\nUnicode café 🌍\n\x0c \t\n\x0c cross-page duplicate marker\n\nrepeated paragraph marker\n\n{long_paragraph}\x0cfinal page marker\n"
        ),
    )
    .unwrap();
    let env = [
        ("PATH", path.as_str()),
        ("PDF_TEXT", text.to_str().unwrap()),
    ];
    assert!(run_with_env(root, &["refresh"], &env).0);
    let rust = search_json(root, &["Rust phrase"]);
    assert_eq!(rust.len(), 1);
    assert_eq!(rust[0]["text"], "fn main() { println!(\"Rust phrase\"); }");

    let cross_page = search_json(root, &["cross-page duplicate marker"]);
    assert_eq!(cross_page.len(), 2);
    assert_eq!(
        cross_page
            .iter()
            .map(|result| result["page"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_ne!(cross_page[0]["line_start"], cross_page[1]["line_start"]);

    let repeated = search_json(root, &["repeated paragraph marker"]);
    assert_eq!(repeated.len(), 3);
    let page_one_repeated = repeated
        .iter()
        .filter(|result| result["page"] == 1)
        .collect::<Vec<_>>();
    assert_eq!(page_one_repeated.len(), 2);
    assert_ne!(
        page_one_repeated[0]["line_start"],
        page_one_repeated[1]["line_start"]
    );
    let wrapped = search_json(root, &["wrapped"]);
    assert_eq!(wrapped.len(), 1);
    assert_eq!(wrapped[0]["page"], 1);
    assert_eq!(wrapped[0]["text"], "wrapped line one\r\nwrapped line two");
    let unicode = search_json(root, &["café"]);
    assert_eq!(unicode.len(), 1);
    assert_eq!(unicode[0]["page"], 1);
    let oversized = search_json(root, &["x"])
        .into_iter()
        .filter(|result| result["path"] == pdf_string)
        .collect::<Vec<_>>();
    assert_eq!(oversized.len(), 2);
    assert!(oversized.iter().all(|result| result["page"] == 3));
    assert!(
        oversized
            .iter()
            .all(|result| result["text"].as_str().unwrap().len() <= 4096)
    );
    assert_eq!(search_json(root, &["final page marker"])[0]["page"], 4);
    assert!(
        search_json(root, &["wrapped"])
            .iter()
            .all(|result| result["page"] == 1)
    );

    // Refresh reads the cached extracted pages and preserves the same page units.
    assert!(run_with_env(root, &["refresh"], &env).0);
    let cached_cross_page = search_json(root, &["cross-page duplicate marker"]);
    assert_eq!(
        cached_cross_page
            .iter()
            .map(|result| result["page"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_ne!(
        cached_cross_page[0]["line_start"],
        cached_cross_page[1]["line_start"]
    );
    assert_eq!(search_json(root, &["repeated paragraph marker"]).len(), 3);
    assert_eq!(search_json(root, &["wrapped"]).len(), 1);
    assert_eq!(search_json(root, &["final page marker"])[0]["page"], 4);

    // Reembedding updates each persisted PDF unit without merging page units.
    let embedder = root.join("embedder");
    fs::write(
        &embedder,
        "#!/bin/sh\ncat >/dev/null\nprintf '[1.0, 0.0]'\n",
    )
    .unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()],).0);
    let reembedded_cross_page = search_json(root, &["cross-page duplicate marker"]);
    assert_eq!(
        reembedded_cross_page
            .iter()
            .map(|result| result["page"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_ne!(
        reembedded_cross_page[0]["line_start"],
        reembedded_cross_page[1]["line_start"]
    );
    assert_eq!(search_json(root, &["repeated paragraph marker"]).len(), 3);
    assert_eq!(search_json(root, &["wrapped"]).len(), 1);
    assert_eq!(search_json(root, &["final page marker"])[0]["page"], 4);
    for mode in ["--semantic", "--combined"] {
        let mut args = vec!["repeated paragraph marker", mode];
        if mode == "--combined" {
            args.extend(["--semantic-limit", "10"]);
        }
        let results = search_json(root, &args);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    result["path"] == pdf_string
                        && result["page"] == 1
                        && result["text"] == "repeated paragraph marker"
                })
                .count(),
            2,
            "{mode}: {results:?}"
        );
    }
}

#[test]
fn imports_retain_latest_fetch_and_reembed_backfills_and_refreshes_vectors() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let fake = root.join("fake-import-embedder");
    fs::write(&fake, "#!/bin/sh\ncat >/dev/null\nprintf '[1.0, 0.0]'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let file = root.join("external.txt");
    fs::write(&file, "old version marker\n").unwrap();
    let file_string = file.to_string_lossy().into_owned();
    assert!(run(root, &["import", &file_string]).0);
    assert!(run(root, &["refresh"]).0);
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);
    assert!(
        search_json(root, &["marker", "--semantic"])
            .iter()
            .any(|x| x["path"] == file_string)
    );
    assert!(run(root, &["retention", "all"]).0);
    fs::write(&file, "newest version marker\n").unwrap();
    assert!(run(root, &["refresh"]).0);
    assert!(run(root, &["retention", "latest"]).0);
    let newest = search_json(root, &["newest"]);
    assert!(
        newest.iter().any(|x| x["path"] == file_string),
        "{newest:?}"
    );
    assert!(
        !search_json(root, &["old version"])
            .iter()
            .any(|x| x["path"] == file_string)
    );
}

#[test]
fn combined_search_merges_imported_candidates_before_global_limit() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let fake = root.join("fake-import-embedder");
    fs::write(&fake, "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *needle*) printf '[1.0, 0.0]' ;; *) printf '[0.0, 1.0]' ;; esac\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);
    let file = root.join("external.txt");
    fs::write(&file, "needle imported exact hit\n").unwrap();
    let file_string = file.to_string_lossy().into_owned();
    assert!(run(root, &["import", &file_string]).0);
    assert!(run(root, &["refresh"]).0);
    let results = search_json(root, &["needle", "--combined", "--semantic-limit", "1"]);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["path"], file_string, "{results:?}");
}

#[test]
fn imported_url_fetch_accepts_data_above_old_limit_and_restricts_redirect_protocols() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let capture = root.join("curl-args.txt");
    let curl = bin.join("curl");
    fs::write(
        &curl,
        "#!/bin/sh\nprintf '%s\n' \"$@\" > \"$CAPTURE\"\nhead -c 10485761 /dev/zero\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["import", "https://example.invalid/source.txt"]).0);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    assert!(
        run_with_env(
            root,
            &["refresh", "--size-limit=ignore"],
            &[("PATH", &path), ("CAPTURE", capture.to_str().unwrap())]
        )
        .0
    );
    let args = fs::read_to_string(capture).unwrap();
    for expected in ["--max-time\n30", "--proto-redir\n=http,https"] {
        assert!(args.contains(expected), "missing {expected:?} in {args:?}");
    }
}

#[test]
fn imported_url_and_semantic_search_use_loopback_and_fake_embeddings() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    let repo = repository();
    let root = repo.path();
    let fake = root.join("fake-import-embedder");
    fs::write(&fake, "#!/bin/sh\ncat >/dev/null\nprintf '[1.0, 0.0]'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["init"]).0);
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for status in ["200 OK", "200 OK", "500 Internal Server Error"] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            if status == "200 OK" {
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\nConnection: close\r\n\r\nurl imported semantic!\n").unwrap();
            } else {
                stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
        }
    });
    let url = format!("http://{address}/source.txt");
    assert!(run(root, &["import", &url]).0);
    assert!(run(root, &["refresh"]).0);
    let first_results = search_json(root, &["semantic", "--semantic"]);
    let first_observation = first_results
        .iter()
        .find(|result| result["path"] == url && result["line_start"] == 1)
        .unwrap()
        .clone();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(run(root, &["refresh", "--imports"]).0);
    let second_results = search_json(root, &["semantic", "--semantic"]);
    let second_observation = second_results
        .iter()
        .find(|result| result["path"] == url && result["line_start"] == 1)
        .unwrap()
        .clone();
    assert!(!run(root, &["refresh", "--imports"]).0);
    server.join().unwrap();
    let results = search_json(root, &["semantic", "--semantic"]);
    let imported = results
        .iter()
        .find(|result| result["path"] == url && result["line_start"] == 1)
        .unwrap_or_else(|| panic!("{results:?}"));
    assert_eq!(imported["blob_id"].as_str().unwrap().len(), 64);
    for field in ["first_retrieved_at", "last_retrieved_at"] {
        let timestamp = imported[field].as_str().unwrap();
        assert!(timestamp.ends_with('Z'), "{field}: {timestamp}");
        assert_eq!(timestamp.len(), 20, "{field}: {timestamp}");
    }
    assert_eq!(imported["blob_id"], second_observation["blob_id"]);
    assert_eq!(imported["blob_id"], first_observation["blob_id"]);
    assert_eq!(
        second_observation["first_retrieved_at"],
        first_observation["first_retrieved_at"]
    );
    assert!(
        second_observation["last_retrieved_at"].as_str().unwrap()
            > first_observation["last_retrieved_at"].as_str().unwrap()
    );
    assert_eq!(
        imported["first_retrieved_at"],
        second_observation["first_retrieved_at"]
    );
    assert_eq!(
        imported["last_retrieved_at"],
        second_observation["last_retrieved_at"]
    );
    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let version_count: i64 = database
        .query_row("SELECT count(*) FROM imported_versions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version_count, 1, "identical response bytes deduplicate");
    let (ok, human) = run(root, &["search", "semantic"]);
    assert!(ok);
    assert!(human.contains(imported["blob_id"].as_str().unwrap()));
    assert!(human.contains(imported["first_retrieved_at"].as_str().unwrap()));
    assert!(human.contains(imported["last_retrieved_at"].as_str().unwrap()));
}

#[test]
fn url_parse_and_embedding_failures_preserve_the_published_snapshot_and_timestamps() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        os::unix::fs::PermissionsExt,
    };

    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);

    let embedder = root.join(".git/url-atomic-embedder");
    fs::write(
        &embedder,
        "#!/bin/sh\ninput=$(cat)\n[ \"$input\" != 'failing imported marker' ] || exit 1\nprintf '[1,0]'\n",
    )
    .unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let bodies: [&[u8]; 3] = [
            b"good url atomic marker\n",
            &[0xff, 0xfe],
            b"failing imported marker\n",
        ];
        for body in bodies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        }
    });
    let url = format!("http://{address}/atomic.txt");
    assert!(run(root, &["import", &url]).0);
    assert!(run(root, &["refresh", "--imports"]).0);
    let published = search_json(root, &["good url atomic marker"])
        .into_iter()
        .find(|result| result["path"] == url)
        .unwrap();

    assert!(!run(root, &["refresh", "--imports"]).0);
    let after_parse_failure = search_json(root, &["good url atomic marker"])
        .into_iter()
        .find(|result| result["path"] == url)
        .unwrap();
    assert_eq!(after_parse_failure, published);

    assert!(!run(root, &["refresh", "--imports"]).0);
    let after_embedding_failure = search_json(root, &["good url atomic marker"])
        .into_iter()
        .find(|result| result["path"] == url)
        .unwrap();
    assert_eq!(after_embedding_failure, published);
    assert!(search_json(root, &["failing imported marker"]).is_empty());
    server.join().unwrap();
}

#[test]
fn url_refresh_keeps_legacy_first_timestamp_unknown() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 24\r\nConnection: close\r\n\r\nlegacy timestamp marker\n",
                )
                .unwrap();
        }
    });
    let url = format!("http://{address}/legacy.txt");
    assert!(run(root, &["import", &url]).0);
    assert!(run(root, &["refresh", "--imports"]).0);

    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    database
        .execute(
            "UPDATE imported_versions SET first_retrieved_at=NULL,last_retrieved_at=NULL WHERE source_id=(SELECT id FROM imported_sources WHERE locator=?1)",
            [&url],
        )
        .unwrap();
    assert!(run(root, &["refresh", "--imports"]).0);
    server.join().unwrap();

    let result = search_json(root, &["legacy timestamp marker"])
        .into_iter()
        .find(|result| result["path"] == url)
        .unwrap();
    assert!(result["first_retrieved_at"].is_null());
    assert!(result["last_retrieved_at"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn imported_url_versions_follow_latest_and_all_hash_retention() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for body in [
            "alpha url version marker\n",
            "beta url version marker\n",
            "alpha url version marker\n",
            "beta url version marker\n",
            "alpha url version marker\n",
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    let url = format!("http://{address}/versions.txt");
    assert!(run(root, &["import", &url]).0);
    assert!(run(root, &["refresh", "--imports"]).0);
    assert!(run(root, &["retention", "all"]).0);
    assert!(run(root, &["refresh", "--imports"]).0);
    assert!(run(root, &["refresh", "--imports"]).0);

    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let hashes: Vec<String> = database
        .prepare("SELECT version FROM imported_versions ORDER BY fetched_order")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        hashes.len(),
        2,
        "all retains distinct hashes, not observations"
    );
    let alpha_hash: String = database
        .query_row(
            "SELECT version FROM imported_versions WHERE content='alpha url version marker\n'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let alpha_times: (String, String) = database
        .query_row(
            "SELECT first_retrieved_at,last_retrieved_at FROM imported_versions WHERE version=?1",
            [&alpha_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(run(root, &["retention", "latest"]).0);
    assert!(run(root, &["refresh", "--imports"]).0);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(run(root, &["refresh", "--imports"]).0);
    server.join().unwrap();
    let remaining: (i64, String, String) = database
        .query_row(
            "SELECT count(*),min(version),min(content) FROM imported_versions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(remaining.0, 1, "latest prunes prior URL hashes");
    assert_eq!(remaining.2, "alpha url version marker\n");
    assert_eq!(
        remaining.1, alpha_hash,
        "SHA-256 remains the version identity"
    );
    let latest_times: (String, String) = database
        .query_row(
            "SELECT first_retrieved_at,last_retrieved_at FROM imported_versions WHERE version=?1",
            [&remaining.1],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(latest_times.0.ends_with('Z') && latest_times.1.ends_with('Z'));
    assert_ne!(alpha_times.0, latest_times.0);
}

#[test]
fn scoped_refresh_indexes_commit_changes_and_reuses_unchanged_units() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    fs::write(root.join("changed.txt"), "old changed marker\n").unwrap();
    fs::write(root.join("unchanged.txt"), "unchanged marker\n").unwrap();
    fs::write(root.join("renamed-old.txt"), "rename marker\n").unwrap();
    fs::write(root.join("deleted.txt"), "deleted marker\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "initial scoped snapshot"]);
    let parent = git_text(root, &["rev-parse", "HEAD"]);

    let config_home = tempfile::tempdir().unwrap();
    let config_dir = config_home.path().join("repodb");
    fs::create_dir_all(&config_dir).unwrap();
    let embedder = root.join(".git/scoped-embedder");
    fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "[tools]\nembedding_command = {:?}\n",
            embedder.to_string_lossy()
        ),
    )
    .unwrap();
    let invoke = |args: &[&str]| {
        let mut args = args.to_vec();
        if args.first() == Some(&"search") {
            args.push("--type=cmcommits");
        }
        let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
            .args(&args)
            .current_dir(root)
            .env("XDG_CONFIG_HOME", config_home.path())
            .env(
                "REPODB_SEMGREP_CORE",
                semgrep_corpus::required_semgrep_core(),
            )
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    };
    assert!(invoke(&["init"]).0);

    let embedding_log = root.join(".git/embedding-log");
    fs::write(
        &embedder,
        format!(
            "#!/bin/sh\ninput=$(cat)\nprintf '%s\\n' \"$input\" >> '{}'\ncase \"$input\" in *'unchanged marker'*|*'rename marker'*|*'unstaged replacement marker'*|*'untracked marker'*) echo 'non-changed unit was re-embedded' >&2; exit 9 ;; esac\nprintf '[1,0]'\n",
            embedding_log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(root.join("changed.txt"), "new changed marker\n").unwrap();
    fs::write(root.join("added.txt"), "newly added marker\n").unwrap();
    fs::remove_file(root.join("deleted.txt")).unwrap();
    git(root, &["mv", "renamed-old.txt", "renamed-new.txt"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "scoped change"]);
    let current = git_text(root, &["rev-parse", "HEAD"]);
    fs::write(root.join("unchanged.txt"), "unstaged replacement marker\n").unwrap();
    fs::write(root.join("untracked.txt"), "untracked marker\n").unwrap();

    let (success, _, stderr) = invoke(&["refresh", "--changed"]);
    assert!(
        success,
        "scoped refresh failed: {stderr}; embedder input: {}",
        fs::read_to_string(&embedding_log).unwrap_or_default()
    );

    for mode in ["exact", "--semantic", "--combined"] {
        let mut args = vec!["search", "new changed marker"];
        if mode != "exact" {
            args.push(mode);
        }
        args.push("--json");
        let (success, output, stderr) = invoke(&args);
        assert!(success, "{mode} search failed: {stderr}");
        let changed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert!(
            changed.iter().any(|result| {
                result["path"] == "changed.txt" && result["commit_id"] == current
            }),
            "{mode}: {changed:?}"
        );
    }

    let (success, output, stderr) = invoke(&["search", "newly added marker", "--json"]);
    assert!(success, "search failed: {stderr}");
    let added: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(added.len(), 1, "{added:?}");
    assert_eq!(added[0]["path"], "added.txt");
    assert_eq!(added[0]["commit_id"], current);

    let (success, output, stderr) = invoke(&["search", "rename marker", "--json"]);
    assert!(success, "search failed: {stderr}");
    let renamed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(renamed.len(), 1, "{renamed:?}");
    assert_eq!(renamed[0]["path"], "renamed-new.txt");
    assert_eq!(renamed[0]["commit_id"], current);

    let (success, output, stderr) = invoke(&["search", "unchanged marker", "--json"]);
    assert!(success, "search failed: {stderr}");
    let unchanged: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(unchanged.len(), 1, "{unchanged:?}");
    assert_eq!(unchanged[0]["path"], "unchanged.txt");
    assert_eq!(unchanged[0]["commit_id"], current);
    assert_eq!(unchanged[0]["text"], "unchanged marker");
    assert!(search_json(root, &["unstaged replacement marker"]).is_empty());
    assert!(search_json(root, &["untracked marker"]).is_empty());

    let (success, output, stderr) =
        invoke(&["search", "deleted marker", "--commit", &parent, "--json"]);
    assert!(success, "historical search failed: {stderr}");
    let historical: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(historical.len(), 1, "{historical:?}");
    assert_eq!(historical[0]["path"], "deleted.txt");
    let (success, output, stderr) =
        invoke(&["search", "rename marker", "--commit", &parent, "--json"]);
    assert!(success, "historical search failed: {stderr}");
    let old_rename: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(old_rename.len(), 1, "{old_rename:?}");
    assert_eq!(old_rename[0]["path"], "renamed-old.txt");
    let (success, output, stderr) = invoke(&["search", "deleted marker", "--json"]);
    assert!(success, "current search failed: {stderr}");
    assert!(
        serde_json::from_str::<Vec<serde_json::Value>>(&output)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn failed_changed_refresh_keeps_the_previous_snapshot() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let parent = git_text(root, &["rev-parse", "HEAD"]);
    let embedder = root.join(".git/scoped-failing-embedder");
    fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);

    fs::write(
        &embedder,
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *'abort scoped'*) exit 7 ;; esac\nprintf '[1,0]'\n",
    )
    .unwrap();
    fs::write(root.join("notes.txt"), "abort scoped marker\n").unwrap();
    git(root, &["add", "notes.txt"]);
    git(root, &["commit", "-qm", "changed content"]);
    let (success, output) = run(root, &["refresh", "--changed"]);
    assert!(!success, "{output}");
    assert_eq!(search_json(root, &["TARGET", "--commit", &parent]).len(), 1);
    assert!(search_json(root, &["abort scoped marker"]).is_empty());
    assert!(search_json(root, &["abort scoped marker", "--all-history"]).is_empty());
}

#[test]
fn changed_refresh_does_not_refresh_registered_non_git_imports() {
    let repo = repository();
    let root = repo.path();
    let imported = tempfile::NamedTempFile::new().unwrap();
    fs::write(imported.path(), "retained import marker\n").unwrap();
    assert!(run(root, &["import", imported.path().to_str().unwrap()]).0);
    assert!(run(root, &["init"]).0);
    assert_eq!(search_json(root, &["retained import marker"]).len(), 1);

    fs::write(imported.path(), "unfetched import update\n").unwrap();
    fs::write(root.join("changed.txt"), "new local commit marker\n").unwrap();
    git(root, &["add", "changed.txt"]);
    git(root, &["commit", "-qm", "local change"]);
    assert!(run(root, &["refresh", "--changed"]).0);

    assert_eq!(search_json(root, &["retained import marker"]).len(), 1);
    assert!(search_json(root, &["unfetched import update"]).is_empty());
    assert_eq!(search_json(root, &["new local commit marker"]).len(), 1);
}

#[test]
fn changed_refresh_indexes_the_initial_branch_commit() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    assert!(run(root, &["init"]).0);

    fs::write(root.join("first.txt"), "initial branch marker\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "initial branch commit"]);
    let commit = git_text(root, &["rev-parse", "HEAD"]);
    let (success, output) = run(root, &["refresh", "--changed"]);
    assert!(success, "{output}");

    let results = search_json(root, &["initial branch marker"]);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["path"], "first.txt");
    assert_eq!(results[0]["commit_id"], commit);
}

#[test]
fn changed_refresh_indexes_all_changes_since_the_last_indexed_commit() {
    let repo = repository();
    let root = repo.path();
    let baseline = git_text(root, &["rev-parse", "HEAD"]);
    assert!(run(root, &["init"]).0);

    fs::write(root.join("middle.txt"), "unindexed intermediate marker\n").unwrap();
    git(root, &["add", "middle.txt"]);
    git(root, &["commit", "-qm", "intermediate change"]);
    fs::write(root.join("latest.txt"), "unindexed latest marker\n").unwrap();
    git(root, &["add", "latest.txt"]);
    git(root, &["commit", "-qm", "latest change"]);
    let current = git_text(root, &["rev-parse", "HEAD"]);

    let (success, output) = run(root, &["refresh", "--changed"]);
    assert!(success, "{output}");

    for (query, path) in [
        ("unindexed intermediate marker", "middle.txt"),
        ("unindexed latest marker", "latest.txt"),
    ] {
        let results = search_json(root, &[query]);
        assert_eq!(results.len(), 1, "{query}: {results:?}");
        assert_eq!(results[0]["path"], path);
        assert_eq!(results[0]["commit_id"], current);
    }
    assert_eq!(
        search_json(root, &["TARGET", "--commit", &baseline]).len(),
        1,
        "the previous indexed commit remains searchable"
    );

    let (success, output) = run(root, &["refresh", "--changed"]);
    assert!(success, "refreshing an already indexed HEAD: {output}");
    assert_eq!(
        search_json(root, &["unindexed intermediate marker", "--all-history"]).len(),
        1,
        "an already indexed HEAD is not duplicated"
    );
}

#[test]
fn changed_refresh_uses_the_first_parent_for_merge_commits() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    fs::write(root.join("base.txt"), "base branch marker\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base"]);
    let main_branch = git_text(root, &["branch", "--show-current"]);
    assert!(run(root, &["init"]).0);

    git(root, &["checkout", "-qb", "side"]);
    fs::write(root.join("side.txt"), "second parent marker\n").unwrap();
    git(root, &["add", "side.txt"]);
    git(root, &["commit", "-qm", "side change"]);
    let second_parent = git_text(root, &["rev-parse", "HEAD"]);
    git(root, &["checkout", "-q", &main_branch]);
    git(root, &["merge", "--no-ff", "-m", "merge side", "side"]);
    let merge_commit = git_text(root, &["rev-parse", "HEAD"]);
    assert_ne!(
        git_text(root, &["rev-parse", "HEAD^1"]),
        second_parent,
        "fixture should make the second parent distinct and unindexed"
    );

    let (success, output) = run(root, &["refresh", "--changed"]);
    assert!(success, "{output}");
    let results = search_json(root, &["second parent marker"]);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["path"], "side.txt");
    assert_eq!(results[0]["commit_id"], merge_commit);
}

#[test]
fn status_reports_missing_database_as_distinct_from_empty_coverage() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init"]);
    let (ok, output) = run(repo.path(), &["status", "--json"]);
    assert!(
        ok,
        "status backend operation should report unavailable index as data: {output}"
    );
    let missing: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(missing["database"]["state"], "missing");

    assert!(run(repo.path(), &["init"]).0);
    let (ok, output) = run(repo.path(), &["status", "--json"]);
    assert!(ok, "{output}");
    let empty: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(empty["database"]["state"], "empty");
    assert!(empty["capabilities"]["exact"]["ready"].as_bool().is_some());
}

#[test]
fn init_refresh_search_and_json_are_repository_local_and_exact() {
    let repo = repository();
    let root = repo.path();
    let (ok, _) = run(root, &["init"]);
    assert!(ok);
    assert!(root.join(".git/repodb.sqlite").is_file());

    let (ok, human) = run(root, &["search", "tArGeT", "--type=cmcommits"]);
    assert!(ok);
    assert!(human.contains("notes.txt"), "{human}");
    assert!(human.contains("notes.txt:1-3"), "{human}");
    assert!(!human.contains("ignored.bin"));

    let (ok, json) = run(root, &["search", "--json", "[literal]", "--type=cmcommits"]);
    assert!(ok);
    let results: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(results[0]["path"], "notes.txt");
    assert_eq!(results[0]["line_start"], 1);
    assert_eq!(results[0]["line_end"], 3);
    let (ok, options_first) = run(root, &["search", "--json", "TARGET", "--type=cmcommits"]);
    assert!(ok);
    let (ok, legacy) = run(root, &["search", "TARGET", "--json", "--type=cmcommits"]);
    assert!(ok);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&options_first).unwrap(),
        serde_json::from_str::<serde_json::Value>(&legacy).unwrap()
    );

    let (ok, output) = run(root, &["search", "[literal]", "--type=cmcommits"]);
    assert!(ok && output.contains("notes.txt"));
    let (ok, output) = run(root, &["search", ".*"]);
    assert!(
        ok && output.trim().is_empty(),
        "literal punctuation: {output}"
    );

    fs::write(root.join("notes.txt"), "Updated phrase\n").unwrap();
    git(root, &["add", "notes.txt"]);
    git(root, &["commit", "-qm", "second"]);
    let (ok, _) = run(root, &["refresh"]);
    assert!(ok);
    let (ok, output) = run(root, &["search", "updated phrase", "--type=cmcommits"]);
    assert!(ok && output.contains("notes.txt"), "{output}");
    let (ok, output) = run(root, &["search", "[literal]", "--type=cmcommits"]);
    assert!(ok && output.trim().is_empty(), "{output}");
}

#[test]
fn post_commit_hook_refreshes_and_missing_hook_guidance_is_explicit() {
    let repo = repository();
    let root = repo.path();
    let (ok, _) = run(root, &["init"]);
    assert!(ok);
    let embedder = root.join(".git/repodb-test-embedder");
    fs::write(
        &embedder,
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *'Rust phrase'*) echo 'unchanged source unit was re-embedded' >&2; exit 9 ;; esac\nprintf '[1,0]'\n",
    )
    .unwrap();
    let hook_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks");
    git(
        root,
        &["config", "core.hooksPath", hook_dir.to_str().unwrap()],
    );
    let binary = Path::new(env!("CARGO_BIN_EXE_repodb"));
    let path = format!(
        "{}:{}",
        binary.parent().unwrap().display(),
        std::env::var("PATH").unwrap()
    );
    fs::write(root.join("notes.txt"), "brandnewhookphrase\n").unwrap();
    git(root, &["add", "notes.txt"]);
    let output = Command::new("git")
        .args(["commit", "-qm", "hook update"])
        .current_dir(root)
        .env("PATH", path)
        .env(
            "REPODB_SEMGREP_CORE",
            semgrep_corpus::required_semgrep_core(),
        )
        .env("XDG_CONFIG_HOME", root.join(".test-xdg-config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (ok, output) = run(root, &["search", "brandnewhookphrase"]);
    assert!(ok && output.contains("notes.txt"), "{output}");
    let guidance =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("AGENTS.md")).unwrap();
    assert!(guidance.contains("If the hook or its configuration is missing"));
    assert!(guidance.contains("ask the user what to do"));
}

#[test]
fn semantic_search_uses_configured_fake_embeddings_and_precise_refs() {
    let repo = repository();
    let root = repo.path();
    let fake = root.join("fake-embedder");
    fs::write(&fake, "#!/bin/sh\ncat >/dev/null\nprintf '[1.0, 0.0]'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["init"]).0);

    let (ok, output) = run(root, &["reembed", "--command", fake.to_str().unwrap()]);
    assert!(ok, "{output}");
    let (ok, output) = run(
        root,
        &["search", "unrelated wording", "--semantic", "--json"],
    );
    assert!(ok, "{output}");
    let results: serde_json::Value = serde_json::from_str(&output).unwrap();
    let content = results
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["component"] == "content" && result["path"] == "notes.txt")
        .unwrap();
    assert_eq!(content["line_start"], 1);
    assert!(content["commit_id"].as_str().is_some());
    assert!(content["distance"].as_f64().is_some());
}

#[test]
fn semantic_search_covers_source_and_documentation_with_returned_provenance() {
    let repo = repository();
    let root = repo.path();
    fs::write(
        root.join("README.md"),
        "Project guidance: use bounded retries for network requests.\n",
    )
    .unwrap();
    git(root, &["add", "README.md"]);
    git(root, &["commit", "-qm", "add project guidance"]);
    assert!(run(root, &["init"]).0);
    let (ok, output) = run(root, &["reembed"]);
    assert!(ok, "{output}");

    let results = search_json(
        root,
        &["How should requests handle network failures?", "--semantic"],
    );
    let source = results
        .iter()
        .find(|result| result["path"] == "src/main.rs");
    let documentation = results.iter().find(|result| result["path"] == "README.md");
    for result in [source, documentation].into_iter().flatten() {
        assert!(result["commit_id"].as_str().is_some(), "{result:?}");
        assert!(result["line_start"].as_u64().is_some(), "{result:?}");
    }
    assert!(
        source.is_some(),
        "semantic search omitted indexed source: {results:?}"
    );
    assert!(
        documentation.is_some(),
        "semantic search omitted indexed documentation: {results:?}"
    );

    let empty = search_json(root, &["anything", "--semantic", "--semantic-limit", "0"]);
    assert!(
        empty.is_empty(),
        "expected empty semantic result set: {empty:?}"
    );
}

#[test]
fn semantic_search_reports_unavailable_embedding_capability_without_exact_fallback() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let database = root.join(".git/repodb.sqlite");
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute("DELETE FROM embedding_config WHERE key='command'", [])
        .unwrap();

    let (ok, output) = run(root, &["search", "TARGET", "--semantic"]);
    assert!(
        !ok,
        "semantic search must not return exact matches: {output}"
    );
    assert!(
        output.is_empty(),
        "failed semantic search returned matches: {output}"
    );
}

#[test]
fn embedding_model_selects_vectors_for_indexing_and_querying() {
    let repo = repository();
    let root = repo.path();
    fs::write(
        root.join("models.txt"),
        "indexed alpha marker\n\nindexed beta marker\n",
    )
    .unwrap();
    git(root, &["add", "models.txt"]);
    git(root, &["commit", "-qm", "model fixtures"]);
    let fake = root.join("model-embedder");
    fs::write(
        &fake,
        r#"#!/bin/sh
input=$(cat)
case "$REPODB_EMBEDDING_MODEL:$input" in
  alpha:*'indexed alpha marker'*|alpha:opaque\ query) printf '[1.0, 0.0]' ;;
  beta:*'indexed beta marker'*|beta:opaque\ query) printf '[0.0, 1.0]' ;;
  *) printf '[0.5, 0.5]' ;;
esac
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["init"]).0);
    for (model, expected) in [
        ("alpha", "indexed alpha marker"),
        ("beta", "indexed beta marker"),
    ] {
        let (ok, output) = run(
            root,
            &[
                "reembed",
                "--command",
                fake.to_str().unwrap(),
                "--model",
                model,
            ],
        );
        assert!(ok, "{output}");
        let (ok, output) = run(root, &["search", "--semantic", "--json", "opaque query"]);
        assert!(ok, "{output}");
        let results: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(results[0]["text"], expected, "model {model}");
    }
}

#[test]
fn built_in_embedding_command_rejects_unsupported_model_before_execution() {
    let repo = repository();
    let root = repo.path();
    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let builtin = bin.join("generate_embeddings");
    fs::write(
        &builtin,
        "#!/bin/sh\ncat >/dev/null\nprintf called >> \"$HOME/called\"\nprintf '[1.0]'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&builtin, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["init"]).0);
    let home_string = home.path().to_str().unwrap();
    let vars = [("HOME", home_string), ("XDG_CONFIG_HOME", "")];
    let (ok, output) = run_with_env(root, &["reembed", "--model", "other"], &vars);
    assert!(!ok, "{output}");
    assert!(!home.path().join("called").exists());
    let (ok, output) = run_with_env(root, &["reembed"], &vars);
    assert!(ok, "{output}");
    assert!(home.path().join("called").exists());
}

#[test]
fn combined_search_ranks_exact_hits_by_their_stored_vectors() {
    let repo = repository();
    let root = repo.path();
    let fake = root.join("fake-embedder");
    fs::write(
        &fake,
        r#"#!/bin/sh
input=$(cat)
case "$input" in
  'First line
Exact [literal] TARGET
last line') printf '[0.0, 1.0]' ;;
  'fn main() { println!("Rust phrase"); }') printf '[0.9, 0.1]' ;;
  TARGET) printf '[1.0, 0.0]' ;;
  initial) printf '[0.5, 0.5]' ;;
  '') printf '[0.5, 0.5]' ;;
  *) printf 'unexpected embedding input: <%s>\n' "$input" >&2; exit 1 ;;
esac
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["init"]).0);
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);
    let (ok, output) = run(
        root,
        &[
            "search",
            "TARGET",
            "--combined",
            "--semantic-limit",
            "1",
            "--json",
        ],
    );
    assert!(ok, "{output}");
    let all_results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    let results = all_results
        .into_iter()
        .filter(|result| result["component"] == "content")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0]["text"],
        "fn main() { println!(\"Rust phrase\"); }"
    );
    assert_eq!(
        results[1]["text"],
        "First line\nExact [literal] TARGET\nlast line"
    );
    assert_eq!(results[1]["line_start"], 1);
    assert_eq!(results[1]["line_end"], 3);
    let exact = search_json(root, &["TARGET"]);
    assert_eq!(exact[0]["text"], results[1]["text"]);
    let semantic = search_json(root, &["TARGET", "--semantic", "--semantic-limit", "1"]);
    assert_eq!(semantic[0], results[0]);
    assert!(results[0]["distance"].as_f64().unwrap() < results[1]["distance"].as_f64().unwrap());
}

#[test]
fn failed_reembed_preserves_previous_vectors_and_configuration() {
    let repo = repository();
    let root = repo.path();
    let old = root.join("old-embedder");
    fs::write(&old, "#!/bin/sh\ncat >/dev/null\nprintf '[1.0, 0.0]'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["init"]).0);
    assert!(
        run(
            root,
            &[
                "reembed",
                "--model",
                "old-model",
                "--command",
                old.to_str().unwrap()
            ]
        )
        .0
    );
    let bad = root.join("failing-embedder");
    fs::write(&bad, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        !run(
            root,
            &[
                "reembed",
                "--model",
                "new-model",
                "--command",
                bad.to_str().unwrap()
            ]
        )
        .0
    );
    let (ok, output) = run(root, &["search", "still works", "--semantic", "--json"]);
    assert!(ok, "{output}");
    assert!(output.contains("notes.txt"), "{output}");
    assert!(output.contains("First line"), "{output}");
}

#[test]
fn uncommitted_phrase_is_not_searchable() {
    let repo = repository();
    let root = repo.path();

    let (ok, _) = run(root, &["init"]);
    assert!(ok);
    let (ok, _) = run(root, &["refresh"]);
    assert!(ok);

    // 1. Untracked file
    let phrase = "UNCOMMITTED_PHRASE";
    fs::write(root.join("uncommitted.txt"), phrase).unwrap();

    let (ok, output) = run(root, &["search", phrase]);
    assert!(ok);
    assert!(
        output.trim().is_empty(),
        "Untracked phrase should not be searchable: {output}"
    );

    // 2. Staged file
    git(root, &["add", "uncommitted.txt"]);
    let (ok, output) = run(root, &["search", phrase]);
    assert!(ok);
    assert!(
        output.trim().is_empty(),
        "Staged phrase should not be searchable: {output}"
    );

    // 3. Unstaged change
    fs::write(root.join("notes.txt"), "Unstaged change\n").unwrap();
    let (ok, output) = run(root, &["search", "Unstaged change"]);
    assert!(ok);
    assert!(
        output.trim().is_empty(),
        "Unstaged change should not be searchable: {output}"
    );

    // 4. Committed file
    git(root, &["commit", "-qm", "add uncommitted"]);
    let (ok, _) = run(root, &["refresh"]);
    assert!(ok);
    let (ok, output) = run(root, &["search", phrase]);
    assert!(ok);
    assert!(
        output.contains("uncommitted.txt"),
        "Committed phrase should be searchable: {output}"
    );
}

fn git_text(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn search_json(repo: &Path, args: &[&str]) -> Vec<serde_json::Value> {
    let mut command = vec!["search", "--json"];
    command.extend_from_slice(args);
    let (ok, output) = run(repo, &command);
    assert!(ok, "search {args:?}: {output}");
    let mut results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    // Older tests in this file assert content-only behavior. New type-set tests
    // opt in explicitly and inspect every component, including the default `all`.
    let explicit_types = args
        .iter()
        .any(|arg| *arg == "--type" || arg.starts_with("--type="));
    if !explicit_types && !args.contains(&"--diffs") {
        results.retain(|result| result["component"] == "content");
    }
    results
}

#[test]
fn search_type_sets_label_grouped_evidence_and_exact_fallback() {
    let repo = repository();
    let root = repo.path();
    fs::write(
        root.join("notes.txt"),
        "rank-type-marker appears in this content\n",
    )
    .unwrap();
    git(root, &["add", "notes.txt"]);
    git(
        root,
        &["commit", "-qm", "rank-type-marker is the change rationale"],
    );
    assert!(run(root, &["init"]).0);

    let (ok, default_output) = run(root, &["search", "--json", "rank-type-marker"]);
    assert!(ok, "{default_output}");
    let default_results: Vec<serde_json::Value> = serde_json::from_str(&default_output).unwrap();
    let default_components = default_results
        .iter()
        .map(|result| result["component"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        default_components,
        ["content", "diff", "commit_message"].into_iter().collect()
    );

    let expected = [
        ("all", &["content", "diff", "commit_message"][..]),
        ("content", &["content", "diff"][..]),
        ("cmdiffs", &["diff", "commit_message"][..]),
        ("diffs", &["diff"][..]),
        ("cmcommits", &["content", "commit_message"][..]),
        ("commits", &["commit_message"][..]),
    ];
    for (type_set, wanted) in expected {
        let type_arg = format!("--type={type_set}");
        let results = search_json(root, &["rank-type-marker", &type_arg]);
        let components = results
            .iter()
            .map(|result| result["component"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            components,
            wanted.iter().copied().collect(),
            "type={type_set}"
        );
        for result in &results {
            assert_eq!(result["ranking_method"], "deterministic_fallback");
            assert!(result.get("distance_qualifier").is_none());
        }
        if type_set == "all" {
            let diff = results
                .iter()
                .find(|result| result["component"] == "diff")
                .unwrap();
            assert_eq!(diff["primary_component"], "diff");
            assert_eq!(diff["matched_components"].as_array().unwrap().len(), 3);
            assert_eq!(diff["group_id"], results[0]["group_id"]);
        }
    }
    let (ok, human) = run(root, &["search", "rank-type-marker", "--type=all"]);
    assert!(ok, "{human}");
    assert!(human.contains("primary=diff"), "{human}");
    assert!(human.contains("matching=commit_message"), "{human}");
    assert!(human.contains("deterministic_fallback"), "{human}");
}

#[test]
fn combined_cli_reserves_exact_and_semantic_only_commit_slots_with_qualified_distance() {
    let repo = repository();
    let root = repo.path();
    git(
        root,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "quota-marker change rationale",
        ],
    );
    assert!(run(root, &["init"]).0);

    let results = search_json(
        root,
        &[
            "quota-marker",
            "--combined",
            "--type=commits",
            "--history=all",
            "--semantic-limit",
            "2",
        ],
    );
    assert_eq!(
        results.len(),
        2,
        "exact and semantic-only slots: {results:?}"
    );
    assert!(results.iter().any(|result| result["exact_match"] == true));
    assert!(results.iter().any(|result| result["exact_match"] == false));
    for result in results {
        assert_eq!(result["ranking_method"], "semantic_distance");
        assert_eq!(result["distance_qualifier"], "cosine_embedding_distance");
        assert!(result["distance"].as_f64().is_some());
    }
    let (ok, human) = run(
        root,
        &[
            "search",
            "quota-marker",
            "--combined",
            "--type=commits",
            "--history=all",
            "--semantic-limit",
            "2",
        ],
    );
    assert!(ok, "{human}");
    assert!(human.contains("matching=commit_message"), "{human}");
    assert!(human.contains("cosine embedding distance"), "{human}");
}

#[test]
fn commit_message_search_is_exposed_with_message_provenance_and_unscored_exact_hits() {
    let repo = repository();
    let root = repo.path();
    git(
        root,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "Transaction ledger design rationale",
        ],
    );
    let commit = git_text(root, &["rev-parse", "HEAD"]);
    assert!(run(root, &["init"]).0);

    let exact = search_json(
        root,
        &["transaction ledger", "--all-history", "--type=commits"],
    );
    let message = exact
        .iter()
        .find(|result| result["kind"] == "commit_message" && result["commit_id"] == commit)
        .unwrap_or_else(|| panic!("commit-message result missing: {exact:?}"));
    assert!(message["line_start"].as_u64().unwrap() > 0);
    assert!(message["line_end"].as_u64().unwrap() >= message["line_start"].as_u64().unwrap());
    assert!(
        message["source_reference"]
            .as_str()
            .unwrap()
            .contains(&commit)
    );
    assert!(message.get("distance").is_none());
    assert!(message.get("confidence").is_none());
    let (ok, human) = run(
        root,
        &[
            "search",
            "transaction ledger",
            "--all-history",
            "--type=commits",
        ],
    );
    assert!(ok, "{human}");
    assert!(human.contains("[commit message]"), "{human}");
    assert!(human.contains(&commit), "{human}");
}

#[test]
fn commit_message_search_reports_missing_git_objects_instead_of_partial_results() {
    let repo = repository();
    let root = repo.path();
    git(
        root,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "Missing object rationale marker",
        ],
    );
    assert!(run(root, &["init"]).0);
    git(root, &["update-ref", "-d", "HEAD"]);
    git(root, &["reflog", "expire", "--expire=now", "--all"]);
    git(root, &["gc", "--prune=now"]);

    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .args(["search", "Missing object rationale", "--all-history"])
        .current_dir(root)
        .env(
            "REPODB_SEMGREP_CORE",
            semgrep_corpus::required_semgrep_core(),
        )
        .env("XDG_CONFIG_HOME", test_config_home(root))
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "missing commit object must fail rather than return partial hits"
    );
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("could not get object info"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn semantic_commit_message_search_returns_distance_without_confidence_claim() {
    let repo = repository();
    let root = repo.path();
    git(
        root,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "Retry network failures safely",
        ],
    );
    let commit = git_text(root, &["rev-parse", "HEAD"]);
    assert!(run(root, &["init"]).0);
    let fake = fake_embedder(root, "cat >/dev/null; printf '[1,0]'");
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);

    let results = search_json(
        root,
        &[
            "handle request outages",
            "--semantic",
            "--all-history",
            "--type=commits",
        ],
    );
    let message = results
        .iter()
        .find(|result| result["kind"] == "commit_message" && result["commit_id"] == commit)
        .unwrap_or_else(|| panic!("semantic commit-message result missing: {results:?}"));
    assert_eq!(message["commit_id"], commit);
    assert!(message["distance"].as_f64().is_some());
    assert!(message.get("confidence").is_none());
}

#[test]
fn history_search_preserves_blob_and_line_references_without_broadening_default_scope() {
    let repo = repository();
    let root = repo.path();
    let old_commit = git_text(root, &["rev-parse", "HEAD"]);
    let old_blob = git_text(root, &["rev-parse", "HEAD:notes.txt"]);
    fs::write(root.join("notes.txt"), "New historical TARGET\n").unwrap();
    git(root, &["add", "notes.txt"]);
    git(root, &["commit", "-qm", "replace"]);
    let new_commit = git_text(root, &["rev-parse", "HEAD"]);
    assert!(run(root, &["init"]).0);
    let current = search_json(root, &["TARGET"]);
    assert_eq!(current.len(), 1);
    assert_eq!(current[0]["commit_id"], new_commit);
    let history = search_json(root, &["TARGET", "--all-history"]);
    assert_eq!(history.len(), 2);
    let old = history
        .iter()
        .find(|r| r["commit_id"] == old_commit)
        .unwrap();
    assert_eq!(old["blob_id"], old_blob);
    assert_eq!(old["path"], "notes.txt");
    assert_eq!(old["line_start"], 1);
    assert_eq!(old["line_end"], 3);
    git(root, &["checkout", "-q", &old_commit]);
    let current = search_json(root, &["TARGET"]);
    assert_eq!(current.len(), 1);
    assert_eq!(current[0]["commit_id"], old_commit);
}

#[test]
fn refresh_update_indexes_an_unreachable_commit_additively() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let baseline = git_text(root, &["rev-parse", "HEAD"]);
    let original_branch = git_text(root, &["branch", "--show-current"]);

    let imported = root.join("imported.txt");
    fs::write(&imported, "PRESERVED_IMPORT_TOKEN\n").unwrap();
    assert!(run(root, &["import", imported.to_str().unwrap()]).0);
    assert!(run(root, &["refresh"]).0);
    fs::write(root.join("notes.txt"), "HISTORICAL_UPDATE_TOKEN\n").unwrap();
    git(root, &["add", "notes.txt"]);
    git(root, &["commit", "-qm", "historical target"]);
    let target = git_text(root, &["rev-parse", "HEAD"]);
    git(root, &["checkout", "-q", &baseline]);
    git(root, &["branch", "-f", &original_branch, &baseline]);
    let reachable_commits = git_text(root, &["rev-list", "--all"]);
    assert!(
        !reachable_commits.lines().any(|commit| commit == target),
        "target commit must be unreachable from all refs before --update"
    );

    let (ok, output) = run(root, &["refresh", "--update", &target[..12]]);
    assert!(ok, "refresh --update: {output}");
    let target_results = search_json(root, &["HISTORICAL_UPDATE_TOKEN", "--commit", &target]);
    assert_eq!(target_results.len(), 1);
    assert_eq!(target_results[0]["commit_id"], target);
    assert!(!search_json(root, &["TARGET", "--commit", &baseline]).is_empty());
    assert_eq!(search_json(root, &["PRESERVED_IMPORT_TOKEN"]).len(), 1);

    let all = search_json(root, &["HISTORICAL_UPDATE_TOKEN", "--all-history"]);
    assert_eq!(all.len(), 1, "target commit alone is indexed");
    let (ok, output) = run(root, &["refresh", "--update", &target]);
    assert!(ok, "repeat update must be a no-op: {output}");
}

#[test]
fn refresh_update_rejects_non_commit_ids_without_changing_searchable_content() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let blob = git_text(root, &["rev-parse", "HEAD:notes.txt"]);

    for invalid in [
        "HEAD",
        "not-a-commit",
        "0000000000000000000000000000000000000000",
        blob.as_str(),
    ] {
        let (ok, output) = run(root, &["refresh", "--update", invalid]);
        assert!(!ok, "accepted {invalid}: {output}");
        assert!(output.is_empty());
        assert_eq!(search_json(root, &["TARGET"]).len(), 1);
    }
}

#[test]
fn refresh_update_embedding_failure_preserves_the_published_snapshot() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let baseline = git_text(root, &["rev-parse", "HEAD"]);
    let config_home = test_config_home(root);
    let embedder = root.join(".git/repodb-test-embedder");
    fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nexit 1\n").unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(root.join("new.txt"), "ATOMIC_UPDATE_TOKEN\n").unwrap();
    git(root, &["add", "new.txt"]);
    git(root, &["commit", "-qm", "new target"]);
    let target = git_text(root, &["rev-parse", "HEAD"]);
    git(root, &["checkout", "-q", &baseline]);

    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .args(["refresh", "--update", &target])
        .current_dir(root)
        .env("XDG_CONFIG_HOME", &config_home)
        .env(
            "REPODB_SEMGREP_CORE",
            semgrep_corpus::required_semgrep_core(),
        )
        .output()
        .unwrap();
    assert!(!output.status.success(), "failed embedder must fail update");
    assert_eq!(
        search_json(root, &["TARGET", "--commit", &baseline]).len(),
        1
    );
    assert!(search_json(root, &["ATOMIC_UPDATE_TOKEN", "--all-history"]).is_empty());
}

#[test]
fn refresh_update_indexes_the_full_tree_of_a_merge_commit() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let main_branch = git_text(root, &["branch", "--show-current"]);
    git(root, &["checkout", "-qb", "side"]);
    fs::write(root.join("side.txt"), "MERGE_SIDE_TOKEN\n").unwrap();
    git(root, &["add", "side.txt"]);
    git(root, &["commit", "-qm", "side"]);
    git(root, &["checkout", "-q", &main_branch]);
    fs::write(root.join("main.txt"), "MERGE_MAIN_TOKEN\n").unwrap();
    git(root, &["add", "main.txt"]);
    git(root, &["commit", "-qm", "main"]);
    git(root, &["merge", "-q", "--no-ff", "side", "-m", "merge"]);
    let merge = git_text(root, &["rev-parse", "HEAD"]);

    let (ok, output) = run(root, &["refresh", "--update", &merge]);
    assert!(ok, "refresh --update merge: {output}");
    for token in ["MERGE_SIDE_TOKEN", "MERGE_MAIN_TOKEN"] {
        let results = search_json(root, &[token, "--commit", &merge]);
        assert_eq!(results.len(), 1, "{token}");
    }
    assert!(search_json(root, &["MERGE_SIDE_TOKEN", "--all-history"]).len() == 1);
}

fn fake_embedder(root: &Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("fake-history-embedder");
    fs::write(&path, format!("#!/bin/sh\ninput=$(cat)\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn exact_commit_inclusion_and_exclusion_select_before_all_search_modes() {
    let repo = repository();
    let root = repo.path();
    let old = git_text(root, &["rev-parse", "HEAD"]);
    fs::write(root.join("notes.txt"), "new first line\ndifferent TARGET\n").unwrap();
    git(root, &["add", "notes.txt"]);
    git(root, &["commit", "-qm", "new target"]);
    let new = git_text(root, &["rev-parse", "HEAD"]);
    assert!(run(root, &["init"]).0);
    let fake = fake_embedder(
        root,
        "case \"$input\" in *'TARGET'*) printf '[1,0]' ;; *) printf '[0,1]' ;; esac",
    );
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);
    for mode in ["--exact", "--semantic", "--combined"] {
        let mut args = vec!["TARGET", "--semantic-limit", "1", "--type=cmcommits"];
        if mode != "--exact" {
            args.push(mode);
        }
        let mut all = args.clone();
        all.push("--all-history");
        let history = search_json(root, &all);
        if mode == "--combined" {
            assert!(history.len() >= 2, "{mode}: {history:?}");
        } else {
            assert_eq!(
                history.len(),
                if mode == "--semantic" {
                    1
                } else if mode == "--exact" {
                    3
                } else {
                    2
                },
                "{mode}"
            );
        }
        if mode == "--combined" {
            assert!(
                history
                    .iter()
                    .filter(|result| result["path"] == "notes.txt")
                    .all(|r| r["line_start"] == 1)
            );
        }
        let mut selected = args.clone();
        selected.extend(["--commit", &old]);
        let result = search_json(root, &selected);
        assert_eq!(result.len(), 1, "{mode}");
        assert_eq!(result[0]["commit_id"], old);
        assert_eq!(
            result[0]["text"],
            "First line\nExact [literal] TARGET\nlast line"
        );
        let mut included = args.clone();
        included.extend([
            "--include-commit",
            &old[..12],
            "--include-commit",
            &new,
            "--exclude-commit",
            &new,
        ]);
        assert_eq!(search_json(root, &included), result, "{mode}");
        let mut excluded = args.clone();
        excluded.extend(["--all-history", "--exclude-commit", &new]);
        assert_eq!(search_json(root, &excluded), result, "{mode}");
        excluded.extend(["--exclude-commit", &old]);
        assert!(search_json(root, &excluded).is_empty(), "{mode}");
    }
    for selector in [
        "HEAD",
        "main",
        "HEAD~1",
        "abcd..efab",
        "zzz",
        "abc",
        "0000000000000000000000000000000000000000",
    ] {
        assert!(
            !run(root, &["search", "TARGET", "--commit", selector]).0,
            "{selector}"
        );
    }
    for flags in [
        vec!["--commit", &old, "--include-commit", &new],
        vec!["--commit", &old, "--commit", &new],
        vec!["--all-history", "--commit", &old],
        vec!["--all-history", "--include-commit", &old],
        vec!["--current-worktree", "--include-commit", &old],
        vec!["--current-worktree", "--all-history"],
    ] {
        let mut args = vec!["search", "TARGET"];
        args.extend(flags);
        assert!(!run(root, &args).0, "{args:?}");
    }
}

#[test]
fn coarse_history_selectors_distinguish_refs_reachable_paths_and_imports() {
    let repo = repository();
    let root = repo.path();
    let main_branch = git_text(root, &["branch", "--show-current"]);
    let base = git_text(root, &["rev-parse", "HEAD"]);
    git(root, &["branch", "side"]);
    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"MAIN_PATH_TOKEN\"); }\n",
    )
    .unwrap();
    fs::write(root.join("notes.txt"), "UNRELATED_TO_SRC_TOKEN\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "main path change"]);
    let main_commit = git_text(root, &["rev-parse", "HEAD"]);

    git(root, &["checkout", "-q", "side"]);
    fs::write(root.join("src/side.rs"), "SIDE_REF_ONLY_TOKEN\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "side-only path"]);
    let side_commit = git_text(root, &["rev-parse", "HEAD"]);
    git(root, &["checkout", "-q", &main_branch]);
    assert!(run(root, &["init"]).0);

    let all = search_json(root, &["SIDE_REF_ONLY_TOKEN", "--history=all"]);
    assert!(all.iter().any(|result| result["commit_id"] == side_commit));
    let bare_all = search_json(root, &["SIDE_REF_ONLY_TOKEN", "--history"]);
    assert_eq!(bare_all, all);
    assert!(search_json(root, &["SIDE_REF_ONLY_TOKEN", "--history=20"]).is_empty());
    assert!(search_json(root, &["SIDE_REF_ONLY_TOKEN", "--history=src/"]).is_empty());
    let additive = search_json(
        root,
        &[
            "SIDE_REF_ONLY_TOKEN",
            "--history=off",
            "--include-commit",
            &side_commit,
        ],
    );
    assert_eq!(additive.len(), 1, "{additive:?}");
    assert!(
        search_json(
            root,
            &[
                "SIDE_REF_ONLY_TOKEN",
                "--history=off",
                "--include-commit",
                &side_commit,
                "--exclude-commit",
                &side_commit,
            ]
        )
        .is_empty()
    );

    let path_results = search_json(root, &["MAIN_PATH_TOKEN", "--history=src/"]);
    assert_eq!(path_results.len(), 1, "{path_results:?}");
    assert_eq!(path_results[0]["path"], "src/main.rs");
    assert_eq!(path_results[0]["commit_id"], main_commit);
    assert_eq!(
        search_json(root, &["MAIN_PATH_TOKEN", "--history=./src/"]),
        path_results
    );
    assert!(search_json(root, &["UNRELATED_TO_SRC_TOKEN", "--history=src/"]).is_empty());
    let path_diffs = search_json(root, &["MAIN_PATH_TOKEN", "--history=src/", "--diffs"]);
    assert!(
        path_diffs
            .iter()
            .any(|result| { result["kind"] == "diff" && result["new_path"] == "src/main.rs" }),
        "{path_diffs:?}"
    );
    assert!(
        search_json(
            root,
            &["UNRELATED_TO_SRC_TOKEN", "--history=src/", "--diffs"]
        )
        .is_empty()
    );
    assert!(
        search_json(
            root,
            &["MAIN_PATH_TOKEN", "--history=src/", "--commit", &base]
        )
        .is_empty()
    );
    let alternate_start = search_json(root, &["Rust phrase", "--history=src/", "--commit", &base]);
    assert!(
        alternate_start
            .iter()
            .any(|result| result["commit_id"] == base)
    );
    let commit_equals = format!("--commit={base}");
    let equals_start = search_json(root, &["Rust phrase", "--history=src/", &commit_equals]);
    assert!(
        equals_start
            .iter()
            .any(|result| result["commit_id"] == base)
    );
    let off_at_start = search_json(root, &["Rust phrase", "--history=off", "--commit", &base]);
    assert!(
        off_at_start
            .iter()
            .any(|result| result["commit_id"] == base)
    );

    let imported = root.join("history-independent.txt");
    fs::write(&imported, "IMPORTED_HISTORY_INDEPENDENT_TOKEN\n").unwrap();
    let imported_path = imported.to_string_lossy().into_owned();
    assert!(run(root, &["import", &imported_path]).0);
    assert!(run(root, &["refresh"]).0);
    let imported_results = search_json(
        root,
        &[
            "IMPORTED_HISTORY_INDEPENDENT_TOKEN",
            "--history=src/main.rs",
        ],
    );
    assert!(
        imported_results
            .iter()
            .any(|result| result["path"] == imported_path),
        "{imported_results:?}"
    );

    for flags in [
        vec!["--history=all", "--commit", &main_commit],
        vec!["--history=all", "--all-history"],
        vec!["--history=0"],
        vec!["--history=../src"],
        vec!["--history=/src"],
    ] {
        let mut args = vec!["search", "MAIN_PATH_TOKEN"];
        args.extend(flags);
        assert!(!run(root, &args).0, "{args:?}");
    }
}

#[test]
fn path_history_labels_detected_rename_links_in_json_and_human_output() {
    let repo = repository();
    let root = repo.path();
    let rename_context = "stable rename-specific context\n".repeat(40);
    fs::write(
        root.join("src/main.rs"),
        format!("OLD_RENAME_TOKEN\n{rename_context}"),
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "old path content"]);
    let old_commit = git_text(root, &["rev-parse", "HEAD"]);
    fs::rename(root.join("src/main.rs"), root.join("src/new.rs")).unwrap();
    fs::write(
        root.join("src/new.rs"),
        format!("NEW_RENAME_TOKEN\n{rename_context}"),
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "rename path"]);
    assert!(run(root, &["init"]).0);

    let results = search_json(root, &["OLD_RENAME_TOKEN", "--history=src/new.rs"]);
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0]["path"], "src/main.rs");
    let links = results[0]["history_path_links"].as_array().unwrap();
    assert!(links.iter().any(|link| {
        link["heuristic"] == true
            && link["parent_commit_id"] == old_commit
            && link["old_path"] == "src/main.rs"
            && link["new_path"] == "src/new.rs"
            && link["similarity_percent"].as_u64().is_some()
    }));
    let (ok, output) = run(
        root,
        &["search", "OLD_RENAME_TOKEN", "--history=src/new.rs"],
    );
    assert!(ok);
    assert!(output.contains("heuristic rename link"), "{output}");
    assert!(output.contains("src/main.rs ->"), "{output}");
}

fn commit_at(root: &Path, message: &str, timestamp: &str) -> String {
    let output = Command::new("git")
        .args(["commit", "-qm", message])
        .env("GIT_AUTHOR_DATE", timestamp)
        .env("GIT_COMMITTER_DATE", timestamp)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    git_text(root, &["rev-parse", "HEAD"])
}

#[test]
fn identical_blob_units_collapse_after_filtering_before_semantic_limit_and_combined_union() {
    let repo = repository();
    let root = repo.path();
    fs::write(root.join("a.txt"), "marker shared\n").unwrap();
    fs::write(root.join("other.txt"), "other semantic\n").unwrap();
    fs::write(root.join("far.txt"), "marker distant\n").unwrap();
    git(root, &["add", "."]);
    let old = commit_at(root, "old occurrence", "2001-01-01T00:00:00Z");
    fs::copy(root.join("a.txt"), root.join("b.txt")).unwrap();
    git(root, &["add", "b.txt"]);
    let new = commit_at(root, "copy unchanged blob", "2002-01-01T00:00:00Z");
    let blob = git_text(root, &["rev-parse", "HEAD:a.txt"]);
    assert!(run(root, &["init"]).0);
    let fake = fake_embedder(
        root,
        "case \"$input\" in marker|*shared*) printf '[1,0]' ;; *other*) printf '[0.8,0.2]' ;; *distant*) printf '[-1,0]' ;; *) printf '[0,1]' ;; esac",
    );
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);
    for mode in ["--exact", "--semantic", "--combined"] {
        let mut args = vec!["marker", "--all-history", "--semantic-limit", "2"];
        if mode != "--exact" {
            args.push(mode);
        }
        let results = search_json(root, &args);
        let shared: Vec<_> = results.iter().filter(|r| r["blob_id"] == blob).collect();
        assert_eq!(shared.len(), 1, "{mode}: {results:?}");
        assert_eq!(shared[0]["commit_id"], new);
        assert_eq!(shared[0]["path"], "a.txt");
        if mode == "--semantic" {
            assert_eq!(results.len(), 2);
            assert_eq!(results[1]["text"], "other semantic");
        }
        if mode == "--combined" {
            assert_eq!(results.len(), 3);
            assert_eq!(results[2]["text"], "marker distant");
            assert!((results[2]["distance"].as_f64().unwrap() - 2.0).abs() < 0.0001);
        }
        args.extend(["--exclude-commit", &new]);
        let older = search_json(root, &args);
        let shared: Vec<_> = older.iter().filter(|r| r["blob_id"] == blob).collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0]["commit_id"], old);
    }
}

#[test]
fn deleted_refs_are_pruned_only_on_refresh_while_shared_reachable_blobs_survive() {
    let repo = repository();
    let root = repo.path();
    let main = git_text(root, &["rev-parse", "HEAD"]);
    let shared_blob = git_text(root, &["rev-parse", "HEAD:notes.txt"]);
    git(root, &["checkout", "-qb", "side"]);
    fs::write(root.join("side.txt"), "side-only content\n").unwrap();
    git(root, &["add", "side.txt"]);
    let side = commit_at(root, "side-only", "2030-01-01T00:00:00Z");
    git(root, &["checkout", "-q", &main]);
    assert!(run(root, &["init"]).0);
    let before = fs::read(root.join(".git/repodb.sqlite")).unwrap();
    git(root, &["branch", "-D", "side"]);
    let old = search_json(root, &["side-only", "--all-history", "--type=all"]);
    assert!(
        old.iter()
            .any(|result| result["path"] == "side.txt" && result["commit_id"] == side)
    );
    assert!(
        old.iter()
            .any(|result| result["kind"] == "commit_message" && result["commit_id"] == side)
    );
    let shared = search_json(root, &["TARGET", "--all-history"]);
    assert_eq!(shared[0]["commit_id"], side);
    assert!(
        fs::read(root.join(".git/repodb.sqlite")).unwrap() == before,
        "search must not modify the snapshot"
    );
    let fake = fake_embedder(root, "printf '[1,0]'");
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);
    assert!(run(root, &["refresh"]).0);
    assert!(search_json(root, &["side-only", "--all-history"]).is_empty());
    let shared = search_json(root, &["TARGET", "--all-history"]);
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0]["commit_id"], main);
    assert_eq!(shared[0]["blob_id"], shared_blob);
    let semantic = search_json(root, &["anything", "--semantic", "--all-history"]);
    assert!(semantic.iter().all(|r| r["commit_id"] == main));
    assert!(semantic.iter().any(|r| r["blob_id"] == shared_blob));
}

#[test]
fn noisy_history_git_stderr_is_drained_without_rejecting_output() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    fs::write(root.join("source.txt"), "previous history marker\n").unwrap();
    git(root, &["add", "source.txt"]);
    git(root, &["commit", "-qm", "initial"]);
    assert!(run(root, &["refresh"]).0);

    fs::write(root.join("source.txt"), "replacement history marker\n").unwrap();
    git(root, &["commit", "-qam", "replacement"]);
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        "#!/bin/sh\nif [ \"$1\" = ls-tree ]; then head -c 1048576 /dev/zero >&2; fi\nexec /usr/bin/git \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let (success, _) = run_with_env(root, &["refresh"], &[("PATH", path.as_str())]);
    assert!(success, "stderr volume should not reject valid Git output");
    assert_eq!(
        search_json(root, &["previous history marker", "--all-history"]).len(),
        1
    );
    assert_eq!(
        search_json(root, &["replacement history marker", "--all-history"]).len(),
        1
    );
}

fn assert_refresh_scans_while_embedding_is_blocked(refresh_args: &[&str]) {
    use std::{
        os::unix::fs::PermissionsExt,
        thread,
        time::{Duration, Instant},
    };

    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let entries = root.join("pipeline");
    fs::create_dir(&entries).unwrap();
    for index in 0..1200 {
        fs::write(
            entries.join(format!("entry-{index:04}.md")),
            format!("pipeline item {index}\n"),
        )
        .unwrap();
    }
    git(root, &["add", "pipeline"]);
    git(root, &["commit", "-qm", "pipeline source"]);

    let scan_log = root.join("scan-log");
    let started = root.join("embedding-started");
    let scan_at_start = root.join("scan-count-at-embedding-start");
    let release = root.join("release-embedding");
    let config_home = test_config_home(root);
    let embedder = root.join(".git/repodb-test-embedder");
    fs::write(
        &embedder,
        "#!/bin/sh\nif [ ! -e \"$REPODB_EMBED_STARTED\" ]; then\n  wc -l < \"$REPODB_SCAN_LOG\" > \"$REPODB_EMBED_SCAN_COUNT\"\n  : > \"$REPODB_EMBED_STARTED\"\n  while [ ! -e \"$REPODB_EMBED_RELEASE\" ]; do sleep 0.01; done\nfi\ncat >/dev/null\nprintf '[1,0]'\n",
    )
    .unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();

    let bin = root.join("intercept-bin");
    fs::create_dir(&bin).unwrap();
    let path = std::env::var_os("PATH").unwrap();
    let real_git = std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .unwrap();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        "#!/bin/sh\nif [ \"$1\" = cat-file ] && [ \"$2\" = blob ]; then printf '.\\n' >> \"$REPODB_SCAN_LOG\"; fi\nexec \"$REPODB_REAL_GIT\" \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let search_path =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .args(refresh_args)
        .current_dir(root)
        .env(
            "REPODB_SEMGREP_CORE",
            semgrep_corpus::required_semgrep_core(),
        )
        .env("XDG_CONFIG_HOME", config_home)
        .env("PATH", search_path)
        .env("REPODB_SCAN_LOG", &scan_log)
        .env("REPODB_EMBED_STARTED", &started)
        .env("REPODB_EMBED_SCAN_COUNT", &scan_at_start)
        .env("REPODB_EMBED_RELEASE", &release)
        .env("REPODB_REAL_GIT", real_git)
        .spawn()
        .unwrap();

    let start_deadline = Instant::now() + Duration::from_secs(20);
    while !started.exists() && Instant::now() < start_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let embedding_started = started.exists();
    let started_after = if embedding_started {
        fs::read_to_string(&scan_at_start)
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap()
    } else {
        0
    };
    let settle_deadline = Instant::now() + Duration::from_secs(10);
    let mut previous_count = started_after;
    let mut stable_since = Instant::now();
    while embedding_started && Instant::now() < settle_deadline {
        thread::sleep(Duration::from_millis(10));
        let scanned = fs::read_to_string(&scan_log)
            .map(|contents| contents.lines().count())
            .unwrap_or(0);
        if scanned != previous_count {
            previous_count = scanned;
            stable_since = Instant::now();
        } else if stable_since.elapsed() >= Duration::from_millis(250) {
            break;
        }
    }
    let scanned_while_blocked = previous_count;
    // Always release the gate before making assertions so a failed test cannot
    // leave the CLI process or its fake embedder stranded.
    fs::write(&release, "continue").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "refresh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(embedding_started, "the fake embedder was never invoked");
    assert!(
        scanned_while_blocked > started_after,
        "Git did not scan ahead while the first embedding was blocked at {started_after} blobs"
    );
    let expected_scan_count = if refresh_args.len() == 1 { 1203 } else { 1200 };
    assert!(
        scanned_while_blocked < expected_scan_count,
        "Git scanned the entire history while embedding was blocked ({scanned_while_blocked} blobs)"
    );
    assert!(
        scanned_while_blocked <= started_after + 2 * 256 + 3,
        "more than two batches were queued/in flight while embedding was blocked: start={started_after}, scanned={scanned_while_blocked}"
    );
    assert_eq!(search_json(root, &["pipeline item 1199"]).len(), 1);
}

#[test]
fn full_refresh_scans_the_next_batch_while_embedding_is_in_flight() {
    assert_refresh_scans_while_embedding_is_blocked(&["refresh"]);
}

#[test]
fn changed_refresh_scans_the_next_batch_while_embedding_is_in_flight() {
    assert_refresh_scans_while_embedding_is_blocked(&["refresh", "--changed"]);
}

fn assert_refresh_batches_are_bounded_and_keep_vector_associations(refresh_args: &[&str]) {
    use std::os::unix::fs::PermissionsExt;

    let repo = repository();
    let root = repo.path();
    let home = root.join("fake-home");
    let builtin = home.join("bin/generate_embeddings");
    fs::create_dir_all(builtin.parent().unwrap()).unwrap();
    fs::write(
        &builtin,
        "#!/usr/bin/env python3\nimport json, re, sys, os\ndata = sys.stdin.buffer.read()\ntexts = data.split(b'\\0') if '-0' in sys.argv else [data]\nwith open(os.environ['REPODB_BATCH_LOG'], 'a') as log: log.write(str(len(texts)) + '\\n')\nfor text in texts:\n    match = re.search(rb'item-(\\d+)', text)\n    index = int(match.group(1)) if match else 600\n    vector = [0.0] * 601\n    vector[index] = 1.0\n    print(json.dumps(vector, separators=(',', ':')))\n",
    )
    .unwrap();
    fs::set_permissions(&builtin, fs::Permissions::from_mode(0o755)).unwrap();
    let config_home = test_config_home(root);
    fs::write(
        config_home.join("repodb/config.toml"),
        format!(
            "[tools]\nembedding_command = {:?}\n",
            builtin.to_string_lossy()
        ),
    )
    .unwrap();
    let batch_log = root.join("embedding-batches");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_repodb"))
            .args(args)
            .current_dir(root)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("REPODB_BATCH_LOG", &batch_log)
            .env(
                "REPODB_SEMGREP_CORE",
                semgrep_corpus::required_semgrep_core(),
            )
            .output()
            .unwrap()
    };
    let init = run(&["init"]);
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let prior_batches = fs::read_to_string(&batch_log).unwrap();
    let prior_unit_count: usize = prior_batches
        .lines()
        .map(|count| count.parse::<usize>().unwrap())
        .sum();
    fs::write(&batch_log, "").unwrap();

    let entries = root.join("pipeline");
    fs::create_dir(&entries).unwrap();
    for index in 0..600 {
        fs::write(
            entries.join(format!("entry-{index:04}.md")),
            format!("item-{index} distinct vector marker\n"),
        )
        .unwrap();
    }
    git(root, &["add", "pipeline"]);
    git(root, &["commit", "-qm", "batched source"]);
    let output = run(refresh_args);
    assert!(
        output.status.success(),
        "refresh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let batch_sizes: Vec<usize> = fs::read_to_string(&batch_log)
        .unwrap()
        .lines()
        .map(|count| count.parse().unwrap())
        .collect();
    assert!(!batch_sizes.is_empty());
    assert!(
        batch_sizes.iter().all(|size| *size <= 256),
        "oversized embedding batches: {batch_sizes:?}"
    );
    let expected_units = if refresh_args.len() == 1 {
        prior_unit_count + 601 + 600 // new content units plus their added-side diff units
    } else {
        600
    };
    assert_eq!(batch_sizes.iter().sum::<usize>(), expected_units);
    for index in [0, 255, 256, 511, 512, 599] {
        let query = format!("item-{index}");
        let search = run(&[
            "search",
            &query,
            "--semantic",
            "--semantic-limit",
            "1",
            "--json",
        ]);
        assert!(
            search.status.success(),
            "semantic search failed: {}",
            String::from_utf8_lossy(&search.stderr)
        );
        let results: Vec<serde_json::Value> = serde_json::from_slice(&search.stdout).unwrap();
        assert_eq!(results[0]["path"], format!("pipeline/entry-{index:04}.md"));
    }
}

#[test]
fn full_refresh_uses_bounded_batches_and_keeps_vectors_with_their_units() {
    assert_refresh_batches_are_bounded_and_keep_vector_associations(&["refresh"]);
}

#[test]
fn changed_refresh_uses_bounded_batches_and_keeps_vectors_with_their_units() {
    assert_refresh_batches_are_bounded_and_keep_vector_associations(&["refresh", "--changed"]);
}

#[test]
fn large_history_tree_refreshes_incrementally_and_keeps_the_snapshot_atomic() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let before = search_json(root, &["TARGET", "--all-history"]);

    let dir = root.join("tree");
    fs::create_dir(&dir).unwrap();
    for index in 0..2_000 {
        fs::write(
            dir.join(format!("file-{index:04}.txt")),
            format!("many tree entries {index}\\n"),
        )
        .unwrap();
    }
    git(root, &["add", "tree"]);
    git(root, &["commit", "-qm", "large tree"]);
    assert!(run(root, &["refresh"]).0);
    let after = search_json(root, &["TARGET", "--all-history"]);
    assert_eq!(after.len(), before.len());
    assert_eq!(after[0]["path"], before[0]["path"]);
    assert_eq!(after[0]["text"], before[0]["text"]);
    assert_eq!(
        search_json(root, &["many tree entries", "--all-history"]).len(),
        2_000
    );
}

#[test]
fn history_refresh_streams_large_commit_history_with_fake_embeddings() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let embedder = root.join("history-embedder");
    fs::write(
        &embedder,
        "#!/bin/sh\ncat >/dev/null\nprintf '[1.0, 0.0]'\n",
    )
    .unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);

    // More commits than the former in-memory history ceiling, all sharing the
    // same tree/blob so the fixture specifically exercises reachability staging
    // and deduplication rather than simply generating many independent files.
    for _ in 0..1_100 {
        git(root, &["commit", "--allow-empty", "-qm", "history"]);
    }
    assert!(run(root, &["refresh"]).0);
    let results = search_json(root, &["TARGET", "--all-history"]);
    assert_eq!(results.len(), 1, "identical blobs must remain deduplicated");
    let db = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let commits: i64 = db
        .query_row("SELECT count(*) FROM git_commits", [], |row| row.get(0))
        .unwrap();
    assert_eq!(commits, 1_101);
}

#[test]
fn history_blob_larger_than_previous_limit_refreshes() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let before = search_json(root, &["TARGET", "--all-history"]);

    let mut large_blob = b"oversized searchable marker\n".to_vec();
    // Keep the Git object above the uncached-blob threshold without generating
    // thousands of indexed units from irrelevant padding.
    large_blob.extend(std::iter::repeat_n(b' ', 32 * 1024 * 1024 + 1));
    fs::write(root.join("oversized.txt"), large_blob).unwrap();
    git(root, &["add", "oversized.txt"]);
    git(root, &["commit", "-qm", "oversized blob"]);

    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .arg("refresh")
        .current_dir(root)
        .env(
            "REPODB_SEMGREP_CORE",
            semgrep_corpus::required_semgrep_core(),
        )
        .env("XDG_CONFIG_HOME", root.join(".test-xdg-config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "large valid blob should refresh: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after = search_json(root, &["TARGET", "--all-history"]);
    assert_eq!(after.len(), before.len());
    assert_eq!(after[0]["text"], before[0]["text"]);
    assert_eq!(
        search_json(root, &["oversized searchable marker", "--all-history"]).len(),
        1
    );
}

#[test]
fn failed_refresh_keeps_exact_and_semantic_snapshot_and_model_dimension() {
    let repo = repository();
    let root = repo.path();
    let old = git_text(root, &["rev-parse", "HEAD"]);
    assert!(run(root, &["init"]).0);
    let fake = fake_embedder(
        root,
        "if [ -f fail-refresh ] && [ \"$input\" = 'new indexed text' ]; then exit 1; fi\nif [ -f wrong-dimension ]; then printf '[1,0,0]'; else printf '[1,0]'; fi",
    );
    assert!(
        run(
            root,
            &[
                "reembed",
                "--command",
                fake.to_str().unwrap(),
                "--model",
                "retained-model"
            ]
        )
        .0
    );
    let exact = search_json(root, &["TARGET"]);
    let semantic = search_json(root, &["question", "--semantic"]);
    git(root, &["checkout", "-qb", "new-side"]);
    fs::write(root.join("new.txt"), "new indexed text\n").unwrap();
    git(root, &["add", "new.txt"]);
    git(root, &["commit", "-qm", "new content"]);
    // A newly checked-out commit is not silently indexed, even for unchanged blobs.
    assert!(search_json(root, &["TARGET"]).is_empty());
    assert!(search_json(root, &["new indexed", "--all-history"]).is_empty());
    git(root, &["checkout", "-q", &old]);
    for sentinel in ["fail-refresh", "wrong-dimension"] {
        fs::write(root.join(sentinel), "").unwrap();
        assert!(!run(root, &["refresh"]).0, "{sentinel}");
        fs::remove_file(root.join(sentinel)).unwrap();
        assert_eq!(search_json(root, &["TARGET"]), exact);
        assert_eq!(search_json(root, &["question", "--semantic"]), semantic);
        assert!(search_json(root, &["new indexed", "--all-history"]).is_empty());
    }
    assert!(run(root, &["refresh"]).0);
    assert_eq!(
        search_json(root, &["new indexed", "--all-history"]).len(),
        1
    );
}

#[test]
fn history_import_dimension_mismatch_keeps_prior_snapshot_and_model_dimension() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let imported = root.join("external.txt");
    fs::write(&imported, "prior imported marker\n").unwrap();
    let locator = imported.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    let embedder = fake_embedder(
        root,
        "case \"$input\" in\n  'new history mismatch marker') printf '[1,0]' ;;\n  'new imported mismatch marker') printf '[1,0,0]' ;;\n  *) printf '[1,0]' ;;\nesac",
    );
    assert!(
        run(
            root,
            &[
                "reembed",
                "--command",
                embedder.to_str().unwrap(),
                "--model",
                "dimension-model"
            ]
        )
        .0
    );
    assert!(run(root, &["retention", "all"]).0);
    assert!(run(root, &["refresh"]).0);
    let prior_exact = search_json(root, &["prior imported marker"]);
    let prior_semantic = search_json(
        root,
        &["prior imported marker", "--semantic", "--all-history"],
    );
    assert_eq!(prior_exact.len(), 1);
    assert!(!prior_semantic.is_empty());

    fs::write(&imported, "new imported mismatch marker\n").unwrap();
    fs::write(
        root.join("new-history.txt"),
        "new history mismatch marker\n",
    )
    .unwrap();
    git(root, &["add", "new-history.txt"]);
    git(root, &["commit", "-qm", "dimension mismatch"]);

    assert!(!run(root, &["refresh"]).0);
    assert_eq!(search_json(root, &["prior imported marker"]), prior_exact);
    assert_eq!(
        search_json(
            root,
            &["prior imported marker", "--semantic", "--all-history"]
        ),
        prior_semantic
    );
    assert!(search_json(root, &["new history mismatch marker", "--all-history"]).is_empty());
    assert!(search_json(root, &["new imported mismatch marker"]).is_empty());
}

#[test]
fn history_and_import_dimension_mismatch_fails_when_model_dimension_is_unknown() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    assert!(run(repo.path(), &["init"]).0);

    let embedder = fake_embedder(
        repo.path(),
        "case \"$input\" in\n  *'two dimensional history marker'*) printf '[1,0]' ;;\n  *) printf '[1,0,0]' ;;\nesac",
    );
    assert!(
        run(
            repo.path(),
            &["reembed", "--command", embedder.to_str().unwrap()]
        )
        .0
    );

    fs::write(
        repo.path().join("history.txt"),
        "two dimensional history marker\n",
    )
    .unwrap();
    git(repo.path(), &["add", "history.txt"]);
    git(repo.path(), &["commit", "-qm", "history dimension"]);

    let imported = repo.path().join("external.txt");
    fs::write(&imported, "three dimensional imported marker\n").unwrap();
    let locator = imported.to_string_lossy().into_owned();
    assert!(run(repo.path(), &["import", &locator]).0);

    assert!(!run(repo.path(), &["refresh"]).0);
    assert!(
        search_json(
            repo.path(),
            &["two dimensional history marker", "--all-history"]
        )
        .is_empty()
    );
    assert!(search_json(repo.path(), &["three dimensional imported marker"]).is_empty());
}

#[test]
fn empty_repository_can_initialize_then_index_its_first_commit() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    assert!(run(root, &["init"]).0);
    assert!(search_json(root, &["anything"]).is_empty());
    let fake = fake_embedder(root, "printf '[1,0]'");
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);
    assert!(search_json(root, &["anything", "--semantic"]).is_empty());
    fs::write(root.join("first.txt"), "first content\n").unwrap();
    git(root, &["add", "first.txt"]);
    git(root, &["commit", "-qm", "first"]);
    assert!(run(root, &["refresh"]).0);
    let exact = search_json(root, &["first", "--type=all"]);
    assert!(exact.iter().any(|result| result["path"] == "first.txt"));
    assert!(
        exact
            .iter()
            .any(|result| result["kind"] == "commit_message")
    );
    let semantic = search_json(root, &["question", "--semantic"]);
    assert!(semantic.iter().any(|result| result["path"] == "first.txt"));
}

#[test]
fn legacy_cli_snapshot_remains_searchable_until_successful_atomic_history_migration() {
    use std::os::unix::fs::PermissionsExt;
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q", "--object-format=sha1"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    fs::write(root.join("legacy.txt"), "legacy marker\n").unwrap();
    git(root, &["add", "legacy.txt"]);
    let old = commit_at(root, "legacy", "2000-01-01T00:00:00Z");
    assert_eq!(old, "dc090cb6bf0c933d305d3cab0b0c49c75845990a");
    fs::write(
        root.join(".git/repodb.sqlite"),
        include_bytes!("fixtures/legacy.sqlite"),
    )
    .unwrap();
    fs::write(root.join("legacy-embedder"), "#!/bin/sh\ninput=$(cat)\n[ \"$REPODB_EMBEDDING_MODEL\" = legacy-model ] || exit 2\nif [ -f fail-refresh ] && [ \"$input\" = 'new marker' ]; then exit 1; fi\nprintf '[1,0]'\n").unwrap();
    fs::set_permissions(
        root.join("legacy-embedder"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let exact = search_json(root, &["legacy"]);
    let semantic = search_json(root, &["question", "--semantic"]);
    assert_eq!(exact.len(), 1);
    assert_eq!(semantic[0]["text"], "legacy marker");
    git(root, &["checkout", "-qb", "future"]);
    fs::write(root.join("new.txt"), "new marker\n").unwrap();
    git(root, &["add", "new.txt"]);
    git(root, &["commit", "-qm", "new"]);
    let new = git_text(root, &["rev-parse", "HEAD"]);
    let new_blob = git_text(root, &["rev-parse", "HEAD:new.txt"]);
    git(root, &["checkout", "-q", &old]);
    fs::write(root.join("fail-refresh"), "").unwrap();
    assert!(!run(root, &["refresh"]).0);
    assert_eq!(search_json(root, &["legacy"]), exact);
    assert_eq!(search_json(root, &["question", "--semantic"]), semantic);
    assert!(search_json(root, &["new marker", "--all-history"]).is_empty());
    fs::remove_file(root.join("fail-refresh")).unwrap();
    // A Git object read failure must also leave the legacy snapshot usable.
    let object = root
        .join(".git/objects")
        .join(&new_blob[..2])
        .join(&new_blob[2..]);
    let saved = root.join("saved-object");
    fs::rename(&object, &saved).unwrap();
    assert!(!run(root, &["refresh"]).0);
    assert_eq!(search_json(root, &["legacy"]), exact);
    assert_eq!(search_json(root, &["question", "--semantic"]), semantic);
    fs::rename(&saved, &object).unwrap();
    assert!(run(root, &["refresh"]).0);
    let migrated = search_json(root, &["new marker", "--commit", &new]);
    assert!(migrated.iter().any(|result| result["blob_id"] == new_blob));
    let migrated_semantic = search_json(root, &["question", "--semantic", "--commit", &new]);
    assert!(migrated_semantic.len() >= 2);
    assert!(migrated_semantic.iter().all(|r| r["distance"] == 0.0));
    assert_eq!(search_json(root, &["legacy"])[0]["commit_id"], old);
}

#[test]
fn populated_v3_history_migrates_atomically_to_range_only_schema() {
    let repo = repository();
    let root = repo.path();
    let original_commit = git_text(root, &["rev-parse", "HEAD"]);
    let original_blob = git_text(root, &["rev-parse", "HEAD:notes.txt"]);
    assert!(run(root, &["init"]).0);

    // Exercise the supported v3 layout with real references, searchable text, and vectors.
    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    database.execute_batch(
        "DROP TABLE IF EXISTS diff_references; DROP TABLE IF EXISTS diff_file_changes; DROP TABLE IF EXISTS diff_detection_settings; DROP TABLE unit_embeddings; DROP TABLE commit_message_units; DROP TABLE git_blob_units; DROP TABLE indexed_units; \
         DROP TABLE git_occurrences; DROP TABLE blob_contents; DROP TABLE git_commits; \
         CREATE TABLE git_commits(commit_id TEXT PRIMARY KEY NOT NULL, timestamp INTEGER NOT NULL); \
         CREATE TABLE blob_contents(blob_id TEXT PRIMARY KEY NOT NULL, content TEXT, status TEXT NOT NULL); \
         CREATE TABLE git_occurrences(commit_id TEXT NOT NULL REFERENCES git_commits, blob_id TEXT NOT NULL REFERENCES blob_contents, path BLOB NOT NULL, segmentation_kind TEXT NOT NULL, PRIMARY KEY(commit_id,path)); \
         CREATE TABLE indexed_units(unit_id INTEGER PRIMARY KEY, text TEXT NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL); \
         CREATE TABLE git_blob_units(blob_id TEXT NOT NULL REFERENCES blob_contents, unit_id INTEGER NOT NULL REFERENCES indexed_units, segmentation_kind TEXT NOT NULL, PRIMARY KEY(blob_id,segmentation_kind,unit_id)); \
         CREATE TABLE unit_embeddings(unit_id INTEGER PRIMARY KEY REFERENCES indexed_units, vector BLOB NOT NULL); \
         PRAGMA user_version=3;",
    ).unwrap();
    database
        .execute("INSERT INTO git_commits VALUES(?1,0)", [&original_commit])
        .unwrap();
    database
        .execute(
            "INSERT INTO blob_contents VALUES(?1,?2,'available')",
            rusqlite::params![
                original_blob,
                "First line\nExact [literal] TARGET\nlast line\n"
            ],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO git_occurrences VALUES(?1,?2,?3,'document')",
            rusqlite::params![original_commit, original_blob, b"notes.txt".as_slice()],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO indexed_units VALUES(1,?1,1,3)",
            ["First line\nExact [literal] TARGET\nlast line"],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO git_blob_units VALUES(?1,1,'document')",
            [&original_blob],
        )
        .unwrap();
    database
        .execute(
            "INSERT INTO unit_embeddings VALUES(1,?1)",
            [vec![0, 0, 128, 63, 0, 0, 0, 0]],
        )
        .unwrap();
    drop(database);

    let exact = search_json(root, &["Exact [literal] TARGET"]);
    let semantic = search_json(root, &["question", "--semantic"]);
    assert_eq!(exact.len(), 1);
    assert_eq!(semantic.len(), 1);

    git(root, &["checkout", "-qb", "later"]);
    fs::write(root.join("later.txt"), "later committed marker\n").unwrap();
    git(root, &["add", "later.txt"]);
    git(root, &["commit", "-qm", "later source"]);
    let later_commit = git_text(root, &["rev-parse", "HEAD"]);
    let later_blob = git_text(root, &["rev-parse", "HEAD:later.txt"]);
    git(root, &["checkout", "-q", &original_commit]);
    let object = root
        .join(".git/objects")
        .join(&later_blob[..2])
        .join(&later_blob[2..]);
    let saved = root.join("saved-later-object");
    fs::rename(&object, &saved).unwrap();
    assert!(!run(root, &["refresh"]).0);
    assert_eq!(search_json(root, &["Exact [literal] TARGET"]), exact);
    assert_eq!(search_json(root, &["question", "--semantic"]), semantic);
    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let version: i64 = database
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
    drop(database);

    fs::rename(&saved, &object).unwrap();
    assert!(run(root, &["refresh"]).0);
    let migrated = search_json(
        root,
        &["Exact [literal] TARGET", "--commit", &original_commit],
    );
    assert_eq!(migrated.len(), 1);
    assert_eq!(
        migrated[0]["text"],
        "First line\nExact [literal] TARGET\nlast line"
    );
    assert_eq!(
        search_json(root, &["later committed marker", "--commit", &later_commit]).len(),
        1
    );
    let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let version: i64 = database
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5);
    for (table, forbidden) in [("blob_contents", "content"), ("indexed_units", "text")] {
        let columns = database
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column == forbidden));
    }
    let (invalid_ranges, units): (i64, i64) = database.query_row(
        "SELECT sum(byte_start < 0 OR byte_end <= byte_start OR byte_end > ?1), count(*) FROM indexed_units JOIN git_blob_units USING(unit_id)",
        ["First line\nExact [literal] TARGET\nlast line\n".len() as i64],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert!(units > 0);
    assert_eq!(invalid_ranges, 0);
}

#[test]
fn diff_search_cli_exposes_added_and_removed_hits_with_bounded_paired_references() {
    let repo = repository();
    let root = repo.path();
    let old_line = format!("removed passage marker {}\n", "o".repeat(10_000));
    let new_line = format!("added passage marker {}\n", "n".repeat(10_000));
    fs::write(
        root.join("notes.txt"),
        format!("First line\n{old_line}last line\n"),
    )
    .unwrap();
    git(root, &["add", "notes.txt"]);
    git(root, &["commit", "-qm", "original passage"]);
    let parent = git_text(root, &["rev-parse", "HEAD"]);
    fs::write(
        root.join("notes.txt"),
        format!("First line\n{new_line}last line\n"),
    )
    .unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"changed source phrase\"); }\n",
    )
    .unwrap();
    git(root, &["add", "notes.txt", "src/main.rs"]);
    git(root, &["commit", "-qm", "replace passage"]);
    let child = git_text(root, &["rev-parse", "HEAD"]);
    assert!(run(root, &["init"]).0);
    assert!(search_json(root, &["removed passage marker"]).is_empty());

    for (query, side, marker) in [
        (
            "removed passage marker",
            "removed",
            "removed passage marker",
        ),
        ("added passage marker", "added", "added passage marker"),
    ] {
        let (ok, output) = run(root, &["search", query, "--diffs", "--json"]);
        assert!(ok, "diff search failed: {output}");
        let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        let hit = results
            .iter()
            .find(|result| result["kind"] == "diff")
            .unwrap_or_else(|| panic!("missing diff hit: {results:?}"));
        assert_eq!(hit["side"], side);
        assert_eq!(hit["text"].as_str().unwrap().len(), 4096);
        assert!(hit["text"].as_str().unwrap().contains(marker));
        assert_eq!(hit["parent_commit_id"], parent);
        assert_eq!(hit["child_commit_id"], child);
        assert_eq!(hit["old_path"], "notes.txt");
        assert_eq!(hit["new_path"], "notes.txt");
        assert_eq!(hit["old_line_start"], 2);
        assert_eq!(hit["new_line_start"], 2);
        assert_eq!(hit["file_change"]["kind"], "modified");
        assert_eq!(hit["file_change"]["heuristic"], false);
        assert_eq!(
            hit["parent_git_show_command"],
            format!("git show '{parent}:notes.txt'")
        );
        assert_eq!(
            hit["child_git_show_command"],
            format!("git show '{child}:notes.txt'")
        );
        for (key, expected) in [
            (
                "parent_git_show_command",
                format!("First line\n{old_line}last line\n"),
            ),
            (
                "child_git_show_command",
                format!("First line\n{new_line}last line\n"),
            ),
        ] {
            let command = hit[key].as_str().unwrap();
            let output = Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{command}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.stdout, expected.as_bytes());
        }
        assert_eq!(hit["matching_line_start"], 2);
        assert_eq!(hit["matching_line_end"], 2);
    }

    let (ok, human) = run(root, &["search", "removed passage marker", "--diffs"]);
    assert!(ok);
    assert!(
        human.contains("diff removed match=notes.txt:2-2"),
        "{human}"
    );
    assert!(
        human.contains(&format!("parent {parent} child {child}")),
        "{human}"
    );
    assert!(
        human.contains("old=notes.txt:2-2 new=notes.txt:2-2"),
        "{human}"
    );
    assert!(
        human.contains("file_change=modified heuristic=false"),
        "{human}"
    );
    assert!(human.contains("detection=rename=50%"), "{human}");
    assert!(human.contains("parent-file-command=git show"), "{human}");

    let (ok, output) = run(
        root,
        &[
            "search",
            "a semantic query",
            "--semantic",
            "--diffs",
            "--json",
        ],
    );
    assert!(ok, "semantic diff search failed: {output}");
    let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    let diff_hits = results
        .iter()
        .filter(|result| result["kind"] == "diff")
        .collect::<Vec<_>>();
    assert!(diff_hits.iter().any(|hit| {
        hit["side"] == "added"
            && hit["text"]
                .as_str()
                .unwrap()
                .contains("added passage marker")
    }));
    assert!(diff_hits.iter().any(|hit| {
        hit["side"] == "removed"
            && hit["text"]
                .as_str()
                .unwrap()
                .contains("removed passage marker")
    }));
    assert!(diff_hits.iter().all(|hit| {
        hit["distance"].is_number()
            && hit["parent_commit_id"] == parent
            && hit["child_commit_id"] == child
            && hit["old_path"].is_string()
            && hit["new_path"].is_string()
    }));

    let (ok, output) = run(
        root,
        &[
            "search",
            "unrelated semantic query",
            "--semantic",
            "--diffs",
            "--semantic-limit",
            "1",
            "--scope",
            "repo-local-source",
            "--json",
        ],
    );
    assert!(ok, "scoped semantic diff search failed: {output}");
    let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    let diff_hits = results
        .iter()
        .filter(|result| result["kind"] == "diff")
        .collect::<Vec<_>>();
    assert_eq!(diff_hits.len(), 1, "{diff_hits:?}");
    assert_eq!(diff_hits[0]["new_path"], "src/main.rs");

    let (ok, output) = run(
        root,
        &[
            "search",
            "removed passage marker",
            "--diffs",
            "--semantic-limit",
            "0",
            "--json",
        ],
    );
    assert!(
        ok,
        "exact diff search with zero semantic limit failed: {output}"
    );
    let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert!(
        results
            .iter()
            .any(|result| result["kind"] == "diff" && result["side"] == "removed")
    );
}

#[test]
fn diff_search_json_reports_merge_parent_and_heuristic_identity() {
    let repo = repository();
    let root = repo.path();
    let rename_context = "rename-specific context ".repeat(100);
    let copy_context = "copy-specific context ".repeat(100);
    fs::write(
        root.join("old-name.txt"),
        format!("before rename marker\n{rename_context}\n"),
    )
    .unwrap();
    fs::write(
        root.join("copy-source.txt"),
        format!("before copy marker\n{copy_context}\n"),
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base for diff provenance"]);
    let base = git_text(root, &["rev-parse", "HEAD"]);
    let main_branch = git_text(root, &["branch", "--show-current"]);
    git(root, &["branch", "side"]);

    git(root, &["checkout", "-q", "side"]);
    fs::write(
        root.join("side-only.txt"),
        "merge parent marker from side\n",
    )
    .unwrap();
    git(root, &["add", "side-only.txt"]);
    git(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "side",
            "--date",
            "2020-01-01T00:00:00Z",
        ],
    );
    let side = git_text(root, &["rev-parse", "HEAD"]);

    git(root, &["checkout", "-q", &main_branch]);
    fs::rename(root.join("old-name.txt"), root.join("new-name.txt")).unwrap();
    fs::write(
        root.join("new-name.txt"),
        format!("after rename marker\n{rename_context}\n"),
    )
    .unwrap();
    fs::copy(root.join("copy-source.txt"), root.join("copied.txt")).unwrap();
    fs::write(
        root.join("copied.txt"),
        format!("after copy marker\n{copy_context}\n"),
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "rename and copy"]);
    let main = git_text(root, &["rev-parse", "HEAD"]);
    git(root, &["merge", "--no-ff", "-m", "merge", "side"]);
    let merge = git_text(root, &["rev-parse", "HEAD"]);
    assert!(run(root, &["init"]).0);

    let (ok, output) = run(
        root,
        &[
            "search",
            "after rename marker",
            "--diffs",
            "--all-history",
            "--json",
        ],
    );
    assert!(ok, "rename diff search failed: {output}");
    let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    let rename = results
        .iter()
        .find(|result| {
            result["kind"] == "diff"
                && result["parent_commit_id"] == side
                && result["child_commit_id"] == merge
                && result["new_path"] == "new-name.txt"
        })
        .unwrap_or_else(|| panic!("missing side-parent rename hit: {results:?}"));
    assert_eq!(rename["file_change"]["kind"], "heuristic_rename");
    assert_eq!(rename["file_change"]["heuristic"], true);
    assert!(rename["file_change"]["similarity_percent"].is_number());
    assert_eq!(
        rename["file_change"]["detection_settings"]["rename_threshold_percent"],
        50
    );
    assert_eq!(rename["old_path"], "old-name.txt");
    assert_eq!(rename["new_path"], "new-name.txt");
    assert_eq!(
        rename["parent_git_show_command"],
        format!("git show '{side}:old-name.txt'")
    );
    assert_eq!(
        rename["child_git_show_command"],
        format!("git show '{merge}:new-name.txt'")
    );
    for (key, expected) in [
        (
            "parent_git_show_command",
            format!("before rename marker\n{rename_context}\n"),
        ),
        (
            "child_git_show_command",
            format!("after rename marker\n{rename_context}\n"),
        ),
    ] {
        let output = Command::new("sh")
            .arg("-c")
            .arg(rename[key].as_str().unwrap())
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, expected.as_bytes());
    }

    let (ok, output) = run(
        root,
        &[
            "search",
            "after copy marker",
            "--diffs",
            "--all-history",
            "--json",
        ],
    );
    assert!(ok, "copy diff search failed: {output}");
    let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    let copy = results
        .iter()
        .find(|result| {
            result["kind"] == "diff"
                && result["parent_commit_id"] == side
                && result["child_commit_id"] == merge
                && result["new_path"] == "copied.txt"
        })
        .unwrap_or_else(|| panic!("missing side-parent copy hit: {results:?}"));
    assert_eq!(copy["file_change"]["kind"], "heuristic_copy");
    assert_eq!(copy["file_change"]["heuristic"], true);
    assert_eq!(copy["old_path"], "copy-source.txt");
    assert_eq!(copy["new_path"], "copied.txt");
    assert_eq!(copy["parent_commit_id"], side);
    assert_eq!(copy["child_commit_id"], merge);
    assert_eq!(
        copy["parent_git_show_command"],
        format!("git show '{side}:copy-source.txt'")
    );
    assert_eq!(
        copy["child_git_show_command"],
        format!("git show '{merge}:copied.txt'")
    );
    for (key, expected) in [
        (
            "parent_git_show_command",
            format!("before copy marker\n{copy_context}\n"),
        ),
        (
            "child_git_show_command",
            format!("after copy marker\n{copy_context}\n"),
        ),
    ] {
        let output = Command::new("sh")
            .arg("-c")
            .arg(copy[key].as_str().unwrap())
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, expected.as_bytes());
    }
    assert_ne!(side, main);
    assert_ne!(side, base);
}

#[test]
fn diff_search_shell_safely_dumps_files_with_unusual_path_bytes() {
    use std::os::unix::ffi::OsStrExt;

    let repo = repository();
    let root = repo.path();
    let path = b"evil$(touch pwned);'\xff\n";
    let contents = "unusual path marker\n";
    let quoted_path = std::ffi::OsStr::new("quote'path.txt");
    let quoted_contents = "quoted path marker\n";
    let removed_contents = "removed-only path marker\n";
    fs::write(root.join(std::ffi::OsStr::from_bytes(path)), contents).unwrap();
    fs::write(root.join(quoted_path), quoted_contents).unwrap();
    fs::write(root.join("deleted-only.txt"), removed_contents).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "add unusual and removable paths"]);
    let add_commit = git_text(root, &["rev-parse", "HEAD"]);
    fs::remove_file(root.join("deleted-only.txt")).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "delete path"]);
    let delete_commit = git_text(root, &["rev-parse", "HEAD"]);
    assert!(run(root, &["init"]).0);

    let (ok, output) = run(
        root,
        &[
            "search",
            "unusual path marker",
            "--diffs",
            "--all-history",
            "--json",
        ],
    );
    assert!(ok, "diff search failed: {output}");
    let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    let hit = results
        .iter()
        .find(|result| result["kind"] == "diff")
        .unwrap_or_else(|| panic!("missing unusual-path diff hit: {results:?}"));
    assert_eq!(hit["old_path"], serde_json::Value::Null);
    assert_eq!(
        hit["new_path_bytes_hex"],
        path.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    assert_eq!(hit["parent_git_show_command"], serde_json::Value::Null);
    assert_eq!(hit["file_change"]["kind"], "added");
    let command = hit["child_git_show_command"].as_str().unwrap();
    assert!(command.is_ascii() && !command.contains('\n'), "{command:?}");
    assert!(command.contains("printf '%b'"), "{command}");
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{command}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, contents.as_bytes());
    assert!(!root.join("pwned").exists());
    assert!(
        hit["child_git_show_command"]
            .as_str()
            .unwrap()
            .contains(&add_commit)
    );

    let (ok, output) = run(
        root,
        &[
            "search",
            "quoted path marker",
            "--diffs",
            "--all-history",
            "--json",
        ],
    );
    assert!(ok, "quoted-path diff search failed: {output}");
    let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    let hit = results
        .iter()
        .find(|result| result["kind"] == "diff")
        .unwrap();
    let command = hit["child_git_show_command"].as_str().unwrap();
    assert!(command.contains("'\\''"), "{command}");
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{command}");
    assert_eq!(output.stdout, quoted_contents.as_bytes());

    let (ok, output) = run(
        root,
        &[
            "search",
            "removed-only path marker",
            "--diffs",
            "--all-history",
            "--json",
        ],
    );
    assert!(ok, "deleted-file diff search failed: {output}");
    let results: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    let hit = results
        .iter()
        .find(|result| result["kind"] == "diff" && result["side"] == "removed")
        .unwrap_or_else(|| panic!("missing deleted-file diff hit: {results:?}"));
    assert_eq!(hit["parent_commit_id"], add_commit);
    assert_eq!(hit["child_commit_id"], delete_commit);
    assert_eq!(hit["new_path"], serde_json::Value::Null);
    assert_eq!(hit["child_git_show_command"], serde_json::Value::Null);
    let command = hit["parent_git_show_command"].as_str().unwrap();
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{command}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, removed_contents.as_bytes());
}

#[test]
fn history_human_output_identifies_commit_blob_and_line_range() {
    let repo = repository();
    let root = repo.path();
    let commit = git_text(root, &["rev-parse", "HEAD"]);
    let blob = git_text(root, &["rev-parse", "HEAD:notes.txt"]);
    assert!(run(root, &["init"]).0);
    let (ok, human) = run(root, &["search", "TARGET", "--all-history"]);
    assert!(ok);
    assert!(human.contains("notes.txt:1-3"), "{human}");
    assert!(human.contains(&commit), "{human}");
    assert!(human.contains(&blob), "{human}");
}

#[test]
fn newest_reference_uses_timestamp_then_commit_id_then_path_including_detached_head() {
    let repo = repository();
    let root = repo.path();
    let base = git_text(root, &["rev-parse", "HEAD"]);
    let path = "odd\tname\n.txt";
    fs::write(root.join(path), "tie marker\nsecond tie marker\n").unwrap();
    git(root, &["add", path]);
    let first = commit_at(root, "first tie", "2001-01-01T00:00:00Z");
    git(root, &["tag", "captured-first"]);
    git(root, &["checkout", "-q", "--detach", &base]);
    fs::write(root.join(path), "tie marker\nsecond tie marker\n").unwrap();
    fs::write(root.join("z-copy.txt"), "tie marker\nsecond tie marker\n").unwrap();
    fs::write(root.join("binary.bin"), b"hidden binary marker\0").unwrap();
    fs::write(root.join("invalid.txt"), b"hidden invalid marker\xff").unwrap();
    git(
        root,
        &["add", path, "z-copy.txt", "binary.bin", "invalid.txt"],
    );
    let detached = commit_at(root, "detached tie", "2001-01-01T00:00:00Z");
    let expected = std::cmp::max(&first, &detached);
    assert!(run(root, &["init"]).0);
    let results = search_json(root, &["tie marker", "--all-history"]);
    assert_eq!(results.len(), 1);
    for result in &results {
        assert_eq!(result["commit_id"], *expected);
        assert_eq!(result["path"], path);
        assert_eq!(result["line_start"], 1);
        assert_eq!(result["line_end"], 2);
    }
    assert!(search_json(root, &["hidden", "--all-history"]).is_empty());
    // Moving away from detached HEAD does not prune its captured occurrence.
    git(root, &["checkout", "-q", &first]);
    assert_eq!(
        search_json(root, &["tie marker", "--commit", &detached]).len(),
        1
    );
    assert!(run(root, &["refresh"]).0);
    let results = search_json(root, &["tie marker", "--all-history"]);
    assert!(results.iter().all(|r| r["commit_id"] == first));
    assert!(!run(root, &["search", "tie marker", "--commit", &detached]).0);
}

#[test]
fn refresh_keeps_history_and_imports_atomic_when_import_preparation_fails() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let imported = root.join("external.txt");
    fs::write(&imported, "previous imported marker\n").unwrap();
    let locator = imported.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    let embedder = root.join("atomic-embedder");
    fs::write(
        &embedder,
        "#!/bin/sh\ninput=$(cat)\n[ \"$input\" != 'failing imported marker' ] || exit 1\nprintf '[1,0]'\n",
    )
    .unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);
    assert!(run(root, &["refresh"]).0);
    assert_eq!(
        search_json(root, &["previous imported marker"])[0]["text"],
        "previous imported marker"
    );
    assert!(run(root, &["retention", "all"]).0);
    fs::write(&imported, "second prior imported marker\n").unwrap();
    assert!(run(root, &["refresh"]).0);
    let prior_semantic = search_json(root, &["imported marker", "--semantic"])
        .into_iter()
        .filter(|r| r["path"] == locator)
        .collect::<Vec<_>>();
    assert!(
        prior_semantic
            .iter()
            .any(|r| r["text"] == "previous imported marker")
    );
    assert!(
        prior_semantic
            .iter()
            .any(|r| r["text"] == "second prior imported marker")
    );

    fs::write(root.join("new-history.txt"), "new history marker\n").unwrap();
    git(root, &["add", "new-history.txt"]);
    git(root, &["commit", "-qm", "new history"]);
    fs::write(&imported, "failing imported marker\n").unwrap();
    assert!(!run(root, &["refresh"]).0);

    assert_eq!(search_json(root, &["TARGET", "--all-history"]).len(), 1);
    assert!(search_json(root, &["new history marker", "--all-history"]).is_empty());
    let after_semantic = search_json(root, &["imported marker", "--semantic"])
        .into_iter()
        .filter(|r| r["path"] == locator)
        .collect::<Vec<_>>();
    assert_eq!(after_semantic, prior_semantic);
    assert_eq!(search_json(root, &["previous imported marker"]).len(), 1);
    assert_eq!(
        search_json(root, &["second prior imported marker"]).len(),
        1
    );
    assert!(search_json(root, &["failing imported marker"]).is_empty());
}

#[test]
fn refresh_keeps_imports_atomic_when_history_preparation_fails() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let imported = root.join("external.txt");
    fs::write(&imported, "previous imported marker\n").unwrap();
    let locator = imported.to_string_lossy().into_owned();
    assert!(run(root, &["import", &locator]).0);
    let embedder = fake_embedder(root, "printf '[1,0]'");
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);
    assert!(run(root, &["retention", "all"]).0);
    assert!(run(root, &["refresh"]).0);
    fs::write(&imported, "second prior imported marker\n").unwrap();
    assert!(run(root, &["refresh"]).0);
    let prior_semantic = search_json(root, &["imported marker", "--semantic"])
        .into_iter()
        .filter(|r| r["path"] == locator)
        .collect::<Vec<_>>();
    assert!(
        prior_semantic
            .iter()
            .any(|r| r["text"] == "previous imported marker")
    );
    assert!(
        prior_semantic
            .iter()
            .any(|r| r["text"] == "second prior imported marker")
    );

    fs::write(&imported, "new imported marker\n").unwrap();
    fs::write(root.join("new-history.txt"), "new history marker\n").unwrap();
    git(root, &["add", "new-history.txt"]);
    git(root, &["commit", "-qm", "new history"]);
    let shim = root.join("git-shim");
    fs::create_dir(&shim).unwrap();
    let real_git = Command::new("which").arg("git").output().unwrap();
    let real_git = String::from_utf8(real_git.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let failing_git = shim.join("git");
    fs::write(
        &failing_git,
        format!("#!/bin/sh\n[ \"$1\" != rev-list ] || exit 1\nexec {real_git} \"$@\"\n"),
    )
    .unwrap();
    fs::set_permissions(&failing_git, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", shim.display(), std::env::var("PATH").unwrap());
    assert!(!run_with_env(root, &["refresh"], &[("PATH", &path)]).0);

    let after_semantic = search_json(root, &["imported marker", "--semantic"])
        .into_iter()
        .filter(|r| r["path"] == locator)
        .collect::<Vec<_>>();
    assert_eq!(after_semantic, prior_semantic);
    assert_eq!(search_json(root, &["previous imported marker"]).len(), 1);
    assert_eq!(
        search_json(root, &["second prior imported marker"]).len(),
        1
    );
    assert!(search_json(root, &["new imported marker"]).is_empty());
    assert!(search_json(root, &["new history marker", "--all-history"]).is_empty());
}

#[test]
fn refresh_does_not_publish_vectors_over_a_concurrently_changed_model() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let old = fake_embedder(
        root,
        "if [ -f pause ] && [ \"$input\" = 'First line\nExact [literal] TARGET\nlast line' ]; then touch prepared; while [ ! -f release ]; do sleep 0.01; done; fi\nprintf '[1,0]'",
    );
    assert!(
        run(
            root,
            &[
                "reembed",
                "--command",
                old.to_str().unwrap(),
                "--model",
                "old"
            ]
        )
        .0
    );
    fs::write(root.join("pause"), "").unwrap();
    let mut refresh = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .arg("refresh")
        .current_dir(root)
        .env(
            "REPODB_SEMGREP_CORE",
            semgrep_corpus::required_semgrep_core(),
        )
        .env("XDG_CONFIG_HOME", root.join(".test-xdg-config"))
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("prepared").exists() {
        if Instant::now() > deadline {
            refresh.kill().unwrap();
            panic!("refresh did not reach the fake embedder before timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let new = root.join("new-model-embedder");
    fs::write(&new, "#!/bin/sh\ncat >/dev/null\nprintf '[0,1]'\n").unwrap();
    fs::set_permissions(&new, fs::Permissions::from_mode(0o755)).unwrap();
    let reembedded = run(
        root,
        &[
            "reembed",
            "--command",
            new.to_str().unwrap(),
            "--model",
            "new",
        ],
    );
    fs::write(root.join("release"), "").unwrap();
    let output = refresh.wait_with_output().unwrap();
    assert!(reembedded.0);
    assert!(
        !output.status.success(),
        "stale refresh must not overwrite a newer model"
    );
    let results = search_json(root, &["question", "--semantic"]);
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|r| r["distance"] == 0.0));
}

#[test]
fn document_paragraph_units_preserve_literal_text_and_inclusive_source_ranges() {
    let repo = repository();
    let root = repo.path();
    let text = "\n \t\nFirst [literal] paragraph\ncontinues with café.\n\t\nSecond paragraph\nlast line\n\n";
    for path in ["paragraph.md", "paragraph.markdown", "paragraph.txt"] {
        fs::write(root.join(path), text).unwrap();
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "paragraphs"]);
    assert!(run(root, &["init"]).0);
    let first = search_json(root, &["First [literal] paragraph\ncontinues"]);
    assert_eq!(first.len(), 1, "identical blobs collapse");
    assert_eq!(first[0]["path"], "paragraph.markdown");
    assert_eq!(
        first[0]["text"],
        "First [literal] paragraph\ncontinues with café."
    );
    assert_eq!(first[0]["line_start"], 3);
    assert_eq!(first[0]["line_end"], 4);
    let second = search_json(root, &["Second paragraph"]);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["text"], "Second paragraph\nlast line");
    assert_eq!(second[0]["line_start"], 6);
    assert_eq!(second[0]["line_end"], 7);
    assert!(search_json(root, &["First .* paragraph"]).is_empty());
}

#[test]
fn long_document_units_are_bounded_lossless_and_use_unicode_safe_source_ranges() {
    let repo = repository();
    let root = repo.path();
    let multiline = format!("{}\n{}", "a".repeat(3000), "b".repeat(3000));
    let unicode = "é🦀".repeat(1500);
    fs::write(root.join("long.md"), &multiline).unwrap();
    fs::write(root.join("unicode.txt"), &unicode).unwrap();
    fs::write(
        root.join("windows.txt"),
        "\r\nfirst\r\nsecond\r\n \t\r\nthird\r\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "long paragraphs"]);
    assert!(run(root, &["init"]).0);
    let all = search_json(root, &[""]);
    for (path, source, expected_ranges) in [
        ("long.md", multiline, vec![(1, 1), (2, 2)]),
        ("unicode.txt", unicode, vec![(1, 1), (1, 1), (1, 1)]),
    ] {
        let units: Vec<_> = all.iter().filter(|r| r["path"] == path).collect();
        assert_eq!(units.len(), expected_ranges.len(), "{path}");
        let reconstructed: String = units.iter().map(|r| r["text"].as_str().unwrap()).collect();
        assert_eq!(reconstructed, source);
        let database = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
        let stored_ranges = database
            .prepare("SELECT u.byte_start,u.byte_end FROM indexed_units u JOIN git_blob_units bu USING(unit_id) JOIN git_occurrences o USING(blob_id) WHERE CAST(o.path AS TEXT)=?1 AND u.unit_id=bu.unit_id ORDER BY u.byte_start")
            .unwrap()
            .query_map([path], |row| Ok((row.get::<_, usize>(0)?, row.get::<_, usize>(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(stored_ranges.len(), units.len(), "{path}");
        for (unit, (stored_start, stored_end)) in units.iter().zip(stored_ranges) {
            let text = unit["text"].as_str().unwrap();
            assert_eq!(&source[stored_start..stored_end], text, "{path}");
            assert_eq!(unit["text"], text);
        }
        for (unit, (start, end)) in units.iter().zip(expected_ranges) {
            assert!(unit["text"].as_str().unwrap().len() <= 4096);
            assert_eq!(unit["line_start"], start);
            assert_eq!(unit["line_end"], end);
        }
    }
    let windows = search_json(root, &["first\r\nsecond"]);
    assert_eq!(windows[0]["text"], "first\r\nsecond");
    assert_eq!(windows[0]["line_start"], 2);
    assert_eq!(windows[0]["line_end"], 3);
    assert_eq!(search_json(root, &["third"])[0]["line_start"], 5);
}

#[test]
fn rust_top_level_constructs_are_embedded_individually_with_source_references() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repository();
    let root = repo.path();
    let source = "fn alpha() {}\n\nstruct Beta;\n";
    fs::write(root.join("constructs.rs"), source).unwrap();
    git(root, &["add", "constructs.rs"]);
    git(root, &["commit", "-qm", "top-level constructs"]);
    let commit = git_text(root, &["rev-parse", "HEAD"]);
    let fake = root.join("construct-embedder");
    let inputs = root.join("embedding-inputs");
    fs::write(
        &fake,
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in 'fn alpha() {}'|'struct Beta') printf '%s\\n' \"$input\" >> \"$REPODB_TEST_INPUTS\" ;; esac\nprintf '[1,0]'\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run(root, &["init"]).0);
    assert!(
        run_with_env(
            root,
            &["reembed", "--command", fake.to_str().unwrap()],
            &[("REPODB_TEST_INPUTS", inputs.to_str().unwrap())],
        )
        .0
    );

    assert_eq!(
        fs::read_to_string(inputs).unwrap(),
        "fn alpha() {}\nstruct Beta\n"
    );
    for (query, text, start, end) in [
        ("fn alpha", "fn alpha() {}", 1, 1),
        ("struct Beta", "struct Beta", 3, 3),
    ] {
        let results = search_json(root, &[query]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["text"], text);
        assert_eq!(results[0]["commit_id"], commit);
        assert_eq!(results[0]["path"], "constructs.rs");
        assert_eq!(results[0]["line_start"], start);
        assert_eq!(results[0]["line_end"], end);
    }
}

#[test]
fn rust_top_level_syntax_preserves_nested_items_attributes_comments_and_source_gaps() {
    let repo = repository();
    let root = repo.path();
    let source = concat!(
        "//! crate documentation\n",
        "#![allow(dead_code)]\n",
        "// leading comment\n",
        "use std::fmt;\n",
        "\n",
        "/// α function docs\n",
        "#[inline]\n",
        "fn alpha() {\n",
        "    let braces = \"} { 🦀\";\n",
        "\n",
        "    if true { /* nested comment */ println!(\"{braces}\"); }\n",
        "}\n",
        "// gap marker\n",
        "fn beta() {\n",
        "    fn nested() {}\n",
        "    nested();\n",
        "}\n",
        "// tail marker\n",
        "fn café() {} fn same_line() {} // same line tail\n",
    );
    fs::write(root.join("syntax.rs"), source).unwrap();
    git(root, &["add", "syntax.rs"]);
    git(root, &["commit", "-qm", "syntax"]);
    assert!(run(root, &["init"]).0);
    let alpha = search_json(root, &["fn alpha"]);
    assert_eq!(alpha.len(), 1);
    assert_eq!(
        alpha[0]["text"],
        "#[inline]\nfn alpha() {\n    let braces = \"} { 🦀\";\n\n    if true { /* nested comment */ println!(\"{braces}\"); }\n}"
    );
    assert_eq!(alpha[0]["line_start"], 7);
    assert_eq!(alpha[0]["line_end"], 12);
    let beta = search_json(root, &["fn nested"]);
    assert_eq!(
        beta[0]["text"],
        "fn beta() {\n    fn nested() {}\n    nested();\n}"
    );
    assert_eq!(beta[0]["line_start"], 14);
    assert_eq!(beta[0]["line_end"], 17);
    for (query, start, end) in [
        ("crate documentation", 1, 3),
        ("function docs", 4, 6),
        ("gap marker", 12, 13),
        ("tail marker", 17, 18),
        ("use std::fmt", 4, 4),
    ] {
        let results = search_json(root, &[query]);
        assert_eq!(results.len(), 1, "{query} must not duplicate gaps");
        assert_eq!(results[0]["line_start"], start);
        assert_eq!(results[0]["line_end"], end);
    }
    for query in ["fn café() {}", "fn same_line() {}"] {
        let results = search_json(root, &[query]);
        assert_eq!(
            results[0]["text"], query,
            "slice original spans, not entire lines"
        );
        assert_eq!(results[0]["line_start"], 19);
        assert_eq!(results[0]["line_end"], 19);
    }
    let tail = search_json(root, &["same line tail"]);
    assert_eq!(tail[0]["line_start"], 19);
    assert_eq!(tail[0]["line_end"], 19);
}

#[test]
fn malformed_rust_and_unsupported_source_keep_comments_imports_and_unparsed_text() {
    let repo = repository();
    let root = repo.path();
    for (path, source) in [
        (
            "broken.rs",
            "// malformed header\nuse std::fmt;\n\nfn unfinished( {\n    unparsed_marker\n",
        ),
        (
            "fallback.py",
            "# unsupported header\nimport os\n\ndef unfinished(:\n    unparsed_marker\n",
        ),
    ] {
        fs::write(root.join(path), source).unwrap();
    }
    fs::write(
        root.join("large.js"),
        format!("// {}\n{}", "x".repeat(5000), "y".repeat(5000)),
    )
    .unwrap();
    fs::write(root.join("empty.rs"), " \n\t\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "fallbacks"]);
    assert!(run(root, &["init"]).0);
    let results = search_json(root, &["unparsed_marker"]);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["text"], "fn unfinished( {\n    unparsed_marker");
    assert_eq!(results[1]["text"], "def unfinished(:\n    unparsed_marker");
    for result in results {
        assert_eq!(result["line_start"], 4);
        assert_eq!(result["line_end"], 5);
    }
    for (query, start, end) in [
        ("malformed header", 1, 2),
        ("use std::fmt;", 1, 2),
        ("unsupported header", 1, 1),
        ("import os", 2, 2),
    ] {
        let results = search_json(root, &[query]);
        assert_eq!(results.len(), 1, "{query}");
        assert_eq!(results[0]["line_start"], start, "{query}");
        assert_eq!(results[0]["line_end"], end, "{query}");
    }
    let all = search_json(root, &[""]);
    assert!(!all.iter().any(|r| r["path"] == "empty.rs"));
    let large: Vec<_> = all.iter().filter(|r| r["path"] == "large.js").collect();
    assert_eq!(large.len(), 4);
    assert!(
        large
            .iter()
            .all(|r| r["text"].as_str().unwrap().len() <= 4096)
    );
    assert_eq!(
        large
            .iter()
            .map(|r| r["text"].as_str().unwrap())
            .collect::<String>(),
        fs::read_to_string(root.join("large.js")).unwrap()
    );
}

#[test]
fn rust_spans_handle_bom_shebang_and_bound_large_constructs_without_text_loss() {
    let repo = repository();
    let root = repo.path();
    let large = format!(
        "fn large() {{\n    let text = \"{}\";\n}}",
        "é".repeat(3000)
    );
    fs::write(root.join("large.rs"), &large).unwrap();
    fs::write(
        root.join("script.rs"),
        "\u{feff}#!/usr/bin/env rust-script\nfn one() {} fn two() {}\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "rust boundaries"]);
    assert!(run(root, &["init"]).0);
    for query in ["fn one() {}", "fn two() {}"] {
        let result = search_json(root, &[query]);
        assert_eq!(result[0]["text"], query);
        assert_eq!(result[0]["line_start"], 2);
        assert_eq!(result[0]["line_end"], 2);
    }
    assert_eq!(search_json(root, &["rust-script"])[0]["line_start"], 1);
    let all = search_json(root, &[""]);
    let units: Vec<_> = all.iter().filter(|r| r["path"] == "large.rs").collect();
    assert!(units.len() > 1);
    assert!(
        units
            .iter()
            .all(|r| r["text"].as_str().unwrap().len() <= 4096)
    );
    assert_eq!(
        units
            .iter()
            .map(|r| r["text"].as_str().unwrap())
            .collect::<String>(),
        large
    );
    assert_eq!(units.first().unwrap()["line_start"], 1);
    assert_eq!(units.last().unwrap()["line_end"], 3);
}

#[test]
fn occurrence_path_kind_is_selected_after_filters_before_units_top_n_and_combined_union() {
    let repo = repository();
    let root = repo.path();
    let source = "fn variant_alpha() {}\nfn variant_beta() {}\n";
    fs::write(root.join("a.md"), source).unwrap();
    git(root, &["add", "a.md"]);
    let document = commit_at(root, "document occurrence", "2001-01-01T00:00:00Z");
    git(root, &["mv", "a.md", "z.rs"]);
    let rust = commit_at(root, "rust occurrence", "2002-01-01T00:00:00Z");
    fs::write(root.join("a.md"), source).unwrap();
    git(root, &["add", "a.md"]);
    let tied = commit_at(root, "same commit path tie", "2003-01-01T00:00:00Z");
    let blob = git_text(root, &["rev-parse", "HEAD:a.md"]);
    assert!(run(root, &["init"]).0);
    let tied_results = search_json(root, &["fn variant", "--commit", &tied]);
    assert_eq!(tied_results.len(), 1, "{tied_results:?}");
    assert!(tied_results.iter().all(|result| result["path"] == "a.md"));
    let markdown = &tied_results[0];
    assert_eq!(
        markdown["text"],
        "fn variant_alpha() {}\nfn variant_beta() {}"
    );
    let fake = fake_embedder(
        root,
        "case \"$input\" in 'fn variant'|'fn variant_alpha() {}') printf '[1,0]' ;; 'fn variant_beta() {}') printf '[0.8,0.2]' ;; *) printf '[0,1]' ;; esac",
    );
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);
    for mode in ["exact", "--semantic", "--combined"] {
        for (scope, commit, path, count) in [
            (vec!["--commit", &document], &document, "a.md", 1),
            (
                vec!["--commit", &rust],
                &rust,
                "z.rs",
                if mode == "--semantic" { 1 } else { 2 },
            ),
        ] {
            let mut args = vec!["fn variant", "--semantic-limit", "1"];
            if mode != "exact" {
                args.push(mode);
            }
            args.extend(scope);
            let results = search_json(root, &args);
            assert_eq!(results.len(), count, "{args:?}");
            for result in &results {
                assert_eq!(result["path"], path);
                assert_eq!(result["commit_id"], *commit);
                assert_eq!(result["blob_id"], blob);
            }
            assert_eq!(
                results[0]["text"],
                if path == "a.md" {
                    "fn variant_alpha() {}\nfn variant_beta() {}"
                } else {
                    "fn variant_alpha() {}"
                }
            );
            assert_eq!(results[0]["line_end"], if path == "a.md" { 2 } else { 1 });
            if mode != "exact" {
                assert!(
                    (results[0]["distance"].as_f64().unwrap()
                        - if path == "a.md" { 1.0 } else { 0.0 })
                    .abs()
                        < 0.0001
                );
            }
            if count == 2 {
                assert_eq!(results[1]["text"], "fn variant_beta() {}");
                assert_eq!(results[1]["line_start"], 2);
                assert_eq!(results[1]["line_end"], 2);
            }
        }
    }
}

#[test]
fn v2_cli_snapshot_keeps_line_units_until_successful_atomic_segmentation_refresh() {
    use std::os::unix::fs::PermissionsExt;
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q", "--object-format=sha1"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    fs::write(root.join("v2.txt"), "v2 first\nv2 marker\nv2 last\n").unwrap();
    git(root, &["add", "v2.txt"]);
    let commit = commit_at(root, "v2", "2000-01-01T00:00:00Z");
    assert_eq!(commit, "0c729620cba6ed31c8f6ef1384e8bb34631eab98");
    let blob = git_text(root, &["rev-parse", "HEAD:v2.txt"]);
    fs::write(
        root.join(".git/repodb.sqlite"),
        include_bytes!("fixtures/v2.sqlite"),
    )
    .unwrap();
    fs::write(
        root.join("v2-embedder"),
        r#"#!/bin/sh
input=$(cat)
[ "$REPODB_EMBEDDING_MODEL" = v2-model ] || exit 2
case "$input" in
  'v2 first
v2 marker
v2 last')
    [ ! -f fail-refresh ] || exit 1
    if [ -f wrong-dimension ]; then printf '[1,0,0]'; else printf '[0,1]'; fi ;;
  *) printf '[1,0]' ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(root.join("v2-embedder"), fs::Permissions::from_mode(0o755)).unwrap();
    let exact = search_json(root, &["v2 marker"]);
    let semantic = search_json(root, &["v2 marker", "--semantic"]);
    let combined = search_json(root, &["v2 marker", "--combined"]);
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0]["text"], "v2 marker");
    assert_eq!(exact[0]["line_start"], 2);
    assert_eq!(exact[0]["line_end"], 2);
    assert_eq!(exact[0]["blob_id"], blob);
    assert_eq!(semantic.len(), 3);
    assert!(semantic.iter().all(|r| r["distance"] == 0.0));
    for sentinel in ["fail-refresh", "wrong-dimension"] {
        fs::write(root.join(sentinel), "").unwrap();
        assert!(!run(root, &["refresh"]).0, "{sentinel}");
        fs::remove_file(root.join(sentinel)).unwrap();
        assert_eq!(search_json(root, &["v2 marker"]), exact);
        assert_eq!(search_json(root, &["v2 marker", "--semantic"]), semantic);
        assert_eq!(search_json(root, &["v2 marker", "--combined"]), combined);
    }
    assert!(run(root, &["refresh"]).0);
    for mode in ["exact", "--semantic", "--combined"] {
        let mut args = vec!["v2 marker"];
        if mode != "exact" {
            args.push(mode);
        }
        let results = search_json(root, &args);
        let file_result = results
            .iter()
            .find(|result| result["path"] == "v2.txt")
            .expect("file result remains searchable after migration");
        assert_eq!(file_result["text"], "v2 first\nv2 marker\nv2 last");
        assert_eq!(file_result["line_start"], 1);
        assert_eq!(file_result["line_end"], 3);
        assert_eq!(file_result["commit_id"], commit);
        assert_eq!(file_result["blob_id"], blob);
        if mode != "exact" {
            assert_eq!(file_result["distance"], 0.0);
        }
    }
}

#[test]
fn local_text_and_imported_git_dimension_mismatch_when_model_dimension_is_unknown() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    assert!(run(root, &["init"]).0);

    let local_text = root.join("local.txt");
    fs::write(&local_text, "local source dimension marker\n").unwrap();
    let local_locator = local_text.to_string_lossy().into_owned();
    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(
        external.path().join("source.txt"),
        "imported git dimension marker\n",
    )
    .unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "source"]);
    let git_locator = external.path().to_string_lossy().into_owned();
    assert!(run(root, &["import", &local_locator]).0);
    assert!(run(root, &["import", &git_locator]).0);

    let embedder = fake_embedder(
        root,
        "printf '%s\\n' \"$input\" >> \"$EMBEDDER_CALL_LOG\"\ncase \"$input\" in\n  *'local source dimension marker'*) printf '[1,0]' ;;\n  *) printf '[1,0,0]' ;;\nesac",
    );
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);
    let call_log = root.join("embedder-calls.log");
    assert!(
        !run_with_env(
            root,
            &["refresh"],
            &[("EMBEDDER_CALL_LOG", call_log.to_str().unwrap())],
        )
        .0
    );
    let calls = fs::read_to_string(call_log).unwrap();
    assert!(calls.contains("local source dimension marker"), "{calls}");
    assert!(calls.contains("imported git dimension marker"), "{calls}");
    assert!(search_json(root, &["local source dimension marker"]).is_empty());
    assert!(search_json(root, &["imported git dimension marker"]).is_empty());
}

#[test]
fn local_text_and_pdf_dimension_mismatch_when_model_dimension_is_unknown() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    assert!(run(root, &["init"]).0);

    let local_text = root.join("local.txt");
    fs::write(&local_text, "local source dimension marker\n").unwrap();
    let local_locator = local_text.to_string_lossy().into_owned();
    let pdf = root.join("source.pdf");
    fs::write(&pdf, b"%PDF-test").unwrap();
    let pdf_locator = pdf.to_string_lossy().into_owned();
    assert!(run(root, &["import", &local_locator]).0);
    assert!(run(root, &["import", &pdf_locator]).0);

    let embedder = fake_embedder(
        root,
        "printf '%s\\n' \"$input\" >> \"$EMBEDDER_CALL_LOG\"\ncase \"$input\" in\n  *'local source dimension marker'*) printf '[1,0]' ;;\n  *) printf '[1,0,0]' ;;\nesac",
    );
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let converter = bin.join("pdftotext");
    fs::write(
        &converter,
        "#!/bin/sh\nprintf invoked > \"$PDF_CONVERTER_MARKER\"\ncat >/dev/null\nprintf 'pdf source dimension marker'\n",
    )
    .unwrap();
    fs::set_permissions(&converter, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let converter_marker = root.join("pdf-converter-invoked");
    let call_log = root.join("embedder-calls.log");
    assert!(
        !run_with_env(
            root,
            &["refresh"],
            &[
                ("PATH", &path),
                ("PDF_CONVERTER_MARKER", converter_marker.to_str().unwrap()),
                ("EMBEDDER_CALL_LOG", call_log.to_str().unwrap()),
            ],
        )
        .0
    );
    assert!(converter_marker.is_file(), "fake pdftotext was not invoked");
    let calls = fs::read_to_string(call_log).unwrap();
    assert!(calls.contains("local source dimension marker"), "{calls}");
    assert!(calls.contains("pdf source dimension marker"), "{calls}");
    assert!(search_json(root, &["local source dimension marker"]).is_empty());
    assert!(search_json(root, &["pdf source dimension marker"]).is_empty());
}

#[test]
fn imported_git_units_reject_mixed_embedding_dimensions_atomically() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    assert!(run(repo.path(), &["init"]).0);

    let external = tempfile::tempdir().unwrap();
    git(external.path(), &["init", "-q"]);
    git(
        external.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(external.path(), &["config", "user.name", "Test"]);
    fs::write(
        external.path().join("lib.rs"),
        "fn short_unit() { println!(\"short marker\"); }\n\nfn long_unit() { println!(\"long marker\"); }\n",
    )
    .unwrap();
    git(external.path(), &["add", "."]);
    git(external.path(), &["commit", "-qm", "source"]);

    let embedder = repo.path().join("fake-embedder");
    fs::write(
        &embedder,
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *short_unit*) printf '[1,0]' ;; *) printf '[1,0,0]' ;; esac\n",
    )
    .unwrap();
    fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        run(
            repo.path(),
            &["reembed", "--command", embedder.to_str().unwrap()]
        )
        .0
    );

    let locator = external.path().to_string_lossy().into_owned();
    assert!(run(repo.path(), &["import", &locator]).0);
    assert!(!run(repo.path(), &["refresh"]).0);
    assert!(search_json(repo.path(), &["short marker"]).is_empty());
    assert!(search_json(repo.path(), &["long marker"]).is_empty());
}

#[path = "semgrep_corpus/corpus_tests.rs"]
mod semgrep_corpus;

fn single_file_repository() -> tempfile::TempDir {
    let repo = repository();
    let root = repo.path();
    fs::remove_file(root.join("src/main.rs")).unwrap();
    fs::remove_dir(root.join("src")).unwrap();
    fs::remove_file(root.join("ignored.bin")).unwrap();
    fs::write(root.join("notes.txt"), "Original semantic passage\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "single semantic file"]);
    repo
}

#[test]
fn worktree_semantic_search_discloses_live_text_and_indexed_head_score() {
    let repo = single_file_repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let indexed_head = git_text(root, &["rev-parse", "HEAD"]);
    fs::write(
        root.join("notes.txt"),
        "// staged prefix\nOriginal semantic passage\n",
    )
    .unwrap();
    git(root, &["add", "notes.txt"]);
    fs::write(
        root.join("notes.txt"),
        "// staged prefix\nLive semantic passage\n",
    )
    .unwrap();
    fs::write(
        root.join("staged-new.txt"),
        "staged addition without vectors\n",
    )
    .unwrap();
    git(root, &["add", "staged-new.txt"]);
    fs::write(
        root.join("untracked-new.txt"),
        "untracked addition without vectors\n",
    )
    .unwrap();

    let (ok, output) = run(
        root,
        &[
            "search",
            "--json",
            "--worktree",
            "--semantic",
            "--semantic-limit",
            "1",
            "--type=cmcommits",
            "query",
        ],
    );
    assert!(ok, "{output}");
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    let result = &report["results"][0];
    assert_eq!(result["text"], "Live semantic passage");
    assert_eq!(result["line_start"], 2);
    assert_eq!(result["commit_id"], indexed_head);
    assert_eq!(
        result["semantic_worktree"]["head_text"],
        "Original semantic passage"
    );
    assert_eq!(result["semantic_worktree"]["head_line_start"], 1);
    assert_eq!(result["semantic_worktree"]["staged_line_start"], 2);
    assert_eq!(result["semantic_worktree"]["live_line_start"], 2);
    assert_eq!(result["semantic_worktree"]["mapping_status"], "mapped");
    assert_eq!(
        result["semantic_worktree"]["score_provenance"],
        "similarity_to_indexed_head_text"
    );
    assert_eq!(result["semantic_worktree"]["changed_live_text"], true);
    assert!(
        report["worktree"]["coverage"]["unembedded_addition_files"]
            .as_u64()
            .unwrap()
            >= 2
    );

    let (ok, human) = run(
        root,
        &[
            "search",
            "--worktree",
            "--semantic",
            "--semantic-limit",
            "1",
            "--type=cmcommits",
            "query",
        ],
    );
    assert!(ok, "{human}");
    assert!(human.contains("similarity_to_indexed_head_text"), "{human}");
    assert!(human.contains("mapping=mapped"), "{human}");
    assert!(human.contains("Original semantic passage"), "{human}");

    let (ok, combined) = run(
        root,
        &[
            "search",
            "--json",
            "--worktree",
            "--combined",
            "--semantic-limit",
            "1",
            "--type=cmcommits",
            "Live semantic passage",
        ],
    );
    assert!(ok, "{combined}");
    let combined: serde_json::Value = serde_json::from_str(&combined).unwrap();
    assert!(combined["worktree"]["semantic_mapping"].is_object());
    assert!(
        combined["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|result| { result["semantic_worktree"]["mapping_status"] == "mapped" })
    );
    assert!(
        combined["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|result| { result["worktree"]["origin"] == "live_worktree" })
    );
}

#[test]
fn worktree_semantic_search_keeps_head_reference_for_deleted_or_ambiguous_ranges() {
    let deleted_repo = single_file_repository();
    let deleted_root = deleted_repo.path();
    assert!(run(deleted_root, &["init"]).0);
    fs::remove_file(deleted_root.join("notes.txt")).unwrap();
    let (ok, output) = run(
        deleted_root,
        &[
            "search",
            "--json",
            "--worktree",
            "--semantic",
            "--semantic-limit",
            "1",
            "--type=cmcommits",
            "query",
        ],
    );
    assert!(ok, "{output}");
    let deleted: serde_json::Value = serde_json::from_str(&output).unwrap();
    let deleted_result = &deleted["results"][0];
    assert_eq!(
        deleted_result["semantic_worktree"]["mapping_status"],
        "deleted"
    );
    assert_eq!(
        deleted_result["semantic_worktree"]["head_text"],
        "Original semantic passage"
    );
    assert!(deleted_result["semantic_worktree"]["live_line_start"].is_null());

    let ambiguous_repo = single_file_repository();
    let ambiguous_root = ambiguous_repo.path();
    assert!(run(ambiguous_root, &["init"]).0);
    fs::write(
        ambiguous_root.join("notes.txt"),
        "First replacement line\nSecond replacement line\n",
    )
    .unwrap();
    let (ok, output) = run(
        ambiguous_root,
        &[
            "search",
            "--json",
            "--worktree",
            "--semantic",
            "--semantic-limit",
            "1",
            "--type=cmcommits",
            "query",
        ],
    );
    assert!(ok, "{output}");
    let ambiguous: serde_json::Value = serde_json::from_str(&output).unwrap();
    let ambiguous_result = &ambiguous["results"][0];
    assert_eq!(
        ambiguous_result["semantic_worktree"]["mapping_status"],
        "ambiguous"
    );
    assert_eq!(ambiguous_result["text"], "Original semantic passage");
    assert!(ambiguous_result["semantic_worktree"]["live_line_start"].is_null());
}

#[test]
fn worktree_semantic_search_rejects_a_stale_active_head() {
    let repo = single_file_repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    fs::write(root.join("new.txt"), "new active commit\n").unwrap();
    git(root, &["add", "new.txt"]);
    git(root, &["commit", "-qm", "move active head"]);

    let (ok, output) = run(
        root,
        &["search", "--json", "--worktree", "--semantic", "query"],
    );
    assert!(!ok, "stale semantic worktree search must fail: {output}");
    let error: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(error["error"]["code"], "stale_or_unindexed_head");
    assert_eq!(error["error"]["refresh_guidance"], "repodb refresh");
}

#[test]
fn worktree_search_returns_live_provenance_without_refreshing_the_index() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let indexed_head = git_text(root, &["rev-parse", "HEAD"]);

    fs::write(root.join("notes.txt"), "staged worktree needle\n").unwrap();
    git(root, &["add", "notes.txt"]);
    fs::write(root.join("notes.txt"), "live worktree needle\n").unwrap();
    let (ok, output) = run(
        root,
        &["search", "--json", "--worktree", "needle", "--type=content"],
    );
    assert!(ok, "{output}");
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(report["results"].as_array().unwrap().len(), 1);
    assert_eq!(report["results"][0]["text"], "live worktree needle");
    assert_eq!(
        report["results"][0]["worktree"]["indexed_head"],
        indexed_head
    );
    assert_eq!(
        report["results"][0]["worktree"]["provenance"],
        serde_json::json!(["staged", "live"])
    );
    assert_eq!(report["worktree"]["indexed_head"], indexed_head);
    assert!(
        report["worktree"]["coverage"]["untracked_files"]
            .as_u64()
            .is_some()
    );

    let (ok, output) = run(root, &["search", "--json", "needle", "--type=content"]);
    assert!(ok, "{output}");
    let results: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert!(results.as_array().unwrap().is_empty(), "{results}");
    let (ok, status) = run(root, &["status", "--json"]);
    assert!(ok, "{status}");
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["indexed_head"], indexed_head);
    assert_eq!(status["live_head"], indexed_head);

    let (ok, human) = run(root, &["search", "--worktree", "needle", "--type=content"]);
    assert!(ok, "{human}");
    assert!(human.contains("provenance=staged,live"), "{human}");
    assert!(human.contains("Worktree view: indexed HEAD"), "{human}");
}

#[test]
fn worktree_search_does_not_return_deleted_head_matches() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    fs::remove_file(root.join("notes.txt")).unwrap();

    let (ok, output) = run(
        root,
        &["search", "--json", "--worktree", "TARGET", "--type=content"],
    );
    assert!(ok, "{output}");
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert!(
        report["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| { result["component"] != "content" || result["path"] != "notes.txt" }),
        "deleted HEAD content must not survive: {report}"
    );
    assert!(
        report["worktree"]["coverage"]["removed_files"]
            .as_u64()
            .unwrap()
            >= 1
    );
}

#[test]
fn worktree_search_excludes_ignored_files_and_includes_git_visible_untracked_text() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(root.join("new.rs"), "visible worktree marker\n").unwrap();
    fs::write(root.join("ignored.rs"), "hidden worktree marker\n").unwrap();

    let (ok, output) = run(
        root,
        &[
            "search",
            "--json",
            "--worktree",
            "worktree marker",
            "--type=content",
        ],
    );
    assert!(ok, "{output}");
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(report["results"].as_array().unwrap().len(), 1);
    assert_eq!(report["results"][0]["path"], "new.rs");
    assert!(
        report["results"][0]["worktree"]["untracked"]
            .as_bool()
            .unwrap()
    );
    assert!(
        report["worktree"]["coverage"]["ignored_files_excluded"]
            .as_u64()
            .unwrap()
            >= 1
    );
}

#[test]
fn worktree_search_rejects_stale_and_alternate_start_heads_without_refreshing() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let indexed_head = git_text(root, &["rev-parse", "HEAD"]);

    fs::write(root.join("notes.txt"), "stale head marker\n").unwrap();
    git(root, &["add", "notes.txt"]);
    git(root, &["commit", "-qm", "new active head"]);
    let (ok, output) = run(
        root,
        &["search", "--json", "--worktree", "marker", "--type=content"],
    );
    assert!(!ok, "stale indexed HEAD must fail: {output}");
    let error: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(error["error"]["code"], "stale_or_unindexed_head");
    assert_eq!(error["error"]["refresh_guidance"], "repodb refresh");
    let (ok, status) = run(root, &["status", "--json"]);
    assert!(ok, "{status}");
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["indexed_head"], indexed_head);
    assert_ne!(status["live_head"], indexed_head);

    assert!(run(root, &["refresh"]).0);
    let (ok, output) = run(
        root,
        &[
            "search",
            "--json",
            "--worktree",
            "--commit",
            &indexed_head,
            "marker",
            "--type=content",
        ],
    );
    assert!(!ok, "alternate --commit must fail: {output}");
    let error: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(error["error"]["code"], "alternate_head");
}
