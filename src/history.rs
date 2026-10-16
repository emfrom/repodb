use crate::{Result, SearchResult, as_bytes, budget::Budget, process, query_vector, segmentation};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader},
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
const EMBEDDING_STDERR_BYTES: usize = 64 * 1024;

fn memory_limit() -> Box<dyn std::error::Error> {
    "history refresh limit exceeded: preparation data exceeds 256 MiB; reduce indexed history and retry".into()
}

const MAX_GIT_STDERR_BYTES: usize = 64 * 1024;

fn git_capture_limits(requested_stdout: usize, preparation_bytes: usize) -> Result<(usize, usize)> {
    let remaining = MAX_REFRESH_MEMORY_BYTES
        .checked_sub(preparation_bytes)
        .ok_or_else(memory_limit)?;
    let stderr = remaining.min(MAX_GIT_STDERR_BYTES);
    let stdout = requested_stdout.min(remaining - stderr);
    Ok((stdout, stderr))
}

fn git_output(
    args: &[&str],
    stdout_limit: usize,
    preparation_bytes: &mut Budget,
) -> Result<Vec<u8>> {
    let (stdout_limit, stderr_limit) = git_capture_limits(stdout_limit, preparation_bytes.used())?;
    let mut child = Command::new("git")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let output = process::capture(&mut child, stdout_limit, stderr_limit)?;
    if output.exceeded {
        return Err(memory_limit());
    }
    let stdout_capacity = output.stdout.capacity();
    let stderr_capacity = output.stderr.capacity();
    preparation_bytes.charge(stdout_capacity, "Git output capacity")?;
    preparation_bytes.charge(stderr_capacity, "Git stderr capacity")?;
    if !output.status.success() {
        let error = format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        drop(output);
        preparation_bytes.release(stdout_capacity + stderr_capacity);
        return Err(error.into());
    }
    drop(output.stderr);
    preparation_bytes.release(stderr_capacity);
    Ok(output.stdout)
}

