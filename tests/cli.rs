use std::{fs, path::Path, process::Command};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
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

fn run_with_env(repo: &Path, args: &[&str], vars: &[(&str, &str)]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_repodb"));
    command.args(args).current_dir(repo);
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
        "pub fn local_alpha() {\n    println!(\"local alpha marker\");\n}"
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
    assert_eq!(local_beta[0]["text"], "pub struct LocalBeta;");
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
fn imported_files_refresh_with_references_and_both_retention_policies() {
    let repo = repository();
    let root = repo.path();
    assert!(run(root, &["init"]).0);
    let file = root.join("external.txt");
    fs::write(&file, "imported first marker\n").unwrap();
    let file_string = file.to_string_lossy().into_owned();
    assert!(run(root, &["import", &file_string]).0);
    assert!(run(root, &["refresh"]).0);
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
    let (success, _) = run(root, &["refresh"]);
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
    assert!(run(root, &["import", &locator]).0);
    assert!(run(root, &["refresh"]).0);
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
    let newest = search_json(root, &["second git marker"]);
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
    assert!(run(root, &["import", &remote]).0);
    assert!(run(root, &["refresh"]).0);
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
    let page_two = search_json(root, &["second page marker"]);
    assert_eq!(page_two[0]["path"], pdf_string);
    assert_eq!(page_two[0]["page"], 2);
    assert_eq!(page_two[0]["line_start"], 2);

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

    // Exceed the former 100,000-page ceiling and verify the final page reference.
    // A large cached array remains reusable without rerunning pdftotext.
    let many_pages = (0..100_001).map(|i| format!("page-{i}"));
    fs::write(&text, many_pages.collect::<Vec<_>>().join("\x0c")).unwrap();
    fs::write(&pdf, b"%PDF-test many pages").unwrap();
    assert!(run_with_env(root, &["refresh"], &env).0);
    let calls_after_conversion = fs::read_to_string(&calls).unwrap();
    assert!(run_with_env(root, &["refresh"], &env).0);
    assert_eq!(fs::read_to_string(&calls).unwrap(), calls_after_conversion);
    let last_page = search_json(root, &["page-100000"]);
    assert!(last_page.iter().any(|result| result["page"] == 100_001));
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
            &["refresh"],
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
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 1024];
        let _ = stream.read(&mut request);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\nConnection: close\r\n\r\nurl imported semantic!\n").unwrap();
    });
    let url = format!("http://{address}/source.txt");
    assert!(run(root, &["import", &url]).0);
    assert!(run(root, &["refresh"]).0);
    server.join().unwrap();
    let results = search_json(root, &["semantic", "--semantic"]);
    assert!(
        results
            .iter()
            .any(|x| x["path"] == url && x["line_start"] == 1),
        "{results:?}"
    );
}

#[test]
fn init_refresh_search_and_json_are_repository_local_and_exact() {
    let repo = repository();
    let root = repo.path();
    let (ok, _) = run(root, &["init"]);
    assert!(ok);
    assert!(root.join(".git/repodb.sqlite").is_file());

    let (ok, human) = run(root, &["search", "tArGeT"]);
    assert!(ok);
    assert!(human.contains("notes.txt"), "{human}");
    assert!(human.contains("notes.txt:1-3"), "{human}");
    assert!(!human.contains("ignored.bin"));

    let (ok, json) = run(root, &["search", "--json", "[literal]"]);
    assert!(ok);
    let results: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(results[0]["path"], "notes.txt");
    assert_eq!(results[0]["line_start"], 1);
    assert_eq!(results[0]["line_end"], 3);
    let (ok, options_first) = run(root, &["search", "--json", "TARGET"]);
    assert!(ok);
    let (ok, legacy) = run(root, &["search", "TARGET", "--json"]);
    assert!(ok);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&options_first).unwrap(),
        serde_json::from_str::<serde_json::Value>(&legacy).unwrap()
    );

    let (ok, output) = run(root, &["search", "[literal]"]);
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
    let (ok, output) = run(root, &["search", "updated phrase"]);
    assert!(ok && output.contains("notes.txt"), "{output}");
    let (ok, output) = run(root, &["search", "[literal]"]);
    assert!(ok && output.trim().is_empty(), "{output}");
}

