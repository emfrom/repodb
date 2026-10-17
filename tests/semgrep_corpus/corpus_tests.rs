use super::{git, git_text, run, run_with_env, search_json};
use std::{collections::BTreeSet, fs, os::unix::fs::PermissionsExt, path::Path};

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
                exercise_fixture_in_fresh_database(&fixture_for($language, "alpha"));
            }

            #[test]
            fn $test_beta() {
                exercise_fixture_in_fresh_database(&fixture_for($language, "beta"));
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

fn fake_semgrep_core(root: &Path) -> std::path::PathBuf {
    let path = root.join("fake-semgrep-core");
    fs::write(
        &path,
        "#!/bin/sh\nif [ \"$1\" = -version ]; then echo 'semgrep-core version: 1.181.0'; exit 0; fi\n[ \"$1\" = -l ] || exit 2\necho \"$2\" >> \"$REPODB_CORPUS_CORE_CALLS\"\nstart=$(LC_ALL=C grep -a -b -o 'REPODB_SYNTAX_BEGIN' \"$6\" | head -n 1 | cut -d: -f1)\nend=$(LC_ALL=C grep -a -b -o 'REPODB_SYNTAX_END' \"$6\" | tail -n 1 | cut -d: -f1)\n[ -n \"$start\" ] && [ -n \"$end\" ] || exit 3\nprintf '{\"Pr\":[{\"tokens\":[{\"OriginTok\":{\"str\":\"REPODB_SYNTAX_BEGIN\",\"bytepos\":%s}},{\"OriginTok\":{\"str\":\"REPODB_SYNTAX_END\",\"bytepos\":%s}}]}]}' \"$start\" \"$end\"\n",
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn prepare_empty_database(root: &Path) {
    assert!(run(root, &["init"]).0);
    let embedder = super::fake_embedder(root, "printf '[1,0]'");
    assert!(run(root, &["reembed", "--command", embedder.to_str().unwrap()]).0);
}

fn parser_calls(path: &Path) -> BTreeSet<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
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

fn exercise_fixture_in_fresh_database(fixture: &Fixture) {
    assert!(fixture.source.lines().count() >= 200, "{}", fixture.path());
    let repo = empty_repository();
    let root = repo.path();
    prepare_empty_database(root);
    let source_path = root.join(fixture.path());
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, fixture.source).unwrap();
    git(root, &["add", fixture.path().as_str()]);
    git(root, &["commit", "-qm", "add one language fixture"]);
    let commit = git_text(root, &["rev-parse", "HEAD"]);
    let core = fake_semgrep_core(root);
    let calls = root.join("core-language-calls");
    assert!(
        run_with_env(
            root,
            &["refresh"],
            &[
                ("REPODB_SEMGREP_CORE", core.to_str().unwrap()),
                ("REPODB_CORPUS_CORE_CALLS", calls.to_str().unwrap()),
            ],
        )
        .0
    );
    assert_eq!(
        parser_calls(&calls),
        fixture.parsers.iter().map(|s| s.to_string()).collect()
    );
    assert_marker_result(fixture, root, &fixture.entity_v1(), &commit);
}

define_corpus![
    (
        fresh_bash_alpha_fixture,
        fresh_bash_beta_fixture,
        "bash",
        "sh",
        &["bash"]
    ),
    (
        fresh_c_alpha_fixture,
        fresh_c_beta_fixture,
        "c",
        "c",
        &["c"]
    ),
    (
        fresh_cpp_alpha_fixture,
        fresh_cpp_beta_fixture,
        "cpp",
        "cpp",
        &["cpp"]
    ),
    (
        fresh_csharp_alpha_fixture,
        fresh_csharp_beta_fixture,
        "csharp",
        "cs",
        &["csharp"]
    ),
    (
        fresh_clojure_alpha_fixture,
        fresh_clojure_beta_fixture,
        "clojure",
        "clj",
        &["clojure"]
    ),
    (
        fresh_dart_alpha_fixture,
        fresh_dart_beta_fixture,
        "dart",
        "dart",
        &["dart"]
    ),
    (
        fresh_go_alpha_fixture,
        fresh_go_beta_fixture,
        "go",
        "go",
        &["go"]
    ),
    (
        fresh_hack_alpha_fixture,
        fresh_hack_beta_fixture,
        "hack",
        "hack",
        &["hack"]
    ),
    (
        fresh_java_alpha_fixture,
        fresh_java_beta_fixture,
        "java",
        "java",
        &["java"]
    ),
    (
        fresh_javascript_alpha_fixture,
        fresh_javascript_beta_fixture,
        "javascript",
        "js",
        &["javascript"]
    ),
    (
        fresh_julia_alpha_fixture,
        fresh_julia_beta_fixture,
        "julia",
        "jl",
        &["julia"]
    ),
    (
        fresh_kotlin_alpha_fixture,
        fresh_kotlin_beta_fixture,
        "kotlin",
        "kt",
        &["kotlin"]
    ),
    (
        fresh_lua_alpha_fixture,
        fresh_lua_beta_fixture,
        "lua",
        "lua",
        &["lua"]
    ),
    (
        fresh_ocaml_alpha_fixture,
        fresh_ocaml_beta_fixture,
        "ocaml",
        "ml",
        &["ocaml"]
    ),
    (
        fresh_php_alpha_fixture,
        fresh_php_beta_fixture,
        "php",
        "php",
        &["php"]
    ),
    (
        fresh_python_alpha_fixture,
        fresh_python_beta_fixture,
        "python",
        "py",
        &["python"]
    ),
    (
        fresh_r_alpha_fixture,
        fresh_r_beta_fixture,
        "r",
        "r",
        &["r"]
    ),
    (
        fresh_ruby_alpha_fixture,
        fresh_ruby_beta_fixture,
        "ruby",
        "rb",
        &["ruby"]
    ),
    (
        fresh_rust_alpha_fixture,
        fresh_rust_beta_fixture,
        "rust",
        "rs",
        &[]
    ),
    (
        fresh_scala_alpha_fixture,
        fresh_scala_beta_fixture,
        "scala",
        "scala",
        &["scala"]
    ),
    (
        fresh_scheme_alpha_fixture,
        fresh_scheme_beta_fixture,
        "scheme",
        "scm",
        &["scheme"]
    ),
    (
        fresh_solidity_alpha_fixture,
        fresh_solidity_beta_fixture,
        "solidity",
        "sol",
        &["solidity"]
    ),
    (
        fresh_swift_alpha_fixture,
        fresh_swift_beta_fixture,
        "swift",
        "swift",
        &["swift"]
    ),
    (
        fresh_typescript_alpha_fixture,
        fresh_typescript_beta_fixture,
        "typescript",
        "ts",
        &["typescript"]
    ),
    (
        fresh_move_alpha_fixture,
        fresh_move_beta_fixture,
        "move",
        "move",
        &["move_on_sui", "move_on_aptos"]
    ),
];

#[test]
fn all_candidate_languages_keep_both_entity_versions_in_reachable_history() {
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
    let core = fake_semgrep_core(root);
    let calls = root.join("core-language-calls");
    let env = [
        ("REPODB_SEMGREP_CORE", core.to_str().unwrap()),
        ("REPODB_CORPUS_CORE_CALLS", calls.to_str().unwrap()),
    ];
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
    assert!(run_with_env(root, &["refresh"], &env).0);

    for fixture in &fixtures {
        assert_marker_result(fixture, root, &fixture.entity_v1(), &version_one);
        assert_marker_result(fixture, root, &fixture.entity_v2(), &version_two);
    }
    let calls = parser_calls(&calls);
    assert!(calls.contains("move_on_sui"));
    assert!(calls.contains("move_on_aptos"));
    assert!(
        !calls.contains("rust"),
        "Rust fixtures use the built-in parser"
    );
}
