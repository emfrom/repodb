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
    first_retrieved_at: Option<String>,
    last_retrieved_at: Option<String>,
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

    fn matches_diff(self, path: &[u8]) -> bool {
        let is_source = !matches!(segmentation::kind(path), "fallback" | "paragraph");
        let path = String::from_utf8_lossy(path);
        let is_doc = documentation_path(&path, false);
        match self {
            Self::Source | Self::RepoLocalSource => is_source,
            Self::RepoLocalDocs | Self::AllDocs => is_doc,
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

fn diff_path_fields(path: Option<&[u8]>) -> (Option<String>, Option<String>) {
    let Some(path) = path else {
        return (None, None);
    };
    let encoded = match String::from_utf8(path.to_vec()) {
        Ok(path) => path,
        Err(error) => encode_git_path(error.into_bytes()),
    };
    (
        Some(readable_path(&encoded)),
        decoded_git_path_hex(&encoded).map(str::to_owned),
    )
}

fn diff_search_json(hit: &history::DiffSearchResult) -> serde_json::Value {
    let reference = &hit.reference;
    let (old_path, old_path_bytes_hex) = diff_path_fields(reference.old_path.as_deref());
    let (new_path, new_path_bytes_hex) = diff_path_fields(reference.new_path.as_deref());
    let side = match reference.side {
        history::DiffSide::Added => "added",
        history::DiffSide::Removed => "removed",
    };
    let mut value = serde_json::json!({
        "kind": "diff",
        "text": hit.text,
        "distance": hit.distance,
        "parent_commit_id": reference.parent_commit_id,
        "child_commit_id": reference.child_commit_id,
        "hunk_id": reference.hunk_id,
        "side": side,
        "old_path": old_path,
        "new_path": new_path,
        "old_line_start": reference.old_line_start,
        "old_line_end": reference.old_line_end,
        "new_line_start": reference.new_line_start,
        "new_line_end": reference.new_line_end,
        "matching_line_start": reference.line_start,
        "matching_line_end": reference.line_end,
        "blob_id": reference.blob_id,
    });
    if let Some(hex) = old_path_bytes_hex {
        value["old_path_bytes_hex"] = serde_json::Value::String(hex);
    }
    if let Some(hex) = new_path_bytes_hex {
        value["new_path_bytes_hex"] = serde_json::Value::String(hex);
    }
    value
}

fn diff_path_display(path: &[u8]) -> String {
    diff_path_fields(Some(path)).0.unwrap_or_default()
}

fn diff_endpoint(path: Option<&[u8]>, start: Option<usize>, end: Option<usize>) -> String {
    let Some(path) = path else {
        return "<absent>".to_owned();
    };
    let path = diff_path_display(path).escape_debug().to_string();
    match (start, end) {
        (Some(start), Some(end)) => format!("{path}:{start}-{end}"),
        _ => format!("{path}:<no lines>"),
    }
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
#[derive(Serialize)]
struct CapabilityReadiness {
    ready: bool,
    reason: Option<String>,
}

fn capability(ready: bool, reason: Option<String>) -> CapabilityReadiness {
    CapabilityReadiness { ready, reason }
}

fn status(db_path: &std::path::Path) -> Result<serde_json::Value> {
    let database_exists = db_path.is_file();
    let indexed_head = if database_exists {
        let c = Connection::open(db_path)?;
        let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if !(0..=4).contains(&version) {
            return Err(format!("unsupported database version {version}").into());
        }
        if version >= 2 {
            c.query_row(
                "SELECT commit_id FROM git_commits ORDER BY timestamp DESC, commit_id DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
        } else {
            c.query_row(
                "SELECT commit_id FROM indexed_files ORDER BY commit_id LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
        }
    } else {
        None
    };
    let live_head = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|head| head.trim().to_owned());
    let porcelain = git(&["status", "--porcelain", "--untracked-files=normal"])?;
    let dirty = !porcelain.is_empty();
    let (mut staged_changes, mut worktree_changes, mut untracked_files) = (false, false, false);
    for line in porcelain
        .split(|byte| *byte == b'\n')
        .filter(|line| line.len() >= 2)
    {
        staged_changes |= line[0] != b' ' && line[0] != b'?';
        worktree_changes |= line[1] != b' ' && line[0] != b'?';
        untracked_files |= line[0..2] == *b"??";
    }
    let staged_coverage = if staged_changes {
        "present-unindexed"
    } else {
        "clean"
    };
    let worktree_coverage = if worktree_changes {
        "present-unindexed"
    } else {
        "clean"
    };
    let untracked_coverage = if untracked_files {
        "present-unindexed"
    } else {
        "clean"
    };
    let (unit_count, unsupported_count) = if database_exists {
        let c = Connection::open(db_path)?;
        let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version >= 2 {
            let units = c
                .query_row("SELECT count(*) FROM indexed_units", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_or(0);
            let unsupported = c
                .query_row(
                    "SELECT count(*) FROM blob_contents WHERE status != 'text'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0);
            (units, unsupported)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };
    let content_state = if !database_exists {
        "missing"
    } else if unit_count == 0 && unsupported_count > 0 {
        "unsupported"
    } else if unit_count == 0 {
        "empty"
    } else {
        "usable"
    };
    let state = if content_state == "usable"
        && indexed_head
            .as_deref()
            .is_some_and(|head| Some(head) != live_head.as_deref())
    {
        "stale"
    } else {
        content_state
    };
    let exact = database_exists && state == "usable";
    let embedder = config::embedding_command()
        .ok()
        .and_then(|command| resolved_executable(&command));
    let semantic_ready = exact && embedder.is_some();
    let pdftotext = resolved_executable("pdftotext").is_some();
    let unavailable_reason = if !database_exists {
        "database is missing"
    } else if state == "unsupported" {
        "index contains only unsupported content"
    } else {
        "index has no searchable units"
    };
    let readiness = serde_json::json!({
        "exact": capability(exact, (!exact).then(|| unavailable_reason.to_owned())),
        "semantic": capability(semantic_ready, (!semantic_ready).then(|| if !exact { unavailable_reason.to_owned() } else { "embedding command is missing".to_owned() })),
        "refresh": capability(embedder.is_some(), embedder.is_none().then(|| "embedding command is missing".to_owned())),
        "document_import": capability(pdftotext, (!pdftotext).then(|| "pdftotext is missing".to_owned()))
    });
    Ok(serde_json::json!({
        "database": { "path": db_path, "state": state, "content_state": content_state },
        "indexed_head": indexed_head,
        "live_head": live_head,
        "worktree": { "dirty": dirty, "staged_coverage": staged_coverage, "worktree_coverage": worktree_coverage, "untracked_coverage": untracked_coverage },
        "indexed_units": { "total": unit_count, "unsupported": unsupported_count },
        "capabilities": readiness
    }))
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
                    first_retrieved_at: None,
                    last_retrieved_at: None,
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
            first_retrieved_at: None,
            last_retrieved_at: None,
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
    let first = a.next().ok_or("usage: repodb [--database <path>] <init|refresh [--imports|--changed|--update COMMIT]|import|retention|reembed|search|status --json> (default database: <git-dir>/repodb.sqlite)")?;
    let (option, cmd) = if first == "--database" {
        let path = a.next().ok_or("--database requires a path")?;
        (Some(path), a.next().ok_or("--database requires a command")?)
    } else {
        (None, first)
    };
    let db_path = selected_database(option)?;
    match cmd.as_str() {
        "status" => {
            let json = match a.next().as_deref() {
                None => false,
                Some("--json") if a.next().is_none() => true,
                _ => return Err("usage: repodb status [--json]".into()),
            };
            let report = status(&db_path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                let database = &report["database"];
                let state = database["state"].as_str().unwrap_or("unknown");
                println!("Database: {state}");
                println!(
                    "Indexed HEAD: {}",
                    report["indexed_head"].as_str().unwrap_or("none")
                );
                println!(
                    "Live HEAD: {}",
                    report["live_head"].as_str().unwrap_or("unavailable")
                );
                println!("Worktree dirty: {}", report["worktree"]["dirty"]);
                for capability in ["exact", "semantic", "refresh", "document_import"] {
                    let entry = &report["capabilities"][capability];
                    let name = match capability {
                        "exact" => "Exact search",
                        "semantic" => "Semantic search",
                        "refresh" => "Refresh",
                        _ => "Document import",
                    };
                    println!(
                        "{name}: {}{}",
                        if entry["ready"] == true {
                            "ready"
                        } else {
                            "not ready"
                        },
                        entry["reason"]
                            .as_str()
                            .map(|reason| format!(" ({reason})"))
                            .unwrap_or_default()
                    );
                }
            }
        }
        "init" | "refresh" => {
            let mut arguments = Vec::new();
            let mut size_limit = imports::SizeLimit::Error;
            let mut size_limit_supplied = false;
            for argument in a {
                if let Some(value) = argument.strip_prefix("--size-limit=") {
                    if size_limit_supplied {
                        return Err("--size-limit may be specified only once".into());
                    }
                    size_limit_supplied = true;
                    size_limit = match value {
                        "error" => imports::SizeLimit::Error,
                        "warn" => imports::SizeLimit::Warn,
                        "ignore" => imports::SizeLimit::Ignore,
                        _ => return Err("--size-limit must be error, warn, or ignore".into()),
                    };
                } else {
                    arguments.push(argument);
                }
            }
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
                || (size_limit_supplied && (changed || update.is_some()))
            {
                return Err(
                    "usage: repodb refresh [--imports | --changed | --update COMMIT] [--size-limit=error|warn|ignore]".into(),
                );
            }
            let mut db = open_db(&db_path)?;
            let mut size_assessments = Vec::new();
            if imports_only {
                let version = history::snapshot_version(&db)?;
                size_assessments = imports::refresh_with_size_limit(&db, size_limit)?;
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
                size_assessments = imports::refresh_with_size_limit(&db, size_limit)?;
                history::prepare_refresh(&db)?;
                imports::validate_dimensions(&db)?;
                let tx = history::publish_transaction(&mut db, version)?;
                imports::publish(&tx)?;
                history::publish_refresh(&tx)?;
                tx.commit()?;
            }
            for assessment in size_assessments
                .into_iter()
                .filter(|assessment| assessment.warn_large)
            {
                println!(
                    "large import warning: source bytes={}, estimated net database growth={}, warning basis={} ({})",
                    assessment.source_bytes,
                    assessment
                        .estimated_net_db_growth_bytes
                        .map_or_else(|| "unknown".into(), |value| value.to_string()),
                    assessment.warning_basis_bytes,
                    assessment.warning_basis,
                );
            }
        }
        "import" => {
            let first = a
                .next()
                .ok_or("import requires one local file, URL, or Git repository")?;
            if first == "--preview" {
                let path = a.next().ok_or(
                    "import --preview requires a local file, directory, or HTTP(S) page URL",
                )?;
                if a.next().is_some() {
                    return Err(
                        "import --preview accepts one local file, directory, or HTTP(S) page URL"
                            .into(),
                    );
                }
                let preview = if path.starts_with("http://") || path.starts_with("https://") {
                    if path.ends_with(".git") {
                        return Err("import --preview does not preview Git repositories".into());
                    }
                    imports::preview_url(&path, &db_path)?
                } else {
                    imports::preview(std::path::Path::new(&path), &db_path)?
                };
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
            let mut include_diffs = false;
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
                    "--diffs" => include_diffs = true,
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
            let has_commit_message_index: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='commit_message_units')",
                [],
                |row| row.get(0),
            )?;
            if history::is_current(&c)? && has_commit_message_index {
                result.extend(history::search_commit_messages(
                    &c, &q, mode, limit, &commits,
                )?);
            }
            result.retain(|candidate| result_scope.matches(candidate));
            if mode == "semantic" || mode == "combined" {
                result.sort_by(|a, b| {
                    a.distance
                        .unwrap_or(f64::INFINITY)
                        .total_cmp(&b.distance.unwrap_or(f64::INFINITY))
                        .then((a.path == "[commit message]").cmp(&(b.path == "[commit message]")))
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
            let mut diff_results = Vec::new();
            if include_diffs {
                if !history::is_current(&c)? {
                    return Err(
                        "diff search requires a current history index; run `repodb refresh`".into(),
                    );
                }
                let has_diff_index: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='diff_references')",
                    [],
                    |row| row.get(0),
                )?;
                if !has_diff_index {
                    return Err("diff search is not indexed; run `repodb refresh`".into());
                }
                let include_diff = |reference: &history::DiffReference| {
                    let path = match reference.side {
                        history::DiffSide::Added => reference.new_path.as_deref(),
                        history::DiffSide::Removed => reference.old_path.as_deref(),
                    };
                    path.is_some_and(|path| result_scope.matches_diff(path))
                };
                let search_diff_results =
                    |search_mode, search_limit| -> Result<Vec<history::DiffSearchResult>> {
                        if matches!(result_scope, SearchScope::All) {
                            history::search_diffs(&c, &q, search_mode, search_limit, &commits)
                        } else {
                            history::search_diffs_filtered(
                                &c,
                                &q,
                                search_mode,
                                search_limit,
                                &commits,
                                &include_diff,
                            )
                        }
                    };
                diff_results = match mode {
                    "exact" => search_diff_results(history::DiffSearchMode::Exact, usize::MAX)?,
                    "semantic" => search_diff_results(history::DiffSearchMode::Semantic, limit)?,
                    "combined" => {
                        let semantic =
                            search_diff_results(history::DiffSearchMode::Semantic, limit)?;
                        let exact =
                            search_diff_results(history::DiffSearchMode::Exact, usize::MAX)?;
                        let mut seen = std::collections::HashSet::new();
                        let mut hits = Vec::new();
                        for hit in semantic.into_iter().chain(exact) {
                            let reference = &hit.reference;
                            let identity = (
                                reference.parent_commit_id.clone(),
                                reference.child_commit_id.clone(),
                                reference.hunk_id,
                                reference.unit_id,
                            );
                            if seen.insert(identity) {
                                hits.push(hit);
                            }
                        }
                        hits
                    }
                    _ => Vec::new(),
                };
            }
            if json {
                let mut values = result
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                values.extend(diff_results.iter().map(diff_search_json));
                for (value, result) in values.iter_mut().zip(&result) {
                    if result.path == "[commit message]" {
                        value["kind"] = serde_json::Value::String("commit_message".to_owned());
                        if let Some(commit) = &result.commit_id {
                            value["source_reference"] = serde_json::Value::String(format!(
                                "git show --no-patch --format=%B {commit}"
                            ));
                        }
                    } else if let Some(hex) = decoded_git_path_hex(&result.path) {
                        value["path"] = serde_json::Value::String(format!("bytes:{hex}"));
                        value["path_bytes_hex"] = serde_json::Value::String(hex.to_owned());
                    }
                }
                println!("{}", serde_json::to_string_pretty(&values)?)
            } else {
                for x in result {
                    let mut reference = String::new();
                    if let Some(commit) = x.commit_id.as_deref() {
                        reference.push_str(&format!(" [commit {commit}]"));
                    }
                    if let Some(blob) = x.blob_id {
                        reference.push_str(&format!(" [blob {blob}]"));
                    }
                    if let Some(repo_id) = x.repo_id {
                        reference.push_str(&format!(" [repo {repo_id}]"));
                    }
                    if let Some(first) = x.first_retrieved_at {
                        reference.push_str(&format!(" [first retrieved {first}]"));
                    }
                    if let Some(last) = x.last_retrieved_at {
                        reference.push_str(&format!(" [last retrieved {last}]"));
                    }
                    let path = readable_path(&x.path);
                    if x.path == "[commit message]" {
                        if let Some(commit) = x.commit_id.as_deref() {
                            reference.push_str(&format!(
                                " [source: git show --no-patch --format=%B {commit}]"
                            ));
                        }
                    }
                    if let Some(page) = x.page {
                        println!("{}:page {}{}: {}", path, page, reference, x.text);
                    } else {
                        println!(
                            "{}:{}-{}{}: {}",
                            path, x.line_start, x.line_end, reference, x.text
                        );
                    }
                }
                for hit in diff_results {
                    let reference = &hit.reference;
                    let (side, matching) = match reference.side {
                        history::DiffSide::Added => (
                            "added",
                            diff_endpoint(
                                reference.new_path.as_deref(),
                                Some(reference.line_start),
                                Some(reference.line_end),
                            ),
                        ),
                        history::DiffSide::Removed => (
                            "removed",
                            diff_endpoint(
                                reference.old_path.as_deref(),
                                Some(reference.line_start),
                                Some(reference.line_end),
                            ),
                        ),
                    };
                    let old = diff_endpoint(
                        reference.old_path.as_deref(),
                        reference.old_line_start,
                        reference.old_line_end,
                    );
                    let new = diff_endpoint(
                        reference.new_path.as_deref(),
                        reference.new_line_start,
                        reference.new_line_end,
                    );
                    let distance = hit
                        .distance
                        .map(|distance| format!(" [embedding distance {distance}]"))
                        .unwrap_or_default();
                    println!(
                        "diff {side} match={matching} [parent {} child {} hunk {}] old={old} new={new}{distance}: {}",
                        reference.parent_commit_id,
                        reference.child_commit_id,
                        reference.hunk_id,
                        hit.text,
                    );
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
