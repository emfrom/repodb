use crate::{Result, SearchResult, as_bytes, embed};
use rusqlite::{Connection, params};
use std::{fs, path::Path, process::Command};

fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS imported_sources(id TEXT PRIMARY KEY, locator TEXT NOT NULL UNIQUE, kind TEXT NOT NULL); CREATE TABLE IF NOT EXISTS imported_versions(source_id TEXT NOT NULL, version TEXT NOT NULL, content TEXT NOT NULL, PRIMARY KEY(source_id,version)); CREATE TABLE IF NOT EXISTS imported_units(source_id TEXT NOT NULL, version TEXT NOT NULL, line INTEGER NOT NULL, text TEXT NOT NULL, vector BLOB, PRIMARY KEY(source_id,version,line)); CREATE TABLE IF NOT EXISTS import_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    c.execute(
        "INSERT OR IGNORE INTO import_config VALUES('retention','latest')",
        [],
    )?;
    Ok(())
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
        c.execute("DELETE FROM imported_versions WHERE version NOT IN (SELECT max(version) FROM imported_versions GROUP BY source_id)", [])?;
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
    let retention: String = c.query_row(
        "SELECT value FROM import_config WHERE key='retention'",
        [],
        |r| r.get(0),
    )?;
    let tx = c.unchecked_transaction()?;
    for (id, locator, kind) in sources {
        let content = if kind == "file" {
            fs::read_to_string(&locator)?
        } else {
            let output = Command::new("curl")
                .args(["--fail", "--location", "--silent", "--show-error", &locator])
                .output()?;
            if !output.status.success() {
                return Err(format!(
                    "URL fetch failed for {locator}: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
                .into());
            }
            String::from_utf8(output.stdout)?
        };
        let version = format!(
            "{:x}",
            content
                .as_bytes()
                .iter()
                .fold(14695981039346656037u64, |h, b| (h ^ *b as u64)
                    .wrapping_mul(1099511628211))
        );
        let mut units = Vec::new();
        for (i, line) in content.lines().enumerate() {
            let vector = cfg
                .as_ref()
                .map(|(cmd, model)| embed(cmd, model, line))
                .transpose()?;
            units.push((i + 1, line.to_owned(), vector.map(|v| as_bytes(&v))));
        }
        tx.execute(
            "INSERT OR IGNORE INTO imported_versions VALUES(?1,?2,?3)",
            params![id, version, content],
        )?;
        for (line, text, vector) in units {
            tx.execute(
                "INSERT OR REPLACE INTO imported_units VALUES(?1,?2,?3,?4,?5)",
                params![id, version, line, text, vector],
            )?;
        }
        if retention == "latest" {
            tx.execute(
                "DELETE FROM imported_versions WHERE source_id=?1 AND version<>?2",
                params![id, version],
            )?;
            tx.execute(
                "DELETE FROM imported_units WHERE source_id=?1 AND version<>?2",
                params![id, version],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn search(c: &Connection, query: &str, mode: &str, limit: usize) -> Result<Vec<SearchResult>> {
    init(c)?;
    let semantic = mode != "exact";
    let mut vector = None;
    if semantic {
        let (cmd,model): (String,String) = c.query_row("SELECT (SELECT value FROM embedding_config WHERE key='command'),(SELECT value FROM embedding_config WHERE key='model')", [], |r| Ok((r.get(0)?,r.get(1)?))).map_err(|_| "no embeddings; run reembed")?;
        vector = Some(as_bytes(&embed(&cmd, &model, query)?));
    }
    let mut st = c.prepare("SELECT s.locator,u.line,u.text,u.version,u.vector FROM imported_units u JOIN imported_sources s ON s.id=u.source_id")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, usize>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<Vec<u8>>>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (locator, line, text, version, stored) = row?;
        if semantic {
            if let (Some(v), Some(q)) = (stored.as_deref(), vector.as_deref()) {
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
                },
            ));
        }
    }
    if semantic {
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.truncate(limit);
    }
    Ok(out.into_iter().map(|x| x.1).collect())
}
