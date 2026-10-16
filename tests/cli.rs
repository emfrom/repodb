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
fn init_refresh_search_and_json_are_repository_local_and_exact() {
    let repo = repository();
    let root = repo.path();
    let (ok, _) = run(root, &["init"]);
    assert!(ok);
    assert!(root.join(".git/repodb.sqlite").is_file());

    let (ok, human) = run(root, &["search", "tArGeT"]);
    assert!(ok);
    assert!(human.contains("notes.txt"), "{human}");
    assert!(human.contains("2"), "{human}");
    assert!(!human.contains("ignored.bin"));

    let (ok, json) = run(root, &["search", "--json", "[literal]"]);
    assert!(ok);
    let results: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(results[0]["path"], "notes.txt");
    assert_eq!(results[0]["line_start"], 2);
    assert_eq!(results[0]["line_end"], 2);
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
        "indexed alpha marker\nindexed beta marker\n",
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
    fs::write(&fake, "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *'Exact [literal] TARGET'*) printf '[0.0, 1.0]' ;; *'fn main'*) printf '[0.9, 0.1]' ;; *) printf '[1.0, 0.0]' ;; esac\n").unwrap();
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
    assert_eq!(results[0]["text"], "First line");
    assert_eq!(results[1]["text"], "Exact [literal] TARGET");
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
    assert_eq!(old["line_start"], 2);
    assert_eq!(old["line_end"], 2);
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
                    .all(|r| r["path"] == "notes.txt" && r["line_start"] == 2)
            );
            assert_ne!(history[0]["blob_id"], history[1]["blob_id"]);
        }
        let mut selected = args.clone();
        selected.extend(["--commit", &old]);
        let result = search_json(root, &selected);
        assert_eq!(result.len(), 1, "{mode}");
        assert_eq!(result[0]["commit_id"], old);
        assert_eq!(result[0]["text"], "Exact [literal] TARGET");
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
    assert!(human.contains("notes.txt:2-2"), "{human}");
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
    assert_eq!(results.len(), 2);
    for (i, result) in results.iter().enumerate() {
        assert_eq!(result["commit_id"], *expected);
        assert_eq!(result["path"], path);
        assert_eq!(result["line_start"], i + 1);
        assert_eq!(result["line_end"], i + 1);
    }
    assert!(search_json(root, &["hidden", "--all-history"]).is_empty());
    // Moving away from detached HEAD does not prune its captured occurrence.
    git(root, &["checkout", "-q", &first]);
    assert_eq!(
        search_json(root, &["tie marker", "--commit", &detached]).len(),
        2
    );
    assert!(run(root, &["refresh"]).0);
    let results = search_json(root, &["tie marker", "--all-history"]);
    assert!(results.iter().all(|r| r["commit_id"] == first));
    assert!(!run(root, &["search", "tie marker", "--commit", &detached]).0);
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
        "if [ -f pause ] && [ \"$input\" = 'First line' ]; then touch prepared; while [ ! -f release ]; do sleep 0.01; done; fi\nprintf '[1,0]'",
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
    assert_eq!(results.len(), 4);
    assert!(results.iter().all(|r| r["distance"] == 0.0));
}
