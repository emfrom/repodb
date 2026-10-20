use rusqlite::{Connection, ffi::sqlite3_auto_extension, params};
use serde::Serialize;
use std::{
    env,
    error::Error,
    path::PathBuf,
    process::{Command, Stdio},
};

mod budget;
mod config;
mod git_objects;
mod git_ranges;
mod history;
mod imports;
mod process;
mod segmentation;
mod semgrep_languages;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

// NUL cannot occur in a Git path, so this internal tag cannot collide with UTF-8 paths.
// Invalid paths are exposed as `bytes:<lowercase-hex>` plus `path_bytes_hex` in JSON.
const ENCODED_GIT_PATH_PREFIX: &str = "\0repodb-git-path:";

fn encode_git_path(path: Vec<u8>) -> String {
    let hex = path
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{ENCODED_GIT_PATH_PREFIX}{hex}")
}

fn decoded_git_path_hex(path: &str) -> Option<&str> {
    path.strip_prefix(ENCODED_GIT_PATH_PREFIX)
}

fn readable_path(path: &str) -> String {
    decoded_git_path_hex(path)
        .map_or_else(|| path.to_owned(), |hex| format!("bytes:{hex} (non-UTF-8)"))
}

#[derive(Serialize)]
struct SearchResult {
    path: String,
    line_start: usize,
    line_end: usize,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    distance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blob_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_id: Option<String>,
}

#[derive(Clone, Copy, Default)]
enum SearchScope {
    Source,
    RepoLocalSource,
    RepoLocalDocs,
    AllDocs,
    #[default]
    All,
}

impl SearchScope {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "source" => Ok(Self::Source),
            "repo-local-source" => Ok(Self::RepoLocalSource),
            "repo-local-docs" => Ok(Self::RepoLocalDocs),
            "all-docs" => Ok(Self::AllDocs),
            "all" => Ok(Self::All),
            _ => Err(format!("scope must be source, repo-local-source, repo-local-docs, all-docs, or all (got {value})").into()),
        }
    }

    fn matches(self, result: &SearchResult) -> bool {
        let repo_local = result.commit_id.is_some() && result.repo_id.is_none();
        let is_source = source_code_path(&result.path);
        let is_doc = documentation_path(&result.path, result.page.is_some());
        match self {
            Self::Source => is_source && (repo_local || result.repo_id.is_some()),
            Self::RepoLocalSource => repo_local && is_source,
            Self::RepoLocalDocs => repo_local && is_doc,
            Self::AllDocs => is_doc,
            Self::All => true,
        }
    }
}

fn source_code_path(path: &str) -> bool {
    !matches!(
        segmentation::kind(path.as_bytes()),
        "fallback" | "paragraph"
    )
}

