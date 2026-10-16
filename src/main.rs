use rusqlite::{Connection, ffi::sqlite3_auto_extension, params};
use serde::Serialize;
use std::{
    env,
    error::Error,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

mod history;
mod imports;
mod segmentation;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

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
}
fn git(args: &[&str]) -> Result<Vec<u8>> {
    let o = Command::new("git").args(args).output()?;
    if !o.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        )
        .into());
    }
    Ok(o.stdout)
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
fn open_db() -> Result<Connection> {
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
    let c = Connection::open(database()?)?;
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

fn is_builtin_embedding_command(command: &str) -> bool {
    let Ok(home) = env::var("HOME") else {
        return false;
    };
    let builtin = PathBuf::from(home).join("bin/generate_embeddings");
    matches!((builtin.canonicalize().ok(), resolved_executable(command)), (Some(a), Some(b)) if a == b)
}

fn embed(command: &str, model: &str, text: &str) -> Result<Vec<f32>> {
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
    child.stdin.take().unwrap().write_all(text.as_bytes())?;
    let o = child.wait_with_output()?;
    if !o.status.success() {
        return Err(format!(
            "embedding command failed: {}",
            String::from_utf8_lossy(&o.stderr)
        )
        .into());
    }
    let v: Vec<f32> = serde_json::from_slice(&o.stdout)?;
    validate_vector(&v)?;
    Ok(v)
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
        .map_err(|_| "no embeddings; run reembed")?;
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
) -> Result<Vec<SearchResult>> {
    let mut s=c.prepare("SELECT path,line,text,commit_id,vec_distance_cosine(vector,?1) d FROM embeddings WHERE commit_id IN (SELECT value FROM json_each(?3)) ORDER BY d,path,line LIMIT ?2")?;
    let rows = s.query_map(
        params![as_bytes(v), limit as i64, serde_json::to_string(commits)?],
        |r| {
            Ok(SearchResult {
                path: r.get(0)?,
                line_start: r.get(1)?,
                line_end: r.get(1)?,
                text: r.get(2)?,
                commit_id: Some(r.get(3)?),
                distance: Some(r.get(4)?),
                blob_id: None,
            })
        },
    )?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
fn semantic(
    c: &Connection,
    q: &str,
    limit: usize,
    commits: &[String],
) -> Result<Vec<SearchResult>> {
    semantic_with_vector(c, &query_vector(c, q)?, limit, commits)
}
fn execute() -> Result<()> {
    let mut a = env::args().skip(1);
    let Some(cmd) = a.next() else {
        return Err("usage: repodb <init|refresh|reembed|search>".into());
    };
    match cmd.as_str() {
        "init" | "refresh" => {
            if a.next().is_some() {
                return Err("unexpected arguments".into());
            }
            let mut db = open_db()?;
            history::refresh(&mut db)?;
            imports::refresh(&db)?;
        }
        "import" => {
            let locator = a.next().ok_or("import requires one local file or URL")?;
            if a.next().is_some() {
                return Err("import accepts one file or URL; directories are not supported".into());
            }
            imports::register(&open_db()?, &locator)?;
        }
        "retention" => {
            let value = a.next().ok_or("retention requires latest or all")?;
            if a.next().is_some() {
                return Err("unexpected arguments".into());
            }
            imports::set_retention(&open_db()?, &value)?;
        }
        "reembed" => {
            let mut command = env::var("HOME").map(|h| format!("{h}/bin/generate_embeddings"))?;
            let mut model = "all-MiniLM-L6-v2".to_owned();
            while let Some(x) = a.next() {
                match x.as_str() {
                    "--command" => command = a.next().ok_or("--command requires path")?,
                    "--model" => model = a.next().ok_or("--model requires id")?,
                    _ => return Err(format!("unexpected argument {x}").into()),
                }
            }
            let mut c = open_db()?;
            if history::is_current(&c)? {
                history::reembed(&mut c, &command, &model)?;
            } else {
                reembed(&mut c, &command, &model)?;
            }
        }
        "search" => {
            let mut query = None;
            let mut scope = history::Scope::default();
            let (mut json, mut mode, mut limit) = (false, "exact", 20usize);
            while let Some(x) = a.next() {
                match x.as_str() {
                    "--json" => json = true,
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
            let mut connection = open_db()?;
            // Scope, model configuration and results belong to one read snapshot.
            let c = connection.transaction()?;
            let commits = scope.resolve(&c)?;
            let mut result = if history::is_current(&c)? {
                history::search(&c, &q, mode, limit, &commits)?
            } else if mode == "exact" {
                exact(&c, &q, &commits)?
            } else if mode == "combined" {
                let v = query_vector(&c, &q)?;
                let mut candidates = semantic_with_vector(&c, &v, limit, &commits)?;
                let mut seen = std::collections::HashSet::new();
                for x in &candidates {
                    seen.insert((x.path.clone(), x.line_start));
                }
                for mut x in exact(&c, &q, &commits)? {
                    if seen.insert((x.path.clone(), x.line_start)) {
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
                semantic(&c, &q, limit, &commits)?
            };
            if mode == "exact" || mode == "semantic" {
                result.extend(imports::search(&c, &q, mode, limit)?);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?)
            } else {
                for x in result {
                    let mut reference = String::new();
                    if let Some(commit) = x.commit_id {
                        reference.push_str(&format!(" [commit {commit}]"));
                    }
                    if let Some(blob) = x.blob_id {
                        reference.push_str(&format!(" [blob {blob}]"));
                    }
                    println!(
                        "{}:{}-{}{}: {}",
                        x.path, x.line_start, x.line_end, reference, x.text
                    )
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
