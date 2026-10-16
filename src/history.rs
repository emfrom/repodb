use crate::{Result, SearchResult, as_bytes, embed, git, query_vector};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::{
    collections::BTreeMap,
    io::Write,
    process::{Command, Stdio},
};

pub fn is_current(c: &Connection) -> Result<bool> {
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    match version {
        0 => Ok(false),
        2 => Ok(true),
        _ => Err(format!("unsupported database version {version}").into()),
    }
}

pub fn snapshot_version(c: &Connection) -> Result<i64> {
    Ok(c.query_row("PRAGMA data_version", [], |r| r.get(0))?)
}

pub fn publish_transaction(c: &mut Connection, version: i64) -> Result<Transaction<'_>> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Preparation happens outside the write transaction. Reject stale work rather
    // than mixing newly published model configuration with older vectors/units.
    if snapshot_version(&tx)? != version {
        return Err("index changed during preparation; retry the command".into());
    }
    Ok(tx)
}

struct Blob {
    content: Option<String>,
    status: &'static str,
}
struct Unit {
    id: usize,
    blob_id: String,
    line: usize,
    text: String,
}
struct Occurrence {
    commit_id: String,
    blob_id: String,
    path: Vec<u8>,
}

fn head() -> Result<Option<String>> {
    let o = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()?;
    if o.status.success() {
        Ok(Some(String::from_utf8(o.stdout)?.trim().to_owned()))
    } else if o.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(format!("cannot read HEAD: {}", String::from_utf8_lossy(&o.stderr)).into())
    }
}

