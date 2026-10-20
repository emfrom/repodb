use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

fn skill(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|error| panic!("could not read {path}: {error}"))
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run(repo: &Path, args: &[&str]) -> String {
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

    let output = Command::new(env!("CARGO_BIN_EXE_repodb"))
        .args(args)
        .current_dir(repo)
        .env("XDG_CONFIG_HOME", config_home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "repodb {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn indexed_test_file() -> (tempfile::TempDir, PathBuf) {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);

    // Keep these markers unique to the indexed fixture, rather than duplicating
    // skill-document wording that can itself appear in this test source.
    let exact_marker = ["zlkfh", "ldfad;l"].concat();
    let semantic_text = [
        "Sartre originated the idea that existence ",
        "precedes essence",
    ]
    .concat();
    let relative_path = PathBuf::from("tests/search-only-fixture.txt");
    let fixture = root.join(&relative_path);
    fs::create_dir_all(fixture.parent().unwrap()).unwrap();
    fs::write(&fixture, format!("{exact_marker}\n{semantic_text}\n")).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "add unique test search fixture"]);
    (repo, relative_path)
}

fn search_results(repo: &Path, args: &[&str]) -> Vec<serde_json::Value> {
    let mut command = vec!["search", "--json"];
    command.extend_from_slice(args);
    serde_json::from_str(&run(repo, &command)).expect("valid JSON search results")
}

#[test]
fn distributed_skills_match_the_repo_local_pi_skills() {
    for name in [
        "repodb-enabler",
        "repodb-errors",
        "repodb-help",
        "repodb-history",
        "repodb-import",
        "repodb-search",
        "repodb-search-exact",
        "repodb-search-semantic",
        "repodb-search-worktree",
        "repodb-status",
    ] {
        let repo_local = skill(&format!(".agents/skills/{name}/SKILL.md"));
        let distributed = skill(&format!("skills/{name}/SKILL.md"));
        assert_eq!(
            distributed, repo_local,
            "skill {name} must stay synchronized"
        );
    }
}

#[test]
fn pi_package_exposes_only_the_enabler_globally() {
    let manifest: serde_json::Value =
        serde_json::from_str(&skill("package.json")).expect("valid Pi package manifest");
    assert_eq!(
        manifest["pi"]["skills"],
        serde_json::json!(["./skills/repodb-enabler"]),
        "global package should register only the enabler skill"
    );

    for helper in [
        "repodb-errors",
        "repodb-help",
        "repodb-history",
        "repodb-import",
        "repodb-search",
        "repodb-search-exact",
        "repodb-search-semantic",
        "repodb-search-worktree",
        "repodb-status",
    ] {
        assert!(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("skills/repodb-enabler")
                .join(format!("../{helper}/SKILL.md"))
                .is_file(),
            "global enabler must be able to read sibling helper {helper}"
        );
    }
}

#[test]
fn focused_skills_match_final_cli_help_and_status_provenance() {
    let (repo, _) = indexed_test_file();
    let root = repo.path();
    let general_help = run(root, &["--help"]);
    let search_help = run(root, &["search", "--help"]);
    let status_help = run(root, &["status", "--help"]);
    let import_help = run(root, &["import", "--help"]);
    let refresh_help = run(root, &["refresh", "--help"]);

    let enabler = skill(".agents/skills/repodb-enabler/SKILL.md");
    for helper in [
        "repodb-status",
        "repodb-history",
        "repodb-search-worktree",
        "repodb-import",
        "repodb-errors",
    ] {
        assert!(enabler.contains(helper), "enabler must route to {helper}");
    }

    let status_skill = skill(".agents/skills/repodb-status/SKILL.md");
    assert!(status_skill.contains("status --json"));
    assert!(status_help.contains("readiness by capability"));
    run(root, &["init"]);
    let status_json: serde_json::Value =
        serde_json::from_str(&run(root, &["status", "--json"])).expect("status JSON");
    assert_eq!(status_json["database"]["state"], "usable");
    assert!(
        status_json["capabilities"]["exact"]["ready"]
            .as_bool()
            .is_some()
    );
    assert!(
        status_json["capabilities"]["semantic"]["ready"]
            .as_bool()
            .is_some()
    );

    let history_skill = skill(".agents/skills/repodb-history/SKILL.md");
    for option in ["--history=all", "--explore", "--cursor", "--byte-budget"] {
        assert!(search_help.contains(option), "search help lacks {option}");
        assert!(
            history_skill.contains(option),
            "history skill lacks {option}"
        );
    }
    assert!(search_help.contains("references only"));

    let worktree_skill = skill(".agents/skills/repodb-search-worktree/SKILL.md");
    assert!(search_help.contains("--worktree"));
    assert!(search_help.contains("no uncommitted embeddings"));
    assert!(search_help.contains("indexed HEAD text"));
    assert!(worktree_skill.contains("--worktree"));
    assert!(worktree_skill.contains("does not embed or persist uncommitted text"));
    assert!(worktree_skill.contains("indexed HEAD text"));
    let errors_skill = skill(".agents/skills/repodb-errors/SKILL.md");
    assert!(errors_skill.contains("result_limit"));
    assert!(search_help.contains("result_limit"));
    assert!(errors_skill.contains("blob ID"));

    let help_skill = skill(".agents/skills/repodb-help/SKILL.md");
    assert!(search_help.contains("--scope SCOPE"));
    assert!(help_skill.contains("`--scope`"));
    assert!(!help_skill.contains("not provided by the current interface"));

    let import_skill = skill(".agents/skills/repodb-import/SKILL.md");
    assert!(import_help.contains("one local file"));
    assert!(import_help.contains("For directory previews"));
    assert!(import_skill.contains("registration imports one file at a time"));
    assert!(import_skill.contains("never crawls links"));
    assert!(import_skill.contains("resolved commit SHA"));
    assert!(import_skill.contains("blob_id`"));
    assert!(import_skill.contains("local/URL documents"));
    assert!(import_help.contains("pins a Git import"));
    assert!(refresh_help.contains("local and URL document imports"));
    assert!(general_help.contains("status [--json]"));
}

#[test]
fn exact_search_finds_unique_content_in_an_indexed_test_file() {
    let (repo, relative_path) = indexed_test_file();
    let root = repo.path();
    run(root, &["init"]);

    let marker = ["zlkfh", "ldfad;l"].concat();
    let results = search_results(root, &[&marker]);
    let content = results
        .iter()
        .filter(|result| result["component"] == "content")
        .collect::<Vec<_>>();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["path"], relative_path.to_string_lossy().as_ref());
}

#[test]
fn semantic_search_finds_test_file_content_by_meaning() {
    let (repo, relative_path) = indexed_test_file();
    let root = repo.path();
    run(root, &["init"]);

    // The test embedder maps query, fixture, and commit message to the same
    // vector. Ensure the indexed test-file content remains among semantic hits.
    let results = search_results(root, &["philosophy", "--semantic"]);
    assert!(results.iter().any(|result| {
        result["component"] == "content"
            && result["path"] == relative_path.to_string_lossy().as_ref()
    }));
}
