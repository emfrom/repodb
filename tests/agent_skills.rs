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
        "repodb-help",
        "repodb-import",
        "repodb-search",
        "repodb-search-exact",
        "repodb-search-semantic",
    ] {
        let repo_local = skill(&format!(".agents/skills/{name}/SKILL.md"));
        let distributed = skill(&format!("skills/{name}/SKILL.md"));
        assert_eq!(distributed, repo_local, "skill {name} must stay synchronized");
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
        "repodb-help",
        "repodb-import",
        "repodb-search",
        "repodb-search-exact",
        "repodb-search-semantic",
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
fn exact_search_finds_unique_content_in_an_indexed_test_file() {
    let (repo, relative_path) = indexed_test_file();
    let root = repo.path();
    run(root, &["init"]);

    let marker = ["zlkfh", "ldfad;l"].concat();
    let results = search_results(root, &[&marker]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["path"], relative_path.to_string_lossy().as_ref());
}

#[test]
fn semantic_search_finds_test_file_content_by_meaning() {
    let (repo, relative_path) = indexed_test_file();
    let root = repo.path();
    run(root, &["init"]);

    // The test embedder is deterministic and maps query and fixture to the same
    // vector. The isolated database contains only this document, so the result
    // proves that semantic search returns the indexed test-file content.
    let results = search_results(root, &["philosophy", "--semantic"]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["path"], relative_path.to_string_lossy().as_ref());
}
