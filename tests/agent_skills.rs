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
fn enabler_checks_repo_availability_and_defers_helper_loading() {
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
        "Pi",
        "globally with `pi install",
        "preview and import one explicitly selected local documentation file or directory",
        "import one explicitly selected URL page without crawling",
        "../repodb-help/SKILL.md",
        "../repodb-import/SKILL.md",
        "../repodb-search/SKILL.md",
        "../repodb-search-exact/SKILL.md",
        "../repodb-search-semantic/SKILL.md",
        "pi install git:github.com/emfrom/repodb",
        "make these helper skills discoverable by name and load them only when needed",
        "only the enabler is registered as a Pi skill",
        "Read helper skills only when they apply",
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
        "A supplied HTTP(S) URL imports exactly that page",
        "Do not crawl links",
        "JSON search results expose the imported version as `blob_id`",
        "A retrieval timestamp is not currently surfaced; timestamp support is tracked separately",
        "`--preview` is local-file/directory only",
        "one selected source per import call",
        "run one explicit `repodb refresh --imports`",
        "without refreshing repository history",
        "URL is the source locator",
        "Do not claim a retrieval timestamp",
        "URL preview and URL growth-warning are unavailable",
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
        "one explicit `repodb refresh --imports`",
        "source provenance",
        "Git repository",
        "`repodb import [--revision REV] REPOSITORY`",
        "default is `HEAD`",
        "resolved commit SHA",
        "Do not import full history",
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
        "the CLI's search provenance identifies the URL and page/line reference rather than surfacing that hash",
        "run one explicit `repodb refresh` to index the newly registered source or sources",
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
        "known literal identifiers, symbols, and phrases",
        "literal matching, not a regular-expression search",
        "repodb search 'QUERY'",
        "default to scope `all`",
        "source-code from this repository and imported Git repositories",
        "all-docs` includes recognized documents from any origin",
        "repository, commit/version, path, and line/page",
        "without fetching or reconstructing the content",
        "indexed Git blob is missing",
        "available blob ID",
        "does not return a partial match or a path/line reference",
        "weak or empty results",
        "semantic/combined search",
        "filesystem tools",
        "Open a known file directly",
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
        "default to scope `all`",
        "source-code from this repository and imported Git repositories",
        "all-docs` includes recognized documents from any origin",
        "semantic search is unavailable",
        "do not silently fall back to exact search",
        "weak or empty results",
        "rephrasing the query",
        "importing/indexing a relevant source",
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
        "exact search for known literal identifiers, symbols, and phrases",
        "semantic search for conceptual questions about code behavior and documentation",
        "combined search when you are unsure whether literal or conceptual matching will find it",
        "repodb search --combined 'QUERY'",
        "default to scope `all`",
        "source-code from this repository and imported Git repositories",
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
fn search_skills_document_scope_names_default_and_cli_passing_consistently() {
    for skill_name in [
        "repodb-search-exact",
        "repodb-search-semantic",
        "repodb-search",
    ] {
        let search_skill = skill(&format!(".agents/skills/{skill_name}/SKILL.md"));
        for required in [
            "`source`",
            "`repo-local-source`",
            "`repo-local-docs`",
            "`all-docs`",
            "`all`",
            "default to scope `all`",
            "`--scope`",
            "--scope source",
            "source-code from this repository and imported Git repositories",
            "this repository’s indexed Git history",
            "recognized documents from any origin",
        ] {
            assert!(
                search_skill.contains(required),
                "{skill_name} must document {required:?}"
            );
        }
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
