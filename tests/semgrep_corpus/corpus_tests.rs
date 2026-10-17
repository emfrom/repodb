use super::{git, git_text, run, run_with_env, search_json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

struct Fixture {
    language: &'static str,
    slot: &'static str,
    extension: &'static str,
    source: &'static str,
    parsers: &'static [&'static str],
}

impl Fixture {
    fn path(&self) -> String {
        format!(
            "src/{}/registry_{}.{}",
            self.language, self.slot, self.extension
        )
    }

    fn entity_v1(&self) -> String {
        if self.language == "ocaml" {
            return format!("repodb_ocaml_{}_entity_v1", self.slot);
        }
        format!(
            "Repodb{}{}EntityV1",
            title_first(self.language),
            title_first(self.slot)
        )
    }

    fn entity_v2(&self) -> String {
        if self.language == "ocaml" {
            return self.entity_v1().replace("entity_v1", "entity_v2");
        }
        self.entity_v1().replace("EntityV1", "EntityV2")
    }
}

fn title_first(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

macro_rules! define_corpus {
    ($(($test_alpha:ident, $test_beta:ident, $language:literal, $extension:literal, $parsers:expr)),+ $(,)?) => {
        fn fixtures() -> Vec<Fixture> {
            let mut fixtures = Vec::new();
            $(
                fixtures.push(Fixture {
                    language: $language,
                    slot: "alpha",
                    extension: $extension,
                    source: include_str!(concat!("sources/", $language, "/registry_alpha.", $extension)),
                    parsers: $parsers,
                });
                fixtures.push(Fixture {
                    language: $language,
                    slot: "beta",
                    extension: $extension,
                    source: include_str!(concat!("sources/", $language, "/registry_beta.", $extension)),
                    parsers: $parsers,
                });
            )+
            fixtures
        }

        fn fixture_for(language: &str, slot: &str) -> Fixture {
            fixtures()
                .into_iter()
                .find(|fixture| fixture.language == language && fixture.slot == slot)
                .unwrap()
        }

        $(
            #[test]
            fn $test_alpha() {
                verify_fixture(&fixture_for($language, "alpha"));
            }

            #[test]
            fn $test_beta() {
                verify_fixture(&fixture_for($language, "beta"));
            }
        )+
    };
}

fn empty_repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    dir
}

pub(super) fn semgrep_core_or_skip(context: &str) -> Option<PathBuf> {
    let core = actual_semgrep_core();
    if core.is_none() {
        if env::var_os("REPODB_REQUIRE_SEMGREP_CORE").is_some() {
            panic!(
                "{context} requires Semgrep core 1.181.0; set REPODB_SEMGREP_CORE or put semgrep on PATH"
            );
        }
        eprintln!("skipping {context}: Semgrep core 1.181.0 is unavailable");
    }
    core
}

fn actual_semgrep_core() -> Option<PathBuf> {
    static CORE: OnceLock<Option<PathBuf>> = OnceLock::new();
    CORE.get_or_init(discover_semgrep_core).clone()
}

fn discover_semgrep_core() -> Option<PathBuf> {
    if let Some(configured) = env::var_os("REPODB_SEMGREP_CORE").map(PathBuf::from) {
        return is_pinned_semgrep_core(&configured).then_some(configured);
    }
    let mut candidates = find_on_path("semgrep-core").into_iter().collect::<Vec<_>>();

    if let Some(semgrep) = find_on_path("semgrep")
        && let Ok(semgrep) = fs::canonicalize(semgrep)
        && let Some(install_root) = semgrep.parent().and_then(Path::parent)
        && let Ok(entries) = fs::read_dir(install_root.join("lib"))
    {
        for entry in entries.flatten() {
            candidates.push(entry.path().join("site-packages/semgrep/bin/semgrep-core"));
        }
    }

    candidates
        .into_iter()
        .find(|candidate| is_pinned_semgrep_core(candidate))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn is_pinned_semgrep_core(path: &Path) -> bool {
    let Ok(output) = Command::new(path).arg("-version").output() else {
        return false;
    };
    output.status.success()
        && format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '.')
        .any(|part| part == "1.181.0")
}