#[test]
fn post_commit_hook_refreshes_and_missing_hook_guidance_is_explicit() {
    let repo = repository();
    let root = repo.path();
    let (ok, _) = run(root, &["init"]);
    assert!(ok);
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
    assert!(!results.as_array().unwrap().is_empty());
    assert_eq!(results[0]["path"], "notes.txt");
    assert_eq!(results[0]["line_start"], 1);
    assert!(results[0]["commit_id"].as_str().is_some());
    assert!(results[0]["distance"].as_f64().is_some());
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
    let (ok, output) = run_with_env(
        root,
        &["reembed", "--model", "other"],
        &[("HOME", home_string)],
    );
    assert!(!ok, "{output}");
    assert!(!home.path().join("called").exists());
    let (ok, output) = run_with_env(root, &["reembed"], &[("HOME", home_string)]);
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
  *) echo 'expected a complete indexed unit or query' >&2; exit 1 ;;
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
    let results: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(results.as_array().unwrap().len(), 2);
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
    serde_json::from_str(&output).unwrap()
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
        let mut args = vec!["TARGET", "--semantic-limit", "1"];
        if mode != "--exact" {
            args.push(mode);
        }
        let mut all = args.clone();
        all.push("--all-history");
        let history = search_json(root, &all);
        assert_eq!(
            history.len(),
            if mode == "--semantic" { 1 } else { 2 },
            "{mode}"
        );
        if mode == "--combined" {
            assert!(
                history
                    .iter()
                    .all(|r| r["path"] == "notes.txt" && r["line_start"] == 1)
            );
            assert_ne!(history[0]["blob_id"], history[1]["blob_id"]);
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
    let old = search_json(root, &["side-only", "--all-history"]);
    assert_eq!(old.len(), 1);
    assert_eq!(old[0]["commit_id"], side);
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

    let mut large_blob = b"oversized searchable marker\\n".to_vec();
    large_blob.extend(std::iter::repeat_n(b'x', 32 * 1024 * 1024 + 1));
    fs::write(root.join("oversized.txt"), large_blob).unwrap();
    git(root, &["add", "oversized.txt"]);
    git(root, &["commit", "-qm", "oversized blob"]);

    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .arg("refresh")
        .current_dir(root)
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
    assert_eq!(search_json(root, &["first"]).len(), 1);
    assert_eq!(search_json(root, &["question", "--semantic"]).len(), 1);
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
    assert_eq!(migrated.len(), 1);
    assert_eq!(migrated[0]["blob_id"], new_blob);
    let migrated_semantic = search_json(root, &["question", "--semantic", "--commit", &new]);
    assert_eq!(migrated_semantic.len(), 2);
    assert!(migrated_semantic.iter().all(|r| r["distance"] == 0.0));
    assert_eq!(search_json(root, &["legacy"])[0]["commit_id"], old);
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
    assert_eq!(results.len(), 2);
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
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in 'fn alpha() {}'|'struct Beta;') printf '%s\\n' \"$input\" >> \"$REPODB_TEST_INPUTS\" ;; esac\nprintf '[1,0]'\n",
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
        "fn alpha() {}\nstruct Beta;\n"
    );
    for (query, text, start, end) in [
        ("fn alpha", "fn alpha() {}", 1, 1),
        ("struct Beta", "struct Beta;", 3, 3),
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
        "/// α function docs\n#[inline]\nfn alpha() {\n    let braces = \"} { 🦀\";\n\n    if true { /* nested comment */ println!(\"{braces}\"); }\n}"
    );
    assert_eq!(alpha[0]["line_start"], 6);
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
        ("gap marker", 13, 13),
        ("tail marker", 18, 18),
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
    for query in [
        "malformed header\nuse std::fmt;",
        "unsupported header\nimport os",
    ] {
        let results = search_json(root, &[query]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["line_start"], 1);
        assert_eq!(results[0]["line_end"], 2);
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
    let fake = fake_embedder(
        root,
        "case \"$input\" in 'fn variant'|'fn variant_alpha() {}') printf '[1,0]' ;; 'fn variant_beta() {}') printf '[0.8,0.2]' ;; *) printf '[0,1]' ;; esac",
    );
    assert!(run(root, &["reembed", "--command", fake.to_str().unwrap()]).0);
    for mode in ["exact", "--semantic", "--combined"] {
        for (scope, commit, path, count) in [
            (vec!["--all-history"], &tied, "a.md", 1),
            (
                vec!["--all-history", "--exclude-commit", &tied],
                &rust,
                "z.rs",
                if mode == "--semantic" { 1 } else { 2 },
            ),
            (vec!["--commit", &document], &document, "a.md", 1),
            (
                vec!["--include-commit", &document, "--include-commit", &rust],
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
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["text"], "v2 first\nv2 marker\nv2 last");
        assert_eq!(results[0]["line_start"], 1);
        assert_eq!(results[0]["line_end"], 3);
        assert_eq!(results[0]["commit_id"], commit);
        assert_eq!(results[0]["blob_id"], blob);
        if mode != "exact" {
            assert_eq!(results[0]["distance"], 1.0);
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
