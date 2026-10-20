use serde::Serialize;
use std::{
    collections::BTreeSet,
    error::Error,
    ffi::OsString,
    fmt,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_MATCHES_PER_FILE: usize = 1_000;
const MAX_TOTAL_MATCHES: usize = 5_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Head,
    Staged,
    Live,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiveMatch {
    /// Raw Git path bytes; callers can preserve paths that are not UTF-8.
    pub path: Vec<u8>,
    pub line_start: usize,
    pub line_end: usize,
    /// Complete live lines containing the exact query.
    pub text: String,
    /// Query occurrences in the same file at each preceding layer. These are
    /// references, not a claim that Git mapped one occurrence to another.
    pub head_line_ranges: Vec<LineRange>,
    pub staged_line_ranges: Vec<LineRange>,
    pub provenance: Vec<Layer>,
    pub head_to_staged_changed: Option<bool>,
    pub staged_to_live_changed: Option<bool>,
    pub untracked: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Coverage {
    pub changed_tracked_files: usize,
    pub untracked_files: usize,
    pub ignored_files_excluded: usize,
    pub unsupported_files_excluded: usize,
    pub binary_files_excluded: usize,
    pub removed_files: usize,
    pub unreadable_files: usize,
    pub oversized_files_excluded: usize,
    pub git_layer_failures: usize,
    pub unembedded_addition_files: usize,
    pub match_limit_reached_files: usize,
    pub total_match_limit_reached: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingStatus {
    Mapped,
    Deleted,
    Ambiguous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedRange {
    pub path: Vec<u8>,
    pub line_start: usize,
    pub line_end: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SemanticLiveMatch {
    pub path: Vec<u8>,
    pub head_line_start: usize,
    pub head_line_end: usize,
    pub head_text: String,
    pub staged_line_start: Option<usize>,
    pub staged_line_end: Option<usize>,
    pub live_line_start: Option<usize>,
    pub live_line_end: Option<usize>,
    pub live_text: Option<String>,
    pub mapping_status: MappingStatus,
    pub changed_live_text: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SemanticMappingReport {
    pub indexed_head: String,
    pub observed_head: Option<String>,
    pub head_changed_during_scan: Option<bool>,
    pub observed_at_unix_ms: u128,
    pub mappings: Vec<SemanticLiveMatch>,
    pub coverage: Coverage,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorktreeSearch {
    pub indexed_head: String,
    pub observed_head: Option<String>,
    pub head_changed_during_scan: Option<bool>,
    pub observed_at_unix_ms: u128,
    /// Every changed or eligible untracked path, including paths with no live
    /// exact match. Callers replace HEAD content results for these paths.
    pub updated_paths: Vec<Vec<u8>>,
    pub matches: Vec<LiveMatch>,
    pub coverage: Coverage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeError {
    Git(String),
    MissingHead,
    UnindexedHead {
        actual_head: String,
    },
    StaleHead {
        indexed_head: String,
        actual_head: String,
    },
    EmptyQuery,
}

impl fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(message) => write!(f, "{message}"),
            Self::MissingHead => write!(f, "repository has no active HEAD"),
            Self::UnindexedHead { actual_head } => write!(
                f,
                "active HEAD {actual_head} is not present in the search index; refresh the repository"
            ),
            Self::StaleHead {
                indexed_head,
                actual_head,
            } => write!(
                f,
                "indexed HEAD {indexed_head} is stale; active HEAD is {actual_head}; refresh the repository"
            ),
            Self::EmptyQuery => write!(f, "worktree exact search requires a nonempty query"),
        }
    }
}

impl Error for WorktreeError {}

type Result<T> = std::result::Result<T, WorktreeError>;

/// Search the best-effort live view of changed and untracked supported text.
///
/// `indexed_head` must be the indexed commit for this repository. The function
/// rejects absent or stale index identity before examining worktree content.
/// It does not write an index, create embeddings, or persist file text. Its
/// result replaces committed content matches for `updated_paths`; unchanged
/// paths remain the caller's indexed-HEAD results. Concurrent changes may be
/// observed inconsistently and are reported through coverage and HEAD metadata.
pub fn search_exact(
    repository: &Path,
    indexed_head: Option<&str>,
    query: &str,
) -> Result<WorktreeSearch> {
    if query.is_empty() {
        return Err(WorktreeError::EmptyQuery);
    }
    let head = active_head(repository)?;
    let Some(indexed_head) = indexed_head else {
        return Err(WorktreeError::UnindexedHead { actual_head: head });
    };
    if indexed_head != head {
        return Err(WorktreeError::StaleHead {
            indexed_head: indexed_head.to_owned(),
            actual_head: head,
        });
    }

    let mut changed = paths_from_nul(&git(
        repository,
        &[
            b"diff",
            b"--no-renames",
            b"--name-only",
            b"-z",
            head.as_bytes(),
            b"--",
        ],
    )?);
    let indexed_paths = paths_from_nul(&git(repository, &[b"ls-files", b"-z"])?);
    let untracked = paths_from_nul(&git(
        repository,
        &[b"ls-files", b"--others", b"--exclude-standard", b"-z"],
    )?);
    let ignored = paths_from_nul(&git(
        repository,
        &[
            b"ls-files",
            b"--others",
            b"--ignored",
            b"--exclude-standard",
            b"-z",
        ],
    )?);
    changed.extend(untracked.iter().cloned());
    let updated_paths = changed.into_iter().collect::<Vec<_>>();
    let mut coverage = Coverage {
        changed_tracked_files: updated_paths.len().saturating_sub(untracked.len()),
        untracked_files: untracked.len(),
        ignored_files_excluded: ignored.len(),
        ..Coverage::default()
    };
    let mut matches = Vec::new();

    for path in &updated_paths {
        if matches.len() >= MAX_TOTAL_MATCHES {
            coverage.total_match_limit_reached = true;
            break;
        }
        if ignored.contains(path) && !indexed_paths.contains(path) {
            continue;
        }
        if !supported_path(path) {
            coverage.unsupported_files_excluded += 1;
            continue;
        }
        let live_path = match safe_live_path(repository, path) {
            Ok(Some(path)) => path,
            Ok(None) => {
                coverage.removed_files += 1;
                continue;
            }
            Err(LivePathError::Unsupported) => {
                coverage.unsupported_files_excluded += 1;
                continue;
            }
            Err(LivePathError::Unreadable) => {
                coverage.unreadable_files += 1;
                continue;
            }
        };
        let live = match read_live(&live_path) {
            Ok(text) => text,
            Err(LiveReadError::Removed) => {
                coverage.removed_files += 1;
                continue;
            }
            Err(LiveReadError::Binary) => {
                coverage.binary_files_excluded += 1;
                continue;
            }
            Err(LiveReadError::Oversized) => {
                coverage.oversized_files_excluded += 1;
                continue;
            }
            Err(LiveReadError::Unsupported) => {
                coverage.unsupported_files_excluded += 1;
                continue;
            }
            Err(LiveReadError::Unreadable) => {
                coverage.unreadable_files += 1;
                continue;
            }
        };

        let head_layer = show_path(repository, &head, path);
        let staged_layer = show_path(repository, ":", path);
        let head_to_staged_changed = layer_changed(&head_layer, &staged_layer);
        let staged_to_live_changed = layer_to_live_changed(&staged_layer, live.as_bytes());
        let mut binary_layer = false;
        let mut oversized_layer = false;
        let head_text = match head_layer {
            LayerContent::Found(bytes) => match text_content(bytes) {
                Ok(text) => Some(text),
                Err(LayerTextError::Binary) => {
                    binary_layer = true;
                    None
                }
                Err(LayerTextError::Oversized) => {
                    oversized_layer = true;
                    None
                }
            },
            LayerContent::Oversized => {
                oversized_layer = true;
                None
            }
            LayerContent::Missing => None,
            LayerContent::Failed => {
                coverage.git_layer_failures += 1;
                None
            }
        };
        let staged_text = match staged_layer {
            LayerContent::Found(bytes) => match text_content(bytes) {
                Ok(text) => Some(text),
                Err(LayerTextError::Binary) => {
                    binary_layer = true;
                    None
                }
                Err(LayerTextError::Oversized) => {
                    oversized_layer = true;
                    None
                }
            },
            LayerContent::Oversized => {
                oversized_layer = true;
                None
            }
            LayerContent::Missing => None,
            LayerContent::Failed => {
                coverage.git_layer_failures += 1;
                None
            }
        };
        coverage.binary_files_excluded += usize::from(binary_layer);
        coverage.oversized_files_excluded += usize::from(oversized_layer);
        let (head_ranges, head_truncated) = head_text
            .as_deref()
            .map(|text| find_ranges(text, query))
            .unwrap_or_default();
        let (staged_ranges, staged_truncated) = staged_text
            .as_deref()
            .map(|text| find_ranges(text, query))
            .unwrap_or_default();
        let (live_ranges, live_truncated) = find_ranges(&live, query);
        if head_truncated || staged_truncated || live_truncated {
            coverage.match_limit_reached_files += 1;
        }
        let provenance = provenance(&head_ranges, &staged_ranges);

        for range in live_ranges {
            if matches.len() >= MAX_TOTAL_MATCHES {
                coverage.total_match_limit_reached = true;
                break;
            }
            let lines = live.split('\n').collect::<Vec<_>>();
            let text = lines
                .iter()
                .skip(range.start - 1)
                .take(range.end - range.start + 1)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            matches.push(LiveMatch {
                path: path.clone(),
                line_start: range.start,
                line_end: range.end,
                text,
                head_line_ranges: head_ranges.clone(),
                staged_line_ranges: staged_ranges.clone(),
                provenance: provenance.clone(),
                head_to_staged_changed,
                staged_to_live_changed,
                untracked: untracked.contains(path),
            });
        }
    }

    let observed_head = active_head(repository).ok();
    let head_changed_during_scan = observed_head
        .as_deref()
        .map(|observed| observed != indexed_head);
    Ok(WorktreeSearch {
        indexed_head: head,
        head_changed_during_scan,
        observed_head,
        observed_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        updated_paths,
        matches,
        coverage,
    })
}

fn layer_changed(previous: &LayerContent, next: &LayerContent) -> Option<bool> {
    match (previous, next) {
        (LayerContent::Failed | LayerContent::Oversized, _)
        | (_, LayerContent::Failed | LayerContent::Oversized) => None,
        (LayerContent::Found(previous), LayerContent::Found(next)) => Some(previous != next),
        (LayerContent::Missing, LayerContent::Missing) => Some(false),
        _ => Some(true),
    }
}

fn layer_to_live_changed(staged: &LayerContent, live: &[u8]) -> Option<bool> {
    match staged {
        LayerContent::Found(staged) => Some(staged != live),
        LayerContent::Missing => Some(true),
        LayerContent::Failed | LayerContent::Oversized => None,
    }
}

/// Map semantic references whose vectors were computed from indexed HEAD text
/// through HEAD→index and index→live Git patches. The operation never embeds
/// staged, live, or untracked text; ambiguous/deleted mappings retain only the
/// original HEAD reference and its matched lines.
pub fn map_semantic_ranges(
    repository: &Path,
    indexed_head: Option<&str>,
    ranges: &[IndexedRange],
) -> Result<SemanticMappingReport> {
    let head = active_head(repository)?;
    let Some(indexed_head) = indexed_head else {
        return Err(WorktreeError::UnindexedHead { actual_head: head });
    };
    if indexed_head != head {
        return Err(WorktreeError::StaleHead {
            indexed_head: indexed_head.to_owned(),
            actual_head: head,
        });
    }
    let mut coverage = semantic_coverage(repository, &head)?;
    let mut mappings = Vec::with_capacity(ranges.len());
    for range in ranges {
        if range.line_start == 0 || range.line_end < range.line_start {
            return Err(WorktreeError::Git(
                "invalid indexed semantic line range".to_owned(),
            ));
        }
        let head_text = match show_path(repository, &head, &range.path) {
            LayerContent::Found(bytes) => text_content(bytes).map_err(|_| {
                WorktreeError::Git("indexed HEAD reference is not bounded UTF-8 text".to_owned())
            })?,
            LayerContent::Oversized => {
                return Err(WorktreeError::Git(
                    "indexed HEAD reference exceeds the live mapping limit".to_owned(),
                ));
            }
            LayerContent::Missing | LayerContent::Failed => {
                return Err(WorktreeError::Git(
                    "indexed HEAD object is unavailable for semantic mapping".to_owned(),
                ));
            }
        };
        let head_lines = source_lines(&head_text);
        let Some(head_text) = line_text(&head_lines, range.line_start, range.line_end) else {
            return Err(WorktreeError::Git(
                "indexed semantic range is outside its HEAD file".to_owned(),
            ));
        };
        let mut mapped = SemanticLiveMatch {
            path: range.path.clone(),
            head_line_start: range.line_start,
            head_line_end: range.line_end,
            head_text: head_text.to_owned(),
            staged_line_start: None,
            staged_line_end: None,
            live_line_start: None,
            live_line_end: None,
            live_text: None,
            mapping_status: MappingStatus::Ambiguous,
            changed_live_text: None,
        };

        let staged = match load_layer_text(repository, ":", &range.path, &mut coverage) {
            LayerTextStatus::Text(text) => text,
            LayerTextStatus::Missing => {
                mapped.mapping_status = live_missing_status(repository, &range.path, &mut coverage);
                mappings.push(mapped);
                continue;
            }
            LayerTextStatus::Unavailable => {
                mapped.mapping_status = MappingStatus::Ambiguous;
                mappings.push(mapped);
                continue;
            }
        };
        let head_to_staged = git_diff_hunks(repository, Some(&head), true, &range.path)?;
        let staged_range = match map_line_range(range.line_start, range.line_end, &head_to_staged) {
            StepMapping::Mapped { start, end } => (start, end),
            StepMapping::Deleted => {
                mapped.mapping_status = live_missing_status(repository, &range.path, &mut coverage);
                mappings.push(mapped);
                continue;
            }
            StepMapping::Ambiguous => {
                mapped.mapping_status = MappingStatus::Ambiguous;
                mappings.push(mapped);
                continue;
            }
        };
        let staged_lines = source_lines(&staged);
        if line_text(&staged_lines, staged_range.0, staged_range.1).is_none() {
            mapped.mapping_status = MappingStatus::Ambiguous;
            mappings.push(mapped);
            continue;
        }
        mapped.staged_line_start = Some(staged_range.0);
        mapped.staged_line_end = Some(staged_range.1);

        let live_path = match safe_live_path(repository, &range.path) {
            Ok(Some(path)) => path,
            Ok(None) => {
                coverage.removed_files += 1;
                mapped.mapping_status = MappingStatus::Deleted;
                mappings.push(mapped);
                continue;
            }
            Err(LivePathError::Unsupported) => {
                coverage.unsupported_files_excluded += 1;
                mapped.mapping_status = MappingStatus::Ambiguous;
                mappings.push(mapped);
                continue;
            }
            Err(LivePathError::Unreadable) => {
                coverage.unreadable_files += 1;
                mapped.mapping_status = MappingStatus::Ambiguous;
                mappings.push(mapped);
                continue;
            }
        };
        let live = match read_live(&live_path) {
            Ok(text) => text,
            Err(LiveReadError::Removed) => {
                coverage.removed_files += 1;
                mapped.mapping_status = MappingStatus::Deleted;
                mappings.push(mapped);
                continue;
            }
            Err(LiveReadError::Binary) => {
                coverage.binary_files_excluded += 1;
                mapped.mapping_status = MappingStatus::Ambiguous;
                mappings.push(mapped);
                continue;
            }
            Err(LiveReadError::Oversized) => {
                coverage.oversized_files_excluded += 1;
                mapped.mapping_status = MappingStatus::Ambiguous;
                mappings.push(mapped);
                continue;
            }
            Err(LiveReadError::Unsupported) => {
                coverage.unsupported_files_excluded += 1;
                mapped.mapping_status = MappingStatus::Ambiguous;
                mappings.push(mapped);
                continue;
            }
            Err(LiveReadError::Unreadable) => {
                coverage.unreadable_files += 1;
                mapped.mapping_status = MappingStatus::Ambiguous;
                mappings.push(mapped);
                continue;
            }
        };
        let staged_to_live = git_diff_hunks(repository, None, false, &range.path)?;
        let live_range = match map_line_range(staged_range.0, staged_range.1, &staged_to_live) {
            StepMapping::Mapped { start, end } => (start, end),
            StepMapping::Deleted => {
                mapped.mapping_status = MappingStatus::Deleted;
                mappings.push(mapped);
                continue;
            }
            StepMapping::Ambiguous => {
                mapped.mapping_status = MappingStatus::Ambiguous;
                mappings.push(mapped);
                continue;
            }
        };
        let live_lines = source_lines(&live);
        let Some(live_text) = line_text(&live_lines, live_range.0, live_range.1) else {
            mapped.mapping_status = MappingStatus::Ambiguous;
            mappings.push(mapped);
            continue;
        };
        mapped.live_line_start = Some(live_range.0);
        mapped.live_line_end = Some(live_range.1);
        mapped.live_text = Some(live_text.to_owned());
        mapped.changed_live_text = Some(live_text != head_text);
        mapped.mapping_status = MappingStatus::Mapped;
        mappings.push(mapped);
    }

    let observed_head = active_head(repository).ok();
    let head_changed_during_scan = observed_head
        .as_deref()
        .map(|observed| observed != indexed_head);
    Ok(SemanticMappingReport {
        indexed_head: head,
        observed_head,
        head_changed_during_scan,
        observed_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        mappings,
        coverage,
    })
}

#[derive(Clone, Copy)]
struct DiffHunk {
    old_start: usize,
    old_len: usize,
    new_start: usize,
    new_len: usize,
}

enum StepMapping {
    Mapped { start: usize, end: usize },
    Deleted,
    Ambiguous,
}

fn map_line_range(start: usize, end: usize, hunks: &[DiffHunk]) -> StepMapping {
    let mut mapped = Vec::new();
    let mut deleted = false;
    for line in start..=end {
        match map_line(line, hunks) {
            StepMapping::Mapped { start, .. } => mapped.push(start),
            StepMapping::Deleted => deleted = true,
            StepMapping::Ambiguous => return StepMapping::Ambiguous,
        }
    }
    if mapped.is_empty() {
        StepMapping::Deleted
    } else if deleted || mapped.windows(2).any(|pair| pair[0] >= pair[1]) {
        StepMapping::Ambiguous
    } else {
        StepMapping::Mapped {
            start: mapped[0],
            end: *mapped.last().unwrap(),
        }
    }
}

fn map_line(line: usize, hunks: &[DiffHunk]) -> StepMapping {
    let mut offset: isize = 0;
    for hunk in hunks {
        if hunk.old_len == 0 {
            if line > hunk.old_start {
                offset += hunk.new_len as isize;
            }
            continue;
        }
        let old_end = hunk
            .old_start
            .saturating_add(hunk.old_len)
            .saturating_sub(1);
        if line < hunk.old_start {
            break;
        }
        if line <= old_end {
            if hunk.old_len == 1 && hunk.new_len == 1 {
                return StepMapping::Mapped {
                    start: hunk.new_start,
                    end: hunk.new_start,
                };
            }
            return if hunk.new_len == 0 {
                StepMapping::Deleted
            } else {
                StepMapping::Ambiguous
            };
        }
        offset += hunk.new_len as isize - hunk.old_len as isize;
    }
    let mapped = line as isize + offset;
    if mapped > 0 {
        StepMapping::Mapped {
            start: mapped as usize,
            end: mapped as usize,
        }
    } else {
        StepMapping::Ambiguous
    }
}

fn parse_diff_hunks(output: &[u8]) -> Vec<DiffHunk> {
    output
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let body = line.strip_prefix(b"@@ -")?;
            let separator = body.iter().position(|byte| *byte == b' ')?;
            let old = parse_diff_range(&body[..separator])?;
            let new_body = body[separator + 1..].strip_prefix(b"+")?;
            let end = new_body.windows(3).position(|window| window == b" @@")?;
            let new = parse_diff_range(&new_body[..end])?;
            Some(DiffHunk {
                old_start: old.0,
                old_len: old.1,
                new_start: new.0,
                new_len: new.1,
            })
        })
        .collect()
}

fn parse_diff_range(value: &[u8]) -> Option<(usize, usize)> {
    let mut fields = value.splitn(2, |byte| *byte == b',');
    let start = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
    let len = match fields.next() {
        Some(field) => std::str::from_utf8(field).ok()?.parse().ok()?,
        None => 1,
    };
    Some((start, len))
}

fn git_diff_hunks(
    repository: &Path,
    head: Option<&str>,
    staged: bool,
    path: &[u8],
) -> Result<Vec<DiffHunk>> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repository).args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--unified=0",
    ]);
    if staged {
        command.arg("--cached");
    }
    if let Some(head) = head {
        command.arg(head);
    }
    let mut pathspec = b":(literal)".to_vec();
    pathspec.extend_from_slice(path);
    command.arg("--").arg(os_string(pathspec));
    let output = capture(&mut command, MAX_GIT_OUTPUT_BYTES)
        .map_err(|error| WorktreeError::Git(format!("cannot run Git diff: {error}")))?;
    if output.oversized || !output.status.success() {
        return Err(WorktreeError::Git(
            "Git diff for semantic range mapping failed or exceeded its capture limit".to_owned(),
        ));
    }
    Ok(parse_diff_hunks(&output.stdout))
}

