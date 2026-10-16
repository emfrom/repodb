use crate::{Result, SearchResult, as_bytes, git, query_vector, segmentation};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};

const MAX_COMMITS: usize = 100_000;
const MAX_OCCURRENCES: usize = 1_000_000;
const MAX_BLOB_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BLOB_BYTES: u64 = 256 * 1024 * 1024;
const MAX_UNITS: usize = 500_000;
const MAX_UNIT_TEXT_BYTES: usize = 256 * 1024 * 1024;
const MAX_VECTOR_BYTES: usize = 256 * 1024 * 1024;
// Bounds RepoDB-owned refresh preparation payloads, including Git tree records,
// captured blob contents, staged units/vectors, and embedding subprocess output.
const MAX_REFRESH_MEMORY_BYTES: usize = 256 * 1024 * 1024;

fn memory_limit() -> Box<dyn std::error::Error> {
    "history refresh limit exceeded: preparation data exceeds 256 MiB; reduce indexed history and retry".into()
}

fn for_each_tree_record(commit_id: &str, mut visit: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    let mut child = Command::new("git")
        .args(["ls-tree", "-r", "-z", "--full-tree", commit_id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut record = Vec::new();
        if stdout.read_until(0, &mut record)? == 0 {
            break;
        }
        record.pop();
        if let Err(error) = visit(&record) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "git ls-tree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

pub fn is_current(c: &Connection) -> Result<bool> {
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    match version {
        0 => Ok(false),
        2 | 3 => Ok(true),
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
    kind: &'static str,
    line_start: usize,
    line_end: usize,
    text: String,
}
struct Occurrence {
    commit_id: String,
    blob_id: String,
    path: Vec<u8>,
    kind: &'static str,
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
        .args(["rev-list", "--timestamp", "--max-count=100001", "--stdin"])
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
    max_bytes: Option<usize>,
) -> Result<(Vec<Vec<u8>>, usize)> {
    let mut dim = dimension;
    let mut out = vec![];
    let mut total_bytes = 0usize;
    for text in texts {
        // During JSON decoding, stdout, decoded f32 values, and the encoded
        // vector coexist. Reserve for all three plus the retained output row.
        let remaining = max_bytes.map(|limit| limit.saturating_sub(total_bytes));
        let output_limit = remaining.map(|bytes| bytes / 5);
        let v = crate::embed_with_output_limit(command, model, text, output_limit)?;
        if dim != 0 && dim != v.len() {
            return Err("embedding dimension mismatch".into());
        }
        dim = v.len();
        let encoded = as_bytes(&v);
        total_bytes += encoded.len();
        if max_bytes.is_some_and(|limit| total_bytes > limit) {
            return Err(format!("history refresh limit exceeded: embedding vectors exceed {} bytes; reduce indexed history and retry", max_bytes.unwrap()).into());
        }
        out.push(encoded);
    }
    Ok((out, dim))
}

pub fn refresh(c: &mut Connection) -> Result<()> {
    let version = snapshot_version(c)?;
    is_current(c)?;
    let commits = reachable_commits()?;
    if commits.len() > MAX_COMMITS {
        return Err(format!("history refresh limit exceeded: at most {MAX_COMMITS} reachable commits; narrow repository refs and retry").into());
    }
    let mut blobs = BTreeMap::new();
    let mut total_blob_bytes = 0u64;
    let mut occurrences = vec![];
    let mut preparation_bytes: usize = commits
        .iter()
        .map(|(id, _)| id.len() + std::mem::size_of::<(String, i64)>())
        .sum();
    if preparation_bytes > MAX_REFRESH_MEMORY_BYTES {
        return Err(memory_limit());
    }
    for (commit_id, _) in &commits {
        for_each_tree_record(commit_id, |record| {
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
                return Ok(());
            }
            let blob_id = fields[2].to_owned();
            if !blobs.contains_key(&blob_id) {
                let size: u64 = String::from_utf8(git(&["cat-file", "-s", &blob_id])?)?
                    .trim()
                    .parse()?;
                if size > MAX_BLOB_BYTES {
                    return Err(format!("history refresh limit exceeded: blob {blob_id} is {size} bytes (maximum {MAX_BLOB_BYTES}); reduce the blob or exclude its commit and retry").into());
                }
                total_blob_bytes = total_blob_bytes.saturating_add(size);
                if total_blob_bytes > MAX_TOTAL_BLOB_BYTES {
                    return Err(format!("history refresh limit exceeded: distinct Git blobs exceed {MAX_TOTAL_BLOB_BYTES} total bytes; reduce indexed history and retry").into());
                }
                let blob_bytes = size as usize;
                let blob_overhead = blob_id.len()
                    + std::mem::size_of::<Blob>()
                    + std::mem::size_of::<(String, Blob)>();
                if preparation_bytes
                    .saturating_add(blob_bytes.saturating_mul(2))
                    .saturating_add(blob_overhead)
                    > MAX_REFRESH_MEMORY_BYTES
                {
                    return Err(memory_limit());
                }
                let bytes = git(&["cat-file", "blob", &blob_id])?;
                preparation_bytes = preparation_bytes
                    .saturating_add(blob_bytes)
                    .saturating_add(blob_overhead);
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
            if occurrences.len() == MAX_OCCURRENCES {
                return Err(format!("history refresh limit exceeded: at most {MAX_OCCURRENCES} commit/path occurrences; narrow repository refs and retry").into());
            }
            let path = &record[tab + 1..];
            preparation_bytes = preparation_bytes.saturating_add(
                path.len() + commit_id.len() + blob_id.len() + std::mem::size_of::<Occurrence>(),
            );
            if preparation_bytes > MAX_REFRESH_MEMORY_BYTES {
                return Err(memory_limit());
            }
            occurrences.push(Occurrence {
                commit_id: commit_id.clone(),
                blob_id,
                path: path.to_vec(),
                kind: segmentation::kind(path),
            });
            Ok(())
        })?;
    }
    let mut units = vec![];
    let variants: BTreeSet<_> = occurrences
        .iter()
        .map(|o| (o.blob_id.as_str(), o.kind))
        .collect();
    for (blob_id, kind) in variants {
        if let Some(content) = &blobs[blob_id].content {
            for unit in segmentation::segment(kind, content) {
                preparation_bytes = preparation_bytes
                    .saturating_add(unit.text.len() + blob_id.len() + std::mem::size_of::<Unit>());
                if preparation_bytes > MAX_REFRESH_MEMORY_BYTES {
                    return Err(memory_limit());
                }
                units.push(Unit {
                    id: units.len() + 1,
                    blob_id: blob_id.to_owned(),
                    kind,
                    line_start: unit.line_start,
                    line_end: unit.line_end,
                    text: unit.text,
                });
            }
        }
    }
    let unit_text_bytes: usize = units.iter().map(|unit| unit.text.len()).sum();
    if units.len() > MAX_UNITS || unit_text_bytes > MAX_UNIT_TEXT_BYTES {
        return Err(format!("history refresh limit exceeded: indexed units must fit within {MAX_UNITS} units and {MAX_UNIT_TEXT_BYTES} text bytes; reduce indexed history and retry").into());
    }
    let cfg = configuration(c)?;
    let embedded = cfg
        .as_ref()
        .map(|(command, model, dim)| {
            let (rows, dimension) = vectors(
                units.iter().map(|unit| unit.text.as_str()),
                command,
                model,
                *dim,
                Some(
                    MAX_VECTOR_BYTES
                        .min(MAX_REFRESH_MEMORY_BYTES.saturating_sub(preparation_bytes)),
                ),
            )?;
            Ok::<_, Box<dyn std::error::Error>>((rows, dimension))
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
        CREATE TABLE git_occurrences(commit_id TEXT NOT NULL REFERENCES git_commits, blob_id TEXT NOT NULL REFERENCES blob_contents, path BLOB NOT NULL, segmentation_kind TEXT NOT NULL, PRIMARY KEY(commit_id,path));
        CREATE TABLE indexed_units(unit_id INTEGER PRIMARY KEY, text TEXT NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL);
        CREATE TABLE git_blob_units(blob_id TEXT NOT NULL REFERENCES blob_contents, unit_id INTEGER NOT NULL REFERENCES indexed_units, segmentation_kind TEXT NOT NULL, PRIMARY KEY(blob_id,segmentation_kind,unit_id));
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
            "INSERT INTO git_occurrences VALUES(?1,?2,?3,?4)",
            params![
                occurrence.commit_id,
                occurrence.blob_id,
                occurrence.path,
                occurrence.kind
            ],
        )?;
    }
    for unit in &units {
        tx.execute(
            "INSERT INTO indexed_units VALUES(?1,?2,?3,?4)",
            params![unit.id, unit.text, unit.line_start, unit.line_end],
        )?;
        tx.execute(
            "INSERT INTO git_blob_units VALUES(?1,?2,?3)",
            params![unit.blob_id, unit.id, unit.kind],
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
    tx.execute_batch("DROP TABLE IF EXISTS indexed_files; DROP TABLE IF EXISTS embeddings; PRAGMA user_version=3;")?;
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
    let (rows, mut dim) = vectors(
        units.iter().map(|(_, text)| text.as_str()),
        command,
        model,
        0,
        None,
    )?;
    let tx = publish_transaction(c, version)?;
    tx.execute("DELETE FROM unit_embeddings", [])?;
    for ((unit_id, _), vector) in units.iter().zip(rows) {
        tx.execute(
            "INSERT INTO unit_embeddings VALUES(?1,?2)",
            params![unit_id, vector],
        )?;
    }
    if let Some(import_dim) = crate::imports::reembed(&tx, command, model)? {
        if dim != 0 && dim != import_dim {
            return Err("embedding dimension mismatch".into());
        }
        dim = import_dim;
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
    // Version 2 snapshots keep their original line units until explicit refresh.
    let schema: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let variant = if schema == 3 {
        "AND b.segmentation_kind=o.segmentation_kind"
    } else {
        ""
    };
    let mut s = c.prepare(&format!(
        "WITH eligible AS (
        SELECT o.*, row_number() OVER (PARTITION BY blob_id
            ORDER BY timestamp DESC, o.commit_id DESC, path ASC) occurrence_rank
        FROM git_occurrences o JOIN git_commits c USING(commit_id)
        WHERE o.commit_id IN (SELECT value FROM json_each(?1)))
        SELECT o.path,u.line_start,u.line_end,u.text,o.commit_id,o.blob_id,
        CASE WHEN ?2 IS NULL THEN NULL ELSE vec_distance_cosine(e.vector,?2) END
        FROM eligible o JOIN git_blob_units b ON b.blob_id=o.blob_id {variant}
        JOIN indexed_units u USING(unit_id) LEFT JOIN unit_embeddings e USING(unit_id)
        WHERE occurrence_rank=1 ORDER BY o.path,u.line_start,o.blob_id,o.commit_id,u.unit_id",
    ))?;
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
                    page: None,
                    repo_id: None,
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