fn verify_corpus_parses(fixture: &Fixture, source_path: &Path, core: &Path) {
    let source = fs::read(source_path).unwrap();
    for parser in fixture.parsers {
        let output = Command::new(core)
            .args(["-l", parser, "-json", "-full_token_info", "-dump_ast"])
            .arg(source_path)
            .output()
            .unwrap_or_else(|error| panic!("could not run Semgrep core {core:?}: {error}"));
        assert!(
            output.status.success(),
            "Semgrep {parser} rejected {}: {}",
            fixture.path(),
            String::from_utf8_lossy(&output.stderr)
        );
        let ast: serde_json::Value =
            serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
                panic!(
                    "Semgrep {parser} returned invalid JSON for {}: {error}",
                    fixture.path()
                )
            });
        assert!(
            ast.get("Pr").is_some_and(|program| !program.is_null()),
            "Semgrep {parser} returned no program AST for {}",
            fixture.path()
        );
        let mut token_count = 0;
        assert_origin_tokens_match_source(&ast, &source, &mut token_count, fixture, parser);
        assert!(
            token_count > 0,
            "Semgrep {parser} produced no origin tokens for {}",
            fixture.path()
        );
    }
}

fn assert_origin_tokens_match_source(
    value: &serde_json::Value,
    source: &[u8],
    count: &mut usize,
    fixture: &Fixture,
    parser: &str,
) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(token) = object.get("OriginTok") {
                let text = token
                    .get("str")
                    .and_then(serde_json::Value::as_str)
                    .unwrap();
                let position = token
                    .get("bytepos")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap() as usize;
                assert_eq!(
                    source.get(position..position.saturating_add(text.len())),
                    Some(text.as_bytes()),
                    "Semgrep {parser} token offset mismatch in {} at byte {position}",
                    fixture.path()
                );
                *count += 1;
            }
            for child in object.values() {
                assert_origin_tokens_match_source(child, source, count, fixture, parser);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                assert_origin_tokens_match_source(child, source, count, fixture, parser);
            }
        }
        _ => {}
    }
}

fn prepare_empty_database(root: &Path) {
    assert!(run(root, &["init"]).0);
    let embedder = super::fake_embedder(root, "printf '[1,0]'");
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);
}

fn assert_marker_result(fixture: &Fixture, root: &Path, marker: &str, commit: &str) {
    let results = search_json(root, &[marker, "--all-history"]);
    let path = fixture.path();
    let source_line = fixture
        .source
        .lines()
        .position(|line| line.contains(&fixture.entity_v1()))
        .map(|line| line + 1)
        .unwrap_or(1);
    assert!(
        results.iter().any(|result| {
            result["commit_id"] == commit
                && result["path"] == path
                && result["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(marker))
                && result["line_start"].as_u64().unwrap_or(0) as usize <= source_line
                && result["line_end"].as_u64().unwrap_or(0) as usize >= source_line
        }),
        "{marker} should be searchable at {path} in commit {commit}; got {results:?}"
    );
}

fn verify_fixture(fixture: &Fixture) {
    assert!(fixture.source.lines().count() >= 200, "{}", fixture.path());
    if fixture.parsers.is_empty() {
        let repo = empty_repository();
        let root = repo.path();
        prepare_empty_database(root);
        let source_path = root.join(fixture.path());
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, fixture.source).unwrap();
        git(root, &["add", fixture.path().as_str()]);
        git(root, &["commit", "-qm", "add built-in Rust fixture"]);
        let commit = git_text(root, &["rev-parse", "HEAD"]);
        assert!(run(root, &["refresh"]).0);
        assert_marker_result(fixture, root, &fixture.entity_v1(), &commit);
        return;
    }

    let Some(core) = semgrep_core_or_skip(&fixture.path()) else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let source_path = root.path().join(fixture.path());
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, fixture.source).unwrap();
    verify_corpus_parses(fixture, &source_path, &core);
}