fn for_each_tree_record(
    commit_id: &str,
    preparation_bytes: &mut Budget,
    mut visit: impl FnMut(&[u8], &mut Budget) -> Result<()>,
) -> Result<()> {
    const TREE_READ_BUFFER_BYTES: usize = 8 * 1024;
    // Reserve both concurrent captures before starting the child, so record growth
    // cannot consume memory needed by stderr or the buffered stdout reader.
    preparation_bytes.charge(MAX_GIT_STDERR_BYTES, "Git stderr capture")?;
    preparation_bytes.charge(TREE_READ_BUFFER_BYTES, "Git stdout read buffer")?;
    let mut child = Command::new("git")
        .args(["ls-tree", "-r", "-z", "--full-tree", commit_id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = BufReader::with_capacity(TREE_READ_BUFFER_BYTES, child.stdout.take().unwrap());
    let stderr = child.stderr.take().unwrap();
    let stderr =
        std::thread::spawn(move || crate::process::drain_bounded(stderr, MAX_GIT_STDERR_BYTES));
    let result = (|| -> Result<()> {
        loop {
            let mut record = Vec::new();
            let record_result = (|| -> Result<bool> {
                loop {
                    let available = stdout.fill_buf()?;
                    if available.is_empty() {
                        break;
                    }
                    let end = available
                        .iter()
                        .position(|byte| *byte == 0)
                        .map_or(available.len(), |n| n + 1);
                    let record_bytes = end - usize::from(available[end - 1] == 0);
                    preparation_bytes.reserve_vec(
                        &mut record,
                        record_bytes,
                        "Git tree record capacity",
                    )?;
                    record.extend_from_slice(&available[..record_bytes]);
                    let terminated = end > record_bytes;
                    stdout.consume(end);
                    if terminated {
                        break;
                    }
                }
                if record.is_empty() {
                    Ok(false)
                } else {
                    visit(&record, preparation_bytes)?;
                    Ok(true)
                }
            })();
            preparation_bytes.release(record.capacity());
            if !record_result? {
                break;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let (stderr, exceeded) = stderr.join().map_err(|_| "git stderr reader panicked")??;
    let stderr_capacity = stderr.capacity();
    preparation_bytes.release(MAX_GIT_STDERR_BYTES - stderr_capacity);
    preparation_bytes.release(TREE_READ_BUFFER_BYTES);
    if let Err(error) = result {
        drop(stderr);
        preparation_bytes.release(stderr_capacity);
        return Err(error);
    }
    if exceeded {
        drop(stderr);
        preparation_bytes.release(stderr_capacity);
        return Err(memory_limit());
    }
    if !status.success() {
        let error = format!("git ls-tree failed: {}", String::from_utf8_lossy(&stderr));
        drop(stderr);
        preparation_bytes.release(stderr_capacity);
        return Err(error.into());
    }
    drop(stderr);
    preparation_bytes.release(stderr_capacity);
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

fn head(preparation_bytes: &mut Budget) -> Result<Option<String>> {
    let (stdout_limit, stderr_limit) = git_capture_limits(128, preparation_bytes.used())?;
    let mut child = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let output = process::capture(&mut child, stdout_limit, stderr_limit)?;
    if output.exceeded {
        return Err(memory_limit());
    }
    let stdout_capacity = output.stdout.capacity();
    let stderr_capacity = output.stderr.capacity();
    preparation_bytes.charge(stdout_capacity, "HEAD stdout capacity")?;
    preparation_bytes.charge(stderr_capacity, "HEAD stderr capacity")?;
    let result = if output.status.success() {
        let id = std::str::from_utf8(&output.stdout)?.trim();
        preparation_bytes.charge(id.len(), "retained HEAD identifier")?;
        Ok(Some(id.to_owned()))
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(format!(
            "cannot read HEAD: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    };
    drop(output);
    preparation_bytes.release(stdout_capacity + stderr_capacity);
    result
}

fn reachable_commits(preparation_bytes: &mut Budget) -> Result<Vec<(String, i64)>> {
    // Capture immutable roots once; later ref changes cannot mix two snapshots.
    let mut roots = git_output(
        &["for-each-ref", "--format=%(objectname)"],
        MAX_REFRESH_MEMORY_BYTES,
        preparation_bytes,
    )?;
    let mut roots_bytes = roots.capacity();
    if let Some(head) = head(preparation_bytes)? {
        let retained_head_bytes = head.len();
        let head = format!("{head}\n");
        let old_capacity = roots.capacity();
        let requested = roots
            .len()
            .checked_add(head.len())
            .ok_or("Git roots capacity overflow")?;
        let delta = requested.saturating_sub(old_capacity);
        preparation_bytes.charge(delta, "Git roots capacity")?;
        if let Err(error) = roots.try_reserve_exact(head.len()) {
            preparation_bytes.release(delta);
            return Err(error.into());
        }
        let actual_delta = roots.capacity().saturating_sub(old_capacity);
        if actual_delta > delta {
            return Err("Git roots allocator exceeded exact capacity bound".into());
        }
        preparation_bytes.release(delta - actual_delta);
        roots.extend_from_slice(head.as_bytes());
        roots_bytes += actual_delta;
        preparation_bytes.release(retained_head_bytes);
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
    let (stdout_limit, stderr_limit) =
        git_capture_limits(MAX_REFRESH_MEMORY_BYTES, preparation_bytes.used())?;
    let output = process::capture_with_input(&mut child, &roots, stdout_limit, stderr_limit)?;
    let stdout_capacity = output.stdout.capacity();
    let stderr_capacity = output.stderr.capacity();
    preparation_bytes.charge(stdout_capacity, "Git commit output capacity")?;
    preparation_bytes.charge(stderr_capacity, "Git stderr capacity")?;
    if output.exceeded {
        return Err(memory_limit());
    }
    if !output.status.success() {
        return Err(format!(
            "git rev-list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let output_bytes = stdout_capacity + stderr_capacity;
    let stdout = String::from_utf8(output.stdout)?;
    let mut commits = Vec::new();
    for line in stdout.lines() {
        let (timestamp, id) = line.split_once(' ').ok_or("invalid git commit record")?;
        preparation_bytes.charge(id.len(), "parsed commit identifiers")?;
        preparation_bytes.reserve_vec(&mut commits, 1, "parsed commit vector")?;
        commits.push((id.to_owned(), timestamp.parse()?));
    }
    // The captured bytes and parsed records coexist during parsing. Retain the
    // record charge after releasing the raw rev-list output.
    drop(stdout);
    drop(output.stderr);
    drop(roots);
    preparation_bytes.release(output_bytes);
    preparation_bytes.release(roots_bytes);
    Ok(commits)
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
        // Each JSON number needs at least one byte, so stdout bytes bound the
        // element count. Vec growth can reserve up to twice that many f32s
        // (8 bytes/element), and the encoded Vec may reserve twice its 4-byte
        // payload (8 bytes/element). Reserve stdout + both capacities (17 bytes/stdout byte),
        // previously retained rows, and embed's bounded 64 KiB stderr capture.
        let output_limit = max_bytes.map(|limit| {
            limit
                .saturating_sub(total_bytes)
                .saturating_sub(EMBEDDING_STDERR_BYTES)
                / 17
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_capture_limits_reserve_stderr_and_use_remaining_budget() {
        let (stdout, stderr) =
            git_capture_limits(MAX_REFRESH_MEMORY_BYTES, MAX_REFRESH_MEMORY_BYTES - 100).unwrap();
        assert_eq!(stdout, 0);
        assert_eq!(stderr, 100);

        let used = MAX_REFRESH_MEMORY_BYTES - MAX_GIT_STDERR_BYTES - 17;
        let (stdout, stderr) = git_capture_limits(MAX_REFRESH_MEMORY_BYTES, used).unwrap();
        assert_eq!(stdout, 17);
        assert_eq!(stderr, MAX_GIT_STDERR_BYTES);
        assert!(stdout + stderr <= MAX_REFRESH_MEMORY_BYTES - used);
        assert!(
            git_capture_limits(MAX_REFRESH_MEMORY_BYTES, MAX_REFRESH_MEMORY_BYTES + 1).is_err()
        );
    }

    #[test]
    fn vectors_reserve_short_numeric_json_decoding_and_encoding_memory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let embedder = dir.path().join("embedder");
        std::fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[0,0]'\n").unwrap();
        std::fs::set_permissions(&embedder, std::fs::Permissions::from_mode(0o755)).unwrap();

        // `[0,0]` is five stdout bytes for two elements. The decoded Vec may
        // reserve four f32 slots (16 bytes), and the encoded Vec may reserve 16;
        // with stdout and stderr that exceeds this remaining budget. The old
        // stdout/5 cap admitted this output even though the combined bound is 37.
        let limit = EMBEDDING_STDERR_BYTES + 36;
        let result = vectors(
            ["unit"].into_iter(),
            embedder.to_str().unwrap(),
            "model",
            0,
            Some(limit),
        );
        assert!(
            result.is_err(),
            "short numeric JSON must respect aggregate bound"
        );
    }

    #[test]
    fn tree_record_vector_capacity_is_charged_before_growth_and_released() {
        let mut budget = Budget::new(8, "history refresh limit exceeded");
        let mut record = Vec::<u8>::new();
        budget.reserve_vec(&mut record, 5, "tree record").unwrap();
        assert!(record.capacity() >= 5);
        assert_eq!(budget.used(), record.capacity());
        record.resize(5, 0);
        let used = budget.used();
        assert!(budget.reserve_vec(&mut record, 4, "tree record").is_err());
        assert_eq!(budget.used(), used, "failed growth rolls back reservation");
        budget.release(record.capacity());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn tree_record_budget_caps_append_and_rolls_back_after_visit() {
        let mut budget = Budget::new(5, "history refresh limit exceeded");
        budget.charge(3, "tree record").unwrap();
        assert_eq!(budget.remaining(), 2);
        assert!(budget.charge(3, "tree record").is_err());
        assert_eq!(budget.used(), 3, "rejected append must not charge bytes");
        budget.release(3);
        assert_eq!(budget.used(), 0, "record charge is rolled back after visit");
    }
}

pub fn refresh(c: &mut Connection) -> Result<()> {
    let version = snapshot_version(c)?;
    is_current(c)?;
    let mut preparation_bytes = Budget::new(
        MAX_REFRESH_MEMORY_BYTES,
        "history refresh limit exceeded: preparation data",
    );
    let commits = reachable_commits(&mut preparation_bytes)?;
    if commits.len() > MAX_COMMITS {
        return Err(format!("history refresh limit exceeded: at most {MAX_COMMITS} reachable commits; narrow repository refs and retry").into());
    }
    let mut blobs = BTreeMap::new();
    let mut total_blob_bytes = 0u64;
    let mut occurrences = vec![];
    for (commit_id, _) in &commits {
        for_each_tree_record(
            commit_id,
            &mut preparation_bytes,
            |record, preparation_bytes| {
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
                    let size: u64 = String::from_utf8(git_output(
                        &["cat-file", "-s", &blob_id],
                        64,
                        preparation_bytes,
                    )?)?
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
                    if blob_bytes.saturating_mul(2).saturating_add(blob_overhead)
                        > preparation_bytes.remaining()
                    {
                        return Err(memory_limit());
                    }
                    let bytes = git_output(
                        &["cat-file", "blob", &blob_id],
                        blob_bytes,
                        preparation_bytes,
                    )?;
                    preparation_bytes.charge(
                        blob_bytes.saturating_add(blob_overhead),
                        "Git blob contents",
                    )?;
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
                preparation_bytes.charge(
                    path.len() + commit_id.len() + blob_id.len(),
                    "Git occurrence payloads",
                )?;
                preparation_bytes.reserve_vec(&mut occurrences, 1, "Git occurrence vector")?;
                occurrences.push(Occurrence {
                    commit_id: commit_id.clone(),
                    blob_id,
                    path: path.to_vec(),
                    kind: segmentation::kind(path),
                });
                Ok(())
            },
        )?;
    }
    let mut units = vec![];
    let mut unit_text_bytes = 0usize;
    let variants: BTreeSet<_> = occurrences
        .iter()
        .map(|o| (o.blob_id.as_str(), o.kind))
        .collect();
    for (blob_id, kind) in variants {
        if let Some(content) = &blobs[blob_id].content {
            let mut staging_error: Option<Box<dyn std::error::Error>> = None;
            segmentation::visit(kind, content, |unit| {
                if units.len() == MAX_UNITS
                    || unit_text_bytes.saturating_add(unit.text.len()) > MAX_UNIT_TEXT_BYTES
                {
                    staging_error = Some(format!("history refresh limit exceeded: indexed units must fit within {MAX_UNITS} units and {MAX_UNIT_TEXT_BYTES} text bytes; reduce indexed history and retry").into());
                    return false;
                }
                let charge = unit.text.len() + blob_id.len();
                if let Err(error) = preparation_bytes.charge(charge, "staged history unit payloads")
                {
                    staging_error = Some(error);
                    return false;
                }
                if let Err(error) =
                    preparation_bytes.reserve_vec(&mut units, 1, "staged history unit vector")
                {
                    preparation_bytes.release(charge);
                    staging_error = Some(error);
                    return false;
                }
                unit_text_bytes += unit.text.len();
                units.push(Unit {
                    id: units.len() + 1,
                    blob_id: blob_id.to_owned(),
                    kind,
                    line_start: unit.line_start,
                    line_end: unit.line_end,
                    text: unit.text.to_owned(),
                });
                true
            });
            if let Some(error) = staging_error {
                return Err(error);
            }
        }
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
                Some(MAX_VECTOR_BYTES.min(preparation_bytes.remaining())),
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
            {
                let mut budget =
                    Budget::new(MAX_REFRESH_MEMORY_BYTES, "history query limit exceeded");
                head(&mut budget)?
            }
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
