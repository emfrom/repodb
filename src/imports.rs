use crate::{Result, SearchResult, as_bytes, embed};
use rusqlite::{Connection, params};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
};

const MAX_URL_BYTES: usize = 10 * 1024 * 1024;

fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS imported_sources(id TEXT PRIMARY KEY, locator TEXT NOT NULL UNIQUE, kind TEXT NOT NULL); CREATE TABLE IF NOT EXISTS imported_versions(source_id TEXT NOT NULL, version TEXT NOT NULL, content TEXT NOT NULL, fetched_order INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(source_id,version)); CREATE TABLE IF NOT EXISTS imported_units(source_id TEXT NOT NULL, version TEXT NOT NULL, line INTEGER NOT NULL, text TEXT NOT NULL, vector BLOB, page INTEGER, PRIMARY KEY(source_id,version,line)); CREATE TABLE IF NOT EXISTS import_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    if !column_exists(c, "imported_units", "page")? {
        c.execute("ALTER TABLE imported_units ADD COLUMN page INTEGER", [])?;
    }
    if !column_exists(c, "imported_versions", "fetched_order")? {
        c.execute(
            "ALTER TABLE imported_versions ADD COLUMN fetched_order INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        c.execute("UPDATE imported_versions SET fetched_order=rowid", [])?;
    }
    c.execute(
        "INSERT OR IGNORE INTO import_config VALUES('retention','latest')",
        [],
    )?;
    Ok(())
}

fn column_exists(c: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(names
        .collect::<std::result::Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column))
}

fn checksum(bytes: &[u8]) -> String {
    format!(
        "{:x}",
        bytes.iter().fold(14695981039346656037u64, |h, b| {
            (h ^ *b as u64).wrapping_mul(1099511628211)
        })
    )
}

