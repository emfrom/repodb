use super::{git, git_text, run_with_env};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

// Share the production AST validator so the test compares actual indexed
// syntax spans, not merely successful parser invocations.
#[allow(dead_code)]
#[path = "../../src/semgrep_languages.rs"]
mod semgrep_languages;

struct Fixture {
    language: &'static str,
    extension: &'static str,
    source: &'static str,
    parsers: &'static [&'static str],
}

impl Fixture {
    fn path(&self) -> String {
        format!("src/{}/sample.{}", self.language, self.extension)
    }

    fn marker(&self) -> &'static str {
        let start = self
            .source
            .find("Repodb")
            .expect("fixture needs a unique marker");
        let end = start
            + self.source[start..]
                .bytes()
                .take_while(u8::is_ascii_alphanumeric)
                .count();
        &self.source[start..end]
    }
}

macro_rules! corpus {
    ($(($test:ident, $language:literal, $extension:literal, $parsers:expr)),+ $(,)?) => {
        fn fixtures() -> Vec<Fixture> {
            vec![$(Fixture {
                language: $language,
                extension: $extension,
                source: include_str!(concat!("sources/", $language, "/sample.", $extension)),
                parsers: $parsers,
            }),+]
        }
        $(
            #[test]
            fn $test() {
                let fixture = fixtures().into_iter().find(|f| f.language == $language).unwrap();
                verify_language(&fixture);
            }
        )+
    }
}

corpus![
    (javascript, "javascript", "js", &["javascript"]),
    (html, "html", "html", &["html"]),
    (python, "python", "py", &["python"]),
    (bash, "bash", "sh", &["bash"]),
    (typescript, "typescript", "ts", &["typescript"]),
    (java, "java", "java", &["java"]),
    (csharp, "csharp", "cs", &["csharp"]),
    (cpp, "cpp", "cpp", &["cpp"]),
    (c, "c", "c", &["c"]),
    (rust, "rust", "rs", &["rust"]),
    (php, "php", "php", &["php"]),
    (go, "go", "go", &["go"]),
    (kotlin, "kotlin", "kt", &["kotlin"]),
    (lua, "lua", "lua", &["lua"]),
    (swift, "swift", "swift", &["swift"]),
    (ruby, "ruby", "rb", &["ruby"]),
    (dart, "dart", "dart", &["dart"]),
    (r, "r", "r", &["r"]),
    (lisp, "lisp", "lisp", &["lisp"]),
    (scala, "scala", "scala", &["scala"]),
    (julia, "julia", "jl", &["julia"]),
    (clojure, "clojure", "clj", &["clojure"]),
    (ocaml, "ocaml", "ml", &["ocaml"]),
    (solidity, "solidity", "sol", &["solidity"]),
    (
        move_sui,
        "move_sui",
        "move",
        &["move_on_sui", "move_on_aptos"]
    ),
    (
        move_aptos,
        "move_aptos",
        "move",
        &["move_on_sui", "move_on_aptos"]
    ),
];

pub(super) fn required_semgrep_core() -> PathBuf {
    static CORE: OnceLock<PathBuf> = OnceLock::new();
    CORE.get_or_init(|| {
        let configured = env::var_os("REPODB_SEMGREP_CORE").map(PathBuf::from);
        let mut candidates = configured.clone().into_iter().collect::<Vec<_>>();
        if configured.is_none() {
            for directory in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
                candidates.push(directory.join("semgrep-core"));
                let semgrep = directory.join("semgrep");
                if let Ok(semgrep) = fs::canonicalize(semgrep)
                    && let Some(install_root) = semgrep.parent().and_then(Path::parent)
                    && let Ok(entries) = fs::read_dir(install_root.join("lib"))
                {
                    for entry in entries.flatten() {
                        candidates
                            .push(entry.path().join("site-packages/semgrep/bin/semgrep-core"));
                    }
                }
            }
        }
        candidates
            .into_iter()
            .find(|path| {
                let Ok(output) = Command::new(path).arg("-version").output() else {
                    return false;
                };
                output.status.success()
                    && format!(
                        "{}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    )
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
                    .any(|part| part == "1.181.0")
            })
            .expect(
                "Semgrep Core v1.181.0 is required; set REPODB_SEMGREP_CORE or put semgrep on PATH",
            )
    })
    .clone()
}

fn fresh_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "test@example.com"]);
    git(repo.path(), &["config", "user.name", "Test"]);
    repo
}

fn search(root: &Path, database: &str, marker: &str) -> Vec<serde_json::Value> {
    let (ok, output) = run_with_env(
        root,
        &[
            "--database",
            database,
            "search",
            "--exact",
            "--json",
            marker,
            "--all-history",
        ],
        &[],
    );
    assert!(ok, "search failed: {output}");
    serde_json::from_str(&output).unwrap()
}