fn live_missing_status(repository: &Path, path: &[u8], coverage: &mut Coverage) -> MappingStatus {
    match safe_live_path(repository, path) {
        Ok(Some(_)) => MappingStatus::Ambiguous,
        Ok(None) => {
            coverage.removed_files += 1;
            MappingStatus::Deleted
        }
        Err(LivePathError::Unsupported) => {
            coverage.unsupported_files_excluded += 1;
            MappingStatus::Ambiguous
        }
        Err(LivePathError::Unreadable) => {
            coverage.unreadable_files += 1;
            MappingStatus::Ambiguous
        }
    }
}

enum LayerTextStatus {
    Text(String),
    Missing,
    Unavailable,
}

fn load_layer_text(
    repository: &Path,
    revision: &str,
    path: &[u8],
    coverage: &mut Coverage,
) -> LayerTextStatus {
    match show_path(repository, revision, path) {
        LayerContent::Found(bytes) => match text_content(bytes) {
            Ok(text) => LayerTextStatus::Text(text),
            Err(LayerTextError::Binary) => {
                coverage.binary_files_excluded += 1;
                LayerTextStatus::Unavailable
            }
            Err(LayerTextError::Oversized) => {
                coverage.oversized_files_excluded += 1;
                LayerTextStatus::Unavailable
            }
        },
        LayerContent::Oversized => {
            coverage.oversized_files_excluded += 1;
            LayerTextStatus::Unavailable
        }
        LayerContent::Missing => match git(
            repository,
            &[b"ls-files", b"--unmerged", b"-z", b"--", path],
        ) {
            Ok(output) if output.is_empty() => LayerTextStatus::Missing,
            Ok(_) | Err(_) => {
                coverage.git_layer_failures += 1;
                LayerTextStatus::Unavailable
            }
        },
        LayerContent::Failed => {
            coverage.git_layer_failures += 1;
            LayerTextStatus::Unavailable
        }
    }
}

