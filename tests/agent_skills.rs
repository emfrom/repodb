use std::fs;
use std::path::Path;

fn skill(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|error| panic!("could not read {path}: {error}"))
}

#[test]
fn enabler_is_project_local_and_defers_repo_db_skill_loading() {
    let enabler = skill(".agents/skills/repodb-enabler/SKILL.md");

    for required in [
        "name: repodb-enabler",
        "repodb_help",
        "repodb_import",
        "repodb_search",
        "repodb_search_exact",
        "repodb_search_semantic",
        "command -v repodb",
        "repodb init",
        "repodb refresh",
        "Do not run searches automatically",
        "repo-local",
        "Pi",
        "global-availability",
    ] {
        assert!(
            enabler.contains(required),
            "enabler must document {required:?}"
        );
    }
}

#[test]
fn helper_skill_guides_repo_db_questions_without_overstating_tool_coverage() {
    let helper = skill(".agents/skills/repodb-help/SKILL.md");

    for required in [
        "name: repodb-help",
        "repodb_help",
        "rg",
        "current filesystem",
        "AGENTS.md",
        "stale",
        "source reference",
        "Do not fabricate",
        "semantic search is unavailable",
    ] {
        assert!(
            helper.contains(required),
            "helper must document {required:?}"
        );
    }
}
