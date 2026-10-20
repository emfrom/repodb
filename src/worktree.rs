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
    pub match_limit_reached_files: usize,
    pub total_match_limit_reached: bool,
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