fn identity(locator: &str) -> String {
    // Stable, reversible identity is not exposed as a filesystem path.
    format!(
        "import:{}",
        locator
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

pub fn register(c: &Connection, locator: &str) -> Result<()> {
    let is_url = locator.starts_with("http://") || locator.starts_with("https://");
    if !is_url {
        let path = Path::new(locator);
        if !path.is_file() {
            return Err("imports must name an individual local file or HTTP(S) URL; directories are not supported".into());
        }
    }
    init(c)?;
    let kind = if is_url { "url" } else { "file" };
    c.execute(
        "INSERT OR IGNORE INTO imported_sources VALUES(?1,?2,?3)",
        params![identity(locator), locator, kind],
    )?;
    Ok(())
}

pub fn set_retention(c: &Connection, value: &str) -> Result<()> {
    if value != "latest" && value != "all" {
        return Err("retention must be latest or all".into());
    }
    init(c)?;
    c.execute(
        "UPDATE import_config SET value=?1 WHERE key='retention'",
        [value],
    )?;
    if value == "latest" {
        c.execute("DELETE FROM imported_versions WHERE EXISTS (SELECT 1 FROM imported_versions newer WHERE newer.source_id=imported_versions.source_id AND newer.fetched_order>imported_versions.fetched_order)", [])?;
        c.execute("DELETE FROM imported_units WHERE NOT EXISTS (SELECT 1 FROM imported_versions v WHERE v.source_id=imported_units.source_id AND v.version=imported_units.version)", [])?;
    }
    Ok(())
}

pub fn refresh(c: &Connection) -> Result<()> {
    init(c)?;
    let mut st = c.prepare("SELECT id,locator,kind FROM imported_sources ORDER BY id")?;
    let sources = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(st);
    let cfg: Option<(String,String)> = c.query_row("SELECT (SELECT value FROM embedding_config WHERE key='command'),(SELECT value FROM embedding_config WHERE key='model')", [], |r| Ok((r.get(0)?,r.get(1)?))).ok();
    let configured_dimension = c
        .query_row(
            "SELECT value FROM embedding_config WHERE key='dimension'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|d| *d != 0);
    let retention: String = c.query_row(
        "SELECT value FROM import_config WHERE key='retention'",
        [],
        |r| r.get(0),
    )?;
    let cache_dir: String = c.query_row("PRAGMA database_list", [], |r| r.get(2))?;
    let cache_dir = Path::new(&cache_dir)
        .parent()
        .ok_or("database has no parent directory")?
        .join("repodb-cache");
    let tx = c.unchecked_transaction()?;
    for (id, locator, kind) in sources {
        let source_bytes = if kind == "file" {
            fs::read(&locator)?
        } else {
            let mut child = Command::new("curl")
                .args([
                    "--fail",
                    "--location",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "30",
                    "--max-filesize",
                    "10485760",
                    "--proto-redir",
                    "=http,https",
                    &locator,
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let mut response = Vec::with_capacity(MAX_URL_BYTES.min(64 * 1024));
            child
                .stdout
                .take()
                .unwrap()
                .take((MAX_URL_BYTES + 1) as u64)
                .read_to_end(&mut response)?;
            let too_large = response.len() > MAX_URL_BYTES;
            if too_large {
                let _ = child.kill();
            }
            let output = child.wait_with_output()?;
            if too_large {
                return Err(
                    format!("URL response exceeds {MAX_URL_BYTES} bytes: {locator}").into(),
                );
            }
            if !output.status.success() {
                return Err(format!(
                    "URL fetch failed for {locator}: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
                .into());
            }
            response
        };
        let is_pdf = source_bytes
            .windows(5)
            .take(1024)
            .any(|window| window == b"%PDF-")
            || Path::new(&locator)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
        let pages = if is_pdf {
            let source_version = checksum(&source_bytes);
            let cache = cache_dir.join(format!("{source_version}.json"));
            if cache.is_file() {
                serde_json::from_slice::<Vec<String>>(&fs::read(cache)?)?
            } else {
                let mut child = Command::new("pdftotext")
                    .args(["-layout", "-enc", "UTF-8", "-", "-"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?;
                child.stdin.take().unwrap().write_all(&source_bytes)?;
                let output = child.wait_with_output()?;
                if !output.status.success() {
                    return Err(format!(
                        "PDF conversion failed for {locator}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )
                    .into());
                }
                let text = String::from_utf8(output.stdout)?;
                let pages = text.split('\u{c}').map(str::to_owned).collect::<Vec<_>>();
                fs::create_dir_all(&cache_dir)?;
                fs::write(cache, serde_json::to_vec(&pages)?)?;
                pages
            }
        } else {
            vec![String::from_utf8(source_bytes.clone())?]
        };
        let content = if is_pdf {
            pages.join("\n")
        } else {
            pages[0].clone()
        };
        let version = checksum(content.as_bytes());
        let mut units = Vec::new();
        let mut dimension = None;
        let lines = if is_pdf {
            pages
                .iter()
                .enumerate()
                .map(|(page, text)| (page + 1, Some(page + 1), text.as_str()))
                .collect::<Vec<_>>()
        } else {
            content
                .lines()
                .enumerate()
                .map(|(line, text)| (line + 1, None, text))
                .collect::<Vec<_>>()
        };
        for (line, page, text) in lines {
            if text.trim().is_empty() {
                continue;
            }
            let vector = cfg
                .as_ref()
                .map(|(cmd, model)| embed(cmd, model, text))
                .transpose()?;
            if let Some(vector) = &vector {
                if dimension.is_some_and(|d| d != vector.len())
                    || configured_dimension.is_some_and(|d| d != vector.len())
                {
                    return Err("embedding dimension mismatch".into());
                }
                dimension = Some(vector.len());
            }
            units.push((line, text.to_owned(), vector.map(|v| as_bytes(&v)), page));
        }
        let fetched_order: i64 = tx.query_row(
            "SELECT COALESCE(MAX(fetched_order),0)+1 FROM imported_versions",
            [],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO imported_versions(source_id,version,content,fetched_order) VALUES(?1,?2,?3,?4) ON CONFLICT(source_id,version) DO UPDATE SET content=excluded.content,fetched_order=excluded.fetched_order",
            params![id, version, content, fetched_order],
        )?;
        for (line, text, vector, page) in units {
            tx.execute(
                "INSERT OR REPLACE INTO imported_units(source_id,version,line,text,vector,page) VALUES(?1,?2,?3,?4,?5,?6)",
                params![id, version, line, text, vector, page],
            )?;
        }
        if retention == "latest" {
            tx.execute(
                "DELETE FROM imported_versions WHERE source_id=?1 AND fetched_order<>?2",
                params![id, fetched_order],
            )?;
            tx.execute(
                "DELETE FROM imported_units WHERE source_id=?1 AND version NOT IN (SELECT version FROM imported_versions WHERE source_id=?1)",
                [&id],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn reembed(c: &Connection, command: &str, model: &str) -> Result<Option<usize>> {
    init(c)?;
    let rows = {
        let mut stmt = c.prepare("SELECT source_id,version,line,text FROM imported_units ORDER BY source_id,version,line")?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, usize>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut vectors = Vec::with_capacity(rows.len());
    let mut dimension = None;
    for (_, _, _, text) in &rows {
        let vector = embed(command, model, text)?;
        if dimension.is_some_and(|d| d != vector.len()) {
            return Err("embedding dimension mismatch".into());
        }
        dimension = Some(vector.len());
        vectors.push(as_bytes(&vector));
    }
    for ((source, version, line, _), vector) in rows.iter().zip(vectors) {
        c.execute(
            "UPDATE imported_units SET vector=?1 WHERE source_id=?2 AND version=?3 AND line=?4",
            params![vector, source, version, line],
        )?;
    }
    Ok(dimension)
}

pub fn search(c: &Connection, query: &str, mode: &str, limit: usize) -> Result<Vec<SearchResult>> {
    init(c)?;
    let semantic = mode != "exact";
    let mut vector = None;
    if semantic {
        let (cmd,model): (String,String) = c.query_row("SELECT (SELECT value FROM embedding_config WHERE key='command'),(SELECT value FROM embedding_config WHERE key='model')", [], |r| Ok((r.get(0)?,r.get(1)?))).map_err(|_| "no embeddings; run reembed")?;
        vector = Some(as_bytes(&embed(&cmd, &model, query)?));
        let dimension: i64 = c
            .query_row(
                "SELECT value FROM embedding_config WHERE key='dimension'",
                [],
                |r| r.get::<_, String>(0),
            )?
            .parse()?;
        if dimension != 0 && vector.as_ref().unwrap().len() != dimension as usize * 4 {
            return Err("query embedding dimension mismatch".into());
        }
    }
    let mut st = c.prepare("SELECT s.locator,u.line,u.text,u.version,u.vector,u.page FROM imported_units u JOIN imported_sources s ON s.id=u.source_id")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, usize>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<Vec<u8>>>(4)?,
            r.get::<_, Option<usize>>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (locator, line, text, version, stored, page) = row?;
        if semantic {
            if let (Some(v), Some(q)) = (stored.as_deref(), vector.as_deref()) {
                if v.len() != q.len() {
                    return Err("imported embedding dimension mismatch".into());
                }
                let d: f64 =
                    c.query_row("SELECT vec_distance_cosine(?1,?2)", params![v, q], |r| {
                        r.get(0)
                    })?;
                out.push((
                    d,
                    SearchResult {
                        path: locator,
                        line_start: line,
                        line_end: line,
                        text,
                        commit_id: None,
                        distance: Some(d),
                        blob_id: Some(version),
                        page,
                    },
                ));
            }
        } else if text.to_lowercase().contains(&query.to_lowercase()) {
            out.push((
                0.0,
                SearchResult {
                    path: locator,
                    line_start: line,
                    line_end: line,
                    text,
                    commit_id: None,
                    distance: None,
                    blob_id: Some(version),
                    page,
                },
            ));
        }
    }
    if semantic {
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        if mode == "combined" {
            let mut rank = 0;
            out.retain(|(_, result)| {
                let keep =
                    rank < limit || result.text.to_lowercase().contains(&query.to_lowercase());
                rank += 1;
                keep
            });
        } else {
            out.truncate(limit);
        }
    }
    Ok(out.into_iter().map(|x| x.1).collect())
}