fn assert_hit(fixture: &Fixture, root: &Path, database: &str, marker: &str, commit: &str) {
    let line = fixture
        .source
        .lines()
        .position(|l| l.contains(fixture.marker()))
        .unwrap()
        + 1;
    assert!(
        search(root, database, marker).iter().any(|result| {
            result["commit_id"] == commit
                && result["path"] == fixture.path()
                && result["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(marker))
                && result["line_start"].as_u64().unwrap_or(0) as usize <= line
                && result["line_end"].as_u64().unwrap_or(0) as usize >= line
        }),
        "{marker} not indexed at {}:{line} for {commit}",
        fixture.path()
    );
}

fn parser_ranges(fixture: &Fixture, root: &Path) -> Vec<Option<Vec<[usize; 2]>>> {
    let source_path = root.join(fixture.path());
    fixture
        .parsers
        .iter()
        .map(|parser| {
            let output = Command::new(required_semgrep_core())
                .args(["-l", parser, "-json", "-full_token_info", "-dump_ast"])
                .arg(&source_path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "Semgrep {parser} rejected {}: {}",
                fixture.path(),
                String::from_utf8_lossy(&output.stderr)
            );
            let ast: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let syntax =
                semgrep_languages::syntax_ranges_from_ast(parser, &ast, fixture.source.as_bytes());
            assert!(
                syntax.as_ref().is_some_and(|spans| !spans.is_empty()),
                "{parser} did not provide usable syntax ranges for {}",
                fixture.path()
            );
            syntax
        })
        .collect()
}

fn verify_language(fixture: &Fixture) {
    let repo = fresh_repo();
    let root = repo.path();
    let path = root.join(fixture.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, fixture.source).unwrap();
    let ranges = parser_ranges(fixture, root);
    git(root, &["add", "src"]);
    git(root, &["commit", "-qm", "index one language"]);
    let commit = git_text(root, &["rev-parse", "HEAD"]);
    let database_dir = tempfile::tempdir().unwrap();
    let database = database_dir.path().join("isolated.sqlite");
    let db_path = database.to_str().unwrap();
    assert!(!database.exists());
    assert!(run_with_env(root, &["--database", db_path, "init"], &[]).0);
    assert!(database.is_file());
    assert!(!root.join(".git/repodb.sqlite").exists());
    assert_hit(fixture, root, db_path, fixture.marker(), &commit);

    let db = rusqlite::Connection::open(database).unwrap();
    let indexed: Vec<(usize, usize)> = db
        .prepare("SELECT u.byte_start,u.byte_end FROM indexed_units u JOIN git_blob_units b USING(unit_id) ORDER BY u.byte_start")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let syntax = match ranges.as_slice() {
        [Some(a)] => Some(a),
        [Some(a), Some(b)] if a == b => Some(a),
        [Some(_), Some(_)] => None, // Move parser disagreement intentionally falls back.
        _ => None,
    };
    if let Some(syntax) = syntax {
        for [start, end] in syntax {
            assert!(
                indexed.contains(&(*start, *end)),
                "{} lost Semgrep syntax span [{start},{end}); indexed: {indexed:?}",
                fixture.path()
            );
        }
    } else {
        assert!(fixture.language.starts_with("move_"));
        assert_eq!(
            indexed,
            vec![(0, fixture.source.trim_end_matches('\n').len())],
            "Move disagreement should use documented paragraph fallback"
        );
    }
}

#[test]
fn all_languages_keep_both_markers_in_reachable_history() {
    let fixtures = fixtures();
    assert_eq!(fixtures.len(), 26); // 24 survey entries + Sui/Aptos Move dialects.
    let repo = fresh_repo();
    let root = repo.path();
    for fixture in &fixtures {
        let path = root.join(fixture.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, fixture.source).unwrap();
    }
    git(root, &["add", "src"]);
    git(root, &["commit", "-qm", "version one parser corpus"]);
    let first = git_text(root, &["rev-parse", "HEAD"]);
    let database_dir = tempfile::tempdir().unwrap();
    let database = database_dir.path().join("all.sqlite");
    let db_path = database.to_str().unwrap();
    assert!(run_with_env(root, &["--database", db_path, "init"], &[]).0);
    for fixture in &fixtures {
        assert_hit(fixture, root, db_path, fixture.marker(), &first);
        fs::write(
            root.join(fixture.path()),
            fixture.source.replace("V1", "V2"),
        )
        .unwrap();
    }
    git(root, &["add", "src"]);
    git(root, &["commit", "-qm", "version two parser corpus"]);
    let second = git_text(root, &["rev-parse", "HEAD"]);
    assert!(run_with_env(root, &["--database", db_path, "refresh"], &[]).0);
    for fixture in &fixtures {
        assert_hit(fixture, root, db_path, fixture.marker(), &first);
        assert_hit(
            fixture,
            root,
            db_path,
            &fixture.marker().replace("V1", "V2"),
            &second,
        );
    }
}
