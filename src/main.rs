use rusqlite::{Connection, ffi::sqlite3_auto_extension, params};
use serde::Serialize;
use std::{
    env,
    error::Error,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

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
    c.execute_batch("CREATE TABLE IF NOT EXISTS indexed_files(path TEXT PRIMARY KEY NOT NULL,commit_id TEXT NOT NULL,content TEXT NOT NULL); CREATE TABLE IF NOT EXISTS embeddings(path TEXT NOT NULL,line INTEGER NOT NULL,text TEXT NOT NULL,commit_id TEXT NOT NULL,vector BLOB NOT NULL,PRIMARY KEY(path,line)); CREATE TABLE IF NOT EXISTS embedding_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
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
fn refresh(c: &mut Connection) -> Result<()> {
    let commit = String::from_utf8(git(&["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned();
    let tree = git(&["ls-tree", "-r", "-z", "--full-tree", "HEAD"])?;
    let mut files = vec![];
    for r in tree.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Some(t) = r.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let meta = String::from_utf8_lossy(&r[..t]);
        let mut f = meta.split_whitespace();
        let _ = f.next();
        let kind = f.next();
        let oid = f.next();
        let Ok(path) = std::str::from_utf8(&r[t + 1..]) else {
            continue;
        };
        if kind != Some("blob") || path.starts_with(".git/") {
            continue;
        }
        let Some(oid) = oid else { continue };
        let b = git(&["cat-file", "blob", oid])?;
        if b.contains(&0) {
            continue;
        }
        if let Ok(text) = String::from_utf8(b) {
            files.push((path.to_owned(), text))
        }
    }
    let cfg:Option<(String,String)>=c.query_row("SELECT (SELECT value FROM embedding_config WHERE key='command'),(SELECT value FROM embedding_config WHERE key='model')",[],|r|Ok((r.get(0)?,r.get(1)?))).ok();
    let tx = c.transaction()?;
    tx.execute("DELETE FROM indexed_files", [])?;
    {
        let mut s = tx.prepare("INSERT INTO indexed_files VALUES(?1,?2,?3)")?;
        for (p, t) in &files {
            s.execute(params![p, commit, t])?;
        }
    }
    tx.execute("DELETE FROM embeddings", [])?;
    if let Some((command, model)) = cfg {
        let mut rows = vec![];
        let mut dim = None;
        for (p, text) in &files {
            for (i, line) in text.lines().enumerate() {
                let v = embed(&command, &model, line)?;
                if dim.is_some_and(|d| d != v.len()) {
                    return Err("embedding dimension mismatch".into());
                }
                dim = Some(v.len());
                rows.push((p.clone(), i + 1, line.to_owned(), commit.clone(), v));
            }
        }
        let mut s = tx.prepare("INSERT INTO embeddings VALUES(?1,?2,?3,?4,?5)")?;
        for (p, i, t, id, v) in rows {
            s.execute(params![p, i, t, id, as_bytes(&v)])?;
        }
        tx.execute(
            "UPDATE embedding_config SET value=?1 WHERE key='dimension'",
            [dim.unwrap_or(0).to_string()],
        )?;
        tx.execute(
            "UPDATE embedding_config SET value=?1 WHERE key='model'",
            [model],
        )?;
    }
    tx.commit()?;
    Ok(())
}
fn as_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn reembed(c: &mut Connection, command: &str, model: &str) -> Result<()> {
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
    let tx = c.transaction()?;
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
fn exact(c: &Connection, q: &str) -> Result<Vec<SearchResult>> {
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
        for (i, l) in content.lines().enumerate() {
            if l.to_lowercase().contains(&q.to_lowercase()) {
                out.push(SearchResult {
                    path: p.clone(),
                    line_start: i + 1,
                    line_end: i + 1,
                    text: l.to_owned(),
                    commit_id: Some(id.clone()),
                    distance: None,
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
    if v.len() as i64 != dim {
        return Err("query embedding dimension mismatch".into());
    }
    Ok(v)
}
fn semantic_with_vector(c: &Connection, v: &[f32], limit: usize) -> Result<Vec<SearchResult>> {
    let mut s=c.prepare("SELECT path,line,text,commit_id,vec_distance_cosine(vector,?1) d FROM embeddings ORDER BY d,path,line LIMIT ?2")?;
    let rows = s.query_map(params![as_bytes(v), limit as i64], |r| {
        Ok(SearchResult {
            path: r.get(0)?,
            line_start: r.get(1)?,
            line_end: r.get(1)?,
            text: r.get(2)?,
            commit_id: Some(r.get(3)?),
            distance: Some(r.get(4)?),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
fn semantic(c: &Connection, q: &str, limit: usize) -> Result<Vec<SearchResult>> {
    semantic_with_vector(c, &query_vector(c, q)?, limit)
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
            refresh(&mut open_db()?)?
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
            reembed(&mut open_db()?, &command, &model)?
        }
        "search" => {
            let mut query = None;
            let (mut json, mut mode, mut limit) = (false, "exact", 20usize);
            while let Some(x) = a.next() {
                match x.as_str() {
                    "--json" => json = true,
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
            let c = open_db()?;
            let result = if mode == "exact" {
                exact(&c, &q)?
            } else if mode == "combined" {
                let v = query_vector(&c, &q)?;
                let mut candidates = semantic_with_vector(&c, &v, limit)?;
                let mut seen = std::collections::HashSet::new();
                for x in &candidates {
                    seen.insert((x.path.clone(), x.line_start));
                }
                for mut x in exact(&c, &q)? {
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
                semantic(&c, &q, limit)?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?)
            } else {
                for x in result {
                    println!("{}:{}-{}: {}", x.path, x.line_start, x.line_end, x.text)
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