fn source_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines = text.split('\n').collect::<Vec<_>>();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn line_text(lines: &[&str], start: usize, end: usize) -> Option<String> {
    if start == 0 || end < start || end > lines.len() {
        return None;
    }
    Some(lines[start - 1..end].join("\n"))
}

fn semantic_coverage(repository: &Path, head: &str) -> Result<Coverage> {
    let changed = paths_from_nul(&git(
        repository,
        &[
            b"diff",
            b"--no-renames",
            b"--name-only",
            b"-z",
            head.as_bytes(),
            b"--",
        ],
    )?);
    let untracked = paths_from_nul(&git(
        repository,
        &[b"ls-files", b"--others", b"--exclude-standard", b"-z"],
    )?);
    let ignored = paths_from_nul(&git(
        repository,
        &[
            b"ls-files",
            b"--others",
            b"--ignored",
            b"--exclude-standard",
            b"-z",
        ],
    )?);
    let staged_additions = paths_from_nul(&git(
        repository,
        &[
            b"diff",
            b"--cached",
            b"--no-renames",
            b"--diff-filter=A",
            b"--name-only",
            b"-z",
            head.as_bytes(),
            b"--",
        ],
    )?);
    let mut additions = staged_additions;
    additions.extend(untracked.iter().cloned());
    Ok(Coverage {
        changed_tracked_files: changed.len(),
        untracked_files: untracked.len(),
        ignored_files_excluded: ignored.len(),
        unembedded_addition_files: additions.len(),
        ..Coverage::default()
    })
}

