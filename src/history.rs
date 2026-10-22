use crate::{
    Result, SearchResult, as_bytes,
    budget::{Budget, MAX_REFRESH_ACCOUNTING_BYTES},
    git_objects::GitBlobReader,
    git_ranges, process, query_vector, segmentation,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread::JoinHandle,
};

const EMBEDDING_STDERR_BYTES: usize = 64 * 1024;
fn memory_limit() -> Box<dyn std::error::Error> {
    "history refresh preparation size overflow".into()
}

const MAX_GIT_STDERR_BYTES: usize = 64 * 1024;
const RENAME_THRESHOLD_PERCENT: u8 = 50;
const COPY_THRESHOLD_PERCENT: u8 = 50;
const FIND_COPIES_HARDER: bool = true;
const DIFF_ALGORITHM: &str = "myers";
const FIND_RENAMES_ARG: &str = "--find-renames=50%";
const FIND_COPIES_ARG: &str = "--find-copies=50%";
const FIND_COPIES_HARDER_ARG: &str = "--find-copies-harder";
const DIFF_ALGORITHM_ARG: &str = "--diff-algorithm=myers";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiffSide {
    Added,
    Removed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiffChangeKind {
    Added,
    Removed,
    Modified,
    HeuristicRename,
    HeuristicCopy,
    TypeChanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiffFileChange {
    pub parent_commit_id: String,
    pub child_commit_id: String,
    pub change_index: i64,
    pub kind: DiffChangeKind,
    pub old_path: Option<Vec<u8>>,
    pub new_path: Option<Vec<u8>>,
    /// Git's heuristic similarity estimate (0–100), not a calibrated probability.
    pub similarity_percent: Option<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiffDetectionSettings {
    pub rename_threshold_percent: u8,
    pub copy_threshold_percent: u8,
    pub find_copies_harder: bool,
    pub git_version: String,
    pub diff_algorithm: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiffMetadata {
    pub settings: DiffDetectionSettings,
    pub changes: Vec<DiffFileChange>,
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
    pub line_start: usize,
    pub line_end: usize,
    pub blob_id: String,
    pub unit_id: i64,
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DiffSearchMode {
    Exact,
    Semantic,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DiffSearchResult {
    pub text: String,
    pub reference: DiffReference,
    pub distance: Option<f64>,
}

pub fn database_git_path(database_path: &Path) -> Result<Option<Vec<u8>>> {
    let database_path = match std::fs::canonicalize(database_path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut budget = Budget::new(MAX_REFRESH_ACCOUNTING_BYTES, "database path discovery");
    let root_bytes = git_output(&["rev-parse", "--show-toplevel"], 4096, &mut budget)?;
    let root_bytes = root_bytes.strip_suffix(b"\n").unwrap_or(&root_bytes);
    let root = Path::new(std::str::from_utf8(root_bytes)?);
    let root = std::fs::canonicalize(root)?;
    let Ok(relative_path) = database_path.strip_prefix(root) else {
        return Ok(None);
    };
    Ok(Some(relative_path.as_os_str().as_encoded_bytes().to_vec()))
}

fn git_capture_limits(
    requested_stdout: usize,
    preparation_bytes: usize,
    total_limit: usize,
) -> Result<(usize, usize)> {
    let remaining = total_limit.checked_sub(preparation_bytes).ok_or_else(|| {
        format!("history refresh accounting exceeded its configured limit ({preparation_bytes} > {total_limit})")
    })?;
    let stdout_reserve = if remaining > MAX_GIT_STDERR_BYTES {
        remaining - MAX_GIT_STDERR_BYTES
    } else {
        remaining / 4
    };
    let minimum_stdout = requested_stdout.min(4096).min(stdout_reserve);
    let stderr = remaining
        .saturating_sub(minimum_stdout)
        .min(MAX_GIT_STDERR_BYTES);
    let stdout = requested_stdout.min(remaining - stderr);
    Ok((stdout, stderr))
}

fn git_output(
    args: &[&str],
    stdout_limit: usize,
    preparation_bytes: &mut Budget,
) -> Result<Vec<u8>> {
    let (stdout_limit, stderr_limit) = git_capture_limits(
        stdout_limit,
        preparation_bytes.used(),
        preparation_bytes.limit(),
    )?;
    let mut command = Command::new("git");
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process::spawn_in_process_group(&mut command)?;
    let output = process::capture_with_input_timeout(
        &mut child,
        &[],
        stdout_limit,
        stderr_limit,
        process::refresh_subprocess_timeout()?,
    )?;
    if output.exceeded {
        return Err(format!(
            "history Git output exceeded its {stdout_limit}-byte budget for {args:?}"
        )
        .into());
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

fn git_diff_output(
    parent: &str,
    child: &str,
    old_path: &[u8],
    new_path: &[u8],
    preparation_bytes: &mut Budget,
) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;

    let (stdout_limit, stderr_limit) = git_capture_limits(
        preparation_bytes
            .remaining()
            .saturating_sub(MAX_GIT_STDERR_BYTES),
        preparation_bytes.used(),
        preparation_bytes.limit(),
    )?;
    let mut command = Command::new("git");
    command
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args([
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--unified=0",
            FIND_RENAMES_ARG,
            FIND_COPIES_ARG,
            FIND_COPIES_HARDER_ARG,
            DIFF_ALGORITHM_ARG,
            parent,
            child,
            "--",
        ])
        .arg(std::ffi::OsStr::from_bytes(old_path));
    if old_path != new_path {
        command.arg(std::ffi::OsStr::from_bytes(new_path));
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child_process = process::spawn_in_process_group(&mut command)?;
    let output = process::capture_with_input_timeout(
        &mut child_process,
        &[],
        stdout_limit,
        stderr_limit,
        process::refresh_subprocess_timeout()?,
    )?;
    if output.exceeded {
        return Err(memory_limit());
    }
    let stdout_capacity = output.stdout.capacity();
    let stderr_capacity = output.stderr.capacity();
    preparation_bytes.charge(stdout_capacity, "Git diff output capacity")?;
    preparation_bytes.charge(stderr_capacity, "Git diff stderr capacity")?;
    if !output.status.success() {
        let error = format!(
            "git diff failed: {}",
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

#[derive(Clone, Copy)]
struct ChangedLines {
    side: DiffSide,
    hunk_id: i64,
    first: usize,
    last: usize,
    old_anchor: Option<(usize, usize)>,
    new_anchor: Option<(usize, usize)>,
}

fn parse_diff_line_range(value: &str) -> Result<(usize, usize)> {
    let value = value.get(1..).ok_or("invalid Git hunk range")?;
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    let start: usize = start.parse()?;
    let count: usize = count.parse()?;
    Ok((start, count))
}

fn flush_changed_lines(
    result: &mut Vec<ChangedLines>,
    old_changed: &mut Option<(usize, usize)>,
    new_changed: &mut Option<(usize, usize)>,
    hunk_id: i64,
    old_anchor: Option<(usize, usize)>,
    new_anchor: Option<(usize, usize)>,
    budget: &mut Budget,
) -> Result<()> {
    for (changed, side) in [
        (old_changed, DiffSide::Removed),
        (new_changed, DiffSide::Added),
    ] {
        if let Some((first, last)) = changed.take() {
            budget.charge_records(1, "Git diff hunk span")?;
            budget.reserve_vec(result, 1, "Git diff hunk spans")?;
            result.push(ChangedLines {
                side,
                hunk_id,
                first,
                last,
                old_anchor,
                new_anchor,
            });
        }
    }
    Ok(())
}

fn changed_lines(
    patch: &[u8],
    first_hunk_id: i64,
    old_line_count: usize,
    new_line_count: usize,
    budget: &mut Budget,
) -> Result<Vec<ChangedLines>> {
    let mut result = Vec::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut old_changed = None;
    let mut new_changed = None;
    let mut old_anchor: Option<(usize, usize)> = None;
    let mut new_anchor: Option<(usize, usize)> = None;
    let mut hunk_id = first_hunk_id;
    let mut seen_hunk = false;
    let mut in_hunk = false;
    for line in patch.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"@@ ") {
            if seen_hunk {
                flush_changed_lines(
                    &mut result,
                    &mut old_changed,
                    &mut new_changed,
                    hunk_id,
                    old_anchor,
                    new_anchor,
                    budget,
                )?;
                hunk_id += 1;
            }
            seen_hunk = true;
            in_hunk = true;
            let header = std::str::from_utf8(line)?;
            let mut parts = header.split_whitespace();
            if parts.next() != Some("@@") {
                return Err("invalid Git hunk header".into());
            }
            let (old_start, old_count) =
                parse_diff_line_range(parts.next().ok_or("missing old Git hunk range")?)?;
            let (new_start, new_count) =
                parse_diff_line_range(parts.next().ok_or("missing new Git hunk range")?)?;
            old_anchor = if old_count == 0 {
                (old_line_count > 0).then_some((old_start.max(1), old_start.max(1)))
            } else {
                Some((old_start, old_start + old_count - 1))
            };
            new_anchor = if new_count == 0 {
                (new_line_count > 0).then_some((new_start.max(1), new_start.max(1)))
            } else {
                Some((new_start, new_start + new_count - 1))
            };
            old_line = old_start;
            new_line = new_start;
            continue;
        }
        if !in_hunk {
            continue;
        }
        match line.first().copied() {
            Some(b'-') => {
                budget.charge_records(1, "removed Git diff line")?;
                old_changed =
                    Some(old_changed.map_or((old_line, old_line), |(first, _)| (first, old_line)));
                old_line += 1;
            }
            Some(b'+') => {
                budget.charge_records(1, "added Git diff line")?;
                new_changed =
                    Some(new_changed.map_or((new_line, new_line), |(first, _)| (first, new_line)));
                new_line += 1;
            }
            Some(b'\\') => {}
            Some(b' ') => {
                old_line += 1;
                new_line += 1;
            }
            _ => {}
        }
    }
    flush_changed_lines(
        &mut result,
        &mut old_changed,
        &mut new_changed,
        hunk_id,
        old_anchor,
        new_anchor,
        budget,
    )?;
    Ok(result)
}

fn line_byte_range(text: &str, first: usize, last: usize) -> Result<(usize, usize)> {
    if first == 0 || last < first {
        return Err("invalid changed line range".into());
    }
    let mut start = None;
    let mut end = None;
    let mut offset = 0usize;
    for (line_index, line) in text.split_inclusive('\n').enumerate() {
        let line_number = line_index + 1;
        if line_number == first {
            start = Some(offset);
        }
        offset += line.len();
        if line_number == last {
            end = Some(offset);
            break;
        }
    }
    let start = start.ok_or("changed line is outside Git blob")?;
    let mut end = end.ok_or("changed line is outside Git blob")?;
    let slice = &text[start..end];
    if slice.ends_with("\r\n") {
        end -= 2;
    } else if slice.ends_with('\n') {
        end -= 1;
    }
    Ok((start, end))
}

struct StageContext<'a, 'connection> {
    c: &'a Connection,
    next_unit_id: &'a mut i64,
    batch: &'a mut EmbeddingBatch,
    pipeline: &'a mut EmbeddingPipeline,
    embedding_dimension: &'a mut Option<usize>,
    embedding_statement: &'a mut rusqlite::Statement<'connection>,
    budget: &'a mut Budget,
}

struct DiffRange<'a> {
    parent: &'a str,
    child: &'a str,
    side: DiffSide,
    hunk_id: i64,
    old_path: &'a [u8],
    new_path: &'a [u8],
    blob_id: &'a str,
    blob_text: &'a str,
    first: usize,
    last: usize,
    old_anchor: Option<(usize, usize)>,
    new_anchor: Option<(usize, usize)>,
}

fn stage_diff_range(context: &mut StageContext<'_, '_>, range: DiffRange<'_>) -> Result<()> {
    let DiffRange {
        parent,
        child,
        side,
        hunk_id,
        old_path,
        new_path,
        blob_id,
        blob_text,
        first,
        last,
        old_anchor,
        new_anchor,
    } = range;
    let StageContext {
        c,
        next_unit_id,
        batch,
        pipeline,
        embedding_dimension,
        embedding_statement,
        budget,
    } = context;
    let next_unit_id = &mut **next_unit_id;
    let (range_start, range_end) = line_byte_range(blob_text, first, last)?;
    let changed_text = &blob_text[range_start..range_end];
    let mut unit_statement =
        c.prepare("INSERT INTO temp.refresh_units VALUES(?1,?2,'diff',?3,?4,?5,?6,?7)")?;
    let mut reference_statement = c.prepare("INSERT INTO temp.refresh_diff_references(parent_commit_id,child_commit_id,hunk_id,side,old_path,new_path,old_line_start,old_line_end,new_line_start,new_line_end,blob_id,unit_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)")?;
    let mut unit_error = None;
    segmentation::visit("fallback", changed_text, |unit| {
        let result = (|| -> Result<()> {
            budget.charge_records(1, "Git diff reference unit")?;
            budget.charge(unit.text.len(), "diff embedding batch text")?;
            *next_unit_id += 1;
            let byte_start = range_start + unit.byte_start;
            let byte_end = range_start + unit.byte_end;
            let line_start = first + unit.line_start - 1;
            let line_end = first + unit.line_end - 1;
            unit_statement.execute(params![
                *next_unit_id,
                blob_id,
                unit.text,
                byte_start,
                byte_end,
                line_start,
                line_end
            ])?;
            reference_statement.execute(params![
                parent,
                child,
                hunk_id,
                if side == DiffSide::Added {
                    "added"
                } else {
                    "removed"
                },
                old_path,
                new_path,
                old_anchor.map(|anchor| anchor.0),
                old_anchor.map(|anchor| anchor.1),
                new_anchor.map(|anchor| anchor.0),
                new_anchor.map(|anchor| anchor.1),
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
    Ok(())
}

fn stage_diff_pair(
    context: &mut StageContext<'_, '_>,
    parent: Option<&str>,
    child: &str,
    excluded_database_path: Option<&[u8]>,
) -> Result<()> {
    let StageContext {
        c,
        next_unit_id,
        batch,
        pipeline,
        embedding_dimension,
        embedding_statement,
        budget,
    } = context;
    let next_unit_id = &mut **next_unit_id;
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let args = if let Some(parent) = parent {
        vec![
            "diff-tree",
            "--no-commit-id",
            "--raw",
            FIND_RENAMES_ARG,
            FIND_COPIES_ARG,
            FIND_COPIES_HARDER_ARG,
            DIFF_ALGORITHM_ARG,
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
            FIND_RENAMES_ARG,
            FIND_COPIES_ARG,
            FIND_COPIES_HARDER_ARG,
            DIFF_ALGORITHM_ARG,
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
    let mut records = output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    let mut hunk_id = 0i64;
    let mut change_index = 0i64;
    let mut file_change_statement =
        c.prepare("INSERT INTO temp.refresh_diff_file_changes VALUES(?1,?2,?3,?4,?5,?6,?7)")?;
    while let Some(metadata_record) = records.next() {
        budget.charge_records(1, "Git diff file record")?;
        let metadata = std::str::from_utf8(metadata_record)?;
        let mut fields_iter = metadata.split_whitespace();
        let fields = [
            fields_iter.next().ok_or("invalid raw Git diff record")?,
            fields_iter.next().ok_or("invalid raw Git diff record")?,
            fields_iter.next().ok_or("invalid raw Git diff record")?,
            fields_iter.next().ok_or("invalid raw Git diff record")?,
            fields_iter.next().ok_or("invalid raw Git diff record")?,
        ];
        if fields_iter.next().is_some() || !fields[0].starts_with(':') {
            return Err("invalid raw Git diff record".into());
        }
        let status_token = fields[4].as_bytes();
        let status = status_token
            .first()
            .copied()
            .ok_or("missing Git diff status")?;
        let first_path = records.next().ok_or("missing Git diff path")?;
        let (kind, old_path, new_path) = match status {
            b'A' => ("added", None, Some(first_path)),
            b'D' => ("removed", Some(first_path), None),
            b'M' => ("modified", Some(first_path), Some(first_path)),
            b'T' => ("type_changed", Some(first_path), Some(first_path)),
            b'R' | b'C' => {
                let second_path = records.next().ok_or("missing Git destination path")?;
                let kind = if status == b'R' {
                    "heuristic_rename"
                } else {
                    "heuristic_copy"
                };
                (kind, Some(first_path), Some(second_path))
            }
            _ => continue,
        };
        if fields[0].trim_start_matches(':') == "160000" || fields[1] == "160000" {
            // Gitlinks reference commits, not blobs; the tracked tree walker skips them too.
            continue;
        }
        if excluded_database_path
            .is_some_and(|excluded| old_path == Some(excluded) || new_path == Some(excluded))
        {
            continue;
        }
        let similarity_percent = if status == b'R' || status == b'C' {
            let score = std::str::from_utf8(&status_token[1..])?.parse::<u8>()?;
            if score > 100 {
                return Err("invalid Git similarity score".into());
            }
            Some(score)
        } else {
            None
        };
        change_index += 1;
        file_change_statement.execute(params![
            parent.unwrap_or(EMPTY_TREE),
            child,
            change_index,
            kind,
            old_path,
            new_path,
            similarity_percent.map(i64::from),
        ])?;

        if matches!(status, b'M' | b'R' | b'C') {
            let Some(parent) = parent else {
                return Err("root diff cannot contain a modified, renamed or copied path".into());
            };
            let (Some(old_path), Some(new_path)) = (old_path, new_path) else {
                return Err("modified Git diff record is missing a path endpoint".into());
            };
            let old_blob_id = fields[2];
            let new_blob_id = fields[3];
            let old_bytes = git_output(
                &["cat-file", "blob", old_blob_id],
                budget.remaining().saturating_sub(MAX_GIT_STDERR_BYTES),
                budget,
            )?;
            let new_bytes = git_output(
                &["cat-file", "blob", new_blob_id],
                budget.remaining().saturating_sub(MAX_GIT_STDERR_BYTES),
                budget,
            )?;
            let (Ok(old_text), Ok(new_text)) = (
                std::str::from_utf8(&old_bytes),
                std::str::from_utf8(&new_bytes),
            ) else {
                continue;
            };
            if old_text.contains('\0') || new_text.contains('\0') {
                continue;
            }
            let patch = git_diff_output(parent, child, old_path, new_path, budget)?;
            let changes = changed_lines(
                &patch,
                hunk_id,
                old_text.lines().count(),
                new_text.lines().count(),
                budget,
            )?;
            let next_hunk_id = changes
                .iter()
                .map(|change| change.hunk_id)
                .max()
                .map_or(hunk_id, |last| last + 1);
            for change in changes {
                let (blob_id, text) = match change.side {
                    DiffSide::Added => (new_blob_id, new_text),
                    DiffSide::Removed => (old_blob_id, old_text),
                };
                stage_diff_range(
                    &mut StageContext {
                        c,
                        next_unit_id,
                        batch,
                        pipeline,
                        embedding_dimension,
                        embedding_statement,
                        budget,
                    },
                    DiffRange {
                        parent,
                        child,
                        side: change.side,
                        hunk_id: change.hunk_id,
                        old_path,
                        new_path,
                        blob_id,
                        blob_text: text,
                        first: change.first,
                        last: change.last,
                        old_anchor: change.old_anchor,
                        new_anchor: change.new_anchor,
                    },
                )?;
            }
            hunk_id = next_hunk_id;
            continue;
        }
        if status == b'T' {
            continue;
        }

        let (side, blob_id) = if status == b'A' {
            (DiffSide::Added, fields[3])
        } else {
            (DiffSide::Removed, fields[2])
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
    let mut command = Command::new("git");
    command
        .args(["ls-tree", "-r", "-z", "--full-tree", commit_id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process::spawn_in_process_group(&mut command)?;
    let process_id = child.id();
    let deadline =
        process::ProcessDeadline::start(process_id, process::refresh_subprocess_timeout()?);
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
                    preparation_bytes.charge_records(1, "Git tree record")?;
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
        process::terminate_process_group(&mut child, process_id);
    }
    let status = child.wait()?;
    let (stderr, _stderr_exceeded) = stderr.join().map_err(|_| "git stderr reader panicked")??;
    let stderr_capacity = stderr.capacity();
    let timed_out = deadline.finish();
    preparation_bytes.release(MAX_GIT_STDERR_BYTES - stderr_capacity);
    preparation_bytes.release(TREE_READ_BUFFER_BYTES);
    if timed_out {
        drop(stderr);
        preparation_bytes.release(stderr_capacity);
        return Err("process timed out".into());
    }
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
        2..=5 => Ok(true),
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
    let (stdout_limit, stderr_limit) =
        git_capture_limits(128, preparation_bytes.used(), preparation_bytes.limit())?;
    let mut command = Command::new("git");
    command
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process::spawn_in_process_group(&mut command)?;
    let output = process::capture_with_input_timeout(
        &mut child,
        &[],
        stdout_limit,
        stderr_limit,
        process::refresh_subprocess_timeout()?,
    )?;
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
    let mut command = Command::new("git");
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process::spawn_in_process_group(&mut command)?;
    let process_id = child.id();
    let deadline =
        process::ProcessDeadline::start(process_id, process::refresh_subprocess_timeout()?);
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
            preparation_bytes.charge_records(1, "Git commit")?;
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
        process::terminate_process_group(&mut child, process_id);
    }
    let status = child.wait()?;
    let (stderr, _) = stderr.join().map_err(|_| "git stderr reader panicked")??;
    let stderr_capacity = stderr.capacity();
    let timed_out = deadline.finish();
    preparation_bytes.release(READ_BUFFER_BYTES);
    preparation_bytes.release(MAX_GIT_STDERR_BYTES - stderr_capacity);
    if timed_out {
        drop(stderr);
        preparation_bytes.release(stderr_capacity);
        if let Some(id) = head_id {
            preparation_bytes.release(id.len());
        }
        return Err("process timed out".into());
    }
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
                        let mut budget = Budget::refresh("history embedding accounting")?;
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
                self.budget.charge_records(1, "history content unit")?;
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
    fn changed_diff_lines_obey_the_refresh_record_limit() {
        let patch = b"@@ -1 +1 @@\n-old\n+new\n";
        let mut budget = Budget::with_record_limit(1024, 1, "test");
        let error = changed_lines(patch, 0, 1, 1, &mut budget).err().unwrap();
        assert!(error.to_string().contains("record limit exceeded"));
    }

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
        enable_vector_extension();
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

        let child_commits = vec![child.clone()];
        assert!(
            search_diffs(&db, "needle", DiffSearchMode::Exact, 0, &child_commits)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_diffs(&db, "needle", DiffSearchMode::Exact, 10, &[])
                .unwrap()
                .is_empty()
        );
        let exact_added =
            search_diffs(&db, "add needle", DiffSearchMode::Exact, 10, &child_commits).unwrap();
        assert_eq!(exact_added.len(), 1);
        assert_eq!(exact_added[0].text, "add needle");
        assert_eq!(exact_added[0].reference.side, DiffSide::Added);
        assert_eq!(exact_added[0].reference.parent_commit_id, parent);
        assert_eq!(exact_added[0].distance, None);
        let exact_removed = search_diffs(
            &db,
            "remove needle",
            DiffSearchMode::Exact,
            10,
            &child_commits,
        )
        .unwrap();
        assert_eq!(exact_removed.len(), 1);
        assert_eq!(exact_removed[0].text, "remove needle");
        assert_eq!(exact_removed[0].reference.side, DiffSide::Removed);

        let semantic = search_diffs(
            &db,
            "semantic query",
            DiffSearchMode::Semantic,
            10,
            &child_commits,
        )
        .unwrap();
        assert_eq!(semantic.len(), 2);
        assert!(semantic.iter().all(|result| result.distance == Some(0.0)));
        assert!(semantic.iter().any(|result| {
            result.reference.side == DiffSide::Added && result.text == "add needle"
        }));
        assert!(semantic.iter().any(|result| {
            result.reference.side == DiffSide::Removed && result.text == "remove needle"
        }));

        let object = repo
            .path()
            .join(".git/objects")
            .join(&added.blob_id[..2])
            .join(&added.blob_id[2..]);
        let saved = repo.path().join("saved-added-blob");
        fs::rename(&object, &saved).unwrap();
        let missing = search_diffs(&db, "add needle", DiffSearchMode::Exact, 10, &child_commits)
            .unwrap_err()
            .to_string();
        let missing_semantic = search_diffs(
            &db,
            "semantic query",
            DiffSearchMode::Semantic,
            10,
            &child_commits,
        )
        .unwrap_err()
        .to_string();
        fs::rename(saved, object).unwrap();
        assert!(missing.contains(&added.blob_id), "{missing}");
        assert!(
            missing_semantic.contains(&added.blob_id),
            "{missing_semantic}"
        );
        std::env::set_current_dir(original_cwd).unwrap();
    }

    static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn all_occurrence_search_returns_every_matching_commit_path_reference() {
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
        fs::write(repo.path().join("a.txt"), "shared occurrence marker\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "first occurrence"]);
        let first = git(&["rev-parse", "HEAD"]);
        fs::write(repo.path().join("b.txt"), "shared occurrence marker\n").unwrap();
        git(&["add", "b.txt"]);
        git(&["commit", "-qm", "second occurrence"]);
        let second = git(&["rev-parse", "HEAD"]);

        enable_vector_extension();
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

        let commits = vec![first, second];
        let compact = search(
            &db,
            "shared occurrence",
            "exact",
            usize::MAX,
            &commits,
            |_| true,
        )
        .unwrap();
        assert_eq!(compact.len(), 1, "ordinary search remains compact");
        let all = search_all_occurrences(
            &db,
            "shared occurrence",
            "exact",
            usize::MAX,
            &commits,
            |_| true,
        )
        .unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter()
                .map(|result| (result.commit_id.as_deref().unwrap(), result.path.as_str()))
                .collect::<std::collections::BTreeSet<_>>(),
            [
                (commits[0].as_str(), "a.txt"),
                (commits[1].as_str(), "a.txt"),
                (commits[1].as_str(), "b.txt"),
            ]
            .into_iter()
            .collect()
        );

        let semantic = search_all_occurrences(
            &db,
            "semantic query",
            "semantic",
            usize::MAX,
            &commits,
            |_| true,
        )
        .unwrap();
        assert_eq!(semantic.len(), 3);
        assert!(semantic.iter().all(|result| result.distance == Some(0.0)));
        std::env::set_current_dir(original_cwd).unwrap();
    }

    fn enable_vector_extension() {
        crate::sqlite_vec_extension::register().unwrap();
    }

    #[test]
    fn full_refresh_indexes_only_modified_hunk_sides_with_blob_relative_ranges() {
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
        let old_text =
            "unchanged-before\nold value\nunchanged-middle\nold value two\nunchanged-after\n";
        let new_text =
            "unchanged-before\nnew value\nunchanged-middle\nnew value two\nunchanged-after\n";
        fs::write(repo.path().join("change.txt"), old_text).unwrap();
        fs::write(repo.path().join("empty.txt"), "").unwrap();
        fs::write(repo.path().join("cleared.txt"), "was present\n").unwrap();
        fs::write(repo.path().join("insertion.txt"), "first\nlast\n").unwrap();
        fs::write(
            repo.path().join("deletion.txt"),
            "first\nremove this line\nlast\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "before"]);
        let parent = git(&["rev-parse", "HEAD"]);
        fs::write(repo.path().join("change.txt"), new_text).unwrap();
        fs::write(repo.path().join("empty.txt"), "now present\n").unwrap();
        fs::write(repo.path().join("cleared.txt"), "").unwrap();
        fs::write(
            repo.path().join("insertion.txt"),
            "first\ninserted line\nlast\n",
        )
        .unwrap();
        fs::write(repo.path().join("deletion.txt"), "first\nlast\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "after"]);
        let child = git(&["rev-parse", "HEAD"]);
        let old_blob = git(&["rev-parse", &format!("{parent}:change.txt")]);
        let new_blob = git(&["rev-parse", &format!("{child}:change.txt")]);

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
        assert_eq!(references.len(), 8);
        let change_references: Vec<_> = references
            .iter()
            .filter(|reference| reference.old_path.as_deref() == Some(b"change.txt".as_slice()))
            .collect();
        let removed: Vec<_> = change_references
            .iter()
            .filter(|reference| reference.side == DiffSide::Removed)
            .copied()
            .collect();
        let added: Vec<_> = change_references
            .iter()
            .filter(|reference| reference.side == DiffSide::Added)
            .copied()
            .collect();
        assert_eq!(removed.len(), 2);
        assert_eq!(added.len(), 2);
        for pair in 0..2 {
            assert_eq!(removed[pair].hunk_id, added[pair].hunk_id);
            assert_eq!(
                removed[pair].old_path.as_deref(),
                Some(b"change.txt".as_slice())
            );
            assert_eq!(
                removed[pair].new_path.as_deref(),
                Some(b"change.txt".as_slice())
            );
            assert_eq!(
                added[pair].old_path.as_deref(),
                Some(b"change.txt".as_slice())
            );
            assert_eq!(
                added[pair].new_path.as_deref(),
                Some(b"change.txt".as_slice())
            );
            assert_eq!(removed[pair].blob_id, old_blob);
            assert_eq!(added[pair].blob_id, new_blob);
        }
        assert_ne!(removed[0].hunk_id, removed[1].hunk_id);
        assert_eq!(removed[0].old_line_start, Some(2));
        assert_eq!(removed[0].old_line_end, Some(2));
        assert_eq!(added[0].new_line_start, Some(2));
        assert_eq!(added[0].new_line_end, Some(2));
        assert_eq!((removed[0].line_start, removed[0].line_end), (2, 2));
        assert_eq!((added[0].line_start, added[0].line_end), (2, 2));
        assert_eq!(removed[1].old_line_start, Some(4));
        assert_eq!(removed[1].old_line_end, Some(4));
        assert_eq!(added[1].new_line_start, Some(4));
        assert_eq!(added[1].new_line_end, Some(4));
        assert_eq!((removed[1].line_start, removed[1].line_end), (4, 4));
        assert_eq!((added[1].line_start, added[1].line_end), (4, 4));
        assert_ne!(removed[0].unit_id, added[0].unit_id);
        assert_eq!(
            (removed[0].byte_start, removed[0].byte_end),
            (
                "unchanged-before\n".len(),
                "unchanged-before\nold value".len()
            )
        );
        assert_eq!(
            (added[0].byte_start, added[0].byte_end),
            (
                "unchanged-before\n".len(),
                "unchanged-before\nnew value".len()
            )
        );
        let old_second_start = "unchanged-before\nold value\nunchanged-middle\n".len();
        let new_second_start = "unchanged-before\nnew value\nunchanged-middle\n".len();
        assert_eq!(
            (removed[1].byte_start, removed[1].byte_end),
            (old_second_start, old_second_start + "old value two".len())
        );
        assert_eq!(
            (added[1].byte_start, added[1].byte_end),
            (new_second_start, new_second_start + "new value two".len())
        );
        let added_to_empty = references
            .iter()
            .find(|reference| {
                reference.side == DiffSide::Added
                    && reference.new_path.as_deref() == Some(b"empty.txt".as_slice())
            })
            .unwrap();
        assert_eq!(added_to_empty.old_line_start, None);
        assert_eq!(added_to_empty.old_line_end, None);
        assert_eq!(added_to_empty.new_line_start, Some(1));
        let removed_from_cleared = references
            .iter()
            .find(|reference| {
                reference.side == DiffSide::Removed
                    && reference.old_path.as_deref() == Some(b"cleared.txt".as_slice())
            })
            .unwrap();
        assert_eq!(removed_from_cleared.new_line_start, None);
        assert_eq!(removed_from_cleared.new_line_end, None);
        assert_eq!(removed_from_cleared.old_line_start, Some(1));
        let inserted = references
            .iter()
            .find(|reference| {
                reference.side == DiffSide::Added
                    && reference.new_path.as_deref() == Some(b"insertion.txt".as_slice())
            })
            .unwrap();
        assert_eq!(inserted.old_line_start, Some(1));
        assert_eq!(inserted.new_line_start, Some(2));
        let deleted = references
            .iter()
            .find(|reference| {
                reference.side == DiffSide::Removed
                    && reference.old_path.as_deref() == Some(b"deletion.txt".as_slice())
            })
            .unwrap();
        assert_eq!(deleted.old_line_start, Some(2));
        assert_eq!(deleted.new_line_start, Some(1));
        std::env::set_current_dir(original_cwd).unwrap();
    }

    #[test]
    fn full_refresh_skips_gitlink_diff_entries_without_aborting() {
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
        fs::write(repo.path().join("base.txt"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-qb", "gitlink-object"]);
        fs::write(repo.path().join("inner.txt"), "inner\n").unwrap();
        git(&["add", "inner.txt"]);
        git(&["commit", "-qm", "inner"]);
        let target = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-qb", "gitlink-parent", &base]);
        let cacheinfo = format!("160000,{target},module");
        git(&["update-index", "--add", "--cacheinfo", &cacheinfo]);
        git(&["commit", "-qm", "add gitlink"]);
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
        let refresh = prepare_refresh(&db);
        std::env::set_current_dir(original_cwd).unwrap();
        refresh.unwrap();
        let tx = db.unchecked_transaction().unwrap();
        publish_refresh(&tx).unwrap();
        tx.commit().unwrap();
        assert!(
            diff_references(&db, Some(&base), Some(&child))
                .unwrap()
                .is_empty()
        );
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
    fn full_refresh_records_heuristic_rename_copy_and_merge_parent_metadata() {
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
        let commit_at = |message: &str, date: &str| {
            let output = Command::new("git")
                .args(["commit", "-qm", message])
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        let shared_context = format!("{}\n", "shared context ".repeat(100));
        fs::write(
            repo.path().join("old-name.txt"),
            format!("alpha\noriginal passage\n{shared_context}omega\n"),
        )
        .unwrap();
        fs::write(
            repo.path().join("copy-source.txt"),
            format!("first\nsource passage\n{shared_context}last\n"),
        )
        .unwrap();
        fs::write(
            repo.path().join("pure-old.txt"),
            "unchanged rename source\n",
        )
        .unwrap();
        git(&["add", "."]);
        commit_at("base", "2020-02-01T00:00:00Z");
        let base = git(&["rev-parse", "HEAD"]);
        let main_branch = git(&["branch", "--show-current"]);
        git(&["branch", "side"]);

        fs::rename(
            repo.path().join("old-name.txt"),
            repo.path().join("new-name.txt"),
        )
        .unwrap();
        fs::rename(
            repo.path().join("pure-old.txt"),
            repo.path().join("pure-new.txt"),
        )
        .unwrap();
        fs::write(
            repo.path().join("new-name.txt"),
            format!("alpha\nupdated passage\n{shared_context}omega\n"),
        )
        .unwrap();
        git(&["add", "-A"]);
        commit_at("rename passage", "2020-03-01T00:00:00Z");
        let rename_commit = git(&["rev-parse", "HEAD"]);

        git(&["checkout", "-q", "side"]);
        fs::copy(
            repo.path().join("copy-source.txt"),
            repo.path().join("copied.txt"),
        )
        .unwrap();
        fs::write(
            repo.path().join("copied.txt"),
            format!("first\ncopied passage\n{shared_context}last\n"),
        )
        .unwrap();
        git(&["add", "copied.txt"]);
        commit_at("copy passage", "2020-01-01T00:00:00Z");
        let copy_commit = git(&["rev-parse", "HEAD"]);

        let merge = Command::new("git")
            .args(["merge", "--no-ff", "-m", "merge", main_branch.as_str()])
            .env("GIT_AUTHOR_DATE", "2019-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2019-01-01T00:00:00Z")
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(
            merge.status.success(),
            "{}",
            String::from_utf8_lossy(&merge.stderr)
        );
        let merge_commit = git(&["rev-parse", "HEAD"]);
        let merge_parents = git(&["rev-list", "--parents", "-n", "1", &merge_commit])
            .split_whitespace()
            .skip(1)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(merge_parents.len(), 2);
        let base_timestamp = git(&["show", "-s", "--format=%ct", &base])
            .parse::<i64>()
            .unwrap();
        let copy_timestamp = git(&["show", "-s", "--format=%ct", &copy_commit])
            .parse::<i64>()
            .unwrap();
        let rename_timestamp = git(&["show", "-s", "--format=%ct", &rename_commit])
            .parse::<i64>()
            .unwrap();
        assert!(copy_timestamp < base_timestamp && base_timestamp < rename_timestamp);

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

        let root_metadata = diff_metadata(
            &db,
            Some("4b825dc642cb6eb9a060e54bf8d69288fbee4904"),
            Some(&base),
        )
        .unwrap();
        assert!(root_metadata.changes.iter().any(|change| {
            change.kind == DiffChangeKind::Added
                && change.old_path.is_none()
                && change.new_path.as_deref() == Some(b"old-name.txt".as_slice())
        }));
        assert_eq!(root_metadata.settings.rename_threshold_percent, 50);
        assert_eq!(root_metadata.settings.copy_threshold_percent, 50);
        assert!(root_metadata.settings.find_copies_harder);
        assert_eq!(root_metadata.settings.diff_algorithm, "myers");
        assert!(
            root_metadata
                .settings
                .git_version
                .starts_with("git version ")
        );

        let rename = diff_metadata(&db, Some(&base), Some(&rename_commit)).unwrap();
        let rename_change = rename
            .changes
            .iter()
            .find(|change| change.kind == DiffChangeKind::HeuristicRename)
            .unwrap();
        assert_eq!(
            rename_change.old_path.as_deref(),
            Some(b"old-name.txt".as_slice())
        );
        assert_eq!(
            rename_change.new_path.as_deref(),
            Some(b"new-name.txt".as_slice())
        );
        assert!(
            rename_change
                .similarity_percent
                .is_some_and(|score| (50..=100).contains(&score))
        );
        let pure_rename = rename
            .changes
            .iter()
            .find(|change| change.old_path.as_deref() == Some(b"pure-old.txt".as_slice()))
            .unwrap();
        assert_eq!(pure_rename.kind, DiffChangeKind::HeuristicRename);
        assert_eq!(
            pure_rename.new_path.as_deref(),
            Some(b"pure-new.txt".as_slice())
        );
        assert_eq!(pure_rename.similarity_percent, Some(100));
        let rename_refs = diff_references(&db, Some(&base), Some(&rename_commit)).unwrap();
        assert!(rename_refs.iter().any(|reference| {
            reference.old_path.as_deref() == Some(b"old-name.txt".as_slice())
                && reference.new_path.as_deref() == Some(b"new-name.txt".as_slice())
        }));

        let copy = diff_metadata(&db, Some(&base), Some(&copy_commit)).unwrap();
        let copy_change = copy
            .changes
            .iter()
            .find(|change| change.kind == DiffChangeKind::HeuristicCopy)
            .unwrap();
        assert_eq!(
            copy_change.old_path.as_deref(),
            Some(b"copy-source.txt".as_slice())
        );
        assert_eq!(
            copy_change.new_path.as_deref(),
            Some(b"copied.txt".as_slice())
        );
        assert!(
            copy_change
                .similarity_percent
                .is_some_and(|score| (50..=100).contains(&score))
        );
        let copy_refs = diff_references(&db, Some(&base), Some(&copy_commit)).unwrap();
        assert!(copy_refs.iter().any(|reference| {
            reference.old_path.as_deref() == Some(b"copy-source.txt".as_slice())
                && reference.new_path.as_deref() == Some(b"copied.txt".as_slice())
        }));

        for parent in &merge_parents {
            let metadata = diff_metadata(&db, Some(parent), Some(&merge_commit)).unwrap();
            assert!(
                !metadata.changes.is_empty(),
                "missing merge-parent pair {parent}"
            );
            assert!(metadata.changes.iter().all(|change| {
                change.parent_commit_id == *parent && change.child_commit_id == merge_commit
            }));
        }
        std::env::set_current_dir(original_cwd).unwrap();
    }

    #[test]
    fn selection_uses_topology_path_scope_and_heuristic_rename_links() {
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
        let commit_at = |message: &str, date: &str| {
            let output = Command::new("git")
                .args(["commit", "-qm", message])
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .current_dir(repo.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::create_dir_all(repo.path().join("src")).unwrap();
        let rename_context = "rename-specific context ".repeat(100);
        fs::write(
            repo.path().join("src/old.rs"),
            format!("old path marker\n{rename_context}\n"),
        )
        .unwrap();
        fs::write(repo.path().join("unrelated.txt"), "base unrelated text\n").unwrap();
        git(&["add", "."]);
        commit_at("base", "2020-02-01T00:00:00Z");
        let base = git(&["rev-parse", "HEAD"]);
        let main_branch = git(&["branch", "--show-current"]);
        git(&["branch", "side"]);

        fs::write(
            repo.path().join("unrelated.txt"),
            "changed unrelated text\n",
        )
        .unwrap();
        git(&["add", "unrelated.txt"]);
        commit_at("unrelated change", "2021-02-01T00:00:00Z");
        let unchanged = git(&["rev-parse", "HEAD"]);
        fs::rename(
            repo.path().join("src/old.rs"),
            repo.path().join("src/new.rs"),
        )
        .unwrap();
        fs::write(
            repo.path().join("src/new.rs"),
            format!("new path marker\n{rename_context}\n"),
        )
        .unwrap();
        git(&["add", "-A"]);
        commit_at("rename file", "2023-02-01T00:00:00Z");
        let renamed = git(&["rev-parse", "HEAD"]);

        git(&["checkout", "-q", "side"]);
        fs::write(repo.path().join("side-only.txt"), "side ancestry\n").unwrap();
        git(&["add", "side-only.txt"]);
        commit_at("side commit with skewed timestamp", "2018-01-01T00:00:00Z");
        let side = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-q", &main_branch]);
        let merge = Command::new("git")
            .args(["merge", "--no-ff", "-m", "merge", "side"])
            .env("GIT_AUTHOR_DATE", "2019-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2019-01-01T00:00:00Z")
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(
            merge.status.success(),
            "{}",
            String::from_utf8_lossy(&merge.stderr)
        );
        let merge_commit = git(&["rev-parse", "HEAD"]);
        let parent_ids = git(&["rev-list", "--parents", "-n", "1", &merge_commit])
            .split_whitespace()
            .skip(1)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(parent_ids.len(), 2);

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

        let topological = select_history(
            &db,
            &HistoryQuery {
                selector: HistorySelector::Topological(3),
                start_commit: Some(merge_commit.clone()),
                include_commits: Vec::new(),
                exclude_commits: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(topological[0].commit_id, merge_commit);
        assert_eq!(topological.len(), 3);
        assert_eq!(
            select_history(
                &db,
                &HistoryQuery {
                    selector: HistorySelector::Topological(3),
                    start_commit: Some(merge_commit.clone()),
                    include_commits: Vec::new(),
                    exclude_commits: Vec::new(),
                },
            )
            .unwrap(),
            topological
        );
        let full_topological = select_history(
            &db,
            &HistoryQuery {
                selector: HistorySelector::Topological(10),
                start_commit: Some(merge_commit.clone()),
                include_commits: Vec::new(),
                exclude_commits: Vec::new(),
            },
        )
        .unwrap();
        for parent in &parent_ids {
            assert!(
                full_topological
                    .iter()
                    .position(|entry| &entry.commit_id == parent)
                    .is_some_and(|position| position > 0)
            );
        }
        let merge_timestamp: i64 = git(&["show", "-s", "--format=%ct", &merge_commit])
            .parse()
            .unwrap();
        let renamed_timestamp: i64 = git(&["show", "-s", "--format=%ct", &renamed])
            .parse()
            .unwrap();
        assert!(merge_timestamp < renamed_timestamp);
        assert_ne!(base, unchanged);
        let alternate_start = select_history(
            &db,
            &HistoryQuery {
                selector: HistorySelector::Topological(1),
                start_commit: Some(renamed.clone()),
                include_commits: Vec::new(),
                exclude_commits: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(alternate_start[0].commit_id, renamed);
        let exact_commit = select_history(
            &db,
            &HistoryQuery {
                selector: HistorySelector::Commit(base.clone()),
                ..HistoryQuery::default()
            },
        )
        .unwrap();
        assert_eq!(exact_commit.len(), 1);
        assert_eq!(exact_commit[0].commit_id, base);
        let all_commits = select_history(
            &db,
            &HistoryQuery {
                selector: HistorySelector::All,
                ..HistoryQuery::default()
            },
        )
        .unwrap();
        let expected_all = git(&["rev-list", "--all"])
            .lines()
            .map(str::to_owned)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            all_commits
                .iter()
                .map(|entry| entry.commit_id.clone())
                .collect::<std::collections::HashSet<_>>(),
            expected_all
        );
        assert!(
            select_history(
                &db,
                &HistoryQuery {
                    selector: HistorySelector::Commit("main".to_owned()),
                    ..HistoryQuery::default()
                }
            )
            .is_err()
        );

        let limited_with_include = select_history(
            &db,
            &HistoryQuery {
                selector: HistorySelector::Topological(1),
                start_commit: Some(renamed.clone()),
                include_commits: vec![side.clone()],
                exclude_commits: vec![renamed.clone()],
            },
        )
        .unwrap();
        assert_eq!(
            limited_with_include
                .iter()
                .map(|entry| entry.commit_id.as_str())
                .collect::<Vec<_>>(),
            [side.as_str()]
        );

        let path_history = select_history(
            &db,
            &HistoryQuery {
                selector: HistorySelector::Path(b"src/new.rs".to_vec()),
                start_commit: Some(merge_commit.clone()),
                include_commits: Vec::new(),
                exclude_commits: Vec::new(),
            },
        )
        .unwrap();
        let path_by_commit = path_history
            .iter()
            .map(|entry| (entry.commit_id.as_str(), entry.path.as_deref().unwrap()))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            path_by_commit.get(merge_commit.as_str()),
            Some(&b"src/new.rs".as_slice())
        );
        assert_eq!(
            path_by_commit.get(renamed.as_str()),
            Some(&b"src/new.rs".as_slice())
        );
        assert_eq!(
            path_by_commit.get(unchanged.as_str()),
            Some(&b"src/old.rs".as_slice())
        );
        assert_eq!(
            path_by_commit.get(base.as_str()),
            Some(&b"src/old.rs".as_slice())
        );
        assert_eq!(
            path_by_commit.get(side.as_str()),
            Some(&b"src/old.rs".as_slice())
        );
        assert!(
            path_history
                .iter()
                .all(|entry| entry.path.as_deref().unwrap().starts_with(b"src/"))
        );
        let rename_parent_entry = path_history
            .iter()
            .find(|entry| entry.commit_id == unchanged)
            .unwrap();
        assert!(rename_parent_entry.heuristic_links.iter().any(|link| {
            link.parent_commit_id == unchanged
                && link.child_commit_id == renamed
                && link.old_path == b"src/old.rs"
                && link.new_path == b"src/new.rs"
                && link.similarity_percent.is_some_and(|score| score >= 50)
        }));
        let side_entry = path_history
            .iter()
            .find(|entry| entry.commit_id == side)
            .unwrap();
        assert!(side_entry.heuristic_links.iter().any(|link| {
            link.parent_commit_id == side
                && link.child_commit_id == merge_commit
                && link.old_path == b"src/old.rs"
                && link.new_path == b"src/new.rs"
        }));

        let directory_history = select_history(
            &db,
            &HistoryQuery {
                selector: HistorySelector::Path(b"src/".to_vec()),
                start_commit: Some(merge_commit.clone()),
                include_commits: Vec::new(),
                exclude_commits: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(directory_history, path_history);

        let old_path_history = select_history(
            &db,
            &HistoryQuery {
                selector: HistorySelector::Path(b"src/old.rs".to_vec()),
                start_commit: Some(merge_commit.clone()),
                include_commits: Vec::new(),
                exclude_commits: Vec::new(),
            },
        )
        .unwrap();
        let merge_path_entry = old_path_history
            .iter()
            .find(|entry| entry.commit_id == merge_commit)
            .unwrap();
        assert_eq!(
            merge_path_entry.path.as_deref(),
            Some(b"src/new.rs".as_slice())
        );
        assert!(
            merge_path_entry
                .heuristic_links
                .iter()
                .any(|link| { link.old_path == b"src/old.rs" && link.new_path == b"src/new.rs" })
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

        let exact = search_commit_messages(
            &db,
            "transaction ledger",
            "exact",
            5,
            std::slice::from_ref(&commit),
        )
        .unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].commit_id.as_deref(), Some(commit.as_str()));
        assert!(exact[0].text.contains("Initial transaction ledger"));
        let semantic = search_commit_messages(
            &db,
            "accounting rationale",
            "semantic",
            5,
            std::slice::from_ref(&commit),
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
            MAX_REFRESH_ACCOUNTING_BYTES,
        )
        .unwrap();
        assert!(stdout > 0);
        assert!(stderr > 0);
        assert_eq!(stdout + stderr, 100);

        let used = MAX_REFRESH_ACCOUNTING_BYTES - MAX_GIT_STDERR_BYTES - 17;
        let (stdout, stderr) = git_capture_limits(
            MAX_REFRESH_ACCOUNTING_BYTES,
            used,
            MAX_REFRESH_ACCOUNTING_BYTES,
        )
        .unwrap();
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
    context: &mut StageContext<'_, '_>,
    commit_id: &str,
    message: &str,
    object_byte_start: usize,
    object_line_start: usize,
    unit_statement: &mut rusqlite::Statement<'_>,
    message_statement: &mut rusqlite::Statement<'_>,
) -> Result<()> {
    let StageContext {
        next_unit_id,
        batch,
        pipeline,
        embedding_dimension,
        embedding_statement,
        budget,
        ..
    } = context;
    let next_unit_id = &mut **next_unit_id;
    let mut staging_error = None;
    segmentation::visit("fallback", message, |unit| {
        let result = (|| -> Result<()> {
            budget.charge_records(1, "commit-message unit")?;
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

#[cfg(test)]
fn prepare_refresh(c: &Connection) -> Result<()> {
    prepare_refresh_excluding(c, None)
}

pub fn prepare_refresh_excluding(
    c: &Connection,
    excluded_database_path: Option<&[u8]>,
) -> Result<()> {
    is_current(c)?;
    let mut preparation_bytes = Budget::refresh("history refresh preparation accounting")?;
    c.execute_batch("PRAGMA temp_store=FILE;
        DROP TABLE IF EXISTS temp.refresh_commits; DROP TABLE IF EXISTS temp.refresh_blobs;
        DROP TABLE IF EXISTS temp.refresh_occurrences; DROP TABLE IF EXISTS temp.refresh_variants;
        DROP TABLE IF EXISTS temp.refresh_units; DROP TABLE IF EXISTS temp.refresh_embeddings;
        DROP TABLE IF EXISTS temp.refresh_commit_message_units;
        DROP TABLE IF EXISTS temp.refresh_diff_file_changes;
        DROP TABLE IF EXISTS temp.refresh_diff_detection_settings;
        CREATE TEMP TABLE refresh_commits(commit_id TEXT PRIMARY KEY, timestamp INTEGER NOT NULL);
        CREATE TEMP TABLE refresh_blobs(blob_id TEXT PRIMARY KEY, content TEXT, status TEXT NOT NULL);
        CREATE TEMP TABLE refresh_occurrences(commit_id TEXT NOT NULL, blob_id TEXT NOT NULL, path BLOB NOT NULL, segmentation_kind TEXT NOT NULL, PRIMARY KEY(commit_id,path));
        CREATE TEMP TABLE refresh_variants(blob_id TEXT NOT NULL, segmentation_kind TEXT NOT NULL, PRIMARY KEY(blob_id,segmentation_kind));
        CREATE TEMP TABLE refresh_units(unit_id INTEGER PRIMARY KEY, blob_id TEXT NOT NULL, segmentation_kind TEXT NOT NULL, text TEXT NOT NULL, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL);
        CREATE TEMP TABLE refresh_commit_message_units(commit_id TEXT NOT NULL, unit_id INTEGER NOT NULL, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL);
        CREATE TEMP TABLE refresh_diff_references(parent_commit_id TEXT NOT NULL,child_commit_id TEXT NOT NULL,hunk_id INTEGER NOT NULL,side TEXT NOT NULL,old_path BLOB,new_path BLOB,old_line_start INTEGER,old_line_end INTEGER,new_line_start INTEGER,new_line_end INTEGER,blob_id TEXT NOT NULL,unit_id INTEGER NOT NULL);
        CREATE TEMP TABLE refresh_diff_file_changes(parent_commit_id TEXT NOT NULL,child_commit_id TEXT NOT NULL,change_index INTEGER NOT NULL,change_kind TEXT NOT NULL,old_path BLOB,new_path BLOB,similarity_percent INTEGER);
        CREATE TEMP TABLE refresh_diff_detection_settings(id INTEGER PRIMARY KEY CHECK(id=1),rename_threshold_percent INTEGER NOT NULL,copy_threshold_percent INTEGER NOT NULL,find_copies_harder INTEGER NOT NULL,git_version TEXT NOT NULL,diff_algorithm TEXT NOT NULL);
        CREATE TEMP TABLE refresh_embeddings(unit_id INTEGER PRIMARY KEY, vector BLOB NOT NULL);
        DROP TABLE IF EXISTS temp.refresh_dimension; CREATE TEMP TABLE refresh_dimension(value INTEGER);
        DROP TABLE IF EXISTS temp.refresh_config; CREATE TEMP TABLE refresh_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    let git_version = String::from_utf8(git_output(&["--version"], 128, &mut preparation_bytes)?)?
        .trim()
        .to_owned();
    c.execute(
        "INSERT INTO temp.refresh_diff_detection_settings VALUES(1,?1,?2,?3,?4,?5)",
        params![
            RENAME_THRESHOLD_PERCENT,
            COPY_THRESHOLD_PERCENT,
            FIND_COPIES_HARDER,
            git_version,
            DIFF_ALGORITHM,
        ],
    )?;
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
                &mut StageContext {
                    c,
                    next_unit_id: &mut unit_count,
                    batch: &mut batch,
                    pipeline: &mut pipeline,
                    embedding_dimension: &mut embedding_dimension,
                    embedding_statement: &mut message_embedding_stage,
                    budget,
                },
                commit_id,
                &message,
                byte_start,
                line_start,
                &mut message_unit_stage,
                &mut message_range_stage,
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
                if excluded_database_path.is_some_and(|excluded| path == excluded) {
                    return Ok(());
                }
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
            let mut stage_context = StageContext {
                c,
                next_unit_id: &mut unit_count,
                batch: &mut batch,
                pipeline: &mut pipeline,
                embedding_dimension: &mut embedding_dimension,
                embedding_statement: &mut embedding_statement,
                budget: &mut preparation_bytes,
            };
            if parents.is_empty() {
                stage_diff_pair(&mut stage_context, None, &child, excluded_database_path)?;
            } else {
                for parent in parents {
                    stage_diff_pair(
                        &mut stage_context,
                        Some(&parent),
                        &child,
                        excluded_database_path,
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
    let mut budget = Budget::refresh("targeted history refresh preparation accounting")?;
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

pub fn refresh_changed_excluding(
    c: &mut Connection,
    excluded_database_path: Option<&[u8]>,
) -> Result<()> {
    refresh_scoped(c, None, excluded_database_path)
}

pub fn refresh_update_excluding(
    c: &mut Connection,
    commit_id: &str,
    excluded_database_path: Option<&[u8]>,
) -> Result<()> {
    refresh_scoped(c, Some(commit_id), excluded_database_path)
}

fn refresh_scoped(
    c: &mut Connection,
    target: Option<&str>,
    excluded_database_path: Option<&[u8]>,
) -> Result<()> {
    let version = snapshot_version(c)?;
    let schema: i64 = c.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !matches!(schema, 4 | 5) {
        return Err(
            "scoped refresh requires a version-4 or version-5 index; run repodb refresh first"
                .into(),
        );
    }

    let mut budget = Budget::refresh("scoped history refresh preparation accounting")?;
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
            if excluded_database_path.is_some_and(|excluded| path == excluded) {
                return Ok(());
            }
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
    tx.execute_batch("DROP TABLE IF EXISTS diff_references; DROP TABLE IF EXISTS diff_file_changes; DROP TABLE IF EXISTS diff_detection_settings; DROP TABLE IF EXISTS unit_embeddings; DROP TABLE IF EXISTS commit_message_units; DROP TABLE IF EXISTS git_blob_units; DROP TABLE IF EXISTS indexed_units; DROP TABLE IF EXISTS git_occurrences; DROP TABLE IF EXISTS blob_contents; DROP TABLE IF EXISTS git_commits;
        CREATE TABLE git_commits(commit_id TEXT PRIMARY KEY NOT NULL, timestamp INTEGER NOT NULL);
        CREATE TABLE blob_contents(blob_id TEXT PRIMARY KEY NOT NULL, status TEXT NOT NULL);
        CREATE TABLE git_occurrences(commit_id TEXT NOT NULL REFERENCES git_commits, blob_id TEXT NOT NULL REFERENCES blob_contents, path BLOB NOT NULL, segmentation_kind TEXT NOT NULL, PRIMARY KEY(commit_id,path));
        CREATE TABLE indexed_units(unit_id INTEGER PRIMARY KEY, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL);
        CREATE TABLE git_blob_units(blob_id TEXT NOT NULL REFERENCES blob_contents, unit_id INTEGER NOT NULL REFERENCES indexed_units, segmentation_kind TEXT NOT NULL, PRIMARY KEY(blob_id,segmentation_kind,unit_id));
        CREATE TABLE commit_message_units(commit_id TEXT NOT NULL REFERENCES git_commits, unit_id INTEGER NOT NULL REFERENCES indexed_units, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL, PRIMARY KEY(commit_id,unit_id));
        CREATE TABLE diff_references(parent_commit_id TEXT NOT NULL,child_commit_id TEXT NOT NULL REFERENCES git_commits,hunk_id INTEGER NOT NULL,side TEXT NOT NULL,old_path BLOB,new_path BLOB,old_line_start INTEGER,old_line_end INTEGER,new_line_start INTEGER,new_line_end INTEGER,blob_id TEXT NOT NULL,unit_id INTEGER NOT NULL REFERENCES indexed_units,byte_start INTEGER NOT NULL,byte_end INTEGER NOT NULL,line_start INTEGER NOT NULL,line_end INTEGER NOT NULL,PRIMARY KEY(parent_commit_id,child_commit_id,hunk_id,side,unit_id));
        CREATE TABLE diff_file_changes(parent_commit_id TEXT NOT NULL,child_commit_id TEXT NOT NULL REFERENCES git_commits,change_index INTEGER NOT NULL,change_kind TEXT NOT NULL CHECK(change_kind IN ('added','removed','modified','heuristic_rename','heuristic_copy','type_changed')),old_path BLOB,new_path BLOB,similarity_percent INTEGER CHECK(similarity_percent IS NULL OR similarity_percent BETWEEN 0 AND 100),PRIMARY KEY(parent_commit_id,child_commit_id,change_index));
        CREATE TABLE diff_detection_settings(id INTEGER PRIMARY KEY CHECK(id=1),rename_threshold_percent INTEGER NOT NULL,copy_threshold_percent INTEGER NOT NULL,find_copies_harder INTEGER NOT NULL,git_version TEXT NOT NULL,diff_algorithm TEXT NOT NULL);
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
        "INSERT INTO diff_file_changes SELECT * FROM temp.refresh_diff_file_changes",
        [],
    )?;
    tx.execute(
        "INSERT INTO diff_detection_settings SELECT * FROM temp.refresh_diff_detection_settings",
        [],
    )?;
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
    tx.execute_batch("DROP TABLE IF EXISTS indexed_files; DROP TABLE IF EXISTS embeddings; PRAGMA user_version=5;")?;
    Ok(())
}

const DIFF_REFERENCE_COLUMNS: &str = "r.parent_commit_id,r.child_commit_id,r.hunk_id,r.side,r.old_path,r.new_path,r.old_line_start,r.old_line_end,r.new_line_start,r.new_line_end,r.blob_id,r.unit_id,r.byte_start,r.byte_end,r.line_start,r.line_end";

fn read_diff_reference(row: &rusqlite::Row<'_>) -> rusqlite::Result<DiffReference> {
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
        unit_id: row.get(11)?,
        byte_start: usize::try_from(row.get::<_, i64>(12)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        byte_end: usize::try_from(row.get::<_, i64>(13)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        line_start: maybe_usize(14)?.ok_or(rusqlite::Error::InvalidQuery)?,
        line_end: maybe_usize(15)?.ok_or(rusqlite::Error::InvalidQuery)?,
    })
}

pub(crate) fn diff_references(
    c: &Connection,
    parent_commit_id: Option<&str>,
    child_commit_id: Option<&str>,
) -> Result<Vec<DiffReference>> {
    let mut statement = c.prepare(&format!(
        "SELECT {DIFF_REFERENCE_COLUMNS} FROM diff_references r
         WHERE (?1 IS NULL OR r.parent_commit_id=?1)
           AND (?2 IS NULL OR r.child_commit_id=?2)
         ORDER BY r.child_commit_id,r.parent_commit_id,r.hunk_id,r.side,r.byte_start"
    ))?;
    let rows = statement.query_map(
        params![parent_commit_id, child_commit_id],
        read_diff_reference,
    )?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn parse_diff_change_kind(value: &str) -> rusqlite::Result<DiffChangeKind> {
    match value {
        "added" => Ok(DiffChangeKind::Added),
        "removed" => Ok(DiffChangeKind::Removed),
        "modified" => Ok(DiffChangeKind::Modified),
        "heuristic_rename" => Ok(DiffChangeKind::HeuristicRename),
        "heuristic_copy" => Ok(DiffChangeKind::HeuristicCopy),
        "type_changed" => Ok(DiffChangeKind::TypeChanged),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

pub(crate) fn diff_metadata(
    c: &Connection,
    parent_commit_id: Option<&str>,
    child_commit_id: Option<&str>,
) -> Result<DiffMetadata> {
    let version: i64 = c.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 5 {
        return Err("diff metadata requires a full refresh with schema version 5".into());
    }
    let settings = c.query_row(
        "SELECT rename_threshold_percent,copy_threshold_percent,find_copies_harder,git_version,diff_algorithm FROM diff_detection_settings WHERE id=1",
        [],
        |row| {
            let threshold = |index| -> rusqlite::Result<u8> {
                u8::try_from(row.get::<_, i64>(index)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)
            };
            Ok(DiffDetectionSettings {
                rename_threshold_percent: threshold(0)?,
                copy_threshold_percent: threshold(1)?,
                find_copies_harder: row.get(2)?,
                git_version: row.get(3)?,
                diff_algorithm: row.get(4)?,
            })
        },
    )?;
    let mut statement = c.prepare(
        "SELECT parent_commit_id,child_commit_id,change_index,change_kind,old_path,new_path,similarity_percent
         FROM diff_file_changes
         WHERE (?1 IS NULL OR parent_commit_id=?1) AND (?2 IS NULL OR child_commit_id=?2)
         ORDER BY child_commit_id,parent_commit_id,change_index",
    )?;
    let rows = statement.query_map(params![parent_commit_id, child_commit_id], |row| {
        let similarity_percent = row
            .get::<_, Option<i64>>(6)?
            .map(|score| u8::try_from(score).map_err(|_| rusqlite::Error::InvalidQuery))
            .transpose()?;
        Ok(DiffFileChange {
            parent_commit_id: row.get(0)?,
            child_commit_id: row.get(1)?,
            change_index: row.get(2)?,
            kind: parse_diff_change_kind(&row.get::<_, String>(3)?)?,
            old_path: row.get(4)?,
            new_path: row.get(5)?,
            similarity_percent,
        })
    })?;
    Ok(DiffMetadata {
        settings,
        changes: rows.collect::<std::result::Result<Vec<_>, _>>()?,
    })
}

pub(crate) fn search_diffs(
    c: &Connection,
    query: &str,
    mode: DiffSearchMode,
    limit: usize,
    child_commit_ids: &[String],
) -> Result<Vec<DiffSearchResult>> {
    search_diffs_filtered(c, query, mode, limit, child_commit_ids, |_| true)
}

pub(crate) fn score_diff_results(
    c: &Connection,
    query: &str,
    results: &mut [DiffSearchResult],
) -> Result<()> {
    if results.iter().all(|result| result.distance.is_some()) {
        return Ok(());
    }
    let vector = query_vector(c, query)?;
    let mut statement = c.prepare(
        "SELECT vec_distance_cosine(e.vector,?1) FROM unit_embeddings e WHERE e.unit_id=?2",
    )?;
    for result in results
        .iter_mut()
        .filter(|result| result.distance.is_none())
    {
        result.distance = statement
            .query_row(
                params![as_bytes(&vector), result.reference.unit_id],
                |row| row.get(0),
            )
            .optional()?;
    }
    Ok(())
}

pub(crate) fn search_diffs_filtered(
    c: &Connection,
    query: &str,
    mode: DiffSearchMode,
    limit: usize,
    child_commit_ids: &[String],
    include: impl Fn(&DiffReference) -> bool,
) -> Result<Vec<DiffSearchResult>> {
    if limit == 0 || child_commit_ids.is_empty() {
        return Ok(Vec::new());
    }
    let commits = serde_json::to_string(child_commit_ids)?;
    let mut results = Vec::new();
    let lowercase_query = query.to_lowercase();
    match mode {
        DiffSearchMode::Exact => {
            let mut statement = c.prepare(&format!(
                "SELECT {DIFF_REFERENCE_COLUMNS} FROM diff_references r
                 WHERE r.child_commit_id IN (SELECT value FROM json_each(?1))
                 ORDER BY r.child_commit_id,r.parent_commit_id,r.hunk_id,r.side,r.byte_start"
            ))?;
            let rows = statement.query_map([commits], read_diff_reference)?;
            let mut reader = GitBlobReader::new(None)?;
            for row in rows {
                let reference = row?;
                if !include(&reference) {
                    continue;
                }
                let text = hydrate_unit(
                    &reference.blob_id,
                    reference.byte_start,
                    reference.byte_end,
                    &mut reader,
                )?;
                if text.to_lowercase().contains(&lowercase_query) {
                    results.push(DiffSearchResult {
                        text,
                        reference,
                        distance: None,
                    });
                    if results.len() == limit {
                        break;
                    }
                }
            }
            reader.finish()?;
        }
        DiffSearchMode::Semantic => {
            let vector = query_vector(c, query)?;
            let mut statement = c.prepare(&format!(
                "SELECT {DIFF_REFERENCE_COLUMNS},vec_distance_cosine(e.vector,?1)
                 FROM diff_references r JOIN unit_embeddings e USING(unit_id)
                 WHERE r.child_commit_id IN (SELECT value FROM json_each(?2))
                 ORDER BY 17,r.child_commit_id,r.parent_commit_id,r.hunk_id,r.side,r.byte_start,r.unit_id"
            ))?;
            let rows = statement.query_map(params![as_bytes(&vector), commits], |row| {
                Ok((read_diff_reference(row)?, row.get::<_, f64>(16)?))
            })?;
            let mut reader = GitBlobReader::new(None)?;
            for row in rows {
                let (reference, distance) = row?;
                if !include(&reference) {
                    continue;
                }
                let text = hydrate_unit(
                    &reference.blob_id,
                    reference.byte_start,
                    reference.byte_end,
                    &mut reader,
                )?;
                results.push(DiffSearchResult {
                    text,
                    reference,
                    distance: Some(distance),
                });
                if results.len() == limit {
                    break;
                }
            }
            reader.finish()?;
        }
    }
    Ok(results)
}

pub fn reembed(c: &mut Connection, command: &str, model: &str) -> Result<()> {
    let version = snapshot_version(c)?;
    c.execute_batch("PRAGMA temp_store=FILE; DROP TABLE IF EXISTS temp.reembed_vectors; CREATE TEMP TABLE reembed_vectors(unit_id INTEGER PRIMARY KEY, vector BLOB NOT NULL);")?;
    let mut preparation_bytes = Budget::refresh("history refresh preparation accounting")?;
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
    if limit == 0 && mode != "combined" {
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
    let mut semantic_count = 0;
    while let Some(row) = rows.next()? {
        let commit_id: String = row.get(0)?;
        let start: usize = row.get(1)?;
        let end: usize = row.get(2)?;
        let line_start: usize = row.get(3)?;
        let line_end: usize = row.get(4)?;
        let distance: Option<f64> = row.get(5)?;
        let text = hydrate_commit_message(&commit_id, start, end)?;
        let exact_match = text.to_lowercase().contains(&lowercase_query);
        if mode == "exact" && !exact_match {
            continue;
        }
        if mode == "combined" && semantic_count >= limit && !exact_match {
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
            requested_url: None,
            final_url: None,
            redirect_chain: None,
            fetch_status: None,
            fetch_error: None,
            worktree: None,
            semantic_worktree: None,
        });
        if mode != "combined" {
            if results.len() == limit {
                break;
            }
        } else if semantic_count < limit {
            semantic_count += 1;
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
    search_impl(c, query, mode, limit, commits, include, false)
}

/// Search every eligible commit/path occurrence instead of collapsing identical
/// blobs to one compact reference. This is the backend enumeration seam for
/// callers that page through complete history; `limit` still bounds semantic
/// candidate rows, so pass `usize::MAX` to enumerate them all.
#[allow(dead_code)] // CLI paging in #69 consumes this public backend operation.
pub fn search_all_occurrences(
    c: &Connection,
    query: &str,
    mode: &str,
    limit: usize,
    commits: &[String],
    include: impl Fn(&SearchResult) -> bool,
) -> Result<Vec<SearchResult>> {
    search_impl(c, query, mode, limit, commits, include, true)
}

fn search_impl(
    c: &Connection,
    query: &str,
    mode: &str,
    limit: usize,
    commits: &[String],
    include: impl Fn(&SearchResult) -> bool,
    all_occurrences: bool,
) -> Result<Vec<SearchResult>> {
    let v = if mode == "exact" {
        None
    } else {
        Some(query_vector(c, query)?)
    };
    // Compact search chooses one eligible reference per blob; all-occurrence
    // search leaves every matching commit/path row available to the caller.
    // Version 2 snapshots keep their original line units until explicit refresh.
    let schema: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let variant = if schema >= 3 {
        "AND b.segmentation_kind=o.segmentation_kind"
    } else {
        ""
    };
    let occurrence_partition = "blob_id";
    let occurrence_filter = if all_occurrences {
        ""
    } else {
        "WHERE occurrence_rank=1"
    };
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
        {occurrence_filter} ORDER BY o.path,u.line_start,o.blob_id,o.commit_id,u.unit_id",
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
                    requested_url: None,
                    final_url: None,
                    redirect_chain: None,
                    fetch_status: None,
                    fetch_error: None,
                    worktree: None,
                    semantic_worktree: None,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistorySelector {
    Head,
    All,
    Commit(String),
    /// Compatibility selector for callers that select an explicit set without a base scope.
    ExactCommits(Vec<String>),
    Topological(usize),
    Path(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryQuery {
    pub selector: HistorySelector,
    pub start_commit: Option<String>,
    pub include_commits: Vec<String>,
    pub exclude_commits: Vec<String>,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            selector: HistorySelector::Head,
            start_commit: None,
            include_commits: Vec::new(),
            exclude_commits: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeuristicPathLink {
    pub parent_commit_id: String,
    pub child_commit_id: String,
    pub old_path: Vec<u8>,
    pub new_path: Vec<u8>,
    pub similarity_percent: Option<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedHistoryEntry {
    pub commit_id: String,
    /// Present for path selectors; directory queries produce one entry per matching file.
    pub path: Option<Vec<u8>>,
    /// Rename links crossing into this commit. These are Git heuristics, not identity proof.
    pub heuristic_links: Vec<HeuristicPathLink>,
}

fn resolve_known_commit(selector: &str, known: &[String]) -> Result<String> {
    if !(4..=64).contains(&selector.len()) || !selector.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!(
            "expected exact commit ID (at least 4 hex digits), not a ref or range: {selector}"
        )
        .into());
    }
    let prefix = selector.to_ascii_lowercase();
    let matches: Vec<_> = known.iter().filter(|id| id.starts_with(&prefix)).collect();
    match matches.as_slice() {
        [id] => Ok((*id).clone()),
        [] => Err(format!("commit {selector} is not in the index; run refresh explicitly").into()),
        _ => Err(format!("ambiguous commit ID {selector}").into()),
    }
}

fn known_commit_ids(c: &Connection, budget: &mut Budget) -> Result<Vec<String>> {
    let table = if is_current(c)? {
        "git_commits"
    } else {
        "indexed_files"
    };
    let mut statement = c.prepare(&format!(
        "SELECT DISTINCT commit_id FROM {table} ORDER BY commit_id"
    ))?;
    let mut rows = statement.query([])?;
    let mut commits = Vec::new();
    while let Some(row) = rows.next()? {
        let commit_id: String = row.get(0)?;
        budget.charge(
            commit_id.len() + std::mem::size_of::<String>(),
            "known history commit IDs",
        )?;
        commits.push(commit_id);
    }
    Ok(commits)
}

fn start_commit_for_history(
    query: &HistoryQuery,
    known: &[String],
    budget: &mut Budget,
) -> Result<Option<String>> {
    if let Some(start) = query.start_commit.as_deref() {
        return Ok(Some(resolve_known_commit(start, known)?));
    }
    let Some(head) = head(budget)? else {
        return Ok(None);
    };
    Ok(known.contains(&head).then_some(head))
}

type TopologicalGraph = (Vec<String>, HashMap<String, Vec<String>>);

fn topological_graph(start: &str, budget: &mut Budget) -> Result<TopologicalGraph> {
    use std::collections::BTreeSet;

    let output = git_output(
        &["rev-list", "--parents", start],
        budget.remaining().saturating_sub(MAX_GIT_STDERR_BYTES),
        budget,
    )?;
    let mut parents_by_commit = HashMap::<String, Vec<String>>::new();
    for record in output
        .split(|byte| *byte == b'\n')
        .filter(|record| !record.is_empty())
    {
        budget.charge(
            record.len() + std::mem::size_of::<String>(),
            "history commit graph",
        )?;
        let record = std::str::from_utf8(record)?;
        let mut fields = record.split_whitespace();
        let commit = fields
            .next()
            .ok_or("Git returned an empty commit graph record")?;
        if commit.len() < 4 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Git returned an invalid commit ID in history graph".into());
        }
        let parents = fields.map(str::to_owned).collect::<Vec<_>>();
        if parents_by_commit
            .insert(commit.to_owned(), parents)
            .is_some()
        {
            return Err("Git returned a duplicate commit in history graph".into());
        }
    }
    let mut remaining_children = parents_by_commit
        .keys()
        .map(|commit| (commit.clone(), 0usize))
        .collect::<HashMap<_, _>>();
    for parents in parents_by_commit.values() {
        for parent in parents {
            if let Some(child_count) = remaining_children.get_mut(parent) {
                *child_count += 1;
            }
        }
    }
    let mut ready = remaining_children
        .iter()
        .filter_map(|(commit, child_count)| (*child_count == 0).then_some(commit.clone()))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(parents_by_commit.len());
    while let Some(commit) = ready.pop_first() {
        order.push(commit.clone());
        if let Some(parents) = parents_by_commit.get(&commit) {
            for parent in parents {
                if let Some(child_count) = remaining_children.get_mut(parent) {
                    *child_count -= 1;
                    if *child_count == 0 {
                        ready.insert(parent.clone());
                    }
                }
            }
        }
    }
    if order.len() != parents_by_commit.len() {
        return Err("Git commit graph is not a directed acyclic graph".into());
    }
    Ok((order, parents_by_commit))
}

fn normalized_history_path(path: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::new();
    for component in path.split(|byte| *byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }
        if !normalized.is_empty() {
            normalized.push(b'/');
        }
        normalized.extend_from_slice(component);
    }
    normalized
}

fn initial_history_paths(
    start: &str,
    requested_path: &[u8],
    budget: &mut Budget,
) -> Result<Vec<Vec<u8>>> {
    if requested_path.starts_with(b"/")
        || requested_path
            .split(|byte| *byte == b'/')
            .any(|part| part == b"..")
    {
        return Err("history path must be repository-relative and cannot contain '..'".into());
    }
    let path = normalized_history_path(requested_path);
    let root_query = path.is_empty();
    let mut directory_prefix = path.clone();
    if !root_query {
        directory_prefix.push(b'/');
    }
    let mut exact = false;
    let mut descendant = root_query;
    let mut matches = Vec::new();
    for_each_tree_record(start, budget, |record, budget| {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("invalid Git tree record")?;
        let metadata = std::str::from_utf8(&record[..tab])?;
        let mut fields = metadata.split_whitespace();
        let _mode = fields.next().ok_or("invalid Git tree metadata")?;
        let object_type = fields.next().ok_or("invalid Git tree metadata")?;
        let _object_id = fields.next().ok_or("invalid Git tree metadata")?;
        if fields.next().is_some() || object_type != "blob" {
            return Ok(());
        }
        let candidate = &record[tab + 1..];
        if candidate == path {
            exact = true;
        }
        if root_query || candidate.starts_with(&directory_prefix) {
            descendant = true;
            budget.charge(
                candidate.len() + std::mem::size_of::<Vec<u8>>(),
                "history path matches",
            )?;
            matches.push(candidate.to_vec());
        }
        Ok(())
    })?;
    if descendant {
        matches.sort();
        matches.dedup();
        Ok(matches)
    } else if exact {
        Ok(vec![path])
    } else {
        Ok(Vec::new())
    }
}

fn add_selected_path(
    selected_paths: &mut HashMap<String, HashMap<Vec<u8>, Vec<HeuristicPathLink>>>,
    commit_id: &str,
    path: Vec<u8>,
    links: Vec<HeuristicPathLink>,
    budget: &mut Budget,
) -> Result<()> {
    let paths = selected_paths.entry(commit_id.to_owned()).or_default();
    if !paths.contains_key(&path) {
        budget.charge(path.len() + 64, "history path selection")?;
        paths.insert(path.clone(), Vec::new());
    }
    let stored_links = paths.get_mut(&path).unwrap();
    for link in links {
        if !stored_links.contains(&link) {
            budget.charge(
                link.old_path.len() + link.new_path.len() + 96,
                "heuristic history path link",
            )?;
            stored_links.push(link);
        }
    }
    Ok(())
}

fn select_path_history(
    c: &Connection,
    start: &str,
    requested_path: &[u8],
    known: &[String],
    budget: &mut Budget,
) -> Result<Vec<SelectedHistoryEntry>> {
    let version: i64 = c.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 5 {
        return Err("path history requires a full refresh with schema version 5".into());
    }
    let start_paths = initial_history_paths(start, requested_path, budget)?;
    let known = known.iter().cloned().collect::<HashSet<_>>();
    let (order, parents_by_commit) = topological_graph(start, budget)?;
    let mut selected_paths = HashMap::<String, HashMap<Vec<u8>, Vec<HeuristicPathLink>>>::new();
    for path in start_paths {
        add_selected_path(&mut selected_paths, start, path, Vec::new(), budget)?;
    }
    if selected_paths.is_empty() {
        let requested = normalized_history_path(requested_path);
        let mut prefix = requested.clone();
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        for parent in parents_by_commit
            .get(start)
            .into_iter()
            .flatten()
            .filter(|parent| known.contains(*parent))
        {
            let metadata = diff_metadata(c, Some(parent), Some(start))?;
            for change in metadata.changes {
                let Some(old_path) = change.old_path.as_deref() else {
                    continue;
                };
                if change.kind != DiffChangeKind::HeuristicRename
                    || (old_path != requested.as_slice() && !old_path.starts_with(&prefix))
                {
                    continue;
                }
                let Some(new_path) = change.new_path else {
                    continue;
                };
                let link = HeuristicPathLink {
                    parent_commit_id: parent.clone(),
                    child_commit_id: start.to_owned(),
                    old_path: old_path.to_vec(),
                    new_path: new_path.clone(),
                    similarity_percent: change.similarity_percent,
                };
                add_selected_path(&mut selected_paths, start, new_path, vec![link], budget)?;
            }
        }
    }
    if selected_paths.is_empty() {
        return Ok(Vec::new());
    }
    for child in &order {
        if !known.contains(child) {
            continue;
        }
        let child_paths = selected_paths
            .get(child)
            .map(|paths| paths.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for path in &child_paths {
            budget.charge(path.len(), "history child path traversal")?;
        }
        if child_paths.is_empty() {
            continue;
        }
        let Some(parents) = parents_by_commit.get(child) else {
            continue;
        };
        for parent in parents.iter().filter(|parent| known.contains(*parent)) {
            let metadata = diff_metadata(c, Some(parent), Some(child))?;
            let metadata_bytes = metadata.changes.iter().try_fold(0usize, |total, change| {
                total
                    .checked_add(std::mem::size_of::<DiffFileChange>())
                    .and_then(|total| total.checked_add(change.parent_commit_id.len()))
                    .and_then(|total| total.checked_add(change.child_commit_id.len()))
                    .and_then(|total| {
                        total.checked_add(change.old_path.as_ref().map_or(0, Vec::len))
                    })
                    .and_then(|total| {
                        total.checked_add(change.new_path.as_ref().map_or(0, Vec::len))
                    })
                    .ok_or("history diff metadata accounting overflow")
            })?;
            budget.charge(metadata_bytes, "history diff metadata")?;
            let changes_by_new_path = metadata
                .changes
                .iter()
                .filter_map(|change| change.new_path.as_deref().map(|path| (path, change)))
                .collect::<HashMap<_, _>>();
            for child_path in &child_paths {
                let change = changes_by_new_path.get(child_path.as_slice()).copied();
                match change {
                    Some(change) if change.kind == DiffChangeKind::Added => {}
                    Some(change) if change.kind == DiffChangeKind::HeuristicCopy => {}
                    Some(change) if change.kind == DiffChangeKind::HeuristicRename => {
                        if let Some(old_path) = change.old_path.clone() {
                            let link = HeuristicPathLink {
                                parent_commit_id: parent.clone(),
                                child_commit_id: child.clone(),
                                old_path: old_path.clone(),
                                new_path: child_path.clone(),
                                similarity_percent: change.similarity_percent,
                            };
                            add_selected_path(
                                &mut selected_paths,
                                parent,
                                old_path,
                                vec![link],
                                budget,
                            )?;
                        }
                    }
                    Some(change) => {
                        if let Some(old_path) = change.old_path.clone() {
                            add_selected_path(
                                &mut selected_paths,
                                parent,
                                old_path,
                                Vec::new(),
                                budget,
                            )?;
                        }
                    }
                    None => add_selected_path(
                        &mut selected_paths,
                        parent,
                        child_path.clone(),
                        Vec::new(),
                        budget,
                    )?,
                }
            }
            budget.release(metadata_bytes);
        }
    }
    let mut entries = Vec::new();
    for commit_id in order.into_iter().filter(|commit| known.contains(commit)) {
        let Some(paths) = selected_paths.remove(&commit_id) else {
            continue;
        };
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        paths.sort_by(|left, right| left.0.cmp(&right.0));
        for (path, mut heuristic_links) in paths {
            heuristic_links.sort_by(|left, right| {
                left.child_commit_id
                    .cmp(&right.child_commit_id)
                    .then(left.old_path.cmp(&right.old_path))
                    .then(left.new_path.cmp(&right.new_path))
            });
            entries.push(SelectedHistoryEntry {
                commit_id: commit_id.clone(),
                path: Some(path),
                heuristic_links,
            });
        }
    }
    Ok(entries)
}

/// Select indexed commits in stable commit-ID order, or by parent-before-child-safe
/// topological traversal from a start commit. Path selectors return one entry per
/// matching file path and follow only Git-detected heuristic renames. Include IDs
/// are additive; exclusions are applied last. Branch labels and timestamp ordering
/// are never used as ancestry.
pub fn select_history(c: &Connection, query: &HistoryQuery) -> Result<Vec<SelectedHistoryEntry>> {
    let mut budget = Budget::new(MAX_REFRESH_ACCOUNTING_BYTES, "history selection accounting");
    if query.start_commit.is_some()
        && matches!(
            query.selector,
            HistorySelector::All | HistorySelector::Commit(_) | HistorySelector::ExactCommits(_)
        )
    {
        return Err("starting commit is only valid with HEAD, topological or path history".into());
    }
    let known = known_commit_ids(c, &mut budget)?;
    let mut entries = match &query.selector {
        HistorySelector::All => known
            .iter()
            .cloned()
            .map(|commit_id| SelectedHistoryEntry {
                commit_id,
                path: None,
                heuristic_links: Vec::new(),
            })
            .collect(),
        HistorySelector::Commit(commit) => vec![SelectedHistoryEntry {
            commit_id: resolve_known_commit(commit, &known)?,
            path: None,
            heuristic_links: Vec::new(),
        }],
        HistorySelector::ExactCommits(commits) => commits
            .iter()
            .map(|commit| {
                Ok(SelectedHistoryEntry {
                    commit_id: resolve_known_commit(commit, &known)?,
                    path: None,
                    heuristic_links: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        HistorySelector::Head => match start_commit_for_history(query, &known, &mut budget)? {
            Some(commit_id) => vec![SelectedHistoryEntry {
                commit_id,
                path: None,
                heuristic_links: Vec::new(),
            }],
            None => Vec::new(),
        },
        HistorySelector::Topological(limit) => {
            if *limit == 0 {
                return Err("topological history limit must be greater than zero".into());
            }
            if let Some(start) = start_commit_for_history(query, &known, &mut budget)? {
                topological_graph(&start, &mut budget)?
                    .0
                    .into_iter()
                    .filter(|commit| known.contains(commit))
                    .take(*limit)
                    .map(|commit_id| SelectedHistoryEntry {
                        commit_id,
                        path: None,
                        heuristic_links: Vec::new(),
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        HistorySelector::Path(path) => {
            if let Some(start) = start_commit_for_history(query, &known, &mut budget)? {
                select_path_history(c, &start, path, &known, &mut budget)?
            } else {
                Vec::new()
            }
        }
    };
    for selector in &query.include_commits {
        let commit_id = resolve_known_commit(selector, &known)?;
        if entries.iter().any(|entry| entry.commit_id == commit_id) {
            continue;
        }
        if let HistorySelector::Path(path) = &query.selector {
            let path_entries = select_path_history(c, &commit_id, path, &known, &mut budget)?;
            entries.extend(
                path_entries
                    .into_iter()
                    .filter(|entry| entry.commit_id == commit_id),
            );
        } else {
            entries.push(SelectedHistoryEntry {
                commit_id,
                path: None,
                heuristic_links: Vec::new(),
            });
        }
    }
    let excluded = query
        .exclude_commits
        .iter()
        .map(|selector| resolve_known_commit(selector, &known))
        .collect::<Result<HashSet<_>>>()?;
    entries.retain(|entry| !excluded.contains(&entry.commit_id));
    let mut seen = HashSet::new();
    entries.retain(|entry| seen.insert((entry.commit_id.clone(), entry.path.clone())));
    Ok(entries)
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
        let selector = if self.all_history {
            HistorySelector::All
        } else if let Some(commit) = &self.commit {
            HistorySelector::Commit(commit.clone())
        } else if !self.include.is_empty() {
            HistorySelector::ExactCommits(self.include.clone())
        } else {
            HistorySelector::Head
        };
        let query = HistoryQuery {
            selector,
            start_commit: None,
            include_commits: Vec::new(),
            exclude_commits: self.exclude.clone(),
        };
        let mut commits = select_history(c, &query)?
            .into_iter()
            .map(|entry| entry.commit_id)
            .collect::<Vec<_>>();
        commits.sort();
        commits.dedup();
        Ok(commits)
    }
}