define_corpus![
    (
        corpus_bash_alpha_fixture,
        corpus_bash_beta_fixture,
        "bash",
        "sh",
        &["bash"]
    ),
    (
        corpus_c_alpha_fixture,
        corpus_c_beta_fixture,
        "c",
        "c",
        &["c"]
    ),
    (
        corpus_cpp_alpha_fixture,
        corpus_cpp_beta_fixture,
        "cpp",
        "cpp",
        &["cpp"]
    ),
    (
        corpus_csharp_alpha_fixture,
        corpus_csharp_beta_fixture,
        "csharp",
        "cs",
        &["csharp"]
    ),
    (
        corpus_clojure_alpha_fixture,
        corpus_clojure_beta_fixture,
        "clojure",
        "clj",
        &["clojure"]
    ),
    (
        corpus_dart_alpha_fixture,
        corpus_dart_beta_fixture,
        "dart",
        "dart",
        &["dart"]
    ),
    (
        corpus_go_alpha_fixture,
        corpus_go_beta_fixture,
        "go",
        "go",
        &["go"]
    ),
    (
        corpus_hack_alpha_fixture,
        corpus_hack_beta_fixture,
        "hack",
        "hack",
        &["hack"]
    ),
    (
        corpus_java_alpha_fixture,
        corpus_java_beta_fixture,
        "java",
        "java",
        &["java"]
    ),
    (
        corpus_javascript_alpha_fixture,
        corpus_javascript_beta_fixture,
        "javascript",
        "js",
        &["javascript"]
    ),
    (
        corpus_julia_alpha_fixture,
        corpus_julia_beta_fixture,
        "julia",
        "jl",
        &["julia"]
    ),
    (
        corpus_kotlin_alpha_fixture,
        corpus_kotlin_beta_fixture,
        "kotlin",
        "kt",
        &["kotlin"]
    ),
    (
        corpus_lua_alpha_fixture,
        corpus_lua_beta_fixture,
        "lua",
        "lua",
        &["lua"]
    ),
    (
        corpus_ocaml_alpha_fixture,
        corpus_ocaml_beta_fixture,
        "ocaml",
        "ml",
        &["ocaml"]
    ),
    (
        corpus_php_alpha_fixture,
        corpus_php_beta_fixture,
        "php",
        "php",
        &["php"]
    ),
    (
        corpus_python_alpha_fixture,
        corpus_python_beta_fixture,
        "python",
        "py",
        &["python"]
    ),
    (
        corpus_r_alpha_fixture,
        corpus_r_beta_fixture,
        "r",
        "r",
        &["r"]
    ),
    (
        corpus_ruby_alpha_fixture,
        corpus_ruby_beta_fixture,
        "ruby",
        "rb",
        &["ruby"]
    ),
    (
        corpus_rust_alpha_fixture,
        corpus_rust_beta_fixture,
        "rust",
        "rs",
        &[]
    ),
    (
        corpus_scala_alpha_fixture,
        corpus_scala_beta_fixture,
        "scala",
        "scala",
        &["scala"]
    ),
    (
        corpus_scheme_alpha_fixture,
        corpus_scheme_beta_fixture,
        "scheme",
        "scm",
        &["scheme"]
    ),
    (
        corpus_solidity_alpha_fixture,
        corpus_solidity_beta_fixture,
        "solidity",
        "sol",
        &["solidity"]
    ),
    (
        corpus_swift_alpha_fixture,
        corpus_swift_beta_fixture,
        "swift",
        "swift",
        &["swift"]
    ),
    (
        corpus_typescript_alpha_fixture,
        corpus_typescript_beta_fixture,
        "typescript",
        "ts",
        &["typescript"]
    ),
    (
        corpus_move_alpha_fixture,
        corpus_move_beta_fixture,
        "move",
        "move",
        &["move_on_sui", "move_on_aptos"]
    ),
];

#[test]
fn all_candidate_languages_keep_both_entity_versions_in_reachable_history() {
    let core = semgrep_core_or_skip("corpus history");
    let fixtures = fixtures();
    assert_eq!(fixtures.len(), 50);
    let repo = empty_repository();
    let root = repo.path();
    prepare_empty_database(root);
    for fixture in &fixtures {
        assert!(fixture.source.lines().count() >= 200, "{}", fixture.path());
        let source_path = root.join(fixture.path());
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(source_path, fixture.source).unwrap();
    }
    git(root, &["add", "--", "src"]);
    git(
        root,
        &["commit", "-qm", "add full version-one language corpus"],
    );
    let version_one = git_text(root, &["rev-parse", "HEAD"]);
    let core_path = core
        .as_deref()
        .unwrap_or_else(|| Path::new("/no/such/semgrep-core"));
    let env = [("REPODB_SEMGREP_CORE", core_path.to_str().unwrap())];
    assert!(run_with_env(root, &["refresh"], &env).0);

    for fixture in &fixtures {
        assert_marker_result(fixture, root, &fixture.entity_v1(), &version_one);
        let updated = fixture
            .source
            .replace(&fixture.entity_v1(), &fixture.entity_v2());
        assert_ne!(
            updated,
            fixture.source,
            "{} must change an entity",
            fixture.path()
        );
        fs::write(root.join(fixture.path()), updated).unwrap();
    }
    git(root, &["add", "--", "src"]);
    git(
        root,
        &["commit", "-qm", "rename all entities to version two"],
    );
    let version_two = git_text(root, &["rev-parse", "HEAD"]);
    assert_ne!(version_one, version_two);
    assert!(
        run_with_env(
            root,
            &["refresh"],
            &[("REPODB_SEMGREP_CORE", "/no/such/semgrep-core")],
        )
        .0
    );

    for fixture in &fixtures {
        assert_marker_result(fixture, root, &fixture.entity_v1(), &version_one);
        assert_marker_result(fixture, root, &fixture.entity_v2(), &version_two);
    }
}
