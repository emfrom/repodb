use crate::{
    Result, SearchResult, as_bytes,
    budget::{Budget, MAX_REFRESH_ACCOUNTING_BYTES},
    git_objects::GitBlobReader,
    git_ranges, process, query_vector, segmentation,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread::JoinHandle,
};

const EMBEDDING_STDERR_BYTES: usize = 64 * 1024;
fn memory_limit() -> Box<dyn std::error::Error> {
    "history refresh preparation size overflow".into()
}

const MAX_GIT_STDERR_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiffSide {
    Added,
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiffReference {
    pub parent_commit_id: String,
    pub child_commit_id: String,
    pub hunk_id: i64,
    pub side: DiffSide,
    pub old_path: Option<Vec<u8>>,
    pub new_path: Option<Vec<u8>>,
    pub old_line_start: Option<usize>,
    pub old_line_end: Option<usize>,
    pub new_line_start: Option<usize>,
    pub new_line_end: Option<usize>,
    pub blob_id: String,
    pub byte_start: usize,
    pub byte_end: usize,
}

fn git_capture_limits(requested_stdout: usize, preparation_bytes: usize) -> Result<(usize, usize)> {
    let remaining = MAX_REFRESH_ACCOUNTING_BYTES
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

fn stage_diff_pair(
    c: &Connection,
    parent: Option<&str>,
    child: &str,
    next_unit_id: &mut i64,
    batch: &mut Vec<(i64, String)>,
    pipeline: &mut EmbeddingPipeline,
    embedding_dimension: &mut Option<usize>,
    embedding_statement: &mut rusqlite::Statement<'_>,
    budget: &mut Budget,
) -> Result<()> {
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let args = if let Some(parent) = parent {
        vec![
            "diff-tree",
            "--no-commit-id",
            "--raw",
            "--no-renames",
            "-r",
            "-z",
            parent,
            child,
        ]
    } else {
        vec![
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--raw",
            "--no-renames",
            "-r",
            "-z",
            child,
        ]
    };
    let output = git_output(
        &args,
        budget.remaining().saturating_sub(MAX_GIT_STDERR_BYTES),
        budget,
    )?;
    let records: Vec<&[u8]> = output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect();
    let mut i = 0;
    let mut hunk_id = 0i64;
    while i < records.len() {
        let metadata = std::str::from_utf8(records[i])?;
        i += 1;
        let fields: Vec<&str> = metadata.split_whitespace().collect();
        if fields.len() != 5 || !fields[0].starts_with(':') {
            return Err("invalid raw Git diff record".into());
        }
        let status = fields[4]
            .as_bytes()
            .first()
            .copied()
            .ok_or("missing Git diff status")?;
        let path = records.get(i).ok_or("missing Git diff path")?.to_vec();
        i += 1;
        if status != b'A' && status != b'D' {
            continue;
        }
        let (side, blob_id, old_path, new_path) = if status == b'A' {
            (DiffSide::Added, fields[3], None, Some(path))
        } else {
            (DiffSide::Removed, fields[2], Some(path), None)
        };
        let bytes = git_output(
            &["cat-file", "blob", blob_id],
            budget.remaining().saturating_sub(MAX_GIT_STDERR_BYTES),
            budget,
        )?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if text.contains('\0') {
            continue;
        }
        let this_hunk_id = hunk_id;
        hunk_id += 1;
        let mut unit_statement =
            c.prepare("INSERT INTO temp.refresh_units VALUES(?1,?2,'diff',?3,?4,?5,?6,?7)")?;
        let mut reference_statement = c.prepare("INSERT INTO temp.refresh_diff_references(parent_commit_id,child_commit_id,hunk_id,side,old_path,new_path,old_line_start,old_line_end,new_line_start,new_line_end,blob_id,unit_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)")?;
        let mut unit_error = None;
        segmentation::visit("fallback", text, |unit| {
            let result = (|| -> Result<()> {
                budget.charge(unit.text.len(), "diff embedding batch text")?;
                *next_unit_id += 1;
                unit_statement.execute(params![
                    *next_unit_id,
                    blob_id,
                    unit.text,
                    unit.byte_start,
                    unit.byte_end,
                    unit.line_start,
                    unit.line_end
                ])?;
                let (old_line_start, old_line_end, new_line_start, new_line_end) = match side {
                    DiffSide::Added => (None, None, Some(unit.line_start), Some(unit.line_end)),
                    DiffSide::Removed => (Some(unit.line_start), Some(unit.line_end), None, None),
                };
                reference_statement.execute(params![
                    parent.unwrap_or(EMPTY_TREE),
                    child,
                    this_hunk_id,
                    if side == DiffSide::Added {
                        "added"
                    } else {
                        "removed"
                    },
                    old_path,
                    new_path,
                    old_line_start,
                    old_line_end,
                    new_line_start,
                    new_line_end,
                    blob_id,
                    *next_unit_id
                ])?;
                batch.push((*next_unit_id, unit.text.to_owned()));
                if batch.len() >= HISTORY_EMBEDDING_BATCH_SIZE {
                    flush_embedding_batch(
                        batch,
                        pipeline,
                        embedding_dimension,
                        embedding_statement,
                        budget,
                    )?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                unit_error = Some(error);
                return false;
            }
            true
        })?;
        if let Some(error) = unit_error {
            return Err(error);
        }
    }
    Ok(())
}

fn commit_message(commit_id: &str, budget: &mut Budget) -> Result<(String, usize, usize)> {
    let size: usize = String::from_utf8(git_output(&["cat-file", "-s", commit_id], 64, budget)?)?
        .trim()
        .parse()?;
    let object = git_output(&["cat-file", "commit", commit_id], size, budget)?;
    let separator = object
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .ok_or("Git commit has no message separator")?;
    let start = separator + 2;
    let message = std::str::from_utf8(&object[start..])?.to_owned();
    Ok((
        message,
        start,
        object[..start]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1,
    ))
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
    let (stderr, _stderr_exceeded) = stderr.join().map_err(|_| "git stderr reader panicked")??;
    let stderr_capacity = stderr.capacity();
    preparation_bytes.release(MAX_GIT_STDERR_BYTES - stderr_capacity);
    preparation_bytes.release(TREE_READ_BUFFER_BYTES);
    if let Err(error) = result {
        drop(stderr);
        preparation_bytes.release(stderr_capacity);
        return Err(error);
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
        2..=4 => Ok(true),
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

fn for_each_reachable_commit(
    preparation_bytes: &mut Budget,
    mut visit: impl FnMut(&str, i64, &mut Budget) -> Result<()>,
) -> Result<()> {
    const READ_BUFFER_BYTES: usize = 8 * 1024;
    preparation_bytes.charge(MAX_GIT_STDERR_BYTES, "Git stderr capture")?;
    preparation_bytes.charge(READ_BUFFER_BYTES, "Git commit read buffer")?;
    let mut args = vec!["rev-list", "--timestamp", "--all"];
    let head_id = head(preparation_bytes)?;
    if let Some(id) = head_id.as_deref() {
        args.push(id);
    }
    let mut child = Command::new("git")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = BufReader::with_capacity(READ_BUFFER_BYTES, child.stdout.take().unwrap());
    let stderr = child.stderr.take().unwrap();
    let stderr =
        std::thread::spawn(move || crate::process::drain_bounded(stderr, MAX_GIT_STDERR_BYTES));
    let result = (|| -> Result<()> {
        let mut line = Vec::new();
        loop {
            line.clear();
            let count = stdout.read_until(b'\n', &mut line)?;
            if count == 0 {
                break;
            }
            preparation_bytes.charge(line.capacity(), "Git commit record")?;
            let parsed = (|| -> Result<()> {
                let record = std::str::from_utf8(&line)?;
                let (timestamp, id) = record
                    .trim_end()
                    .split_once(' ')
                    .ok_or("invalid git commit record")?;
                visit(id, timestamp.parse()?, preparation_bytes)
            })();
            preparation_bytes.release(line.capacity());
            parsed?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let (stderr, _) = stderr.join().map_err(|_| "git stderr reader panicked")??;
    preparation_bytes.release(READ_BUFFER_BYTES);
    preparation_bytes.release(MAX_GIT_STDERR_BYTES - stderr.capacity());
    let stderr_capacity = stderr.capacity();
    if let Err(error) = result {
        drop(stderr);
        preparation_bytes.release(stderr_capacity);
        return Err(error);
    }
    if !status.success() {
        let error = format!("git rev-list failed: {}", String::from_utf8_lossy(&stderr));
        drop(stderr);
        preparation_bytes.release(stderr_capacity);
        return Err(error.into());
    }
    drop(stderr);
    preparation_bytes.release(stderr_capacity);
    if let Some(id) = head_id {
        preparation_bytes.release(id.len());
    }
    Ok(())
}

pub(crate) fn configuration(c: &Connection) -> Result<(String, String, usize)> {
    let count: usize = c.query_row("SELECT count(*) FROM embedding_config", [], |r| r.get(0))?;
    if count == 0 {
        return Ok((
            crate::config::embedding_command()?,
            crate::config::DEFAULT_EMBEDDING_MODEL.to_owned(),
            0,
        ));
    }
    let value = |key| -> Result<String> {
        Ok(c.query_row(
            "SELECT value FROM embedding_config WHERE key=?1",
            [key],
            |r| r.get(0),
        )?)
    };
    Ok((
        value("command")?,
        value("model")?,
        value("dimension")?.parse()?,
    ))
}

fn vectors_batch(
    texts: &[&str],
    command: &str,
    model: &str,
    dimension: usize,
    budget: &mut Budget,
) -> Result<(Vec<Vec<u8>>, usize)> {
    if !crate::is_builtin_embedding_command(command) || texts.len() < 2 {
        return vectors(texts.iter().copied(), command, model, dimension, budget);
    }
    let input_bytes = texts
        .iter()
        .map(|text| {
            if text.trim().is_empty() {
                "<empty RepoDB unit>".len()
            } else {
                text.len()
            }
        })
        .sum::<usize>()
        .saturating_add(texts.len().saturating_sub(1));
    budget.charge(input_bytes, "embedding batch input")?;
    let output_limit = budget.remaining().saturating_sub(EMBEDDING_STDERR_BYTES) / 17;
    let workspace = output_limit
        .checked_mul(17)
        .and_then(|bytes| bytes.checked_add(EMBEDDING_STDERR_BYTES))
        .ok_or_else(memory_limit)?;
    if let Err(error) = budget.charge(workspace, "embedding batch workspace") {
        budget.release(input_bytes);
        return Err(error);
    }
    let vectors = match crate::embed_many_with_output_limit(command, model, texts, output_limit) {
        Ok(vectors) => vectors,
        Err(error) => {
            budget.release(input_bytes + workspace);
            return Err(error);
        }
    };
    budget.release(input_bytes);
    let decoded_capacity = vectors
        .iter()
        .map(|vector| vector.capacity() * std::mem::size_of::<f32>())
        .sum::<usize>();
    budget.release(workspace.saturating_sub(decoded_capacity));
    let mut encoded = Vec::with_capacity(vectors.len());
    let mut dim = dimension;
    for vector in vectors {
        if dim != 0 && dim != vector.len() {
            budget.release(decoded_capacity);
            return Err("embedding dimension mismatch".into());
        }
        dim = vector.len();
        let payload = vector.len() * std::mem::size_of::<f32>();
        let mut bytes = Vec::new();
        if let Err(error) = budget.reserve_vec(&mut bytes, payload, "encoded history vector") {
            budget.release(decoded_capacity);
            return Err(error);
        }
        for value in &vector {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        encoded.push(bytes);
    }
    budget.release(decoded_capacity);
    Ok((encoded, dim))
}

fn vectors<'a>(
    texts: impl Iterator<Item = &'a str>,
    command: &str,
    model: &str,
    dimension: usize,
    budget: &mut Budget,
) -> Result<(Vec<Vec<u8>>, usize)> {
    let mut dim = dimension;
    let mut out = vec![];
    for text in texts {
        budget.reserve_vec(&mut out, 1, "history vector rows")?;
        let remaining = budget.remaining();
        let output_limit = remaining.saturating_sub(EMBEDDING_STDERR_BYTES) / 17;
        // Each JSON number needs at least one byte. The stdout limit therefore
        // bounds both Vec capacities (at most 8 bytes/element each); with stdout
        // and bounded stderr, 17 bytes per stdout byte covers the full live peak.
        let workspace = output_limit
            .checked_mul(17)
            .and_then(|bytes| bytes.checked_add(EMBEDDING_STDERR_BYTES))
            .ok_or_else(memory_limit)?;
        budget.charge(workspace, "embedding workspace")?;
        let v = match crate::embed_with_output_limit(command, model, text, Some(output_limit)) {
            Ok(v) => v,
            Err(error) => {
                budget.release(workspace);
                return Err(error);
            }
        };
        let decoded_capacity = v.capacity() * std::mem::size_of::<f32>();
        budget.release(workspace - decoded_capacity);
        if dim != 0 && dim != v.len() {
            budget.release(decoded_capacity);
            return Err("embedding dimension mismatch".into());
        }
        dim = v.len();
        let payload = v
            .len()
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(memory_limit)?;
        let mut encoded = Vec::new();
        if let Err(error) = budget.reserve_vec(&mut encoded, payload, "encoded history vector") {
            budget.release(decoded_capacity);
            return Err(error);
        }
        for value in &v {
            encoded.extend_from_slice(&value.to_le_bytes());
        }
        budget.release(decoded_capacity);
        out.push(encoded);
    }
    Ok((out, dim))
}

const HISTORY_EMBEDDING_BATCH_SIZE: usize = 256;

type EmbeddingBatch = Vec<(i64, String)>;
type EmbeddingResult = std::result::Result<(Vec<(i64, Vec<u8>)>, usize, usize), (String, usize)>;

struct EmbeddingPipeline {
    input: Option<SyncSender<EmbeddingBatch>>,
    output: Option<Receiver<EmbeddingResult>>,
    worker: Option<JoinHandle<()>>,
    in_flight: usize,
}

impl EmbeddingPipeline {
    fn new(command: String, model: String, dimension: usize) -> Result<Self> {
        let (input_tx, input_rx) = mpsc::sync_channel::<EmbeddingBatch>(1);
        let (output_tx, output_rx) = mpsc::sync_channel::<EmbeddingResult>(1);
        let worker = std::thread::Builder::new()
            .name("repodb-history-embedder".into())
            .spawn(move || {
                while let Ok(batch) = input_rx.recv() {
                    let batch_bytes = batch.iter().map(|(_, text)| text.len()).sum();
                    let result = (|| -> Result<_> {
                        let texts: Vec<&str> =
                            batch.iter().map(|(_, text)| text.as_str()).collect();
                        let mut budget = Budget::new(
                            MAX_REFRESH_ACCOUNTING_BYTES,
                            "history embedding accounting",
                        );
                        let (vectors, dim) =
                            vectors_batch(&texts, &command, &model, dimension, &mut budget)?;
                        if vectors.len() != batch.len() {
                            return Err(
                                "embedding command returned the wrong number of vectors".into()
                            );
                        }
                        Ok((
                            batch
                                .iter()
                                .zip(vectors)
                                .map(|((id, _), vector)| (*id, vector))
                                .collect(),
                            dim,
                            batch_bytes,
                        ))
                    })()
                    .map_err(|error| (error.to_string(), batch_bytes));
                    let failed = result.is_err();
                    if output_tx.send(result).is_err() || failed {
                        break;
                    }
                }
            })?;
        Ok(Self {
            input: Some(input_tx),
            output: Some(output_rx),
            worker: Some(worker),
            in_flight: 0,
        })
    }

    fn receive(
        &mut self,
        statement: &mut rusqlite::Statement<'_>,
        dimension: &mut Option<usize>,
        budget: &mut Budget,
    ) -> Result<()> {
        let result = self
            .output
            .as_ref()
            .ok_or("embedding pipeline is closed")?
            .recv()
            .map_err(|_| "embedding worker stopped")?;
        self.in_flight -= 1;
        let (vectors, dim, batch_bytes) = match result {
            Ok(result) => result,
            Err((error, batch_bytes)) => {
                budget.release(batch_bytes);
                return Err(error.into());
            }
        };
        budget.release(batch_bytes);
        if dimension.is_some_and(|expected| expected != dim) {
            for (_, vector) in vectors {
                let capacity = vector.capacity();
                budget.charge(capacity, "embedding result vector")?;
                budget.release(capacity);
            }
            return Err("embedding dimension mismatch".into());
        }
        for (unit_id, vector) in vectors {
            let capacity = vector.capacity();
            budget.charge(capacity, "embedding result vector")?;
            let result = statement.execute(params![unit_id, vector]);
            budget.release(capacity);
            result?;
        }
        *dimension = Some(dim);
        Ok(())
    }

    fn submit(
        &mut self,
        mut batch: EmbeddingBatch,
        statement: &mut rusqlite::Statement<'_>,
        dimension: &mut Option<usize>,
        budget: &mut Budget,
    ) -> Result<()> {
        let sender = self
            .input
            .as_ref()
            .ok_or("embedding pipeline is closed")?
            .clone();
        loop {
            match sender.try_send(batch) {
                Ok(()) => {
                    self.in_flight += 1;
                    return Ok(());
                }
                Err(TrySendError::Full(returned)) => {
                    batch = returned;
                    if self.in_flight == 0 {
                        return Err("embedding input queue is unexpectedly full".into());
                    }
                    self.receive(statement, dimension, budget)?;
                }
                Err(TrySendError::Disconnected(_)) => return Err("embedding worker stopped".into()),
            }
        }
    }

    fn finish(
        &mut self,
        statement: &mut rusqlite::Statement<'_>,
        dimension: &mut Option<usize>,
        budget: &mut Budget,
    ) -> Result<()> {
        while self.in_flight != 0 {
            self.receive(statement, dimension, budget)?;
        }
        self.input.take();
        self.output.take();
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "embedding worker panicked")?;
        }
        Ok(())
    }
}

impl Drop for EmbeddingPipeline {
    fn drop(&mut self) {
        self.input.take();
        self.output.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn flush_embedding_batch(
    batch: &mut Vec<(i64, String)>,
    pipeline: &mut EmbeddingPipeline,
    embedding_dimension: &mut Option<usize>,
    statement: &mut rusqlite::Statement<'_>,
    budget: &mut Budget,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let owned = std::mem::take(batch);
    pipeline.submit(owned, statement, embedding_dimension, budget)
}

struct StagedUnitWriter<'a, 'connection> {
    next_unit_id: &'a mut i64,
    unit_statement: &'a mut rusqlite::Statement<'connection>,
    batch: &'a mut EmbeddingBatch,
    pipeline: &'a mut EmbeddingPipeline,
    embedding_dimension: &'a mut Option<usize>,
    embedding_statement: &'a mut rusqlite::Statement<'connection>,
    budget: &'a mut Budget,
    budget_label: &'static str,
}

impl StagedUnitWriter<'_, '_> {
    fn segment_blob(&mut self, content: &str, blob_id: &str, kind: &str) -> Result<()> {
        let mut staging_error = None;
        let segmentation_result = segmentation::visit(kind, content, |unit| {
            let staged = (|| -> Result<()> {
                *self.next_unit_id += 1;
                let text = unit.text.to_owned();
                self.unit_statement.execute(params![
                    *self.next_unit_id,
                    blob_id,
                    kind,
                    &text,
                    unit.byte_start,
                    unit.byte_end,
                    unit.line_start,
                    unit.line_end
                ])?;
                self.budget.charge(text.len(), self.budget_label)?;
                self.batch.push((*self.next_unit_id, text));
                if self.batch.len() == HISTORY_EMBEDDING_BATCH_SIZE {
                    flush_embedding_batch(
                        self.batch,
                        self.pipeline,
                        self.embedding_dimension,
                        self.embedding_statement,
                        self.budget,
                    )?;
                }
                Ok(())
            })();
            if let Err(error) = staged {
                staging_error = Some(error);
                return false;
            }
            true
        });
        segmentation_result?;
        if let Some(error) = staging_error {
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_refresh_indexes_added_and_removed_diff_references() {
        use std::{fs, os::unix::fs::PermissionsExt, process::Command};

        let _cwd_lock = CWD_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let original_cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write(repo.path().join("removed.txt"), "remove needle\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "parent"]);
        let parent = git(&["rev-parse", "HEAD"]);
        fs::remove_file(repo.path().join("removed.txt")).unwrap();
        fs::write(repo.path().join("added.txt"), "add needle\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "child"]);
        let child = git(&["rev-parse", "HEAD"]);

        let embedder = repo.path().join("embedder");
        fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
        fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE embedding_config(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TEMP TABLE import_refresh_dimension(value INTEGER);").unwrap();
        for (key, value) in [
            ("command", embedder.to_str().unwrap()),
            ("model", "test"),
            ("dimension", "0"),
        ] {
            db.execute(
                "INSERT INTO embedding_config VALUES(?1,?2)",
                params![key, value],
            )
            .unwrap();
        }
        std::env::set_current_dir(repo.path()).unwrap();
        prepare_refresh(&db).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        publish_refresh(&tx).unwrap();
        tx.commit().unwrap();

        let references = diff_references(&db, Some(&parent), Some(&child)).unwrap();
        assert_eq!(references.len(), 2);
        let added = references
            .iter()
            .find(|reference| reference.side == DiffSide::Added)
            .unwrap();
        assert_eq!(added.child_commit_id, child);
        assert_eq!(added.old_path, None);
        assert_eq!(added.new_path.as_deref(), Some(b"added.txt".as_slice()));
        assert_eq!(added.old_line_start, None);
        assert_eq!(added.new_line_start, Some(1));
        assert_eq!(added.byte_start, 0);
        assert_eq!(added.byte_end, "add needle".len());
        let removed = references
            .iter()
            .find(|reference| reference.side == DiffSide::Removed)
            .unwrap();
        assert_eq!(removed.old_path.as_deref(), Some(b"removed.txt".as_slice()));
        assert_eq!(removed.new_path, None);
        assert_eq!(removed.old_line_start, Some(1));
        assert_eq!(removed.new_line_start, None);
        assert_eq!(removed.byte_start, 0);
        assert_eq!(removed.byte_end, "remove needle".len());
        assert_ne!(added.blob_id, removed.blob_id);
        std::env::set_current_dir(original_cwd).unwrap();
    }

    static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn enable_vector_extension() {
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut i8,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite_vec::sqlite3_vec_init as *const ()
            )))
        };
    }

    #[test]
    fn full_refresh_indexes_root_and_each_merge_parent() {
        use std::{fs, os::unix::fs::PermissionsExt, process::Command};

        let _cwd_lock = CWD_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let original_cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write(repo.path().join("root.txt"), "root content\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "root"]);
        let root = git(&["rev-parse", "HEAD"]);
        git(&["branch", "side"]);
        fs::write(repo.path().join("main.txt"), "main addition\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "main side"]);
        let first_parent = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-q", "side"]);
        fs::write(repo.path().join("other.txt"), "other addition\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "other side"]);
        let second_parent = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-q", "--detach", &first_parent]);
        git(&["merge", "--no-ff", "-qm", "merge", &second_parent]);
        let merge = git(&["rev-parse", "HEAD"]);

        let embedder = repo.path().join("embedder");
        fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
        fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE embedding_config(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TEMP TABLE import_refresh_dimension(value INTEGER);").unwrap();
        for (key, value) in [
            ("command", embedder.to_str().unwrap()),
            ("model", "test"),
            ("dimension", "0"),
        ] {
            db.execute(
                "INSERT INTO embedding_config VALUES(?1,?2)",
                params![key, value],
            )
            .unwrap();
        }
        std::env::set_current_dir(repo.path()).unwrap();
        prepare_refresh(&db).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        publish_refresh(&tx).unwrap();
        tx.commit().unwrap();

        let root_refs = diff_references(&db, None, Some(&root)).unwrap();
        assert!(
            root_refs
                .iter()
                .any(|reference| reference.new_path.as_deref() == Some(b"root.txt".as_slice()))
        );
        let merge_refs = diff_references(&db, None, Some(&merge)).unwrap();
        assert!(
            merge_refs
                .iter()
                .any(|reference| reference.parent_commit_id == first_parent
                    && reference.new_path.as_deref() == Some(b"other.txt".as_slice()))
        );
        assert!(
            merge_refs
                .iter()
                .any(|reference| reference.parent_commit_id == second_parent
                    && reference.new_path.as_deref() == Some(b"main.txt".as_slice()))
        );
        std::env::set_current_dir(original_cwd).unwrap();
    }

    #[test]
    fn commit_message_search_hydrates_exact_and_semantic_hits_from_git() {
        use std::{fs, os::unix::fs::PermissionsExt, process::Command};

        let _cwd_lock = CWD_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let original_cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write(repo.path().join("file.txt"), "tracked content\n").unwrap();
        git(&["add", "."]);
        git(&[
            "commit",
            "-qm",
            "Initial transaction ledger\n\nDetailed rationale body",
        ]);
        let commit = git(&["rev-parse", "HEAD"]);
        let embedder = repo.path().join("embedder");
        fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
        fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
        enable_vector_extension();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE embedding_config(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TEMP TABLE import_refresh_dimension(value INTEGER);",
        )
        .unwrap();
        for (key, value) in [
            ("command", embedder.to_str().unwrap()),
            ("model", "test"),
            ("dimension", "0"),
        ] {
            db.execute(
                "INSERT INTO embedding_config VALUES(?1,?2)",
                params![key, value],
            )
            .unwrap();
        }
        std::env::set_current_dir(repo.path()).unwrap();
        prepare_refresh(&db).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        publish_refresh(&tx).unwrap();
        tx.commit().unwrap();

        let exact =
            search_commit_messages(&db, "transaction ledger", "exact", 5, &[commit.clone()])
                .unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].commit_id.as_deref(), Some(commit.as_str()));
        assert!(exact[0].text.contains("Initial transaction ledger"));
        let semantic = search_commit_messages(
            &db,
            "accounting rationale",
            "semantic",
            5,
            &[commit.clone()],
        )
        .unwrap();
        assert!(
            semantic
                .iter()
                .any(|result| result.text.contains("Initial transaction ledger"))
        );
        assert!(
            semantic
                .iter()
                .all(|result| result.commit_id.as_deref() == Some(commit.as_str()))
        );
        assert!(semantic.iter().all(|result| result.distance == Some(0.0)));
        let stored_message: i64 = db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='commit_message_units'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(stored_message, 1);
        let persisted_text_columns: i64 = db
            .query_row(
                "SELECT count(*) FROM pragma_table_info('indexed_units') WHERE name='text'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted_text_columns, 0);
        std::env::set_current_dir(original_cwd).unwrap();
    }

    #[test]
    fn commit_message_search_fails_when_indexed_git_object_is_missing() {
        use std::{fs, os::unix::fs::PermissionsExt, process::Command};

        let _cwd_lock = CWD_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let original_cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write(repo.path().join("file.txt"), "content\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "Missing object message marker"]);
        let commit = git(&["rev-parse", "HEAD"]);
        enable_vector_extension();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE embedding_config(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TEMP TABLE import_refresh_dimension(value INTEGER);"
        )
        .unwrap();
        let embedder = repo.path().join("embedder");
        fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1,0]'\n").unwrap();
        fs::set_permissions(&embedder, fs::Permissions::from_mode(0o755)).unwrap();
        for (key, value) in [
            ("command", embedder.to_str().unwrap()),
            ("model", "test"),
            ("dimension", "0"),
        ] {
            db.execute(
                "INSERT INTO embedding_config VALUES(?1,?2)",
                params![key, value],
            )
            .unwrap();
        }
        std::env::set_current_dir(repo.path()).unwrap();
        prepare_refresh(&db).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        publish_refresh(&tx).unwrap();
        tx.commit().unwrap();
        git(&["update-ref", "-d", "HEAD"]);
        let output = Command::new("git")
            .args(["reflog", "expire", "--expire=now", "--all"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = Command::new("git")
            .args(["gc", "--prune=now"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(output.status.success());

        let error = match search_commit_messages(&db, "Missing object", "exact", 5, &[commit]) {
            Ok(_) => panic!("missing commit object was silently ignored"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("could not get object info"),
            "{error}"
        );
        std::env::set_current_dir(original_cwd).unwrap();
    }

    #[test]
    fn git_capture_limits_reserve_stderr_and_use_remaining_budget() {
        let (stdout, stderr) = git_capture_limits(
            MAX_REFRESH_ACCOUNTING_BYTES,
            MAX_REFRESH_ACCOUNTING_BYTES - 100,
        )
        .unwrap();
        assert_eq!(stdout, 0);
        assert_eq!(stderr, 100);

        let used = MAX_REFRESH_ACCOUNTING_BYTES - MAX_GIT_STDERR_BYTES - 17;
        let (stdout, stderr) = git_capture_limits(MAX_REFRESH_ACCOUNTING_BYTES, used).unwrap();
        assert_eq!(stdout, 17);
        assert_eq!(stderr, MAX_GIT_STDERR_BYTES);
        assert!(stdout + stderr <= MAX_REFRESH_ACCOUNTING_BYTES - used);
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
            &mut Budget::new(limit, "history refresh limit exceeded"),
        );
        assert!(
            result.is_err(),
            "short numeric JSON must respect aggregate bound"
        );
    }

    #[test]
    fn history_vector_row_capacity_obeys_budget_boundary() {
        let row_size = std::mem::size_of::<Vec<u8>>();
        let mut budget = Budget::new(row_size, "history refresh limit exceeded");
        let mut rows = Vec::<Vec<u8>>::new();
        budget
            .reserve_vec(&mut rows, 1, "history vector rows")
            .unwrap();
        assert_eq!(budget.used(), rows.capacity() * row_size);
        let excessive_growth = rows.capacity() + 1;
        assert!(
            budget
                .reserve_vec(&mut rows, excessive_growth, "history vector rows")
                .is_err()
        );
        assert_eq!(budget.used(), rows.capacity() * row_size);
        budget.release(rows.capacity() * row_size);
        assert_eq!(budget.used(), 0);
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

fn stage_commit_message(
    commit_id: &str,
    message: &str,
    object_byte_start: usize,
    object_line_start: usize,
    next_unit_id: &mut i64,
    unit_statement: &mut rusqlite::Statement<'_>,
    message_statement: &mut rusqlite::Statement<'_>,
    batch: &mut EmbeddingBatch,
    pipeline: &mut EmbeddingPipeline,
    embedding_dimension: &mut Option<usize>,
    embedding_statement: &mut rusqlite::Statement<'_>,
    budget: &mut Budget,
) -> Result<()> {
    let mut staging_error = None;
    segmentation::visit("fallback", message, |unit| {
        let result = (|| -> Result<()> {
            *next_unit_id += 1;
            let byte_start = object_byte_start + unit.byte_start;
            let byte_end = object_byte_start + unit.byte_end;
            let line_start = object_line_start + unit.line_start - 1;
            let line_end = object_line_start + unit.line_end - 1;
            unit_statement.execute(params![
                *next_unit_id,
                commit_id,
                "commit-message",
                unit.text,
                byte_start,
                byte_end,
                line_start,
                line_end
            ])?;
            message_statement.execute(params![
                commit_id,
                *next_unit_id,
                byte_start,
                byte_end,
                line_start,
                line_end
            ])?;
            budget.charge(unit.text.len(), "commit message embedding text")?;
            batch.push((*next_unit_id, unit.text.to_owned()));
            if batch.len() == HISTORY_EMBEDDING_BATCH_SIZE {
                flush_embedding_batch(
                    batch,
                    pipeline,
                    embedding_dimension,
                    embedding_statement,
                    budget,
                )?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            staging_error = Some(error);
            false
        } else {
            true
        }
    })?;
    if let Some(error) = staging_error {
        return Err(error);
    }
    Ok(())
}

pub fn prepare_refresh(c: &Connection) -> Result<()> {
    is_current(c)?;
    let mut preparation_bytes = Budget::new(
        MAX_REFRESH_ACCOUNTING_BYTES,
        "history refresh preparation accounting",
    );
    c.execute_batch("PRAGMA temp_store=FILE;
        DROP TABLE IF EXISTS temp.refresh_commits; DROP TABLE IF EXISTS temp.refresh_blobs;
        DROP TABLE IF EXISTS temp.refresh_occurrences; DROP TABLE IF EXISTS temp.refresh_variants;
        DROP TABLE IF EXISTS temp.refresh_units; DROP TABLE IF EXISTS temp.refresh_embeddings;
        DROP TABLE IF EXISTS temp.refresh_commit_message_units;
        CREATE TEMP TABLE refresh_commits(commit_id TEXT PRIMARY KEY, timestamp INTEGER NOT NULL);
        CREATE TEMP TABLE refresh_blobs(blob_id TEXT PRIMARY KEY, content TEXT, status TEXT NOT NULL);
        CREATE TEMP TABLE refresh_occurrences(commit_id TEXT NOT NULL, blob_id TEXT NOT NULL, path BLOB NOT NULL, segmentation_kind TEXT NOT NULL, PRIMARY KEY(commit_id,path));
        CREATE TEMP TABLE refresh_variants(blob_id TEXT NOT NULL, segmentation_kind TEXT NOT NULL, PRIMARY KEY(blob_id,segmentation_kind));
        CREATE TEMP TABLE refresh_units(unit_id INTEGER PRIMARY KEY, blob_id TEXT NOT NULL, segmentation_kind TEXT NOT NULL, text TEXT NOT NULL, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL);
        CREATE TEMP TABLE refresh_commit_message_units(commit_id TEXT NOT NULL, unit_id INTEGER NOT NULL, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL);
        CREATE TEMP TABLE refresh_diff_references(parent_commit_id TEXT NOT NULL,child_commit_id TEXT NOT NULL,hunk_id INTEGER NOT NULL,side TEXT NOT NULL,old_path BLOB,new_path BLOB,old_line_start INTEGER,old_line_end INTEGER,new_line_start INTEGER,new_line_end INTEGER,blob_id TEXT NOT NULL,unit_id INTEGER NOT NULL);
        CREATE TEMP TABLE refresh_embeddings(unit_id INTEGER PRIMARY KEY, vector BLOB NOT NULL);
        DROP TABLE IF EXISTS temp.refresh_dimension; CREATE TEMP TABLE refresh_dimension(value INTEGER);
        DROP TABLE IF EXISTS temp.refresh_config; CREATE TEMP TABLE refresh_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    let (command, model, configured_dimension) = configuration(c)?;
    let mut embedding_dimension = None;
    let mut pipeline =
        EmbeddingPipeline::new(command.clone(), model.clone(), configured_dimension)?;
    let mut batch = Vec::with_capacity(HISTORY_EMBEDDING_BATCH_SIZE);
    let mut unit_count = 0i64;
    {
        let mut commit_stage =
            c.prepare("INSERT OR IGNORE INTO temp.refresh_commits VALUES(?1,?2)")?;
        let mut message_unit_stage =
            c.prepare("INSERT INTO temp.refresh_units VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
        let mut message_range_stage =
            c.prepare("INSERT INTO temp.refresh_commit_message_units VALUES(?1,?2,?3,?4,?5,?6)")?;
        let mut message_embedding_stage =
            c.prepare("INSERT INTO temp.refresh_embeddings VALUES(?1,?2)")?;
        let mut blob_exists = c.prepare("SELECT 1 FROM temp.refresh_blobs WHERE blob_id=?1")?;
        let mut blob_stage = c.prepare("INSERT INTO temp.refresh_blobs VALUES(?1,?2,?3)")?;
        let mut blob_content =
            c.prepare("SELECT content FROM temp.refresh_blobs WHERE blob_id=?1")?;
        let mut variant_exists = c.prepare(
            "SELECT EXISTS(SELECT 1 FROM temp.refresh_variants WHERE blob_id=?1 AND segmentation_kind=?2)",
        )?;
        let mut variant_stage =
            c.prepare("INSERT OR IGNORE INTO temp.refresh_variants VALUES(?1,?2)")?;
        let mut occurrence_stage =
            c.prepare("INSERT OR IGNORE INTO temp.refresh_occurrences VALUES(?1,?2,?3,?4)")?;
        let mut unit_stage =
            c.prepare("INSERT INTO temp.refresh_units VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
        let mut embedding_statement =
            c.prepare("INSERT INTO temp.refresh_embeddings VALUES(?1,?2)")?;
        for_each_reachable_commit(&mut preparation_bytes, |commit_id, timestamp, budget| {
            commit_stage.execute(params![commit_id, timestamp])?;
            let (message, byte_start, line_start) = commit_message(commit_id, budget)?;
            stage_commit_message(
                commit_id,
                &message,
                byte_start,
                line_start,
                &mut unit_count,
                &mut message_unit_stage,
                &mut message_range_stage,
                &mut batch,
                &mut pipeline,
                &mut embedding_dimension,
                &mut message_embedding_stage,
                budget,
            )?;
            for_each_tree_record(commit_id, budget, |record, budget| {
                let tab = record
                    .iter()
                    .position(|b| *b == b'\t')
                    .ok_or("invalid git tree record")?;
                let meta = std::str::from_utf8(&record[..tab])?;
                let mut fields = meta.split_whitespace();
                let _mode = fields.next().ok_or("invalid git tree metadata")?;
                let object_type = fields.next().ok_or("invalid git tree metadata")?;
                let blob_id = fields.next().ok_or("invalid git tree metadata")?;
                if fields.next().is_some() {
                    return Err("invalid git tree metadata".into());
                }
                if object_type != "blob" {
                    return Ok(());
                }
                let path = &record[tab + 1..];
                let kind = segmentation::kind(path);
                let content = if blob_exists.exists([blob_id])? {
                    occurrence_stage.execute(params![commit_id, blob_id, path, kind])?;
                    if variant_exists
                        .query_row(params![blob_id, kind], |row| row.get::<_, bool>(0))?
                    {
                        return Ok(());
                    }
                    blob_content.query_row([blob_id], |row| row.get::<_, Option<String>>(0))?
                } else {
                    let size: u64 =
                        String::from_utf8(git_output(&["cat-file", "-s", blob_id], 64, budget)?)?
                            .trim()
                            .parse()?;
                    let bytes = git_output(
                        &["cat-file", "blob", blob_id],
                        usize::try_from(size).unwrap_or(usize::MAX),
                        budget,
                    )?;
                    let (content, status) = if bytes.contains(&0) {
                        (None, "binary")
                    } else {
                        match String::from_utf8(bytes) {
                            Ok(content) => (Some(content), "text"),
                            Err(_) => (None, "unsupported"),
                        }
                    };
                    blob_stage.execute(params![blob_id, content.as_deref(), status])?;
                    occurrence_stage.execute(params![commit_id, blob_id, path, kind])?;
                    content
                };
                if let Some(content) = content
                    && variant_stage.execute(params![blob_id, kind])? != 0
                {
                    StagedUnitWriter {
                        next_unit_id: &mut unit_count,
                        unit_statement: &mut unit_stage,
                        batch: &mut batch,
                        pipeline: &mut pipeline,
                        embedding_dimension: &mut embedding_dimension,
                        embedding_statement: &mut embedding_statement,
                        budget,
                        budget_label: "history embedding batch text",
                    }
                    .segment_blob(&content, blob_id, kind)?;
                }
                Ok(())
            })
        })?;
        let commits = {
            let mut statement =
                c.prepare("SELECT commit_id FROM temp.refresh_commits ORDER BY commit_id")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for child in commits {
            let record = String::from_utf8(git_output(
                &["rev-list", "--parents", "-n", "1", &child],
                1024,
                &mut preparation_bytes,
            )?)?;
            let mut fields = record.split_whitespace();
            if fields.next() != Some(child.as_str()) {
                return Err("invalid Git parent record".into());
            }
            let parents: Vec<String> = fields.map(str::to_owned).collect();
            if parents.is_empty() {
                stage_diff_pair(
                    c,
                    None,
                    &child,
                    &mut unit_count,
                    &mut batch,
                    &mut pipeline,
                    &mut embedding_dimension,
                    &mut embedding_statement,
                    &mut preparation_bytes,
                )?;
            } else {
                for parent in parents {
                    stage_diff_pair(
                        c,
                        Some(&parent),
                        &child,
                        &mut unit_count,
                        &mut batch,
                        &mut pipeline,
                        &mut embedding_dimension,
                        &mut embedding_statement,
                        &mut preparation_bytes,
                    )?;
                }
            }
        }
        flush_embedding_batch(
            &mut batch,
            &mut pipeline,
            &mut embedding_dimension,
            &mut embedding_statement,
            &mut preparation_bytes,
        )?;
        pipeline.finish(
            &mut embedding_statement,
            &mut embedding_dimension,
            &mut preparation_bytes,
        )?;
    }
    if let Some(dimension) = embedding_dimension {
        c.execute(
            "INSERT INTO temp.refresh_dimension VALUES(?1)",
            [dimension as i64],
        )?;
    }
    let has_import_dimension: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE type='table' AND name='import_refresh_dimension')",
        [],
        |row| row.get(0),
    )?;
    let imported_dimension = if has_import_dimension {
        c.query_row(
            "SELECT value FROM temp.import_refresh_dimension LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(|dimension| dimension as usize)
    } else {
        None
    };
    let dimension = embedding_dimension
        .or(imported_dimension)
        .unwrap_or(configured_dimension);
    for (key, value) in [
        ("command", command),
        ("model", model),
        ("dimension", dimension.to_string()),
    ] {
        c.execute(
            "INSERT INTO temp.refresh_config VALUES(?1,?2)",
            params![key, value],
        )?;
    }
    Ok(())
}

pub fn resolve_update_commit(commit_id: &str) -> Result<String> {
    if commit_id.len() < 4 || !commit_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("--update requires a full or unique hexadecimal commit ID (not a ref or revision expression)".into());
    }
    let mut budget = Budget::new(
        MAX_REFRESH_ACCOUNTING_BYTES,
        "targeted history refresh preparation accounting",
    );
    let output = git_output(
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{commit_id}^{{commit}}"),
        ],
        256,
        &mut budget,
    )?;
    let canonical = std::str::from_utf8(&output)?.trim();
    if canonical.is_empty() || !canonical.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("cannot resolve {commit_id} to a commit object").into());
    }
    Ok(canonical.to_owned())
}

pub fn refresh_changed(c: &mut Connection) -> Result<()> {
    refresh_scoped(c, None)
}

pub fn refresh_update(c: &mut Connection, commit_id: &str) -> Result<()> {
    refresh_scoped(c, Some(commit_id))
}

fn refresh_scoped(c: &mut Connection, target: Option<&str>) -> Result<()> {
    let version = snapshot_version(c)?;
    let schema: i64 = c.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if schema != 4 {
        return Err("scoped refresh requires a version-4 index; run repodb refresh first".into());
    }

    let mut budget = Budget::new(
        MAX_REFRESH_ACCOUNTING_BYTES,
        "scoped history refresh preparation accounting",
    );
    let target_commit_id = match target {
        Some(commit_id) => commit_id.to_owned(),
        None => {
            let id = head(&mut budget)?.ok_or("scoped refresh requires a committed HEAD")?;
            let branch = git_output(
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
                1024,
                &mut budget,
            )?;
            if std::str::from_utf8(&branch)?.trim().is_empty() {
                return Err("scoped refresh requires HEAD to be on a branch".into());
            }
            id
        }
    };
    let already_indexed: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM git_commits WHERE commit_id=?1)",
        [&target_commit_id],
        |row| row.get(0),
    )?;
    if already_indexed {
        return Ok(());
    }

    let parent = if target.is_some() {
        None
    } else {
        let mut cursor = target_commit_id.clone();
        let mut indexed_ancestor = None;
        loop {
            let parent_record = String::from_utf8(git_output(
                &["rev-list", "--parents", "-n", "1", &cursor],
                256,
                &mut budget,
            )?)?;
            let mut parents = parent_record.split_whitespace();
            if parents.next() != Some(cursor.as_str()) {
                return Err("Git returned an invalid first-parent record".into());
            }
            let Some(first_parent) = parents.next().map(str::to_owned) else {
                break;
            };
            let parent_indexed: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM git_commits WHERE commit_id=?1)",
                [&first_parent],
                |row| row.get(0),
            )?;
            if parent_indexed {
                indexed_ancestor = Some(first_parent);
                break;
            }
            cursor = first_parent;
        }
        indexed_ancestor
    };
    let timestamp: i64 = String::from_utf8(git_output(
        &["show", "-s", "--format=%ct", &target_commit_id],
        128,
        &mut budget,
    )?)?
    .trim()
    .parse()?;

    c.execute_batch("PRAGMA temp_store=FILE;
        DROP TABLE IF EXISTS temp.scoped_commit; DROP TABLE IF EXISTS temp.scoped_blobs;
        DROP TABLE IF EXISTS temp.scoped_occurrences; DROP TABLE IF EXISTS temp.scoped_variants;
        DROP TABLE IF EXISTS temp.scoped_units; DROP TABLE IF EXISTS temp.scoped_embeddings;
        DROP TABLE IF EXISTS temp.scoped_config;
        CREATE TEMP TABLE scoped_commit(commit_id TEXT PRIMARY KEY,timestamp INTEGER NOT NULL);
        CREATE TEMP TABLE scoped_blobs(blob_id TEXT PRIMARY KEY,status TEXT NOT NULL,content TEXT);
        CREATE TEMP TABLE scoped_occurrences(commit_id TEXT NOT NULL,blob_id TEXT NOT NULL,path BLOB NOT NULL,segmentation_kind TEXT NOT NULL,PRIMARY KEY(commit_id,path));
        CREATE TEMP TABLE scoped_variants(blob_id TEXT NOT NULL,segmentation_kind TEXT NOT NULL,PRIMARY KEY(blob_id,segmentation_kind));
        CREATE TEMP TABLE scoped_units(unit_id INTEGER PRIMARY KEY,blob_id TEXT NOT NULL,segmentation_kind TEXT NOT NULL,text TEXT NOT NULL,byte_start INTEGER NOT NULL,byte_end INTEGER NOT NULL,line_start INTEGER NOT NULL,line_end INTEGER NOT NULL);
        CREATE TEMP TABLE scoped_embeddings(unit_id INTEGER PRIMARY KEY,vector BLOB NOT NULL);
        CREATE TEMP TABLE scoped_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    c.execute(
        "INSERT INTO temp.scoped_commit VALUES(?1,?2)",
        params![target_commit_id, timestamp],
    )?;

    let (command, model, configured_dimension) = configuration(c)?;
    let mut next_unit_id: i64 = c.query_row(
        "SELECT COALESCE(MAX(unit_id),0) FROM indexed_units",
        [],
        |row| row.get(0),
    )?;
    let mut embedding_dimension = None;
    let mut pipeline =
        EmbeddingPipeline::new(command.clone(), model.clone(), configured_dimension)?;
    let mut batch = Vec::with_capacity(HISTORY_EMBEDDING_BATCH_SIZE);
    {
        let mut parent_occurrence = c.prepare(
            "SELECT blob_id,segmentation_kind FROM git_occurrences WHERE commit_id=?1 AND path=?2",
        )?;
        let mut existing_variant = c.prepare(
            "SELECT EXISTS(SELECT 1 FROM git_blob_units WHERE blob_id=?1 AND segmentation_kind=?2)",
        )?;
        let mut staged_variant = c.prepare(
            "SELECT EXISTS(SELECT 1 FROM temp.scoped_variants WHERE blob_id=?1 AND segmentation_kind=?2)",
        )?;
        let mut existing_blob = c.prepare("SELECT status FROM blob_contents WHERE blob_id=?1")?;
        let mut staged_blob =
            c.prepare("SELECT status,content FROM temp.scoped_blobs WHERE blob_id=?1")?;
        let mut blob_stage =
            c.prepare("INSERT OR IGNORE INTO temp.scoped_blobs VALUES(?1,?2,?3)")?;
        let mut variant_stage =
            c.prepare("INSERT OR IGNORE INTO temp.scoped_variants VALUES(?1,?2)")?;
        let mut occurrence_stage =
            c.prepare("INSERT INTO temp.scoped_occurrences VALUES(?1,?2,?3,?4)")?;
        let mut unit_stage =
            c.prepare("INSERT INTO temp.scoped_units VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
        let mut embedding_statement =
            c.prepare("INSERT INTO temp.scoped_embeddings VALUES(?1,?2)")?;

        for_each_tree_record(&target_commit_id, &mut budget, |record, budget| {
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or("invalid Git tree record")?;
            let metadata = std::str::from_utf8(&record[..tab])?;
            let mut fields = metadata.split_whitespace();
            let _mode = fields.next().ok_or("invalid Git tree metadata")?;
            let object_type = fields.next().ok_or("invalid Git tree metadata")?;
            let blob_id = fields.next().ok_or("invalid Git tree metadata")?;
            if fields.next().is_some() {
                return Err("invalid Git tree metadata".into());
            }
            if object_type != "blob" {
                return Ok(());
            }
            let path = &record[tab + 1..];
            let kind = segmentation::kind(path);
            let unchanged = if let Some(parent) = parent.as_deref() {
                parent_occurrence
                    .query_row(params![parent, path], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .optional()?
                    .is_some_and(|(parent_blob, parent_kind)| {
                        parent_blob == blob_id && parent_kind == kind
                    })
            } else {
                false
            };
            if unchanged {
                occurrence_stage.execute(params![target_commit_id, blob_id, path, kind])?;
                return Ok(());
            }

            let has_existing_variant: bool =
                existing_variant.query_row(params![blob_id, kind], |row| row.get(0))?;
            let has_staged_variant: bool =
                staged_variant.query_row(params![blob_id, kind], |row| row.get(0))?;
            if has_existing_variant || has_staged_variant {
                occurrence_stage.execute(params![target_commit_id, blob_id, path, kind])?;
                return Ok(());
            }

            let staged = staged_blob
                .query_row([blob_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .optional()?;
            let content_from_staging = staged.is_some();
            let (status, content) = if let Some((status, content)) = staged {
                (status, content)
            } else {
                let stored_status = existing_blob
                    .query_row([blob_id], |row| row.get::<_, String>(0))
                    .optional()?;
                if stored_status
                    .as_deref()
                    .is_some_and(|status| status != "text")
                {
                    variant_stage.execute(params![blob_id, kind])?;
                    occurrence_stage.execute(params![target_commit_id, blob_id, path, kind])?;
                    return Ok(());
                }
                let size: u64 =
                    String::from_utf8(git_output(&["cat-file", "-s", blob_id], 64, budget)?)?
                        .trim()
                        .parse()?;
                let bytes = git_output(
                    &["cat-file", "blob", blob_id],
                    usize::try_from(size).unwrap_or(usize::MAX),
                    budget,
                )?;
                let (content, status) = if bytes.contains(&0) {
                    (None, "binary")
                } else {
                    match String::from_utf8(bytes) {
                        Ok(content) => (Some(content), "text"),
                        Err(_) => (None, "unsupported"),
                    }
                };
                blob_stage.execute(params![blob_id, status, content])?;
                (status.to_owned(), content)
            };

            if status == "text" {
                let content = content.ok_or("text Git blob is missing staged content")?;
                if content_from_staging {
                    budget.charge(content.len(), "staged scoped Git blob text")?;
                }
                StagedUnitWriter {
                    next_unit_id: &mut next_unit_id,
                    unit_statement: &mut unit_stage,
                    batch: &mut batch,
                    pipeline: &mut pipeline,
                    embedding_dimension: &mut embedding_dimension,
                    embedding_statement: &mut embedding_statement,
                    budget,
                    budget_label: "scoped embedding batch text",
                }
                .segment_blob(&content, blob_id, kind)?;
                if content_from_staging {
                    budget.release(content.len());
                }
            }
            variant_stage.execute(params![blob_id, kind])?;
            occurrence_stage.execute(params![target_commit_id, blob_id, path, kind])?;
            Ok(())
        })?;
        flush_embedding_batch(
            &mut batch,
            &mut pipeline,
            &mut embedding_dimension,
            &mut embedding_statement,
            &mut budget,
        )?;
        pipeline.finish(
            &mut embedding_statement,
            &mut embedding_dimension,
            &mut budget,
        )?;
    }
    if let Some(dimension) = embedding_dimension {
        for (key, value) in [
            ("command", command),
            ("model", model),
            ("dimension", dimension.to_string()),
        ] {
            c.execute(
                "INSERT INTO temp.scoped_config VALUES(?1,?2)",
                params![key, value],
            )?;
        }
    }

    let tx = publish_transaction(c, version)?;
    tx.execute(
        "INSERT INTO git_commits SELECT * FROM temp.scoped_commit",
        [],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO blob_contents(blob_id,status) SELECT blob_id,status FROM temp.scoped_blobs",
        [],
    )?;
    tx.execute(
        "INSERT INTO indexed_units SELECT unit_id,byte_start,byte_end,line_start,line_end FROM temp.scoped_units",
        [],
    )?;
    tx.execute(
        "INSERT INTO git_blob_units SELECT blob_id,unit_id,segmentation_kind FROM temp.scoped_units",
        [],
    )?;
    tx.execute(
        "INSERT INTO unit_embeddings SELECT * FROM temp.scoped_embeddings",
        [],
    )?;
    tx.execute(
        "INSERT INTO git_occurrences SELECT * FROM temp.scoped_occurrences",
        [],
    )?;
    {
        let mut config = tx.prepare("SELECT key,value FROM temp.scoped_config ORDER BY key")?;
        let mut rows = config.query([])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            tx.execute(
                "INSERT INTO embedding_config VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn publish_refresh(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("DROP TABLE IF EXISTS diff_references; DROP TABLE IF EXISTS unit_embeddings; DROP TABLE IF EXISTS commit_message_units; DROP TABLE IF EXISTS git_blob_units; DROP TABLE IF EXISTS indexed_units; DROP TABLE IF EXISTS git_occurrences; DROP TABLE IF EXISTS blob_contents; DROP TABLE IF EXISTS git_commits;
        CREATE TABLE git_commits(commit_id TEXT PRIMARY KEY NOT NULL, timestamp INTEGER NOT NULL);
        CREATE TABLE blob_contents(blob_id TEXT PRIMARY KEY NOT NULL, status TEXT NOT NULL);
        CREATE TABLE git_occurrences(commit_id TEXT NOT NULL REFERENCES git_commits, blob_id TEXT NOT NULL REFERENCES blob_contents, path BLOB NOT NULL, segmentation_kind TEXT NOT NULL, PRIMARY KEY(commit_id,path));
        CREATE TABLE indexed_units(unit_id INTEGER PRIMARY KEY, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL);
        CREATE TABLE git_blob_units(blob_id TEXT NOT NULL REFERENCES blob_contents, unit_id INTEGER NOT NULL REFERENCES indexed_units, segmentation_kind TEXT NOT NULL, PRIMARY KEY(blob_id,segmentation_kind,unit_id));
        CREATE TABLE commit_message_units(commit_id TEXT NOT NULL REFERENCES git_commits, unit_id INTEGER NOT NULL REFERENCES indexed_units, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL, PRIMARY KEY(commit_id,unit_id));
        CREATE TABLE diff_references(parent_commit_id TEXT NOT NULL,child_commit_id TEXT NOT NULL REFERENCES git_commits,hunk_id INTEGER NOT NULL,side TEXT NOT NULL,old_path BLOB,new_path BLOB,old_line_start INTEGER,old_line_end INTEGER,new_line_start INTEGER,new_line_end INTEGER,blob_id TEXT NOT NULL,unit_id INTEGER NOT NULL REFERENCES indexed_units,byte_start INTEGER NOT NULL,byte_end INTEGER NOT NULL,line_start INTEGER NOT NULL,line_end INTEGER NOT NULL,PRIMARY KEY(parent_commit_id,child_commit_id,hunk_id,side,unit_id));
        CREATE TABLE unit_embeddings(unit_id INTEGER PRIMARY KEY REFERENCES indexed_units, vector BLOB NOT NULL);")?;
    tx.execute(
        "INSERT INTO git_commits SELECT * FROM temp.refresh_commits",
        [],
    )?;
    tx.execute(
        "INSERT INTO blob_contents SELECT blob_id,status FROM temp.refresh_blobs",
        [],
    )?;
    tx.execute(
        "INSERT INTO git_occurrences SELECT * FROM temp.refresh_occurrences",
        [],
    )?;
    tx.execute(
        "INSERT INTO indexed_units SELECT unit_id,byte_start,byte_end,line_start,line_end FROM temp.refresh_units",
        [],
    )?;
    tx.execute("INSERT INTO git_blob_units SELECT blob_id,unit_id,segmentation_kind FROM temp.refresh_units WHERE segmentation_kind NOT IN ('commit-message','diff')", [])?;
    tx.execute(
        "INSERT INTO commit_message_units SELECT * FROM temp.refresh_commit_message_units",
        [],
    )?;
    tx.execute("INSERT INTO diff_references SELECT r.parent_commit_id,r.child_commit_id,r.hunk_id,r.side,r.old_path,r.new_path,r.old_line_start,r.old_line_end,r.new_line_start,r.new_line_end,r.blob_id,r.unit_id,u.byte_start,u.byte_end,u.line_start,u.line_end FROM temp.refresh_diff_references r JOIN temp.refresh_units u USING(unit_id)", [])?;
    tx.execute(
        "INSERT INTO unit_embeddings SELECT * FROM temp.refresh_embeddings",
        [],
    )?;
    let config = {
        let mut statement = tx.prepare("SELECT key,value FROM temp.refresh_config ORDER BY key")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (key, value) in config {
        tx.execute(
            "INSERT INTO embedding_config(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
    }
    tx.execute_batch("DROP TABLE IF EXISTS indexed_files; DROP TABLE IF EXISTS embeddings; PRAGMA user_version=4;")?;
    Ok(())
}

pub(crate) fn diff_references(
    c: &Connection,
    parent_commit_id: Option<&str>,
    child_commit_id: Option<&str>,
) -> Result<Vec<DiffReference>> {
    let mut statement = c.prepare("SELECT parent_commit_id,child_commit_id,hunk_id,side,old_path,new_path,old_line_start,old_line_end,new_line_start,new_line_end,blob_id,byte_start,byte_end FROM diff_references WHERE (?1 IS NULL OR parent_commit_id=?1) AND (?2 IS NULL OR child_commit_id=?2) ORDER BY child_commit_id,parent_commit_id,hunk_id,side,byte_start")?;
    let rows = statement.query_map(params![parent_commit_id, child_commit_id], |row| {
        let side: String = row.get(3)?;
        let maybe_usize = |index| -> rusqlite::Result<Option<usize>> {
            row.get::<_, Option<i64>>(index)?
                .map(|value| {
                    usize::try_from(value)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
                })
                .transpose()
        };
        Ok(DiffReference {
            parent_commit_id: row.get(0)?,
            child_commit_id: row.get(1)?,
            hunk_id: row.get(2)?,
            side: match side.as_str() {
                "added" => DiffSide::Added,
                "removed" => DiffSide::Removed,
                _ => return Err(rusqlite::Error::InvalidQuery),
            },
            old_path: row.get(4)?,
            new_path: row.get(5)?,
            old_line_start: maybe_usize(6)?,
            old_line_end: maybe_usize(7)?,
            new_line_start: maybe_usize(8)?,
            new_line_end: maybe_usize(9)?,
            blob_id: row.get(10)?,
            byte_start: usize::try_from(row.get::<_, i64>(11)?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            byte_end: usize::try_from(row.get::<_, i64>(12)?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn reembed(c: &mut Connection, command: &str, model: &str) -> Result<()> {
    let version = snapshot_version(c)?;
    c.execute_batch("PRAGMA temp_store=FILE; DROP TABLE IF EXISTS temp.reembed_vectors; CREATE TEMP TABLE reembed_vectors(unit_id INTEGER PRIMARY KEY, vector BLOB NOT NULL);")?;
    let mut preparation_bytes = Budget::new(
        MAX_REFRESH_ACCOUNTING_BYTES,
        "history refresh preparation accounting",
    );
    let mut dim = 0;
    let schema: i64 = c.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    {
        let mut units = c.prepare(if schema >= 4 {
            "SELECT unit_id,blob_id,byte_start,byte_end FROM indexed_units JOIN git_blob_units USING(unit_id) ORDER BY unit_id"
        } else {
            "SELECT unit_id,text FROM indexed_units ORDER BY unit_id"
        })?;
        let mut rows = units.query([])?;
        let mut blob_cache = GitBlobReader::new(None)?;
        let mut staged = c.prepare("INSERT INTO temp.reembed_vectors VALUES(?1,?2)")?;
        while let Some(row) = rows.next()? {
            let unit_id: usize = row.get(0)?;
            let text: String = if schema >= 4 {
                let blob_id: String = row.get(1)?;
                let start: usize = row.get(2)?;
                let end: usize = row.get(3)?;
                hydrate_unit(&blob_id, start, end, &mut blob_cache)?
            } else {
                row.get(1)?
            };
            let (mut vector, dimension) = vectors(
                std::iter::once(text.as_str()),
                command,
                model,
                dim,
                &mut preparation_bytes,
            )?;
            let encoded = vector.pop().ok_or("embedding produced no vector")?;
            staged.execute(params![unit_id, encoded])?;
            dim = dimension;
        }
        blob_cache.finish()?;
    }
    let has_commit_messages: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='commit_message_units')",
        [],
        |row| row.get(0),
    )?;
    if schema >= 4 && has_commit_messages {
        let mut rows = c.prepare(
            "SELECT commit_id,unit_id,byte_start,byte_end FROM commit_message_units ORDER BY commit_id,unit_id",
        )?;
        let mut messages = rows.query([])?;
        let mut staged = c.prepare("INSERT INTO temp.reembed_vectors VALUES(?1,?2)")?;
        while let Some(row) = messages.next()? {
            let commit_id: String = row.get(0)?;
            let unit_id: usize = row.get(1)?;
            let start: usize = row.get(2)?;
            let end: usize = row.get(3)?;
            let text = hydrate_commit_message(&commit_id, start, end)?;
            let (mut vector, dimension) = vectors(
                std::iter::once(text.as_str()),
                command,
                model,
                dim,
                &mut preparation_bytes,
            )?;
            let encoded = vector.pop().ok_or("embedding produced no vector")?;
            staged.execute(params![unit_id, encoded])?;
            dim = dimension;
        }
    }
    let tx = publish_transaction(c, version)?;
    tx.execute("DELETE FROM unit_embeddings", [])?;
    tx.execute("INSERT INTO unit_embeddings SELECT unit_id,vector FROM temp.reembed_vectors ORDER BY unit_id", [])?;
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
    c.execute_batch("DROP TABLE temp.reembed_vectors")?;
    Ok(())
}

fn hydrate_unit(
    blob_id: &str,
    start: usize,
    end: usize,
    reader: &mut GitBlobReader,
) -> Result<String> {
    let bytes = reader.blob(blob_id)?;
    git_ranges::slice_utf8(&bytes, blob_id, start, end, "indexed")
}

pub fn search_commit_messages(
    c: &Connection,
    query: &str,
    mode: &str,
    limit: usize,
    commits: &[String],
) -> Result<Vec<SearchResult>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let query_embedding = if mode == "exact" {
        None
    } else {
        Some(query_vector(c, query)?)
    };
    let mut statement = c.prepare(
        "SELECT m.commit_id,m.byte_start,m.byte_end,m.line_start,m.line_end,
         CASE WHEN ?2 IS NULL THEN NULL ELSE vec_distance_cosine(e.vector,?2) END
         FROM commit_message_units m JOIN unit_embeddings e USING(unit_id)
         WHERE m.commit_id IN (SELECT value FROM json_each(?1))
         ORDER BY CASE WHEN ?2 IS NULL THEN m.commit_id END,
                  CASE WHEN ?2 IS NOT NULL THEN vec_distance_cosine(e.vector,?2) END,
                  m.commit_id,m.unit_id",
    )?;
    let mut rows = statement.query(params![
        serde_json::to_string(commits)?,
        query_embedding.as_ref().map(|vector| as_bytes(vector))
    ])?;
    let lowercase_query = query.to_lowercase();
    let mut results = Vec::new();
    while let Some(row) = rows.next()? {
        let commit_id: String = row.get(0)?;
        let start: usize = row.get(1)?;
        let end: usize = row.get(2)?;
        let line_start: usize = row.get(3)?;
        let line_end: usize = row.get(4)?;
        let distance: Option<f64> = row.get(5)?;
        let text = hydrate_commit_message(&commit_id, start, end)?;
        if mode == "exact" && !text.to_lowercase().contains(&lowercase_query) {
            continue;
        }
        results.push(SearchResult {
            path: "[commit message]".to_owned(),
            line_start,
            line_end,
            text,
            commit_id: Some(commit_id),
            distance,
            blob_id: None,
            page: None,
            repo_id: None,
            first_retrieved_at: None,
            last_retrieved_at: None,
        });
        if results.len() == limit {
            break;
        }
    }
    Ok(results)
}

fn hydrate_commit_message(commit_id: &str, start: usize, end: usize) -> Result<String> {
    let mut budget = Budget::new(
        MAX_REFRESH_ACCOUNTING_BYTES,
        "commit message hydration limit exceeded",
    );
    let size: usize =
        String::from_utf8(git_output(&["cat-file", "-s", commit_id], 64, &mut budget)?)?
            .trim()
            .parse()?;
    let object = git_output(&["cat-file", "commit", commit_id], size, &mut budget)?;
    if start > end || end > object.len() {
        return Err(format!("invalid indexed commit message range for {commit_id}").into());
    }
    String::from_utf8(object[start..end].to_vec()).map_err(Into::into)
}

pub fn search(
    c: &Connection,
    query: &str,
    mode: &str,
    limit: usize,
    commits: &[String],
    include: impl Fn(&SearchResult) -> bool,
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
    let variant = if schema >= 3 {
        "AND b.segmentation_kind=o.segmentation_kind"
    } else {
        ""
    };
    let occurrence_partition = "blob_id";
    let range_columns = if schema >= 4 {
        "u.byte_start,u.byte_end,''"
    } else {
        "NULL,NULL,u.text"
    };
    let mut s = c.prepare(&format!(
        "WITH eligible AS (
        SELECT o.*, row_number() OVER (PARTITION BY {occurrence_partition}
            ORDER BY timestamp DESC, o.commit_id DESC, path ASC) occurrence_rank
        FROM git_occurrences o JOIN git_commits c USING(commit_id)
        WHERE o.commit_id IN (SELECT value FROM json_each(?1)))
        SELECT o.path,u.line_start,u.line_end,{range_columns},o.commit_id,o.blob_id,
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
                r.get::<_, Option<usize>>(3)?,
                r.get::<_, Option<usize>>(4)?,
                r.get::<_, String>(5)?,
                SearchResult {
                    path: String::new(),
                    line_start: r.get(1)?,
                    line_end: r.get(2)?,
                    text: String::new(),
                    commit_id: Some(r.get(6)?),
                    blob_id: Some(r.get(7)?),
                    distance: r.get(8)?,
                    page: None,
                    repo_id: None,
                    first_retrieved_at: None,
                    last_retrieved_at: None,
                },
            ))
        },
    )?;
    let mut results = vec![];
    for row in rows {
        let (path, byte_start, byte_end, legacy_text, mut result) = row?;
        result.path = match String::from_utf8(path) {
            Ok(path) => path,
            Err(error) => crate::encode_git_path(error.into_bytes()),
        };
        results.push((byte_start, byte_end, legacy_text, result));
    }
    let mut blob_reader = GitBlobReader::new(None)?;
    let hydrate = |candidate: &mut (Option<usize>, Option<usize>, String, SearchResult),
                   reader: &mut GitBlobReader|
     -> Result<()> {
        candidate.3.text = match (candidate.0, candidate.1) {
            (Some(start), Some(end)) => hydrate_unit(
                candidate
                    .3
                    .blob_id
                    .as_deref()
                    .ok_or("missing Git blob ID")?,
                start,
                end,
                reader,
            )?,
            (None, None) => candidate.2.clone(),
            _ => return Err("invalid indexed Git byte range".into()),
        };
        Ok(())
    };
    results.retain(|candidate| include(&candidate.3));
    let lowercase_query = query.to_lowercase();
    if mode == "exact" {
        let mut hydration_error = None;
        results.retain_mut(|candidate| {
            if hydration_error.is_some() {
                return false;
            }
            match hydrate(candidate, &mut blob_reader) {
                Ok(()) => candidate.3.text.to_lowercase().contains(&lowercase_query),
                Err(error) => {
                    hydration_error = Some(error);
                    false
                }
            }
        });
        if let Some(error) = hydration_error {
            return Err(error);
        }
    }
    if mode != "exact" {
        results.sort_by(|a, b| {
            a.3.distance
                .unwrap()
                .total_cmp(&b.3.distance.unwrap())
                .then(a.3.path.cmp(&b.3.path))
                .then(a.3.line_start.cmp(&b.3.line_start))
                .then(a.3.blob_id.cmp(&b.3.blob_id))
        });
        let mut index = 0;
        if mode == "combined" {
            let mut hydration_error = None;
            results.retain_mut(|candidate| {
                if hydration_error.is_some() {
                    return false;
                }
                match hydrate(candidate, &mut blob_reader) {
                    Ok(()) => {
                        let keep = index < limit
                            || candidate.3.text.to_lowercase().contains(&lowercase_query);
                        index += 1;
                        keep
                    }
                    Err(error) => {
                        hydration_error = Some(error);
                        false
                    }
                }
            });
            if let Some(error) = hydration_error {
                return Err(error);
            }
        } else {
            results.truncate(limit);
        }
        if mode == "semantic" {
            for candidate in &mut results {
                hydrate(candidate, &mut blob_reader)?;
            }
        }
    }
    blob_reader.finish()?;
    Ok(results.into_iter().map(|candidate| candidate.3).collect())
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
                    Budget::new(MAX_REFRESH_ACCOUNTING_BYTES, "history query accounting");
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