fn provenance(head: &[LineRange], staged: &[LineRange]) -> Vec<Layer> {
    let mut layers = Vec::with_capacity(3);
    if !head.is_empty() {
        layers.push(Layer::Head);
    }
    if !staged.is_empty() {
        layers.push(Layer::Staged);
    }
    layers.push(Layer::Live);
    layers
}

fn find_ranges(text: &str, query: &str) -> (Vec<LineRange>, bool) {
    let text = text.to_lowercase();
    let query = query.to_lowercase();
    if query.is_empty() {
        return (Vec::new(), false);
    }
    let mut ranges = BTreeSet::new();
    let mut truncated = false;
    for (offset, _) in text.match_indices(&query) {
        let end = offset + query.len();
        let start_line = text[..offset].bytes().filter(|byte| *byte == b'\n').count() + 1;
        let end_line = text[..end].bytes().filter(|byte| *byte == b'\n').count() + 1;
        let range = (start_line, end_line);
        if !ranges.contains(&range) && ranges.len() == MAX_MATCHES_PER_FILE {
            truncated = true;
            break;
        }
        ranges.insert(range);
    }
    (
        ranges
            .into_iter()
            .map(|(start, end)| LineRange { start, end })
            .collect(),
        truncated,
    )
}

enum LivePathError {
    Unsupported,
    Unreadable,
}