fn reachable_commits() -> Result<Vec<(String, i64)>> {
    // Capture immutable roots once; later ref changes cannot mix two snapshots.
    let mut roots = git(&["for-each-ref", "--format=%(objectname)"])?;
    if let Some(head) = head()? {
        roots.extend_from_slice(format!("{head}\n").as_bytes());
    }
    if roots.is_empty() {
        return Ok(vec![]);
    }
    let mut child = Command::new("git")
        .args(["rev-list", "--timestamp", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(&roots)?;
    let o = child.wait_with_output()?;
    if !o.status.success() {
        return Err(format!(
            "git rev-list failed: {}",
            String::from_utf8_lossy(&o.stderr)
        )
        .into());
    }
    String::from_utf8(o.stdout)?
        .lines()
        .map(|line| {
            let (timestamp, id) = line.split_once(' ').ok_or("invalid git commit record")?;
            Ok((id.to_owned(), timestamp.parse()?))
        })
        .collect()
}

fn configuration(c: &Connection) -> Result<Option<(String, String, usize)>> {
    let count: usize = c.query_row("SELECT count(*) FROM embedding_config", [], |r| r.get(0))?;
    if count == 0 {
        return Ok(None);
    }
    let value = |key| -> Result<String> {
        Ok(c.query_row(
            "SELECT value FROM embedding_config WHERE key=?1",
            [key],
            |r| r.get(0),
        )?)
    };
    Ok(Some((
        value("command")?,
        value("model")?,
        value("dimension")?.parse()?,
    )))
}

fn vectors<'a>(
    texts: impl Iterator<Item = &'a str>,
    command: &str,
    model: &str,
    dimension: usize,
) -> Result<(Vec<Vec<u8>>, usize)> {
    let mut dim = dimension;
    let mut out = vec![];
    for text in texts {
        let v = embed(command, model, text)?;
        if dim != 0 && dim != v.len() {
            return Err("embedding dimension mismatch".into());
        }
        dim = v.len();
        out.push(as_bytes(&v));
    }
    Ok((out, dim))
}

pub fn refresh(c: &mut Connection) -> Result<()> {
    let version = snapshot_version(c)?;
    is_current(c)?;
    let commits = reachable_commits()?;
    let mut blobs = BTreeMap::new();
    let mut occurrences = vec![];
    for (commit_id, _) in &commits {
        let tree = git(&["ls-tree", "-r", "-z", "--full-tree", commit_id])?;
        for record in tree.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let tab = record
                .iter()
                .position(|b| *b == b'\t')
                .ok_or("invalid git tree record")?;
            let meta = std::str::from_utf8(&record[..tab])?;
            let fields: Vec<_> = meta.split_whitespace().collect();
            if fields.len() != 3 {
                return Err("invalid git tree metadata".into());
            }
            if fields[1] != "blob" {
                continue;
            }
            let blob_id = fields[2].to_owned();
            if !blobs.contains_key(&blob_id) {
                let bytes = git(&["cat-file", "blob", &blob_id])?;
                let blob = if bytes.contains(&0) {
                    Blob {
                        content: None,
                        status: "binary",
                    }
                } else {
                    match String::from_utf8(bytes) {
                        Ok(content) => Blob {
                            content: Some(content),
                            status: "text",
                        },
                        Err(_) => Blob {
                            content: None,
                            status: "unsupported",
                        },
                    }
                };
                blobs.insert(blob_id.clone(), blob);
            }
            occurrences.push(Occurrence {
                commit_id: commit_id.clone(),
                blob_id,
                path: record[tab + 1..].to_vec(),
            });
        }
    }
    let mut units = vec![];
    for (blob_id, blob) in &blobs {
        if let Some(content) = &blob.content {
            for (i, line) in content.lines().enumerate() {
                units.push(Unit {
                    id: units.len() + 1,
                    blob_id: blob_id.clone(),
                    line: i + 1,
                    text: line.to_owned(),
                });
            }
        }
    }
    let cfg = configuration(c)?;
    let embedded = cfg
        .as_ref()
        .map(|(command, model, dim)| {
            vectors(
                units.iter().map(|unit| unit.text.as_str()),
                command,
                model,
                *dim,
            )
        })
        .transpose()?;
    // Nothing published until Git reads and all embedding generation succeed.
    let tx = publish_transaction(c, version)?;
    tx.execute_batch("DROP TABLE IF EXISTS unit_embeddings;
        DROP TABLE IF EXISTS git_blob_units; DROP TABLE IF EXISTS indexed_units;
        DROP TABLE IF EXISTS git_occurrences; DROP TABLE IF EXISTS blob_contents;
        DROP TABLE IF EXISTS git_commits;
        CREATE TABLE git_commits(commit_id TEXT PRIMARY KEY NOT NULL, timestamp INTEGER NOT NULL);
        CREATE TABLE blob_contents(blob_id TEXT PRIMARY KEY NOT NULL, content TEXT, status TEXT NOT NULL);
        CREATE TABLE git_occurrences(commit_id TEXT NOT NULL REFERENCES git_commits, blob_id TEXT NOT NULL REFERENCES blob_contents, path BLOB NOT NULL, PRIMARY KEY(commit_id,path));
        CREATE TABLE indexed_units(unit_id INTEGER PRIMARY KEY, text TEXT NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL);
        CREATE TABLE git_blob_units(blob_id TEXT NOT NULL REFERENCES blob_contents, unit_id INTEGER NOT NULL REFERENCES indexed_units, PRIMARY KEY(blob_id,unit_id));
        CREATE TABLE unit_embeddings(unit_id INTEGER PRIMARY KEY REFERENCES indexed_units, vector BLOB NOT NULL);")?;
    for (id, timestamp) in commits {
        tx.execute(
            "INSERT INTO git_commits VALUES(?1,?2)",
            params![id, timestamp],
        )?;
    }
    for (id, blob) in blobs {
        tx.execute(
            "INSERT INTO blob_contents VALUES(?1,?2,?3)",
            params![id, blob.content, blob.status],
        )?;
    }
    for occurrence in occurrences {
        tx.execute(
            "INSERT INTO git_occurrences VALUES(?1,?2,?3)",
            params![occurrence.commit_id, occurrence.blob_id, occurrence.path],
        )?;
    }
    for unit in &units {
        tx.execute(
            "INSERT INTO indexed_units VALUES(?1,?2,?3,?3)",
            params![unit.id, unit.text, unit.line],
        )?;
        tx.execute(
            "INSERT INTO git_blob_units VALUES(?1,?2)",
            params![unit.blob_id, unit.id],
        )?;
    }
    if let Some((rows, dim)) = embedded {
        for (unit, vector) in units.iter().zip(rows) {
            tx.execute(
                "INSERT INTO unit_embeddings VALUES(?1,?2)",
                params![unit.id, vector],
            )?;
        }
        tx.execute(
            "UPDATE embedding_config SET value=?1 WHERE key='dimension'",
            [dim.to_string()],
        )?;
    }
    tx.execute_batch("DROP TABLE IF EXISTS indexed_files; DROP TABLE IF EXISTS embeddings; PRAGMA user_version=2;")?;
    tx.commit()?;
    Ok(())
}

pub fn reembed(c: &mut Connection, command: &str, model: &str) -> Result<()> {
    let version = snapshot_version(c)?;
    let units = {
        let mut s = c.prepare("SELECT unit_id,text FROM indexed_units ORDER BY unit_id")?;
        s.query_map([], |r| Ok((r.get::<_, usize>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let (rows, dim) = vectors(
        units.iter().map(|(_, text)| text.as_str()),
        command,
        model,
        0,
    )?;
    let tx = publish_transaction(c, version)?;
    tx.execute("DELETE FROM unit_embeddings", [])?;
    for ((unit_id, _), vector) in units.iter().zip(rows) {
        tx.execute(
            "INSERT INTO unit_embeddings VALUES(?1,?2)",
            params![unit_id, vector],
        )?;
    }
    for (key, value) in [
        ("command", command),
        ("model", model),
        ("dimension", &dim.to_string()),
    ] {
        tx.execute("INSERT INTO embedding_config VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value])?;
    }
    tx.commit()?;
    Ok(())
}

pub fn search(
    c: &Connection,
    query: &str,
    mode: &str,
    limit: usize,
    commits: &[String],
) -> Result<Vec<SearchResult>> {
    let v = if mode == "exact" {
        None
    } else {
        Some(query_vector(c, query)?)
    };
    // Filter occurrences first, then choose one reference per blob. Every indexed
    // unit of that blob shares the reference; duplicate occurrences never use top-N.
    let mut s = c.prepare(
        "WITH eligible AS (
        SELECT o.*, row_number() OVER (PARTITION BY blob_id
            ORDER BY timestamp DESC, o.commit_id DESC, path ASC) occurrence_rank
        FROM git_occurrences o JOIN git_commits c USING(commit_id)
        WHERE o.commit_id IN (SELECT value FROM json_each(?1)))
        SELECT o.path,u.line_start,u.line_end,u.text,o.commit_id,o.blob_id,
        CASE WHEN ?2 IS NULL THEN NULL ELSE vec_distance_cosine(e.vector,?2) END
        FROM eligible o JOIN git_blob_units b USING(blob_id)
        JOIN indexed_units u USING(unit_id) LEFT JOIN unit_embeddings e USING(unit_id)
        WHERE occurrence_rank=1 ORDER BY o.path,u.line_start,o.blob_id,o.commit_id",
    )?;
    let rows = s.query_map(
        params![
            serde_json::to_string(commits)?,
            v.as_ref().map(|v| as_bytes(v))
        ],
        |r| {
            let path: Vec<u8> = r.get(0)?;
            Ok((
                path,
                SearchResult {
                    path: String::new(),
                    line_start: r.get(1)?,
                    line_end: r.get(2)?,
                    text: r.get(3)?,
                    commit_id: Some(r.get(4)?),
                    blob_id: Some(r.get(5)?),
                    distance: r.get(6)?,
                },
            ))
        },
    )?;
    let mut results = vec![];
    for row in rows {
        let (path, mut result) = row?;
        let Ok(path) = String::from_utf8(path) else {
            continue;
        };
        result.path = path;
        results.push(result);
    }
    if mode == "exact" {
        results.retain(|r| r.text.to_lowercase().contains(&query.to_lowercase()));
    } else {
        results.sort_by(|a, b| {
            a.distance
                .unwrap()
                .total_cmp(&b.distance.unwrap())
                .then(a.path.cmp(&b.path))
                .then(a.line_start.cmp(&b.line_start))
                .then(a.blob_id.cmp(&b.blob_id))
        });
        let mut index = 0;
        results.retain(|r| {
            let keep = index < limit
                || (mode == "combined" && r.text.to_lowercase().contains(&query.to_lowercase()));
            index += 1;
            keep
        });
    }
    Ok(results)
}

#[derive(Default)]
pub struct Scope {
    pub all_history: bool,
    pub current_worktree: bool,
    pub commit: Option<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Scope {
    pub fn resolve(&self, c: &Connection) -> Result<Vec<String>> {
        let selectors = usize::from(self.all_history)
            + usize::from(self.current_worktree)
            + usize::from(self.commit.is_some())
            + usize::from(!self.include.is_empty());
        if selectors > 1 {
            return Err("choose only one of --current-worktree, --all-history, --commit or --include-commit".into());
        }
        let mut s = c.prepare(if is_current(c)? {
            "SELECT commit_id FROM git_commits"
        } else {
            "SELECT DISTINCT commit_id FROM indexed_files"
        })?;
        let known = s
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let resolve = |selector: &str| -> Result<String> {
            if !(4..=64).contains(&selector.len())
                || !selector.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(format!("expected exact commit ID (at least 4 hex digits), not a ref or range: {selector}").into());
            }
            let prefix = selector.to_ascii_lowercase();
            let matches: Vec<_> = known.iter().filter(|id| id.starts_with(&prefix)).collect();
            match matches.as_slice() {
                [id] => Ok((*id).clone()),
                [] => Err(
                    format!("commit {selector} is not in the index; run refresh explicitly").into(),
                ),
                _ => Err(format!("ambiguous commit ID {selector}").into()),
            }
        };
        let mut eligible = if self.all_history {
            known.clone()
        } else if let Some(commit) = &self.commit {
            vec![resolve(commit)?]
        } else if !self.include.is_empty() {
            self.include
                .iter()
                .map(|id| resolve(id))
                .collect::<Result<Vec<_>>>()?
        } else {
            head()?
                .into_iter()
                .filter(|id| known.contains(id))
                .collect()
        };
        for excluded in &self.exclude {
            let id = resolve(excluded)?;
            eligible.retain(|candidate| candidate != &id);
        }
        eligible.sort();
        eligible.dedup();
        Ok(eligible)
    }
}