fn documentation_path(path: &str, is_page: bool) -> bool {
    if is_page {
        return true;
    }
    let clean_path = path.split(['?', '#']).next().unwrap_or(path);
    let name = clean_path.rsplit('/').next().unwrap_or(clean_path);
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "readme"
            | "license"
            | "copying"
            | "notice"
            | "changelog"
            | "changes"
            | "authors"
            | "contributors"
            | "contributing"
            | "code_of_conduct"
            | "security"
            | "support"
    ) {
        return true;
    }
    matches!(
        lower.rsplit_once('.').map(|(_, ext)| ext),
        Some("md" | "markdown" | "txt" | "pdf" | "html" | "htm" | "org")
    )
}
fn git(args: &[&str]) -> Result<Vec<u8>> {
    let output = process::capture(
        &mut Command::new("git")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
        256 * 1024 * 1024,
        64 * 1024,
    )?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output.stdout)
}
fn root() -> Result<PathBuf> {
    Ok(PathBuf::from(
        String::from_utf8(git(&["rev-parse", "--show-toplevel"])?)?.trim(),
    ))
}
fn database() -> Result<PathBuf> {
    let g = String::from_utf8(git(&["rev-parse", "--git-dir"])?)?;
    let p = PathBuf::from(g.trim());
    Ok((if p.is_absolute() { p } else { root()?.join(p) }).join("repodb.sqlite"))
}
fn selected_database(option: Option<String>) -> Result<PathBuf> {
    match option {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            Ok(if path.is_absolute() {
                path
            } else {
                env::current_dir()?.join(path)
            })
        }
        Some(_) => Err("--database requires a nonempty path".into()),
        None => database(),
    }
}
fn open_db(path: &std::path::Path) -> Result<Connection> {
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(sqlite_vec::sqlite3_vec_init as *const ())))
    };
    let c = Connection::open(path)?;
    if !history::is_current(&c)? {
        c.execute_batch("CREATE TABLE IF NOT EXISTS indexed_files(path TEXT PRIMARY KEY NOT NULL,commit_id TEXT NOT NULL,content TEXT NOT NULL); CREATE TABLE IF NOT EXISTS embeddings(path TEXT NOT NULL,line INTEGER NOT NULL,text TEXT NOT NULL,commit_id TEXT NOT NULL,vector BLOB NOT NULL,PRIMARY KEY(path,line)); CREATE TABLE IF NOT EXISTS embedding_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    }
    Ok(c)
}
fn resolved_executable(command: &str) -> Option<PathBuf> {
    let path = PathBuf::from(command);
    if path.components().count() > 1 {
        return path.canonicalize().ok();
    }
    env::split_paths(&env::var_os("PATH")?).find_map(|dir| {
        let candidate = dir.join(&path);
        candidate
            .is_file()
            .then(|| candidate.canonicalize().ok())
            .flatten()
    })
}

pub(crate) fn is_builtin_embedding_command(command: &str) -> bool {
    let Ok(home) = env::var("HOME") else {
        return false;
    };
    let builtin = PathBuf::from(home).join("bin/generate_embeddings");
    matches!((builtin.canonicalize().ok(), resolved_executable(command)), (Some(a), Some(b)) if a == b)
}

fn embed(command: &str, model: &str, text: &str) -> Result<Vec<f32>> {
    embed_with_output_limit(command, model, text, None)
}

pub(crate) fn embed_with_output_limit(
    command: &str,
    model: &str,
    text: &str,
    output_limit: Option<usize>,
) -> Result<Vec<f32>> {
    if is_builtin_embedding_command(command) && model != "all-MiniLM-L6-v2" {
        return Err(
            "the built-in generate_embeddings command only supports all-MiniLM-L6-v2".into(),
        );
    }
    let mut child = Command::new(command)
        .env("REPODB_EMBEDDING_MODEL", model)
        .args(["--json", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Some embedders return no row for empty/whitespace input. Preserve a
    // searchable vector for every indexed unit with a stable sentinel instead.
    let input = if is_builtin_embedding_command(command) && text.trim().is_empty() {
        "<empty RepoDB unit>"
    } else {
        text
    };
    let output = match output_limit {
        Some(limit) => process::capture_with_input(&mut child, input.as_bytes(), limit, 64 * 1024)?,
        None => process::capture_with_input_unbounded(&mut child, input.as_bytes())?,
    };
    if output.exceeded {
        let limit = output_limit.expect("bounded capture has a limit");
        return Err(format!(
            "embedding command output could not be captured completely (limit {limit})"
        )
        .into());
    }
    if !output.status.success() {
        return Err("embedding command failed".into());
    }
    let v: Vec<f32> = serde_json::from_slice(&output.stdout)?;
    validate_vector(&v)?;
    Ok(v)
}
pub(crate) fn embed_many_with_output_limit(
    command: &str,
    model: &str,
    texts: &[&str],
    output_limit: usize,
) -> Result<Vec<Vec<f32>>> {
    if !is_builtin_embedding_command(command)
        || texts.len() < 2
        || texts.iter().any(|text| text.contains('\0'))
    {
        return texts
            .iter()
            .map(|text| embed_with_output_limit(command, model, text, Some(output_limit)))
            .collect();
    }
    if model != config::DEFAULT_EMBEDDING_MODEL {
        return Err(
            "the built-in generate_embeddings command only supports all-MiniLM-L6-v2".into(),
        );
    }
    let mut input = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        if index != 0 {
            input.push(0);
        }
        input.extend_from_slice(if text.trim().is_empty() {
            b"<empty RepoDB unit>"
        } else {
            text.as_bytes()
        });
    }
    const MAX_BATCH_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
    let output_limit = output_limit.min(MAX_BATCH_OUTPUT_BYTES);
    let mut child = Command::new(command)
        .env("REPODB_EMBEDDING_MODEL", model)
        .args(["--json", "-0", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut vectors = Vec::with_capacity(texts.len());
    let output =
        process::capture_lines_with_input(&mut child, input, output_limit, 64 * 1024, |line| {
            if line.iter().all(u8::is_ascii_whitespace) {
                return Ok(());
            }
            let vector: Vec<f32> = serde_json::from_slice(line).map_err(std::io::Error::other)?;
            validate_vector(&vector).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
            })?;
            vectors.push(vector);
            Ok(())
        })?;
    if output.exceeded {
        return Err(format!(
            "embedding command output exceeded the {output_limit}-byte capture limit"
        )
        .into());
    }
    if !output.status.success() {
        return Err(format!(
            "embedding command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    if vectors.len() != texts.len() {
        return Err("embedding command returned the wrong number of vectors".into());
    }
    Ok(vectors)
}

fn validate_vector(v: &[f32]) -> Result<()> {
    if v.is_empty() || v.iter().any(|x| !x.is_finite()) {
        return Err("embedding must be a nonempty finite vector".into());
    }
    if v.iter().all(|x| *x == 0.0) {
        return Err("embedding must have nonzero norm for cosine distance".into());
    }
    Ok(())
}
fn as_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn reembed(c: &mut Connection, command: &str, model: &str) -> Result<()> {
    let version = history::snapshot_version(c)?;
    let mut st = c.prepare("SELECT path,commit_id,content FROM indexed_files ORDER BY path")?;
    let files = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(st);
    let mut rows = vec![];
    let mut dim = None;
    for (p, id, content) in files {
        for (i, line) in content.lines().enumerate() {
            let v = embed(command, model, line)?;
            if dim.is_some_and(|d| d != v.len()) {
                return Err("embedding dimension mismatch".into());
            }
            dim = Some(v.len());
            rows.push((p.clone(), i + 1, line.to_owned(), id.clone(), v));
        }
    }
    let tx = history::publish_transaction(c, version)?;
    tx.execute("DELETE FROM embeddings", [])?;
    {
        let mut st = tx.prepare("INSERT INTO embeddings VALUES(?1,?2,?3,?4,?5)")?;
        for (p, i, t, id, v) in rows {
            st.execute(params![p, i, t, id, as_bytes(&v)])?;
        }
    }
    if let Some(import_dim) = imports::reembed(&tx, command, model)? {
        if dim.is_some_and(|d| d != import_dim) {
            return Err("embedding dimension mismatch".into());
        }
        dim = Some(import_dim);
    }
    for (k, v) in [
        ("command", command),
        ("model", model),
        ("dimension", &dim.unwrap_or(0).to_string()),
    ] {
        tx.execute("INSERT INTO embedding_config VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![k,v])?;
    }
    tx.commit()?;
    Ok(())
}
fn exact(c: &Connection, q: &str, commits: &[String]) -> Result<Vec<SearchResult>> {
    let mut s = c.prepare("SELECT path,content,commit_id FROM indexed_files ORDER BY path")?;
    let rows = s.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = vec![];
    for row in rows {
        let (p, content, id) = row?;
        if !commits.contains(&id) {
            continue;
        }
        for (i, l) in content.lines().enumerate() {
            if l.to_lowercase().contains(&q.to_lowercase()) {
                out.push(SearchResult {
                    path: p.clone(),
                    line_start: i + 1,
                    line_end: i + 1,
                    text: l.to_owned(),
                    commit_id: Some(id.clone()),
                    distance: None,
                    blob_id: None,
                    page: None,
                    repo_id: None,
                })
            }
        }
    }
    Ok(out)
}
fn query_vector(c: &Connection, q: &str) -> Result<Vec<f32>> {
    let command: String = c
        .query_row(
            "SELECT value FROM embedding_config WHERE key='command'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "no embeddings; run refresh")?;
    let model: String = c.query_row(
        "SELECT value FROM embedding_config WHERE key='model'",
        [],
        |r| r.get(0),
    )?;
    let v = embed(&command, &model, q)?;
    let dim: i64 = c
        .query_row(
            "SELECT value FROM embedding_config WHERE key='dimension'",
            [],
            |r| r.get::<_, String>(0),
        )?
        .parse()?;
    // Zero records an empty index whose model dimension is not established yet.
    if dim != 0 && v.len() as i64 != dim {
        return Err("query embedding dimension mismatch".into());
    }
    Ok(v)
}
fn semantic_with_vector(
    c: &Connection,
    v: &[f32],
    limit: usize,
    commits: &[String],
    include: impl Fn(&SearchResult) -> bool,
) -> Result<Vec<SearchResult>> {
    let mut s=c.prepare("SELECT path,line,text,commit_id,vec_distance_cosine(vector,?1) d FROM embeddings WHERE commit_id IN (SELECT value FROM json_each(?2)) ORDER BY d,path,line")?;
    let rows = s.query_map(params![as_bytes(v), serde_json::to_string(commits)?], |r| {
        Ok(SearchResult {
            path: r.get(0)?,
            line_start: r.get(1)?,
            line_end: r.get(1)?,
            text: r.get(2)?,
            commit_id: Some(r.get(3)?),
            distance: Some(r.get(4)?),
            blob_id: None,
            page: None,
            repo_id: None,
        })
    })?;
    let mut results = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    results.retain(|result| include(result));
    results.truncate(limit);
    Ok(results)
}
fn semantic(
    c: &Connection,
    q: &str,
    limit: usize,
    commits: &[String],
    include: impl Fn(&SearchResult) -> bool,
) -> Result<Vec<SearchResult>> {
    semantic_with_vector(c, &query_vector(c, q)?, limit, commits, include)
}
fn execute() -> Result<()> {
    let mut a = env::args().skip(1);
    let first = a.next().ok_or("usage: repodb [--database <path>] <init|refresh [--imports|--changed|--update COMMIT]|import|retention|reembed|search> (default database: <git-dir>/repodb.sqlite)")?;
    let (option, cmd) = if first == "--database" {
        let path = a.next().ok_or("--database requires a path")?;
        (Some(path), a.next().ok_or("--database requires a command")?)
    } else {
        (None, first)
    };
    let db_path = selected_database(option)?;
    match cmd.as_str() {
        "init" | "refresh" => {
            let arguments: Vec<_> = a.collect();
            let imports_only = arguments.len() == 1 && arguments[0] == "--imports";
            let changed = arguments.len() == 1 && arguments[0] == "--changed";
            let update = if arguments.len() == 2 && arguments[0] == "--update" {
                Some(history::resolve_update_commit(&arguments[1])?)
            } else {
                None
            };
            if (!arguments.is_empty() && !imports_only && !changed && update.is_none())
                || (cmd == "init" && (imports_only || changed || update.is_some()))
                || (changed && update.is_some())
                || (imports_only && (changed || update.is_some()))
            {
                return Err(
                    "usage: repodb refresh [--imports | --changed | --update COMMIT]".into(),
                );
            }
            let mut db = open_db(&db_path)?;
            if imports_only {
                let version = history::snapshot_version(&db)?;
                imports::refresh(&db)?;
                imports::validate_dimensions(&db)?;
                let tx = history::publish_transaction(&mut db, version)?;
                imports::publish(&tx)?;
                tx.commit()?;
            } else if changed {
                history::refresh_changed(&mut db)?;
            } else if let Some(commit_id) = update {
                history::refresh_update(&mut db, &commit_id)?;
            } else {
                let version = history::snapshot_version(&db)?;
                imports::refresh(&db)?;
                history::prepare_refresh(&db)?;
                imports::validate_dimensions(&db)?;
                let tx = history::publish_transaction(&mut db, version)?;
                imports::publish(&tx)?;
                history::publish_refresh(&tx)?;
                tx.commit()?;
            }
        }
        "import" => {
            let first = a
                .next()
                .ok_or("import requires one local file, URL, or Git repository")?;
            if first == "--preview" {
                let path = a
                    .next()
                    .ok_or("import --preview requires a local file or directory")?;
                if a.next().is_some() {
                    return Err("import --preview accepts one local file or directory".into());
                }
                let preview = imports::preview(std::path::Path::new(&path), &db_path)?;
                println!("{}", serde_json::to_string(&preview)?);
            } else {
                let (revision, locator) = if first == "--revision" {
                    let revision = a
                        .next()
                        .ok_or("import --revision requires a branch, tag, or commit")?;
                    let locator = a
                        .next()
                        .ok_or("import --revision requires one Git repository")?;
                    (Some(revision), locator)
                } else {
                    (None, first)
                };
                if a.next().is_some() {
                    return Err("import accepts one file, URL, or Git repository".into());
                }
                imports::register(&open_db(&db_path)?, &locator, revision.as_deref())?;
            }
        }
        "retention" => {
            let value = a.next().ok_or("retention requires latest or all")?;
            if a.next().is_some() {
                return Err("unexpected arguments".into());
            }
            imports::set_retention(&open_db(&db_path)?, &value)?;
        }
        "reembed" => {
            let mut command = config::embedding_command()?;
            let mut model = config::DEFAULT_EMBEDDING_MODEL.to_owned();
            while let Some(x) = a.next() {
                match x.as_str() {
                    "--command" => command = a.next().ok_or("--command requires path")?,
                    "--model" => model = a.next().ok_or("--model requires id")?,
                    _ => return Err(format!("unexpected argument {x}").into()),
                }
            }
            let mut c = open_db(&db_path)?;
            if history::is_current(&c)? {
                history::reembed(&mut c, &command, &model)?;
            } else {
                reembed(&mut c, &command, &model)?;
            }
        }
        "search" => {
            let mut query = None;
            let mut scope = history::Scope::default();
            let mut result_scope = SearchScope::default();
            let mut result_scope_set = false;
            let (mut json, mut mode, mut limit) = (false, "exact", 20usize);
            while let Some(x) = a.next() {
                match x.as_str() {
                    "--json" => json = true,
                    "--scope" => {
                        if result_scope_set {
                            return Err("--scope may only be specified once".into());
                        }
                        result_scope =
                            SearchScope::parse(&a.next().ok_or("--scope requires a value")?)?;
                        result_scope_set = true;
                    }
                    "--all-history" => scope.all_history = true,
                    "--current-worktree" => scope.current_worktree = true,
                    "--commit" => {
                        if scope.commit.is_some() {
                            return Err("--commit may only be specified once".into());
                        }
                        scope.commit = Some(a.next().ok_or("--commit requires ID")?);
                    }
                    "--include-commit" => scope
                        .include
                        .push(a.next().ok_or("--include-commit requires ID")?),
                    "--exclude-commit" => scope
                        .exclude
                        .push(a.next().ok_or("--exclude-commit requires ID")?),
                    "--semantic" => mode = "semantic",
                    "--combined" => mode = "combined",
                    "--semantic-limit" => limit = a.next().ok_or("missing limit")?.parse()?,
                    _ if x.starts_with('-') => {
                        return Err(format!("unexpected argument {x}").into());
                    }
                    _ if query.is_none() => query = Some(x),
                    _ => return Err(format!("unexpected argument {x}").into()),
                }
            }
            let q = query.ok_or("search requires query")?;
            let mut connection = open_db(&db_path)?;
            // Scope, model configuration and results belong to one read snapshot.
            let c = connection.transaction()?;
            let commits = scope.resolve(&c)?;
            let mut result = if history::is_current(&c)? {
                history::search(&c, &q, mode, limit, &commits, |result| {
                    result_scope.matches(result)
                })?
            } else if mode == "exact" {
                exact(&c, &q, &commits)?
            } else if mode == "combined" {
                let v = query_vector(&c, &q)?;
                let mut candidates = semantic_with_vector(&c, &v, limit, &commits, |result| {
                    result_scope.matches(result)
                })?;
                let mut seen = std::collections::HashSet::new();
                for x in &candidates {
                    seen.insert((x.path.clone(), x.line_start));
                }
                for mut x in exact(&c, &q, &commits)? {
                    if result_scope.matches(&x) && seen.insert((x.path.clone(), x.line_start)) {
                        let distance: f64 = c.query_row(
                            "SELECT vec_distance_cosine(vector,?1) FROM embeddings WHERE path=?2 AND line=?3",
                            params![as_bytes(&v), x.path, x.line_start as i64], |r| r.get(0),
                        )?;
                        x.distance = Some(distance);
                        candidates.push(x);
                    }
                }
                candidates.sort_by(|a, b| {
                    a.distance
                        .unwrap()
                        .total_cmp(&b.distance.unwrap())
                        .then(a.path.cmp(&b.path))
                        .then(a.line_start.cmp(&b.line_start))
                });
                candidates
            } else {
                semantic(&c, &q, limit, &commits, |result| {
                    result_scope.matches(result)
                })?
            };
            if mode == "exact" || mode == "semantic" || mode == "combined" {
                result.extend(imports::search(&c, &q, mode, limit, |result| {
                    result_scope.matches(result)
                })?);
            }
            result.retain(|candidate| result_scope.matches(candidate));
            if mode == "semantic" || mode == "combined" {
                result.sort_by(|a, b| {
                    a.distance
                        .unwrap_or(f64::INFINITY)
                        .total_cmp(&b.distance.unwrap_or(f64::INFINITY))
                        .then(a.path.cmp(&b.path))
                        .then(a.line_start.cmp(&b.line_start))
                });
                if mode == "semantic" {
                    result.truncate(limit);
                } else {
                    let mut rank = 0;
                    result.retain(|candidate| {
                        let keep = rank < limit
                            || candidate.text.to_lowercase().contains(&q.to_lowercase());
                        rank += 1;
                        keep
                    });
                }
            }
            if json {
                let mut values = result
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for (value, result) in values.iter_mut().zip(&result) {
                    if let Some(hex) = decoded_git_path_hex(&result.path) {
                        value["path"] = serde_json::Value::String(format!("bytes:{hex}"));
                        value["path_bytes_hex"] = serde_json::Value::String(hex.to_owned());
                    }
                }
                println!("{}", serde_json::to_string_pretty(&values)?)
            } else {
                for x in result {
                    let mut reference = String::new();
                    if let Some(commit) = x.commit_id {
                        reference.push_str(&format!(" [commit {commit}]"));
                    }
                    if let Some(blob) = x.blob_id {
                        reference.push_str(&format!(" [blob {blob}]"));
                    }
                    if let Some(repo_id) = x.repo_id {
                        reference.push_str(&format!(" [repo {repo_id}]"));
                    }
                    let path = readable_path(&x.path);
                    if let Some(page) = x.page {
                        println!("{}:page {}{}: {}", path, page, reference, x.text);
                    } else {
                        println!(
                            "{}:{}-{}{}: {}",
                            path, x.line_start, x.line_end, reference, x.text
                        );
                    }
                }
            }
        }
        _ => return Err(format!("unknown command: {cmd}").into()),
    }
    Ok(())
}
fn main() {
    if let Err(e) = execute() {
        eprintln!("repodb: {e}");
        std::process::exit(2)
    }
}