enum LiveReadError {
    Removed,
    Binary,
    Oversized,
    Unsupported,
    Unreadable,
}

fn read_live(path: &Path) -> std::result::Result<String, LiveReadError> {
    use std::io::Read;

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LiveReadError::Removed
        } else if error.raw_os_error() == Some(libc::ELOOP) {
            LiveReadError::Unsupported
        } else {
            LiveReadError::Unreadable
        }
    })?;
    let metadata = file.metadata().map_err(|_| LiveReadError::Unreadable)?;
    if !metadata.is_file() {
        return Err(LiveReadError::Unsupported);
    }
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(LiveReadError::Oversized);
    }
    let mut text = String::new();
    let mut bounded = file.take(MAX_FILE_BYTES.saturating_add(1) as u64);
    bounded.read_to_string(&mut text).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            LiveReadError::Binary
        } else if error.kind() == std::io::ErrorKind::NotFound {
            LiveReadError::Removed
        } else {
            LiveReadError::Unreadable
        }
    })?;
    if text.len() > MAX_FILE_BYTES {
        return Err(LiveReadError::Oversized);
    }
    if text.as_bytes().contains(&0) {
        return Err(LiveReadError::Binary);
    }
    Ok(text)
}

fn safe_live_path(
    repository: &Path,
    path: &[u8],
) -> std::result::Result<Option<PathBuf>, LivePathError> {
    let components = path.split(|byte| *byte == b'/').collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|part| part.is_empty() || *part == b"." || *part == b"..")
    {
        return Err(LivePathError::Unsupported);
    }
    let mut current = repository.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        current.push(os_string(component.to_vec()));
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(LivePathError::Unreadable),
        };
        if metadata.file_type().is_symlink() {
            return Err(LivePathError::Unsupported);
        }
        let last = index + 1 == components.len();
        if (last && !metadata.file_type().is_file()) || (!last && !metadata.file_type().is_dir()) {
            return Err(LivePathError::Unsupported);
        }
    }
    Ok(Some(current))
}

