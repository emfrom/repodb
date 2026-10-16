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
    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
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

    let (ok, json) = run(root, &["search", "[literal]", "--json"]);
    assert!(ok);
    let results: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(results[0]["path"], "notes.txt");
    assert_eq!(results[0]["line_start"], 2);
    assert_eq!(results[0]["line_end"], 2);

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
    let db = rusqlite::Connection::open(root.join(".git/repodb.sqlite")).unwrap();
    let model: String = db
        .query_row(
            "SELECT value FROM embedding_config WHERE key='model'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(model, "old-model");
    let command: String = db
        .query_row(
            "SELECT value FROM embedding_config WHERE key='command'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(command, old.to_str().unwrap());
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
