use std::fs;
use std::path::Path;

fn skill(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|error| panic!("could not read {path}: {error}"))
}

#[test]
fn distributed_skills_match_the_repo_local_pi_skills() {
    for skill_name in [
        "repodb-enabler",
        "repodb-help",
        "repodb-import",
        "repodb-search",
        "repodb-search-exact",
        "repodb-search-semantic",
    ] {
        let local = skill(&format!(".agents/skills/{skill_name}/SKILL.md"));
        let distributed = skill(&format!("skills/{skill_name}/SKILL.md"));
        assert_eq!(
            distributed, local,
            "distributed {skill_name} must match the repo-local skill"
        );
    }
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
        "preview and import one explicitly selected local documentation file or directory",
        ".agents/skills/repodb-import/SKILL.md",
        ".agents/skills/repodb-search/SKILL.md",
        ".agents/skills/repodb-search-exact/SKILL.md",
        ".agents/skills/repodb-search-semantic/SKILL.md",
    ] {
        assert!(
            enabler.contains(required),
            "enabler must document {required:?}"
        );
    }
}

#[test]
fn import_skill_documents_preview_scope_and_policy_contract() {
    let import = skill(".agents/skills/repodb-import/SKILL.md");

    for required in [
        "name: repodb-import",
        "repodb import --preview PATH",
        "No imports have been performed until the preview has been shown",
        "every entry in `selected_supported_local_files`",
        "every entry in `unsupported_regular_files` with its reason",
        "`total_selected_source_bytes`",
        "`current_db_bytes`",
        "`estimated_net_db_growth_bytes`",
        "`warning_basis_bytes`",
        "`warn_large`",
        "50 MB (50,000,000 bytes)",
        "excluded symlinks and `.git` directories",
        "Markdown, plain-text, PDF, HTML, or Org formats",
        "plus extensionless files",
        "`.rs` files for backward compatibility",
        "preview-supported but out of skill scope",
        "do not import it",
        "These totals describe all preview-selected supported files",
        "Do not initialize",
        "Do not block solely on size",
        "one explicit `repodb refresh`",
        "source provenance",
    ] {
        assert!(
            import.contains(required),
            "import skill must document {required:?}"
        );
    }

    for obsolete in [
        "`source_bytes`",
        "`database_bytes`",
        "`estimated_growth_bytes`",
    ] {
        assert!(
            !import.contains(obsolete),
            "import skill must use the current preview field names, not {obsolete:?}"
        );
    }
}

#[test]
fn search_skills_document_modes_provenance_and_limitations() {
    let exact = skill(".agents/skills/repodb-search-exact/SKILL.md");
    for required in [
        "name: repodb-search-exact",
        "known identifiers, symbols, or literal phrases",
        "repodb search 'QUERY'",
        "all indexed sources by default",
        "repository, commit/version, path, and line/page",
        "source content is missing",
        "semantic/combined search",
        "does not refresh the index",
    ] {
        assert!(
            exact.contains(required),
            "exact search skill must document {required:?}"
        );
    }

    let semantic = skill(".agents/skills/repodb-search-semantic/SKILL.md");
    for required in [
        "name: repodb-search-semantic",
        "conceptual questions about code behavior and documentation",
        "standards, README content, and `AGENTS.md`",
        "repodb search --semantic 'QUERY'",
        "all indexed sources by default",
        "semantic search is unavailable",
        "do not silently fall back to exact search",
        "weak or empty results",
        "provenance actually returned",
        "without fetching or reconstructing it",
    ] {
        assert!(
            semantic.contains(required),
            "semantic search skill must document {required:?}"
        );
    }

    let combined = skill(".agents/skills/repodb-search/SKILL.md");
    for required in [
        "name: repodb-search",
        "combined search",
        "a literal phrase and a concept",
        "repodb search --combined 'QUERY'",
        "all indexed sources by default",
        "filesystem tools",
        "provenance",
        "source content is missing",
        "weak or empty",
        "does not refresh the index",
    ] {
        assert!(
            combined.contains(required),
            "combined search skill must document {required:?}"
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
