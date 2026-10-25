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

fn run(repo: &Path, args: &[&str], config: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_repodb"))
        .args(args)
        .current_dir(repo)
        .env("XDG_CONFIG_HOME", config)
        .output()
        .unwrap()
}

fn repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "test@example.com"]);
    git(repo.path(), &["config", "user.name", "Test"]);
    fs::write(
        repo.path().join("source.txt"),
        "indexed searchable content\n",
    )
    .unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "initial"]);
    repo
}

fn config(repo: &Path, embedding_command: &str) -> std::path::PathBuf {
    let config = repo.join("config");
    fs::create_dir_all(config.join("repodb")).unwrap();
    fs::write(
        config.join("repodb/config.toml"),
        format!("[tools]\nembedding_command = {:?}\n", embedding_command),
    )
    .unwrap();
    config
}

#[test]
fn status_json_reports_missing_index_without_creating_or_refreshing_it() {
    let repo = repo();
    let cfg = config(repo.path(), "/definitely/missing/embedder");
    let db = repo.path().join(".git/repodb.sqlite");
    let before = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo.path())
        .output()
        .unwrap()
        .stdout;
    let output = run(repo.path(), &["status", "--json"], &cfg);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["database"]["state"], "missing");
    assert_eq!(value["indexed_head"], serde_json::Value::Null);
    assert_eq!(value["live_head"], String::from_utf8_lossy(&before).trim());
    assert_eq!(value["capabilities"]["exact"]["ready"], false);
    assert_eq!(
        value["capabilities"]["semantic"]["reason"],
        "database is missing"
    );
    assert!(
        !db.exists(),
        "status must not initialize or refresh the index"
    );
}

#[test]
fn status_rejects_a_corrupted_database_without_replacing_it() {
    let repo = repo();
    let root = repo.path();
    let embedder = root.join("embedder");
    fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cfg = config(root, embedder.to_str().unwrap());
    assert!(run(root, &["init"], &cfg).status.success());
    let database = root.join(".git/repodb.sqlite");
    fs::write(&database, b"not a valid SQLite database").unwrap();

    let output = run(root, &["status", "--json"], &cfg);
    assert!(!output.status.success());
    assert_eq!(fs::read(&database).unwrap(), b"not a valid SQLite database");
}

#[test]
fn status_rejects_a_structurally_corrupted_database_without_replacing_it() {
    for missing_table in ["indexed_units", "blob_contents"] {
        let repo = repo();
        let root = repo.path();
        let embedder = root.join("embedder");
        fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cfg = config(root, embedder.to_str().unwrap());
        let initialized = run(root, &["init"], &cfg);
        assert!(
            initialized.status.success(),
            "{}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        let database = root.join(".git/repodb.sqlite");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(&format!(
                "PRAGMA foreign_keys=OFF; DROP TABLE {missing_table}"
            ))
            .unwrap();
        drop(connection);
        let corrupted_database = fs::read(&database).unwrap();

        let output = run(root, &["status", "--json"], &cfg);
        assert!(
            !output.status.success(),
            "status must reject a database missing {missing_table}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(fs::read(&database).unwrap(), corrupted_database);
    }
}

#[test]
fn status_human_output_explains_readiness_and_freshness() {
    let repo = repo();
    let cfg = config(repo.path(), "/definitely/missing/embedder");
    let output = run(repo.path(), &["status"], &cfg);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let human = String::from_utf8(output.stdout).unwrap();
    assert!(human.contains("Database: missing"), "{human}");
    assert!(human.contains("Exact search: not ready"), "{human}");
    assert!(human.contains("embedding command is missing"), "{human}");
}

#[test]
fn status_distinguishes_ready_stale_and_dirty_without_refreshing() {
    let repo = repo();
    let root = repo.path();
    let embedder = root.join("embedder");
    fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cfg = config(root, embedder.to_str().unwrap());
    assert!(run(root, &["init"], &cfg).status.success());
    let output = run(root, &["status", "--json"], &cfg);
    let ready: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ready["database"]["state"], "usable");
    assert_eq!(ready["indexed_head"], ready["live_head"]);
    assert_eq!(ready["capabilities"]["exact"]["ready"], true);
    assert_eq!(ready["capabilities"]["semantic"]["ready"], true);
    let missing_tool_cfg = config(root, "/definitely/missing/embedder");
    let missing_tool_output = run(root, &["status", "--json"], &missing_tool_cfg);
    let missing_tool: serde_json::Value =
        serde_json::from_slice(&missing_tool_output.stdout).unwrap();
    assert_eq!(missing_tool["capabilities"]["exact"]["ready"], true);
    assert_eq!(missing_tool["capabilities"]["semantic"]["ready"], false);
    assert_eq!(
        missing_tool["capabilities"]["semantic"]["reason"],
        "embedding command is missing"
    );

    fs::write(root.join("source.txt"), "new committed content\n").unwrap();
    git(root, &["commit", "-am", "advance"]);
    let stale_output = run(root, &["status", "--json"], &cfg);
    let stale: serde_json::Value = serde_json::from_slice(&stale_output.stdout).unwrap();
    assert_ne!(stale["indexed_head"], stale["live_head"]);
    assert_eq!(stale["database"]["state"], "stale");

    fs::write(root.join("untracked.txt"), "untracked\n").unwrap();
    let dirty_output = run(root, &["status", "--json"], &cfg);
    let dirty: serde_json::Value = serde_json::from_slice(&dirty_output.stdout).unwrap();
    assert_eq!(dirty["worktree"]["dirty"], true);
    assert!(
        !dirty["worktree"]["untracked_coverage"]
            .as_str()
            .unwrap()
            .is_empty()
    );
}