fn supported_path(path: &[u8]) -> bool {
    let name = path.rsplit(|byte| *byte == b'/').next().unwrap_or(path);
    if name == b"Dockerfile" {
        return true;
    }
    let extension = name
        .iter()
        .rposition(|byte| *byte == b'.')
        .map(|index| &name[index + 1..]);
    if matches!(extension, Some(b"md" | b"markdown" | b"txt" | b"move")) {
        return true;
    }
    crate::semgrep_languages::language_for_extension(extension) != "fallback"
}

enum LayerTextError {
    Binary,
    Oversized,
}

fn text_content(bytes: Vec<u8>) -> std::result::Result<String, LayerTextError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(LayerTextError::Oversized);
    }
    if bytes.contains(&0) {
        return Err(LayerTextError::Binary);
    }
    String::from_utf8(bytes).map_err(|_| LayerTextError::Binary)
}

enum LayerContent {
    Found(Vec<u8>),
    Missing,
    Oversized,
    Failed,
}

fn show_path(repository: &Path, revision: &str, path: &[u8]) -> LayerContent {
    let mut spec = if revision == ":" {
        b":./".to_vec()
    } else {
        let mut prefix = revision.as_bytes().to_vec();
        prefix.extend_from_slice(b":./");
        prefix
    };
    spec.extend_from_slice(path);
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repository)
        .arg("show")
        .arg(os_string(spec));
    match capture(&mut command, MAX_FILE_BYTES) {
        Ok(captured) if captured.oversized => LayerContent::Oversized,
        Ok(captured) if captured.status.success() => LayerContent::Found(captured.stdout),
        Ok(_) => LayerContent::Missing,
        Err(_) => LayerContent::Failed,
    }
}

struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    oversized: bool,
}

fn capture(command: &mut Command, limit: usize) -> std::io::Result<CapturedOutput> {
    use std::io::Read;

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = Vec::new();
    let read_result = child
        .stdout
        .as_mut()
        .expect("piped stdout")
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut stdout);
    if let Err(error) = read_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let oversized = stdout.len() > limit;
    if oversized {
        let _ = child.kill();
    }
    let status = child.wait()?;
    Ok(CapturedOutput {
        status,
        stdout,
        oversized,
    })
}

fn active_head(repository: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .map_err(|error| WorktreeError::Git(format!("cannot inspect active HEAD: {error}")))?;
    if !output.status.success() {
        return Err(WorktreeError::MissingHead);
    }
    String::from_utf8(output.stdout)
        .map(|head| head.trim().to_owned())
        .map_err(|error| WorktreeError::Git(format!("Git returned a non-UTF-8 HEAD: {error}")))
}

fn git(repository: &Path, arguments: &[&[u8]]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repository);
    for argument in arguments {
        command.arg(os_string(argument.to_vec()));
    }
    let output = capture(&mut command, MAX_GIT_OUTPUT_BYTES)
        .map_err(|error| WorktreeError::Git(format!("cannot run Git: {error}")))?;
    if output.oversized {
        return Err(WorktreeError::Git(format!(
            "Git worktree listing exceeded the {MAX_GIT_OUTPUT_BYTES}-byte inspection limit"
        )));
    }
    if !output.status.success() {
        return Err(WorktreeError::Git(format!(
            "Git worktree inspection failed with status {}",
            output.status
        )));
    }
    Ok(output.stdout)
}

fn paths_from_nul(output: &[u8]) -> BTreeSet<Vec<u8>> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

#[cfg(unix)]
fn os_string(bytes: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes)
}

#[cfg(not(unix))]
fn os_string(bytes: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&bytes).into_owned())
}
