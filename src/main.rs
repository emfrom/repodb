use repodb::ranking::{
    EvidenceCandidate, EvidenceComponent, RankedEvidence, RankingMethod, SearchMode, TypeSet,
    rank_evidence,
};
use rusqlite::{Connection, ffi::sqlite3_auto_extension, params};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    env,
    error::Error,
    path::PathBuf,
    process::{Command, Stdio},
};

mod budget;
mod config;
mod git_objects;
mod git_ranges;
mod history;
mod imports;
mod process;
mod segmentation;
mod semgrep_languages;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn contains_text_omission(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            [
                "text_omitted_for_size",
                "head_text_omitted_for_size",
                "live_text_omitted_for_size",
            ]
            .iter()
            .any(|field| object.get(*field) == Some(&serde_json::Value::Bool(true)))
                || object.values().any(contains_text_omission)
        }
        serde_json::Value::Array(values) => values.iter().any(contains_text_omission),
        _ => false,
    }
}

fn reference_cli_error(json: bool, reference_id: &str) -> Box<dyn Error> {
    let message = format!("reference ID was not found in this search: {reference_id}");
    if json {
        println!(
            "{}",
            serde_json::json!({
                "error": {
                    "code": "reference_not_found",
                    "message": message,
                }
            })
        );
    }
    std::io::Error::other(message).into()
}

fn pagination_cli_error(json: bool, message: impl Into<String>) -> Box<dyn Error> {
    let message = message.into();
    if json {
        let code = if message.contains("cursor") {
            "invalid_cursor"
        } else {
            "result_limit"
        };
        println!(
            "{}",
            serde_json::json!({
                "error": {
                    "code": code,
                    "message": message,
                }
            })
        );
    }
    std::io::Error::other(message).into()
}

fn worktree_cli_error(
    json: bool,
    code: &str,
    message: impl Into<String>,
    refresh_guidance: bool,
) -> Box<dyn Error> {
    let message = message.into();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "error": {
                    "code": code,
                    "message": message,
                    "refresh_guidance": refresh_guidance.then_some("repodb refresh"),
                }
            })
        );
    }
    std::io::Error::other(message).into()
}

// NUL cannot occur in a Git path, so this internal tag cannot collide with UTF-8 paths.
// Invalid paths are exposed as `bytes:<lowercase-hex>` plus `path_bytes_hex` in JSON.
const ENCODED_GIT_PATH_PREFIX: &str = "\0repodb-git-path:";

fn encode_git_path(path: Vec<u8>) -> String {
    let hex = path
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{ENCODED_GIT_PATH_PREFIX}{hex}")
}

fn decoded_git_path_hex(path: &str) -> Option<&str> {
    path.strip_prefix(ENCODED_GIT_PATH_PREFIX)
}

fn readable_path(path: &str) -> String {
    decoded_git_path_hex(path)
        .map_or_else(|| path.to_owned(), |hex| format!("bytes:{hex} (non-UTF-8)"))
}

#[derive(Clone, Serialize)]
struct SearchResult {
    path: String,
    line_start: usize,
    line_end: usize,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    distance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blob_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_id: Option<String>,
    first_retrieved_at: Option<String>,
    last_retrieved_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    redirect_chain: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree: Option<WorktreeEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic_worktree: Option<SemanticWorktreeEvidence>,
}

#[derive(Clone, Serialize)]
struct SemanticWorktreeEvidence {
    origin: &'static str,
    score_provenance: &'static str,
    indexed_head: String,
    head_line_start: usize,
    head_line_end: usize,
    head_text: String,
    staged_line_start: Option<usize>,
    staged_line_end: Option<usize>,
    live_line_start: Option<usize>,
    live_line_end: Option<usize>,
    mapping_status: repodb::worktree::MappingStatus,
    changed_live_text: Option<bool>,
    #[serde(skip)]
    indexed_exact_match: bool,
}

#[derive(Clone, Serialize)]
struct WorktreeEvidence {
    origin: &'static str,
    indexed_head: String,
    provenance: Vec<repodb::worktree::Layer>,
    head_line_ranges: Vec<repodb::worktree::LineRange>,
    staged_line_ranges: Vec<repodb::worktree::LineRange>,
    head_to_staged_changed: Option<bool>,
    staged_to_live_changed: Option<bool>,
    head_line_mapping_status: &'static str,
    untracked: bool,
}

#[derive(Clone)]
enum EvidenceDisplay {
    Search(Box<SearchResult>),
    Diff(Box<history::DiffSearchResult>),
}

fn component_name(component: EvidenceComponent) -> &'static str {
    match component {
        EvidenceComponent::Content => "content",
        EvidenceComponent::Diff => "diff",
        EvidenceComponent::CommitMessage => "commit_message",
    }
}

fn evidence_path_bytes(path: &str) -> Vec<u8> {
    let Some(hex) = decoded_git_path_hex(path) else {
        return path.as_bytes().to_vec();
    };
    let decoded = hex
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|byte| u8::from_str_radix(byte, 16).ok())
        })
        .collect::<Option<Vec<_>>>();
    decoded.unwrap_or_else(|| path.as_bytes().to_vec())
}

fn search_evidence(
    result: &SearchResult,
    query: &str,
    mode: &str,
    order: usize,
) -> Result<EvidenceCandidate> {
    let component = if result.path == "[commit message]" {
        EvidenceComponent::CommitMessage
    } else {
        EvidenceComponent::Content
    };
    let source_id = if result.worktree.is_some() {
        "live-worktree".to_owned()
    } else if result.semantic_worktree.is_some() {
        "live-semantic-worktree".to_owned()
    } else if let Some(repo_id) = &result.repo_id {
        format!(
            "git-import:{repo_id}:{}",
            result.commit_id.as_deref().unwrap_or("")
        )
    } else if result.commit_id.is_none() && result.blob_id.is_some() {
        format!(
            "document-import:{}:{}:{}:{}",
            result.path,
            result.blob_id.as_deref().unwrap_or(""),
            result.first_retrieved_at.as_deref().unwrap_or(""),
            result.last_retrieved_at.as_deref().unwrap_or(""),
        )
    } else {
        "local".to_owned()
    };
    let reference_id = format!(
        "{order:020}:{}",
        serde_json::to_string(&(
            component,
            &source_id,
            &result.path,
            &result.commit_id,
            &result.blob_id,
            result.page,
            result.line_start,
            result.line_end,
        ))?
    );
    let exact = mode == "exact"
        || result
            .semantic_worktree
            .as_ref()
            .map(|worktree| worktree.indexed_exact_match)
            .unwrap_or_else(|| result.text.to_lowercase().contains(&query.to_lowercase()));
    Ok(EvidenceCandidate {
        component,
        source_id,
        reference_id,
        commit_id: result.commit_id.clone(),
        change_commit_id: (component == EvidenceComponent::CommitMessage)
            .then(|| result.commit_id.clone())
            .flatten(),
        parent_commit_id: None,
        path: (component != EvidenceComponent::CommitMessage)
            .then(|| evidence_path_bytes(&result.path)),
        side: None,
        line_start: Some(result.line_start),
        line_end: Some(result.line_end),
        exact,
        distance: result.distance,
        payload: serde_json::to_value(result)?,
    })
}

fn diff_reference_json(reference: &history::DiffReference) -> serde_json::Value {
    let (old_path, old_path_bytes_hex) = diff_path_fields(reference.old_path.as_deref());
    let (new_path, new_path_bytes_hex) = diff_path_fields(reference.new_path.as_deref());
    let side = match reference.side {
        history::DiffSide::Added => "added",
        history::DiffSide::Removed => "removed",
    };
    let mut value = serde_json::json!({
        "parent_commit_id": reference.parent_commit_id,
        "child_commit_id": reference.child_commit_id,
        "hunk_id": reference.hunk_id,
        "side": side,
        "old_path": old_path,
        "new_path": new_path,
        "old_line_start": reference.old_line_start,
        "old_line_end": reference.old_line_end,
        "new_line_start": reference.new_line_start,
        "new_line_end": reference.new_line_end,
        "blob_id": reference.blob_id,
    });
    if let Some(hex) = old_path_bytes_hex {
        value["old_path_bytes_hex"] = serde_json::Value::String(hex);
    }
    if let Some(hex) = new_path_bytes_hex {
        value["new_path_bytes_hex"] = serde_json::Value::String(hex);
    }
    value
}

#[derive(Clone, Copy, Default)]
enum SearchScope {
    Source,
    RepoLocalSource,
    RepoLocalDocs,
    AllDocs,
    #[default]
    All,
}

impl SearchScope {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "source" => Ok(Self::Source),
            "repo-local-source" => Ok(Self::RepoLocalSource),
            "repo-local-docs" => Ok(Self::RepoLocalDocs),
            "all-docs" => Ok(Self::AllDocs),
            "all" => Ok(Self::All),
            _ => Err(format!("scope must be source, repo-local-source, repo-local-docs, all-docs, or all (got {value})").into()),
        }
    }

    fn matches(self, result: &SearchResult) -> bool {
        let repo_local = result.worktree.is_some()
            || result.semantic_worktree.is_some()
            || (result.commit_id.is_some() && result.repo_id.is_none());
        let path_bytes = (result.worktree.is_some() || result.semantic_worktree.is_some())
            .then(|| evidence_path_bytes(&result.path));
        let is_source = path_bytes.as_deref().map_or_else(
            || source_code_path(&result.path),
            |path| !matches!(segmentation::kind(path), "fallback" | "paragraph"),
        );
        let path_for_docs = path_bytes.as_deref().map_or_else(
            || result.path.clone(),
            |path| String::from_utf8_lossy(path).into_owned(),
        );
        let is_doc = documentation_path(&path_for_docs, result.page.is_some());
        match self {
            Self::Source => is_source && (repo_local || result.repo_id.is_some()),
            Self::RepoLocalSource => repo_local && is_source,
            Self::RepoLocalDocs => repo_local && is_doc,
            Self::AllDocs => is_doc,
            Self::All => true,
        }
    }

    fn matches_diff(self, path: &[u8]) -> bool {
        let is_source = !matches!(segmentation::kind(path), "fallback" | "paragraph");
        let path = String::from_utf8_lossy(path);
        let is_doc = documentation_path(&path, false);
        match self {
            Self::Source | Self::RepoLocalSource => is_source,
            Self::RepoLocalDocs | Self::AllDocs => is_doc,
            Self::All => true,
        }
    }
}

fn source_code_path(path: &str) -> bool {
    !matches!(
        segmentation::kind(path.as_bytes()),
        "fallback" | "paragraph"
    )
}

fn documentation_path(path: &str, is_page: bool) -> bool {
    if is_page {
        return true;
    }
    let clean_path = path.split(['?', '#']).next().unwrap_or(path);
    let name = clean_path.rsplit('/').next().unwrap_or(clean_path);
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "readme"
            | "license"
            | "copying"
            | "notice"
            | "changelog"
            | "changes"
            | "authors"
            | "contributors"
            | "contributing"
            | "code_of_conduct"
            | "security"
            | "support"
    ) {
        return true;
    }
    matches!(
        lower.rsplit_once('.').map(|(_, ext)| ext),
        Some("md" | "markdown" | "txt" | "pdf" | "html" | "htm" | "org")
    )
}

fn diff_path_fields(path: Option<&[u8]>) -> (Option<String>, Option<String>) {
    let Some(path) = path else {
        return (None, None);
    };
    let encoded = match String::from_utf8(path.to_vec()) {
        Ok(path) => path,
        Err(error) => encode_git_path(error.into_bytes()),
    };
    (
        Some(readable_path(&encoded)),
        decoded_git_path_hex(&encoded).map(str::to_owned),
    )
}

fn posix_shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn git_show_command(commit_id: &str, path: &[u8]) -> String {
    let printable_path = std::str::from_utf8(path).ok().filter(|path| {
        path.bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    });
    if let Some(path) = printable_path {
        return format!(
            "git show {}",
            posix_shell_quote(&format!("{commit_id}:{path}"))
        );
    }

    // Encode unusual path bytes as POSIX printf octal escapes. A sentinel keeps
    // command substitution from stripping trailing newlines in the decoded path.
    let path_bytes = path
        .iter()
        .map(|byte| format!("\\0{byte:03o}"))
        .collect::<String>();
    format!(
        "_repodb_path=$(printf '%b' '{path_bytes}'; printf .); _repodb_path=${{_repodb_path%.}}; git show {}\"$_repodb_path\"",
        posix_shell_quote(&format!("{commit_id}:"))
    )
}

fn diff_change_kind_label(kind: history::DiffChangeKind) -> &'static str {
    match kind {
        history::DiffChangeKind::Added => "added",
        history::DiffChangeKind::Removed => "removed",
        history::DiffChangeKind::Modified => "modified",
        history::DiffChangeKind::HeuristicRename => "heuristic_rename",
        history::DiffChangeKind::HeuristicCopy => "heuristic_copy",
        history::DiffChangeKind::TypeChanged => "type_changed",
    }
}

#[derive(Default)]
struct DiffMetadataCache {
    schema_version: Option<i64>,
    by_parent_child: HashMap<(String, String), history::DiffMetadata>,
}

fn diff_file_change_json(
    c: &Connection,
    reference: &history::DiffReference,
    cache: &mut DiffMetadataCache,
) -> Result<serde_json::Value> {
    let schema_version = if let Some(version) = cache.schema_version {
        version
    } else {
        let version: i64 = c.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        cache.schema_version = Some(version);
        version
    };
    if schema_version < 5 {
        return Ok(serde_json::json!({"available": false, "refresh_required": true}));
    }
    let key = (
        reference.parent_commit_id.clone(),
        reference.child_commit_id.clone(),
    );
    if !cache.by_parent_child.contains_key(&key) {
        let metadata = history::diff_metadata(c, Some(&key.0), Some(&key.1))?;
        cache.by_parent_child.insert(key.clone(), metadata);
    }
    let metadata = cache.by_parent_child.get(&key).unwrap();
    let change = metadata.changes.iter().find(|change| {
        change.old_path == reference.old_path && change.new_path == reference.new_path
    });
    let (kind, heuristic, similarity_percent) = change.map_or((None, false, None), |change| {
        let heuristic = matches!(
            change.kind,
            history::DiffChangeKind::HeuristicRename | history::DiffChangeKind::HeuristicCopy
        );
        (
            Some(diff_change_kind_label(change.kind)),
            heuristic,
            change.similarity_percent,
        )
    });
    Ok(serde_json::json!({
        "available": change.is_some(),
        "kind": kind,
        "heuristic": heuristic,
        "similarity_percent": similarity_percent,
        "detection_settings": {
            "rename_threshold_percent": metadata.settings.rename_threshold_percent,
            "copy_threshold_percent": metadata.settings.copy_threshold_percent,
            "find_copies_harder": metadata.settings.find_copies_harder,
            "git_version": metadata.settings.git_version,
            "diff_algorithm": metadata.settings.diff_algorithm,
        }
    }))
}

fn diff_search_json(
    c: &Connection,
    hit: &history::DiffSearchResult,
    cache: &mut DiffMetadataCache,
) -> Result<serde_json::Value> {
    let reference = &hit.reference;
    let parent_git_show_command = reference
        .old_path
        .as_deref()
        .map(|path| git_show_command(&reference.parent_commit_id, path));
    let child_git_show_command = reference
        .new_path
        .as_deref()
        .map(|path| git_show_command(&reference.child_commit_id, path));
    let file_change = diff_file_change_json(c, reference, cache)?;
    let (old_path, old_path_bytes_hex) = diff_path_fields(reference.old_path.as_deref());
    let (new_path, new_path_bytes_hex) = diff_path_fields(reference.new_path.as_deref());
    let side = match reference.side {
        history::DiffSide::Added => "added",
        history::DiffSide::Removed => "removed",
    };
    let mut value = serde_json::json!({
        "kind": "diff",
        "text": hit.text,
        "distance": hit.distance,
        "parent_commit_id": reference.parent_commit_id,
        "child_commit_id": reference.child_commit_id,
        "hunk_id": reference.hunk_id,
        "side": side,
        "old_path": old_path,
        "new_path": new_path,
        "old_line_start": reference.old_line_start,
        "old_line_end": reference.old_line_end,
        "new_line_start": reference.new_line_start,
        "new_line_end": reference.new_line_end,
        "matching_line_start": reference.line_start,
        "matching_line_end": reference.line_end,
        "blob_id": reference.blob_id,
        "parent_git_show_command": parent_git_show_command,
        "child_git_show_command": child_git_show_command,
        "file_change": file_change,
    });
    if let Some(hex) = old_path_bytes_hex {
        value["old_path_bytes_hex"] = serde_json::Value::String(hex);
    }
    if let Some(hex) = new_path_bytes_hex {
        value["new_path_bytes_hex"] = serde_json::Value::String(hex);
    }
    Ok(value)
}

fn annotated_ranked_payload(
    candidate: &EvidenceCandidate,
    group: &RankedEvidence,
    group_id: usize,
    history_path_links: &HashMap<(String, String), Vec<history::HeuristicPathLink>>,
) -> serde_json::Value {
    let mut value = candidate.payload.clone();
    if !value.is_object() {
        value = serde_json::json!({"reference": value});
    }
    value["reference_id"] = serde_json::Value::String(candidate.reference_id.clone());
    value["component"] = serde_json::Value::String(component_name(candidate.component).to_owned());
    value["primary_component"] =
        serde_json::Value::String(component_name(group.primary).to_owned());
    value["matching_component"] =
        serde_json::Value::String(component_name(candidate.component).to_owned());
    let mut matched_components = group
        .components
        .iter()
        .map(|item| component_name(item.component))
        .collect::<Vec<_>>();
    matched_components.sort_unstable();
    matched_components.dedup();
    value["matched_components"] = serde_json::Value::Array(
        matched_components
            .iter()
            .map(|component| serde_json::Value::String((*component).to_owned()))
            .collect(),
    );
    value["ranking_method"] = serde_json::Value::String(
        match group.ranking_method {
            RankingMethod::SemanticDistance => "semantic_distance",
            RankingMethod::Deterministic => "deterministic_fallback",
        }
        .to_owned(),
    );
    value["exact_match"] = serde_json::Value::Bool(group.exact_match);
    value["group_id"] = serde_json::Value::Number(group_id.into());
    if let Some(score_source) = group.score_source {
        value["score_source"] = serde_json::Value::String(component_name(score_source).to_owned());
    }
    if let Some(distance) = group.distance {
        value["distance"] = serde_json::json!(distance);
        value["distance_qualifier"] =
            serde_json::Value::String("cosine_embedding_distance".to_owned());
    } else {
        value.as_object_mut().unwrap().remove("distance");
        value.as_object_mut().unwrap().remove("distance_qualifier");
    }
    if candidate.component == EvidenceComponent::CommitMessage {
        value["kind"] = serde_json::Value::String("commit_message".to_owned());
        if let Some(commit) = &candidate.commit_id {
            value["source_reference"] =
                serde_json::Value::String(format!("git show --no-patch --format=%B {commit}"));
        }
        value["related_diffs"] = serde_json::Value::Array(group.related_diffs.clone());
    } else if candidate.component == EvidenceComponent::Content {
        value["kind"] = serde_json::Value::String("content".to_owned());
        if let Some(commit_id) = candidate.commit_id.as_ref()
            && let Some(path) = candidate.payload["path"].as_str()
            && let Some(links) = history_path_links.get(&(commit_id.clone(), path.to_owned()))
        {
            value["history_path_links"] =
                serde_json::Value::Array(links.iter().map(history_path_link_json).collect());
        }
        if let Some(path) = candidate.payload["path"].as_str()
            && let Some(hex) = decoded_git_path_hex(path)
        {
            value["path"] = serde_json::Value::String(format!("bytes:{hex}"));
            value["path_bytes_hex"] = serde_json::Value::String(hex.to_owned());
        }
    }
    value
}

fn ranked_human_prefix(
    group: &RankedEvidence,
    group_id: usize,
    component: EvidenceComponent,
) -> String {
    let matched = group
        .components
        .iter()
        .map(|item| component_name(item.component))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let ranking = match group.ranking_method {
        RankingMethod::SemanticDistance => format!(
            "semantic_distance score_component={} distance={} (cosine embedding distance)",
            component_name(group.score_source.unwrap()),
            group.distance.unwrap(),
        ),
        RankingMethod::Deterministic => "deterministic_fallback".to_owned(),
    };
    format!(
        "[group {group_id} primary={} matching={} matched={matched} ranking={ranking}] ",
        component_name(group.primary),
        component_name(component),
    )
}

fn worktree_warnings(report: &repodb::worktree::WorktreeSearch) -> Vec<String> {
    let mut warnings = Vec::new();
    if report.head_changed_during_scan == Some(true) {
        warnings.push("HEAD changed during inspection; the live view is best-effort".to_owned());
    } else if report.head_changed_during_scan.is_none() {
        warnings.push("could not re-observe HEAD after inspection".to_owned());
    }
    for (count, subject) in [
        (
            report.coverage.ignored_files_excluded,
            "ignored files excluded",
        ),
        (
            report.coverage.unsupported_files_excluded,
            "unsupported files excluded",
        ),
        (
            report.coverage.binary_files_excluded,
            "binary files excluded",
        ),
        (report.coverage.unreadable_files, "unreadable files skipped"),
        (
            report.coverage.oversized_files_excluded,
            "oversized files excluded",
        ),
        (
            report.coverage.git_layer_failures,
            "Git layer reads unavailable",
        ),
        (
            report.coverage.match_limit_reached_files,
            "files hit the per-file result limit",
        ),
    ] {
        if count > 0 {
            warnings.push(format!("{count} {subject}"));
        }
    }
    if report.coverage.total_match_limit_reached {
        warnings.push("overall live-result limit reached".to_owned());
    }
    warnings
}

fn worktree_report_json(report: &repodb::worktree::WorktreeSearch) -> serde_json::Value {
    serde_json::json!({
        "enabled": true,
        "indexed_head": report.indexed_head,
        "observed_head": report.observed_head,
        "head_changed_during_scan": report.head_changed_during_scan,
        "observed_at_unix_ms": report.observed_at_unix_ms,
        "updated_path_count": report.updated_paths.len(),
        "coverage": report.coverage,
        "warnings": worktree_warnings(report),
    })
}

fn semantic_worktree_warnings(report: &repodb::worktree::SemanticMappingReport) -> Vec<String> {
    let mut warnings = Vec::new();
    if report.head_changed_during_scan == Some(true) {
        warnings.push(
            "HEAD changed during semantic mapping; live locations are best-effort".to_owned(),
        );
    } else if report.head_changed_during_scan.is_none() {
        warnings.push("could not re-observe HEAD after semantic mapping".to_owned());
    }
    if report.coverage.unembedded_addition_files > 0 {
        warnings.push(format!(
            "{} staged or untracked addition files have no semantic coverage",
            report.coverage.unembedded_addition_files
        ));
    }
    for (count, subject) in [
        (
            report.coverage.ignored_files_excluded,
            "ignored files excluded",
        ),
        (
            report.coverage.unsupported_files_excluded,
            "unsupported files excluded",
        ),
        (
            report.coverage.binary_files_excluded,
            "binary files excluded",
        ),
        (report.coverage.unreadable_files, "unreadable files skipped"),
        (
            report.coverage.oversized_files_excluded,
            "oversized files excluded",
        ),
        (
            report.coverage.git_layer_failures,
            "Git layer reads unavailable",
        ),
    ] {
        if count > 0 {
            warnings.push(format!("{count} {subject}"));
        }
    }
    warnings
}

fn semantic_worktree_report_json(
    report: &repodb::worktree::SemanticMappingReport,
) -> serde_json::Value {
    let count_status = |status| {
        report
            .mappings
            .iter()
            .filter(|mapping| mapping.mapping_status == status)
            .count()
    };
    serde_json::json!({
        "enabled": true,
        "mode": "semantic",
        "indexed_head": report.indexed_head,
        "observed_head": report.observed_head,
        "head_changed_during_scan": report.head_changed_during_scan,
        "observed_at_unix_ms": report.observed_at_unix_ms,
        "mapping_counts": {
            "mapped": count_status(repodb::worktree::MappingStatus::Mapped),
            "deleted": count_status(repodb::worktree::MappingStatus::Deleted),
            "ambiguous": count_status(repodb::worktree::MappingStatus::Ambiguous),
        },
        "coverage": report.coverage,
        "warnings": semantic_worktree_warnings(report),
    })
}

fn print_semantic_worktree_summary(report: &repodb::worktree::SemanticMappingReport) {
    let observed = report.observed_head.as_deref().unwrap_or("unavailable");
    let mapped = report
        .mappings
        .iter()
        .filter(|mapping| mapping.mapping_status == repodb::worktree::MappingStatus::Mapped)
        .count();
    let deleted = report
        .mappings
        .iter()
        .filter(|mapping| mapping.mapping_status == repodb::worktree::MappingStatus::Deleted)
        .count();
    let ambiguous = report
        .mappings
        .iter()
        .filter(|mapping| mapping.mapping_status == repodb::worktree::MappingStatus::Ambiguous)
        .count();
    println!(
        "Semantic worktree view: indexed HEAD {}; observed HEAD {}; best-effort observation at {} ms",
        report.indexed_head, observed, report.observed_at_unix_ms
    );
    println!(
        "Semantic mappings: {mapped} mapped, {deleted} deleted, {ambiguous} ambiguous; {} staged or untracked addition files without semantic coverage",
        report.coverage.unembedded_addition_files
    );
    for warning in semantic_worktree_warnings(report) {
        println!("Semantic worktree warning: {warning}");
    }
}

fn print_worktree_summary(report: &repodb::worktree::WorktreeSearch) {
    let observed = report.observed_head.as_deref().unwrap_or("unavailable");
    println!(
        "Worktree view: indexed HEAD {}; observed HEAD {}; best-effort observation at {} ms",
        report.indexed_head, observed, report.observed_at_unix_ms
    );
    println!(
        "Worktree coverage: {} changed tracked, {} untracked, {} ignored, {} unsupported, {} binary, {} removed, {} unreadable",
        report.coverage.changed_tracked_files,
        report.coverage.untracked_files,
        report.coverage.ignored_files_excluded,
        report.coverage.unsupported_files_excluded,
        report.coverage.binary_files_excluded,
        report.coverage.removed_files,
        report.coverage.unreadable_files,
    );
    for warning in worktree_warnings(report) {
        println!("Worktree warning: {warning}");
    }
}

fn print_ranked_human(
    ranked: &[RankedEvidence],
    displays: &HashMap<String, EvidenceDisplay>,
    c: &Connection,
    history_path_links: &HashMap<(String, String), Vec<history::HeuristicPathLink>>,
    cache: &mut DiffMetadataCache,
    page: &repodb::pagination::SearchPage,
    explore: bool,
) -> Result<()> {
    for (group_id, group) in ranked.iter().enumerate() {
        for candidate in &group.components {
            let Some(page_result) = page.results.iter().find(|result| {
                result["reference_id"].as_str() == Some(candidate.reference_id.as_str())
            }) else {
                continue;
            };
            let text_omitted = explore || contains_text_omission(page_result);
            let prefix = format!(
                "{}[reference_id={}] ",
                ranked_human_prefix(group, group_id, candidate.component),
                candidate.reference_id
            );
            match displays.get(&candidate.reference_id) {
                Some(EvidenceDisplay::Search(x)) => {
                    let mut reference = String::new();
                    if let Some(worktree) = x.worktree.as_ref() {
                        let layers = worktree
                            .provenance
                            .iter()
                            .map(|layer| format!("{layer:?}").to_lowercase())
                            .collect::<Vec<_>>()
                            .join(",");
                        reference.push_str(&format!(
                            " [live worktree indexed HEAD {} provenance={layers}]",
                            worktree.indexed_head
                        ));
                    } else if let Some(worktree) = x.semantic_worktree.as_ref() {
                        let mapping = format!("{:?}", worktree.mapping_status).to_lowercase();
                        reference.push_str(&format!(
                            " [semantic live mapping={mapping} indexed HEAD {} score_provenance={}] [matched HEAD lines {}-{}: {}]",
                            worktree.indexed_head,
                            worktree.score_provenance,
                            worktree.head_line_start,
                            worktree.head_line_end,
                            if text_omitted { "[text omitted]" } else { &worktree.head_text },
                        ));
                        if worktree.changed_live_text == Some(true) {
                            reference.push_str(" [live text differs from indexed HEAD]");
                        }
                    } else if let Some(commit) = x.commit_id.as_deref() {
                        reference.push_str(&format!(" [commit {commit}]"));
                        if let Some(links) =
                            history_path_links.get(&(commit.to_owned(), x.path.clone()))
                        {
                            for link in links {
                                let similarity = link.similarity_percent.map_or_else(
                                    || "similarity estimate unavailable".to_owned(),
                                    |score| format!("similarity estimate {score}%"),
                                );
                                reference.push_str(&format!(
                                    " [heuristic rename link {}:{} -> {}:{}; {similarity}]",
                                    link.parent_commit_id,
                                    diff_path_display(&link.old_path).escape_debug(),
                                    link.child_commit_id,
                                    diff_path_display(&link.new_path).escape_debug(),
                                ));
                            }
                        }
                    }
                    if let Some(blob) = x.blob_id.as_deref() {
                        reference.push_str(&format!(" [blob {blob}]"));
                    }
                    if let Some(repo_id) = x.repo_id.as_deref() {
                        reference.push_str(&format!(" [repo {repo_id}]"));
                    }
                    if let Some(first) = x.first_retrieved_at.as_deref() {
                        reference.push_str(&format!(" [first retrieved {first}]"));
                    }
                    if let Some(last) = x.last_retrieved_at.as_deref() {
                        reference.push_str(&format!(" [last retrieved {last}]"));
                    }
                    if let Some(requested_url) = x.requested_url.as_deref() {
                        reference.push_str(&format!(" [requested URL {requested_url}]"));
                    }
                    if let Some(final_url) = x.final_url.as_deref() {
                        reference.push_str(&format!(" [final URL {final_url}]"));
                    }
                    if let Some(redirect_chain) = x.redirect_chain.as_ref()
                        && !redirect_chain.is_empty()
                    {
                        reference.push_str(&format!(" [redirect chain {redirect_chain:?}]"));
                    }
                    if x.fetch_status.as_deref() == Some("stale") {
                        reference.push_str(" [stale website snapshot]");
                        if let Some(error) = x.fetch_error.as_deref() {
                            reference.push_str(&format!(" [last fetch failed: {error}]"));
                        }
                    }
                    if candidate.component == EvidenceComponent::CommitMessage {
                        if let Some(commit) = x.commit_id.as_deref() {
                            reference.push_str(&format!(
                                " [source: git show --no-patch --format=%B {commit}]"
                            ));
                        }
                        if !group.related_diffs.is_empty() {
                            reference.push_str(&format!(
                                " [related parent diff references: {}]",
                                group.related_diffs.len()
                            ));
                        }
                    }
                    let path = readable_path(&x.path);
                    let distance = group.distance.map_or_else(String::new, |distance| {
                        let basis = x
                            .semantic_worktree
                            .as_ref()
                            .map(|_| "; similarity to indexed HEAD text")
                            .unwrap_or("");
                        format!(" [cosine embedding distance {distance}{basis}]")
                    });
                    let text = if explore {
                        "[reference only]"
                    } else if text_omitted {
                        "[text omitted for size]"
                    } else {
                        &x.text
                    };
                    if let Some(page) = x.page {
                        println!("{prefix}{path}:page {page}{reference}{distance}: {text}");
                    } else {
                        println!(
                            "{prefix}{path}:{}-{}{reference}{distance}: {text}",
                            x.line_start, x.line_end
                        );
                    }
                }
                Some(EvidenceDisplay::Diff(hit)) => {
                    let reference = &hit.reference;
                    let (side, matching) = match reference.side {
                        history::DiffSide::Added => (
                            "added",
                            diff_endpoint(
                                reference.new_path.as_deref(),
                                Some(reference.line_start),
                                Some(reference.line_end),
                            ),
                        ),
                        history::DiffSide::Removed => (
                            "removed",
                            diff_endpoint(
                                reference.old_path.as_deref(),
                                Some(reference.line_start),
                                Some(reference.line_end),
                            ),
                        ),
                    };
                    let old = diff_endpoint(
                        reference.old_path.as_deref(),
                        reference.old_line_start,
                        reference.old_line_end,
                    );
                    let new = diff_endpoint(
                        reference.new_path.as_deref(),
                        reference.new_line_start,
                        reference.new_line_end,
                    );
                    let file_change = diff_file_change_json(c, reference, cache)?;
                    let change_kind = file_change["kind"].as_str().unwrap_or("unavailable");
                    let heuristic = file_change["heuristic"].as_bool().unwrap_or(false);
                    let similarity = file_change["similarity_percent"]
                        .as_u64()
                        .map(|score| format!("{score}%"))
                        .unwrap_or_else(|| "n/a".to_owned());
                    let settings = &file_change["detection_settings"];
                    let detection = if file_change["refresh_required"] == true {
                        "refresh required".to_owned()
                    } else {
                        format!(
                            "rename={}%,copy={}%,copies-harder={},algorithm={},git={}",
                            settings["rename_threshold_percent"],
                            settings["copy_threshold_percent"],
                            settings["find_copies_harder"],
                            settings["diff_algorithm"],
                            settings["git_version"],
                        )
                    };
                    let parent_command = reference
                        .old_path
                        .as_deref()
                        .map(|path| git_show_command(&reference.parent_commit_id, path))
                        .unwrap_or_else(|| "<absent>".to_owned());
                    let child_command = reference
                        .new_path
                        .as_deref()
                        .map(|path| git_show_command(&reference.child_commit_id, path))
                        .unwrap_or_else(|| "<absent>".to_owned());
                    let distance = group.distance.map_or_else(String::new, |distance| {
                        format!(" [cosine embedding distance {distance}]")
                    });
                    let text = if explore {
                        "[reference only]"
                    } else if text_omitted {
                        "[text omitted for size]"
                    } else {
                        &hit.text
                    };
                    println!(
                        "{prefix}diff {side} match={matching} [parent {} child {} hunk {}] old={old} new={new} file_change={change_kind} heuristic={heuristic} similarity={similarity} detection={detection} parent-file-command={parent_command} child-file-command={child_command}{distance}: {text}",
                        reference.parent_commit_id, reference.child_commit_id, reference.hunk_id,
                    );
                }
                None => return Err("ranked evidence lost its display reference".into()),
            }
        }
    }
    Ok(())
}

fn diff_path_display(path: &[u8]) -> String {
    diff_path_fields(Some(path)).0.unwrap_or_default()
}

fn internal_git_path(path: &[u8]) -> String {
    match String::from_utf8(path.to_vec()) {
        Ok(path) => path,
        Err(error) => encode_git_path(error.into_bytes()),
    }
}

fn history_path_link_json(link: &history::HeuristicPathLink) -> serde_json::Value {
    let (old_path, old_path_bytes_hex) = diff_path_fields(Some(&link.old_path));
    let (new_path, new_path_bytes_hex) = diff_path_fields(Some(&link.new_path));
    let mut value = serde_json::json!({
        "heuristic": true,
        "parent_commit_id": link.parent_commit_id,
        "child_commit_id": link.child_commit_id,
        "old_path": old_path,
        "new_path": new_path,
        "similarity_percent": link.similarity_percent,
    });
    if let Some(hex) = old_path_bytes_hex {
        value["old_path_bytes_hex"] = serde_json::Value::String(hex);
    }
    if let Some(hex) = new_path_bytes_hex {
        value["new_path_bytes_hex"] = serde_json::Value::String(hex);
    }
    value
}

fn parse_history_selector(value: &str) -> Result<history::HistorySelector> {
    match value {
        "off" => Ok(history::HistorySelector::Head),
        "all" => Ok(history::HistorySelector::All),
        "" => Err("--history= requires off, all, a path, or a positive commit count".into()),
        _ if value.bytes().all(|byte| byte.is_ascii_digit()) => {
            let limit: usize = value.parse()?;
            if limit == 0 {
                return Err("--history commit count must be greater than zero".into());
            }
            Ok(history::HistorySelector::Topological(limit))
        }
        _ => Ok(history::HistorySelector::Path(value.as_bytes().to_vec())),
    }
}

fn diff_endpoint(path: Option<&[u8]>, start: Option<usize>, end: Option<usize>) -> String {
    let Some(path) = path else {
        return "<absent>".to_owned();
    };
    let path = diff_path_display(path).escape_debug().to_string();
    match (start, end) {
        (Some(start), Some(end)) => format!("{path}:{start}-{end}"),
        _ => format!("{path}:<no lines>"),
    }
}

fn git(args: &[&str]) -> Result<Vec<u8>> {
    let output = process::capture(
        &mut Command::new("git")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
        256 * 1024 * 1024,
        64 * 1024,
    )?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output.stdout)
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
fn selected_database(option: Option<String>) -> Result<PathBuf> {
    match option {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            Ok(if path.is_absolute() {
                path
            } else {
                env::current_dir()?.join(path)
            })
        }
        Some(_) => Err("--database requires a nonempty path".into()),
        None => database(),
    }
}
fn open_db(path: &std::path::Path) -> Result<Connection> {
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
    let c = Connection::open(path)?;
    if !history::is_current(&c)? {
        c.execute_batch("CREATE TABLE IF NOT EXISTS indexed_files(path TEXT PRIMARY KEY NOT NULL,commit_id TEXT NOT NULL,content TEXT NOT NULL); CREATE TABLE IF NOT EXISTS embeddings(path TEXT NOT NULL,line INTEGER NOT NULL,text TEXT NOT NULL,commit_id TEXT NOT NULL,vector BLOB NOT NULL,PRIMARY KEY(path,line)); CREATE TABLE IF NOT EXISTS embedding_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    }
    Ok(c)
}
#[derive(Serialize)]
struct CapabilityReadiness {
    ready: bool,
    reason: Option<String>,
}

fn capability(ready: bool, reason: Option<String>) -> CapabilityReadiness {
    CapabilityReadiness { ready, reason }
}

fn status(db_path: &std::path::Path) -> Result<serde_json::Value> {
    let database_exists = db_path.is_file();
    let indexed_head = if database_exists {
        let c = Connection::open(db_path)?;
        let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if !(0..=5).contains(&version) {
            return Err(format!("unsupported database version {version}").into());
        }
        if version >= 2 {
            c.query_row(
                "SELECT commit_id FROM git_commits ORDER BY timestamp DESC, commit_id DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
        } else {
            c.query_row(
                "SELECT commit_id FROM indexed_files ORDER BY commit_id LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
        }
    } else {
        None
    };
    let live_head = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|head| head.trim().to_owned());
    let porcelain = git(&["status", "--porcelain", "--untracked-files=normal"])?;
    let dirty = !porcelain.is_empty();
    let (mut staged_changes, mut worktree_changes, mut untracked_files) = (false, false, false);
    for line in porcelain
        .split(|byte| *byte == b'\n')
        .filter(|line| line.len() >= 2)
    {
        staged_changes |= line[0] != b' ' && line[0] != b'?';
        worktree_changes |= line[1] != b' ' && line[0] != b'?';
        untracked_files |= line[0..2] == *b"??";
    }
    let staged_coverage = if staged_changes {
        "present-unindexed"
    } else {
        "clean"
    };
    let worktree_coverage = if worktree_changes {
        "present-unindexed"
    } else {
        "clean"
    };
    let untracked_coverage = if untracked_files {
        "present-unindexed"
    } else {
        "clean"
    };
    let (unit_count, unsupported_count) = if database_exists {
        let c = Connection::open(db_path)?;
        let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version >= 2 {
            let units = c
                .query_row("SELECT count(*) FROM indexed_units", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_or(0);
            let unsupported = c
                .query_row(
                    "SELECT count(*) FROM blob_contents WHERE status != 'text'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0);
            (units, unsupported)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };
    let content_state = if !database_exists {
        "missing"
    } else if unit_count == 0 && unsupported_count > 0 {
        "unsupported"
    } else if unit_count == 0 {
        "empty"
    } else {
        "usable"
    };
    let state = if content_state == "usable"
        && indexed_head
            .as_deref()
            .is_some_and(|head| Some(head) != live_head.as_deref())
    {
        "stale"
    } else {
        content_state
    };
    let exact = database_exists && state == "usable";
    let embedder = config::embedding_command()
        .ok()
        .and_then(|command| resolved_executable(&command));
    let semantic_ready = exact && embedder.is_some();
    let pdftotext = resolved_executable("pdftotext").is_some();
    let unavailable_reason = if !database_exists {
        "database is missing"
    } else if state == "unsupported" {
        "index contains only unsupported content"
    } else {
        "index has no searchable units"
    };
    let readiness = serde_json::json!({
        "exact": capability(exact, (!exact).then(|| unavailable_reason.to_owned())),
        "semantic": capability(semantic_ready, (!semantic_ready).then(|| if !exact { unavailable_reason.to_owned() } else { "embedding command is missing".to_owned() })),
        "refresh": capability(embedder.is_some(), embedder.is_none().then(|| "embedding command is missing".to_owned())),
        "document_import": capability(pdftotext, (!pdftotext).then(|| "pdftotext is missing".to_owned()))
    });
    Ok(serde_json::json!({
        "database": { "path": db_path, "state": state, "content_state": content_state },
        "indexed_head": indexed_head,
        "live_head": live_head,
        "worktree": { "dirty": dirty, "staged_coverage": staged_coverage, "worktree_coverage": worktree_coverage, "untracked_coverage": untracked_coverage },
        "indexed_units": { "total": unit_count, "unsupported": unsupported_count },
        "capabilities": readiness
    }))
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

pub(crate) fn is_builtin_embedding_command(command: &str) -> bool {
    let Ok(home) = env::var("HOME") else {
        return false;
    };
    let builtin = PathBuf::from(home).join("bin/generate_embeddings");
    matches!((builtin.canonicalize().ok(), resolved_executable(command)), (Some(a), Some(b)) if a == b)
}

fn embed(command: &str, model: &str, text: &str) -> Result<Vec<f32>> {
    embed_with_output_limit(command, model, text, None)
}

pub(crate) fn embed_with_output_limit(
    command: &str,
    model: &str,
    text: &str,
    output_limit: Option<usize>,
) -> Result<Vec<f32>> {
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
    // Some embedders return no row for empty/whitespace input. Preserve a
    // searchable vector for every indexed unit with a stable sentinel instead.
    let input = if is_builtin_embedding_command(command) && text.trim().is_empty() {
        "<empty RepoDB unit>"
    } else {
        text
    };
    let output = match output_limit {
        Some(limit) => process::capture_with_input(&mut child, input.as_bytes(), limit, 64 * 1024)?,
        None => process::capture_with_input_unbounded(&mut child, input.as_bytes())?,
    };
    if output.exceeded {
        let limit = output_limit.expect("bounded capture has a limit");
        return Err(format!(
            "embedding command output could not be captured completely (limit {limit})"
        )
        .into());
    }
    if !output.status.success() {
        return Err("embedding command failed".into());
    }
    let v: Vec<f32> = serde_json::from_slice(&output.stdout)?;
    validate_vector(&v)?;
    Ok(v)
}
pub(crate) fn embed_many_with_output_limit(
    command: &str,
    model: &str,
    texts: &[&str],
    output_limit: usize,
) -> Result<Vec<Vec<f32>>> {
    if !is_builtin_embedding_command(command)
        || texts.len() < 2
        || texts.iter().any(|text| text.contains('\0'))
    {
        return texts
            .iter()
            .map(|text| embed_with_output_limit(command, model, text, Some(output_limit)))
            .collect();
    }
    if model != config::DEFAULT_EMBEDDING_MODEL {
        return Err(
            "the built-in generate_embeddings command only supports all-MiniLM-L6-v2".into(),
        );
    }
    let mut input = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        if index != 0 {
            input.push(0);
        }
        input.extend_from_slice(if text.trim().is_empty() {
            b"<empty RepoDB unit>"
        } else {
            text.as_bytes()
        });
    }
    const MAX_BATCH_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
    let output_limit = output_limit.min(MAX_BATCH_OUTPUT_BYTES);
    let mut child = Command::new(command)
        .env("REPODB_EMBEDDING_MODEL", model)
        .args(["--json", "-0", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut vectors = Vec::with_capacity(texts.len());
    let output =
        process::capture_lines_with_input(&mut child, input, output_limit, 64 * 1024, |line| {
            if line.iter().all(u8::is_ascii_whitespace) {
                return Ok(());
            }
            let vector: Vec<f32> = serde_json::from_slice(line).map_err(std::io::Error::other)?;
            validate_vector(&vector).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
            })?;
            vectors.push(vector);
            Ok(())
        })?;
    if output.exceeded {
        return Err(format!(
            "embedding command output exceeded the {output_limit}-byte capture limit"
        )
        .into());
    }
    if !output.status.success() {
        return Err(format!(
            "embedding command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    if vectors.len() != texts.len() {
        return Err("embedding command returned the wrong number of vectors".into());
    }
    Ok(vectors)
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
fn as_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn reembed(c: &mut Connection, command: &str, model: &str) -> Result<()> {
    let version = history::snapshot_version(c)?;
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
    let tx = history::publish_transaction(c, version)?;
    tx.execute("DELETE FROM embeddings", [])?;
    {
        let mut st = tx.prepare("INSERT INTO embeddings VALUES(?1,?2,?3,?4,?5)")?;
        for (p, i, t, id, v) in rows {
            st.execute(params![p, i, t, id, as_bytes(&v)])?;
        }
    }
    if let Some(import_dim) = imports::reembed(&tx, command, model)? {
        if dim.is_some_and(|d| d != import_dim) {
            return Err("embedding dimension mismatch".into());
        }
        dim = Some(import_dim);
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
fn exact(c: &Connection, q: &str, commits: &[String]) -> Result<Vec<SearchResult>> {
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
        if !commits.contains(&id) {
            continue;
        }
        for (i, l) in content.lines().enumerate() {
            if l.to_lowercase().contains(&q.to_lowercase()) {
                out.push(SearchResult {
                    path: p.clone(),
                    line_start: i + 1,
                    line_end: i + 1,
                    text: l.to_owned(),
                    commit_id: Some(id.clone()),
                    distance: None,
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
        .map_err(|_| "no embeddings; run refresh")?;
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
    // Zero records an empty index whose model dimension is not established yet.
    if dim != 0 && v.len() as i64 != dim {
        return Err("query embedding dimension mismatch".into());
    }
    Ok(v)
}
fn semantic_with_vector(
    c: &Connection,
    v: &[f32],
    limit: usize,
    commits: &[String],
    include: impl Fn(&SearchResult) -> bool,
) -> Result<Vec<SearchResult>> {
    let mut s=c.prepare("SELECT path,line,text,commit_id,vec_distance_cosine(vector,?1) d FROM embeddings WHERE commit_id IN (SELECT value FROM json_each(?2)) ORDER BY d,path,line")?;
    let rows = s.query_map(params![as_bytes(v), serde_json::to_string(commits)?], |r| {
        Ok(SearchResult {
            path: r.get(0)?,
            line_start: r.get(1)?,
            line_end: r.get(1)?,
            text: r.get(2)?,
            commit_id: Some(r.get(3)?),
            distance: Some(r.get(4)?),
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
        })
    })?;
    let mut results = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    results.retain(|result| include(result));
    results.truncate(limit);
    Ok(results)
}
fn semantic(
    c: &Connection,
    q: &str,
    limit: usize,
    commits: &[String],
    include: impl Fn(&SearchResult) -> bool,
) -> Result<Vec<SearchResult>> {
    semantic_with_vector(c, &query_vector(c, q)?, limit, commits, include)
}
const GENERAL_HELP: &str = r#"RepoDB — local repository and source search

Usage:
  repodb --version
  repodb [--database PATH] <COMMAND> [OPTIONS]
  repodb [--database PATH] help [COMMAND]

Commands:
  init                 Initialize the index and capture the checked-out repository
  refresh              Refresh indexed repository and registered imports
  import SOURCE        Register one local file, URL page, or Git repository
  retention latest|N   Set imported-source version retention (default: latest)
  redirect-limit N     Set the maximum followed URL redirects (default: 5; max: 20)
  reembed              Rebuild vectors with an embedding command/model
  search QUERY         Search indexed content, diffs, and commit messages
  status [--json]      Report index freshness and capability readiness

Global options:
  --database PATH      Use a separate database (default: <git-dir>/repodb.sqlite)
  --version            Print the application version and exit
  -h, --help           Show this help or help for a command

Search defaults to checked-out HEAD and combined matching. Coarse options include:
  --history=off|all|N|PATH   Select the committed view (default: off; bare --history means all)
  --exact | --semantic | --combined  Choose exact-only, semantic-only, or combined matching (default: combined)
  --type and --scope          Select evidence components and source origins
  --worktree                 Opt into live inspection at the active indexed HEAD
  --current-worktree         Compatibility name; remains HEAD-only
  --explore                  Page through all eligible references without text
  --limit N                  Results per page (default: 5; 50 with --explore)
  --byte-budget N            Serialized response budget (default: 5000)
  --cursor CURSOR            Continue a snapshot; --fetch-reference ID retrieves omitted text
See `repodb search --help` for full option details.

Refresh size policy is `--size-limit=error|warn|ignore` (default: error) and
applies to local/URL document imports, not Git content. See `repodb refresh --help`.

Imports are explicit and single-source: a URL import fetches only that page and
never crawls linked pages. Search never fetches or refreshes; refreshing
registered URLs is explicit. See `repodb import --help` and `repodb refresh --help`.

Examples:
  repodb init
  repodb search 'literal text' --json
  repodb search 'literal text' --exact --json
  repodb search 'why is this implemented?' --semantic --history=all
  repodb search 'literal text' --explore --history=all --json
  repodb import --preview https://example.com/page
  repodb status --json
"#;

const SEARCH_HELP: &str = r#"Search indexed content, historical diffs, commit messages, and imported sources.

Usage:
  repodb [--database PATH] search QUERY [OPTIONS]

Matching and evidence (default: combined):
  --exact                    Exact-only matching
  --semantic                 Semantic-only matching by embedding similarity
  --combined                 Combine semantic candidates with exact matches
  --semantic-limit N         Semantic candidate limit (default: 20)
  --type SET                 all|content|cmdiffs|diffs|cmcommits|commits (default: all)
                             all=content+diffs+messages; content=content+diffs;
                             cmdiffs=messages+diffs; cmcommits=messages+content;
                             diffs=diffs only; commits=messages only
  --scope SCOPE              source|repo-local-source|repo-local-docs|all-docs|all
                             (default: all; source includes imported Git sources;
                             repo-local-* restricts to this repository; all-docs
                             includes indexed docs from all origins)

History selection:
  --history=off|all|N|PATH   Select HEAD, all indexed history, N topological commits,
                             or one path and its detected rename history (default: off)
  --history                  Alias for --history=all
  --commit ID                Starting commit for history traversal
  --include-commit ID        Add one exact commit ID
  --exclude-commit ID        Exclude one exact commit ID (exclusion wins)
  --all-history              Compatibility alias for --history=all
  --current-worktree         Compatibility name for HEAD-only; it does not inspect
                             staged or live file contents

Worktree and result budgets:
  --worktree                 Opt into best-effort exact/semantic inspection against
                             the active indexed HEAD; no uncommitted embeddings
  --explore                  Enumerate every eligible occurrence as references only
  --limit N                  Results per page (default: 5, or 50 with --explore)
  --byte-budget N            Maximum serialized response bytes (default: 5000)
  --cursor CURSOR            Continue the same result snapshot
  --fetch-reference ID       Fetch one full result by its returned reference_id;
                             repeat the same query/options and raise byte budget if needed
  --json                     Emit machine-readable results and pagination metadata
  -h, --help                 Show this help

A complete untruncated ordinary JSON response retains the legacy results-array shape.
Paginated, explore, and worktree responses include results, snapshot_id, cursor,
truncated, total_count when known, and worktree coverage when applicable. Text is
never cut mid-result: an oversized result returns its reference with
text_omitted_for_size; use --fetch-reference ID to retrieve it separately. If a
minimal reference cannot fit, search returns a result_limit error.

The default history view is HEAD. Matching defaults to combined exact and
semantic evidence; `--exact` and `--semantic` select one mode explicitly, while
`--combined` may be used to state the default. `--history=all` searches all indexed
history; `--history=N` uses deterministic topological order. `--current-worktree`
remains HEAD-only, while `--worktree` is opt-in and rejects stale/alternate HEADs. Semantic
worktree similarity is based on indexed HEAD text; live text is not embedded.
Search does not refresh, fetch URLs, or crawl links.

Examples:
  repodb search 'literal text' --history=all --json
  repodb search 'why does this retry?' --semantic --scope source
  repodb search 'literal text' --explore --history=all --limit 50 --json
  repodb search 'literal text' --json --cursor SNAPSHOT:OFFSET
"#;

const REFRESH_HELP: &str = r#"Initialize or refresh the local index.

Usage:
  repodb init [--size-limit=error|warn|ignore]
  repodb refresh [--imports | --changed | --update COMMIT]
                 [--size-limit=error|warn|ignore]

`init` creates the index and captures the checked-out repository plus registered imports.
`refresh` performs a full refresh. `--imports` refreshes registered external sources
only; `--changed` indexes commits since the previous snapshot; `--update COMMIT`
adds the selected commit. `--changed` and `--update` cannot be combined with
--imports or --size-limit.

`--size-limit` applies to local and URL document imports during `init`, a full
`refresh`, or `refresh --imports`: `error` (default) aborts oversized documents,
`warn` continues and reports them, and `ignore` skips the policy. `--changed` and
`--update` do not accept this policy. Git repositories and Git history are not
size-gated. `import SOURCE` only registers a source; refreshing it is explicit.
Search never refreshes or fetches.

Examples:
  repodb init --size-limit=warn
  repodb refresh --size-limit=warn
  repodb refresh --imports --size-limit=ignore
  repodb refresh --changed
  repodb refresh --update COMMIT_ID
"#;

const IMPORT_HELP: &str = r#"Register one source, previewing it without side effects, or refresh registered sources.

Usage:
  repodb [--database PATH] import SOURCE
  repodb [--database PATH] import --revision REF GIT_REPOSITORY
  repodb [--database PATH] import --preview FILE_OR_DIRECTORY_OR_URL

SOURCE is one local file, one HTTP(S) page URL, or one Git repository. A URL
import fetches only the selected page; it does not crawl links. Search never
fetches. `repodb refresh` or `repodb refresh --imports` explicitly refreshes
registered URLs and other imports.

`--preview` reports local source selection or fetch size/warnings without registering
or indexing; it accepts one local file/directory or HTTP(S) page URL, not a Git repo.
For directory previews, register each selected local file separately.
`--revision` pins a Git import to one resolved branch, tag, or commit. Size policy is
selected at refresh time with `--size-limit`, not at registration.

Examples:
  repodb import ./docs/guide.md
  repodb import https://example.com/guide
  repodb import --preview https://example.com/guide
  repodb import --revision v1.2.0 https://example.com/project.git
  repodb refresh --imports
"#;

const STATUS_HELP: &str = r#"Report database, indexed/live HEAD, freshness, and readiness by capability.

Usage:
  repodb [--database PATH] status [--json]

Status does not initialize, refresh, fetch, or mutate the index. JSON distinguishes
missing database/index state from an empty searchable snapshot.
"#;

const RETENTION_HELP: &str = r#"Choose a bounded history for imported source versions.

Usage:
  repodb [--database PATH] retention latest|N

`latest` is the default and keeps only the newest distinct version per source. A
number from 2 to 100 keeps that many newest distinct versions per source; `all`
is a compatibility alias for 2. Setting a lower limit prunes excess versions and
reports the count. This applies to imported documents, URLs, and pinned Git
snapshots, not native repository history. It does not fetch sources; use an
explicit refresh.
"#;

const REDIRECT_LIMIT_HELP: &str = r#"Set the finite maximum number of HTTP(S) redirects followed when fetching URLs.

Usage:
  repodb [--database PATH] redirect-limit N

The default is 5 and the hard maximum is 20. A limit of 0 disables redirects.
This affects explicit URL refreshes and does not crawl page links.
"#;

const REEMBED_HELP: &str = r#"Rebuild stored vectors using the configured or supplied embedding command/model.

Usage:
  repodb [--database PATH] reembed [--command PATH] [--model ID]

Reembedding does not fetch or refresh external sources.
"#;

fn print_help(command: Option<&str>) -> Result<()> {
    let help = match command {
        None => GENERAL_HELP,
        Some("search") => SEARCH_HELP,
        Some("init" | "refresh") => REFRESH_HELP,
        Some("import") => IMPORT_HELP,
        Some("status") => STATUS_HELP,
        Some("retention") => RETENTION_HELP,
        Some("redirect-limit") => REDIRECT_LIMIT_HELP,
        Some("reembed") => REEMBED_HELP,
        Some(command) => return Err(format!("unknown help topic: {command}").into()),
    };
    print!("{help}");
    Ok(())
}

fn execute() -> Result<()> {
    let mut arguments = env::args().skip(1).collect::<Vec<_>>();
    let version_requested = match arguments.as_slice() {
        [flag] => flag == "--version",
        [database_flag, _, version_flag] => {
            database_flag == "--database" && version_flag == "--version"
        }
        _ => false,
    };
    if version_requested {
        println!("repodb {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if arguments.is_empty() {
        return print_help(None);
    }
    if matches!(arguments[0].as_str(), "--help" | "-h") {
        if arguments.len() > 2 {
            return Err("usage: repodb --help [COMMAND]".into());
        }
        return print_help(arguments.get(1).map(String::as_str));
    }
    let option = if arguments
        .first()
        .is_some_and(|argument| argument == "--database")
    {
        if arguments.len() < 2 {
            return Err("--database requires a path".into());
        }
        let path = arguments.remove(1);
        if path.is_empty() {
            return Err("--database requires a path".into());
        }
        arguments.remove(0);
        Some(path)
    } else {
        None
    };
    if arguments.is_empty() {
        return print_help(None);
    }
    let cmd = arguments.remove(0);
    if cmd == "help" {
        if arguments.len() > 1 {
            return Err("usage: repodb help [COMMAND]".into());
        }
        return print_help(arguments.first().map(String::as_str));
    }
    if matches!(cmd.as_str(), "--help" | "-h") {
        if arguments.len() > 1 {
            return Err("usage: repodb [--database PATH] --help [COMMAND]".into());
        }
        return print_help(arguments.first().map(String::as_str));
    }
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        return print_help(Some(&cmd));
    }
    let mut a = arguments.into_iter();
    let db_path = selected_database(option)?;
    match cmd.as_str() {
        "status" => {
            let json = match a.next().as_deref() {
                None => false,
                Some("--json") if a.next().is_none() => true,
                _ => return Err("usage: repodb status [--json]".into()),
            };
            let report = status(&db_path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                let database = &report["database"];
                let state = database["state"].as_str().unwrap_or("unknown");
                println!("Database: {state}");
                println!(
                    "Indexed HEAD: {}",
                    report["indexed_head"].as_str().unwrap_or("none")
                );
                println!(
                    "Live HEAD: {}",
                    report["live_head"].as_str().unwrap_or("unavailable")
                );
                println!("Worktree dirty: {}", report["worktree"]["dirty"]);
                for capability in ["exact", "semantic", "refresh", "document_import"] {
                    let entry = &report["capabilities"][capability];
                    let name = match capability {
                        "exact" => "Exact search",
                        "semantic" => "Semantic search",
                        "refresh" => "Refresh",
                        _ => "Document import",
                    };
                    println!(
                        "{name}: {}{}",
                        if entry["ready"] == true {
                            "ready"
                        } else {
                            "not ready"
                        },
                        entry["reason"]
                            .as_str()
                            .map(|reason| format!(" ({reason})"))
                            .unwrap_or_default()
                    );
                }
            }
        }
        "init" | "refresh" => {
            let mut arguments = Vec::new();
            let mut size_limit = imports::SizeLimit::Error;
            let mut size_limit_supplied = false;
            for argument in a {
                if let Some(value) = argument.strip_prefix("--size-limit=") {
                    if size_limit_supplied {
                        return Err("--size-limit may be specified only once".into());
                    }
                    size_limit_supplied = true;
                    size_limit = match value {
                        "error" => imports::SizeLimit::Error,
                        "warn" => imports::SizeLimit::Warn,
                        "ignore" => imports::SizeLimit::Ignore,
                        _ => return Err("--size-limit must be error, warn, or ignore".into()),
                    };
                } else {
                    arguments.push(argument);
                }
            }
            let imports_only = arguments.len() == 1 && arguments[0] == "--imports";
            let changed = arguments.len() == 1 && arguments[0] == "--changed";
            let update = if arguments.len() == 2 && arguments[0] == "--update" {
                Some(history::resolve_update_commit(&arguments[1])?)
            } else {
                None
            };
            if (!arguments.is_empty() && !imports_only && !changed && update.is_none())
                || (cmd == "init" && (imports_only || changed || update.is_some()))
                || (changed && update.is_some())
                || (imports_only && (changed || update.is_some()))
                || (size_limit_supplied && (changed || update.is_some()))
            {
                return Err(
                    "usage: repodb refresh [--imports | --changed | --update COMMIT] [--size-limit=error|warn|ignore]".into(),
                );
            }
            let mut db = open_db(&db_path)?;
            let mut size_assessments = Vec::new();
            let excluded_database_path = if imports_only {
                None
            } else {
                history::database_git_path(&db_path)?
            };
            if imports_only {
                let version = history::snapshot_version(&db)?;
                size_assessments = imports::refresh_with_size_limit(&db, size_limit)?;
                imports::validate_dimensions(&db)?;
                let tx = history::publish_transaction(&mut db, version)?;
                imports::publish(&tx)?;
                tx.commit()?;
            } else if changed {
                history::refresh_changed_excluding(&mut db, excluded_database_path.as_deref())?;
            } else if let Some(commit_id) = update {
                history::refresh_update_excluding(
                    &mut db,
                    &commit_id,
                    excluded_database_path.as_deref(),
                )?;
            } else {
                let version = history::snapshot_version(&db)?;
                size_assessments = imports::refresh_with_size_limit(&db, size_limit)?;
                history::prepare_refresh_excluding(&db, excluded_database_path.as_deref())?;
                imports::validate_dimensions(&db)?;
                let tx = history::publish_transaction(&mut db, version)?;
                imports::publish(&tx)?;
                history::publish_refresh(&tx)?;
                tx.commit()?;
            }
            for pruned in imports::pruning_report(&db)? {
                println!(
                    "pruned {} historical version(s) for {}",
                    pruned.count, pruned.source
                );
            }
            for assessment in size_assessments
                .into_iter()
                .filter(|assessment| assessment.warn_large)
            {
                println!(
                    "large import warning: source bytes={}, estimated net database growth={}, warning basis={} ({})",
                    assessment.source_bytes,
                    assessment
                        .estimated_net_db_growth_bytes
                        .map_or_else(|| "unknown".into(), |value| value.to_string()),
                    assessment.warning_basis_bytes,
                    assessment.warning_basis,
                );
            }
        }
        "import" => {
            let first = a
                .next()
                .ok_or("import requires one local file, URL, or Git repository")?;
            if first == "--preview" {
                let path = a.next().ok_or(
                    "import --preview requires a local file, directory, or HTTP(S) page URL",
                )?;
                if a.next().is_some() {
                    return Err(
                        "import --preview accepts one local file, directory, or HTTP(S) page URL"
                            .into(),
                    );
                }
                let preview = if path.starts_with("http://") || path.starts_with("https://") {
                    if path.ends_with(".git") {
                        return Err("import --preview does not preview Git repositories".into());
                    }
                    imports::preview_url(&path, &db_path)?
                } else {
                    imports::preview(std::path::Path::new(&path), &db_path)?
                };
                println!("{}", serde_json::to_string(&preview)?);
            } else {
                let (revision, locator) = if first == "--revision" {
                    let revision = a
                        .next()
                        .ok_or("import --revision requires a branch, tag, or commit")?;
                    let locator = a
                        .next()
                        .ok_or("import --revision requires one Git repository")?;
                    (Some(revision), locator)
                } else {
                    (None, first)
                };
                if a.next().is_some() {
                    return Err("import accepts one file, URL, or Git repository".into());
                }
                imports::register(&open_db(&db_path)?, &locator, revision.as_deref())?;
            }
        }
        "retention" => {
            let value = a
                .next()
                .ok_or("retention requires latest or a version limit")?;
            if a.next().is_some() {
                return Err("unexpected arguments".into());
            }
            let pruned = imports::set_retention(&open_db(&db_path)?, &value)?;
            if pruned > 0 {
                println!("pruned {pruned} stored imported version(s)");
            }
        }
        "redirect-limit" => {
            let value: usize = a
                .next()
                .ok_or("redirect-limit requires a nonnegative integer")?
                .parse()?;
            if a.next().is_some() {
                return Err("unexpected arguments".into());
            }
            imports::set_url_max_redirects(&open_db(&db_path)?, value)?;
            println!("URL redirect limit set to {value}");
        }
        "reembed" => {
            let mut command = config::embedding_command()?;
            let mut model = config::DEFAULT_EMBEDDING_MODEL.to_owned();
            while let Some(x) = a.next() {
                match x.as_str() {
                    "--command" => command = a.next().ok_or("--command requires path")?,
                    "--model" => model = a.next().ok_or("--model requires id")?,
                    _ => return Err(format!("unexpected argument {x}").into()),
                }
            }
            let mut c = open_db(&db_path)?;
            if history::is_current(&c)? {
                history::reembed(&mut c, &command, &model)?;
            } else {
                reembed(&mut c, &command, &model)?;
            }
        }
        "search" => {
            let mut query = None;
            let mut scope = history::Scope::default();
            let mut history_selector = None;
            let mut result_scope = SearchScope::default();
            let mut result_scope_set = false;
            let mut selected_types = TypeSet::All;
            let mut type_set_selected = false;
            let mut diff_flag = false;
            let mut worktree_flag = false;
            let mut explore = false;
            let mut result_limit_set = false;
            let mut result_limit = 5usize;
            let mut byte_budget = 5_000usize;
            let mut cursor = None;
            let mut fetch_reference = None;
            let (mut json, mut mode, mut semantic_limit) = (false, "combined", 20usize);
            while let Some(x) = a.next() {
                match x.as_str() {
                    "--json" => json = true,
                    "--scope" => {
                        if result_scope_set {
                            return Err("--scope may only be specified once".into());
                        }
                        result_scope =
                            SearchScope::parse(&a.next().ok_or("--scope requires a value")?)?;
                        result_scope_set = true;
                    }
                    "--history" => {
                        if history_selector.is_some() {
                            return Err("--history may only be specified once".into());
                        }
                        history_selector = Some(history::HistorySelector::All);
                    }
                    "--all-history" => scope.all_history = true,
                    "--current-worktree" => scope.current_worktree = true,
                    "--worktree" => worktree_flag = true,
                    "--explore" => explore = true,
                    "--limit" | "--result-limit" => {
                        result_limit = a.next().ok_or("--limit requires a value")?.parse()?;
                        result_limit_set = true;
                    }
                    "--byte-budget" => {
                        byte_budget = a.next().ok_or("--byte-budget requires a value")?.parse()?;
                    }
                    "--cursor" => cursor = Some(a.next().ok_or("--cursor requires a value")?),
                    "--fetch-reference" => {
                        fetch_reference = Some(
                            a.next()
                                .ok_or("--fetch-reference requires a reference ID")?,
                        );
                    }
                    "--commit" => {
                        if scope.commit.is_some() {
                            return Err("--commit may only be specified once".into());
                        }
                        scope.commit = Some(a.next().ok_or("--commit requires ID")?);
                    }
                    _ if x.starts_with("--commit=") => {
                        if scope.commit.is_some() {
                            return Err("--commit may only be specified once".into());
                        }
                        let commit = &x[9..];
                        if commit.is_empty() {
                            return Err("--commit= requires ID".into());
                        }
                        scope.commit = Some(commit.to_owned());
                    }
                    "--include-commit" => scope
                        .include
                        .push(a.next().ok_or("--include-commit requires ID")?),
                    "--exclude-commit" => scope
                        .exclude
                        .push(a.next().ok_or("--exclude-commit requires ID")?),
                    _ if x.starts_with("--history=") => {
                        if history_selector.is_some() {
                            return Err("--history may only be specified once".into());
                        }
                        history_selector = Some(parse_history_selector(&x[10..])?);
                    }
                    "--exact" => mode = "exact",
                    "--semantic" => mode = "semantic",
                    "--combined" => mode = "combined",
                    "--diffs" => diff_flag = true,
                    "--type" => {
                        if type_set_selected {
                            return Err("--type may only be specified once".into());
                        }
                        selected_types = TypeSet::parse(
                            &a.next().ok_or("--type requires a value")?,
                        )
                        .map_err(|error| {
                            std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
                        })?;
                        type_set_selected = true;
                    }
                    _ if x.starts_with("--type=") => {
                        if type_set_selected {
                            return Err("--type may only be specified once".into());
                        }
                        selected_types = TypeSet::parse(&x[7..]).map_err(|error| {
                            std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
                        })?;
                        type_set_selected = true;
                    }
                    "--semantic-limit" => {
                        semantic_limit = a.next().ok_or("missing limit")?.parse()?;
                    }
                    _ if x.starts_with('-') => {
                        return Err(format!("unexpected argument {x}").into());
                    }
                    _ if query.is_none() => query = Some(x),
                    _ => return Err(format!("unexpected argument {x}").into()),
                }
            }
            if explore && fetch_reference.is_some() {
                return Err("--explore cannot be combined with --fetch-reference".into());
            }
            if fetch_reference.is_some() && cursor.is_some() {
                return Err("--fetch-reference cannot be combined with --cursor".into());
            }
            if explore && !result_limit_set {
                result_limit = 50;
            } else if fetch_reference.is_some() && !result_limit_set {
                result_limit = 1;
            }
            let q = query.ok_or("search requires query")?;
            if worktree_flag {
                if !selected_types
                    .components()
                    .contains(&EvidenceComponent::Content)
                {
                    return Err(worktree_cli_error(
                        json,
                        "content_type_required",
                        "--worktree requires a type set that includes content",
                        false,
                    ));
                }
                if scope.all_history
                    || scope.current_worktree
                    || scope.commit.is_some()
                    || !scope.include.is_empty()
                    || !scope.exclude.is_empty()
                    || history_selector
                        .as_ref()
                        .is_some_and(|selector| *selector != history::HistorySelector::Head)
                {
                    return Err(worktree_cli_error(
                        json,
                        "alternate_head",
                        "--worktree requires the active HEAD; alternate history selectors are not supported",
                        false,
                    ));
                }
            }
            if mode == "semantic" && semantic_limit == 0 && !explore {
                let page = repodb::pagination::page_results(
                    &[],
                    result_limit,
                    byte_budget.saturating_sub(1),
                    cursor.as_deref(),
                )
                .map_err(|error| pagination_cli_error(json, error))?;
                if json {
                    println!("{}", serde_json::to_string(&page)?);
                }
                return Ok(());
            }
            let mut connection = open_db(&db_path)?;
            // Scope, model configuration and results belong to one read snapshot.
            let c = connection.transaction()?;
            let (commits, history_path_filter, history_path_links) = if let Some(selector) =
                history_selector
            {
                if scope.all_history || scope.current_worktree {
                    return Err("--history conflicts with legacy --all-history and --current-worktree; use one selector".into());
                }
                let start_commit = match (&selector, scope.commit.as_ref()) {
                    (
                        history::HistorySelector::Head
                        | history::HistorySelector::Topological(_)
                        | history::HistorySelector::Path(_),
                        commit,
                    ) => commit.cloned(),
                    (_, Some(_)) => {
                        return Err("--commit is a starting commit only with --history=off, --history=<path>, or --history=<n>; it conflicts with --history=all".into());
                    }
                    _ => None,
                };
                let path_history = matches!(&selector, history::HistorySelector::Path(_));
                let entries = history::select_history(
                    &c,
                    &history::HistoryQuery {
                        selector,
                        start_commit,
                        include_commits: scope.include.clone(),
                        exclude_commits: scope.exclude.clone(),
                    },
                )
                .map_err(|error| {
                    if worktree_flag {
                        worktree_cli_error(
                            json,
                            "stale_or_unindexed_head",
                            format!("--worktree requires the active indexed HEAD: {error}"),
                            true,
                        )
                    } else {
                        std::io::Error::other(error.to_string()).into()
                    }
                })?;
                let mut commits = Vec::new();
                let mut seen_commits = HashSet::new();
                let mut path_filter = path_history.then(HashSet::new);
                let mut path_links = HashMap::new();
                for entry in entries {
                    if seen_commits.insert(entry.commit_id.clone()) {
                        commits.push(entry.commit_id.clone());
                    }
                    if let Some(path) = entry.path {
                        let path = internal_git_path(&path);
                        if let Some(paths) = &mut path_filter {
                            paths.insert((entry.commit_id.clone(), path.clone()));
                        }
                        if !entry.heuristic_links.is_empty() {
                            path_links.insert((entry.commit_id, path), entry.heuristic_links);
                        }
                    }
                }
                (commits, path_filter, path_links)
            } else {
                let commits = scope.resolve(&c).map_err(|error| {
                    if worktree_flag {
                        worktree_cli_error(
                            json,
                            "stale_or_unindexed_head",
                            format!("--worktree requires the active indexed HEAD: {error}"),
                            true,
                        )
                    } else {
                        std::io::Error::other(error.to_string()).into()
                    }
                })?;
                (commits, None, HashMap::new())
            };
            let worktree_report = if worktree_flag && mode != "semantic" {
                Some(
                    repodb::worktree::search_exact(
                        &env::current_dir()?,
                        commits.first().map(String::as_str),
                        &q,
                    )
                    .map_err(|error| {
                        let (code, refresh) = match error {
                            repodb::worktree::WorktreeError::MissingHead => ("missing_head", false),
                            repodb::worktree::WorktreeError::StaleHead { .. }
                            | repodb::worktree::WorktreeError::UnindexedHead { .. } => {
                                ("stale_or_unindexed_head", true)
                            }
                            _ => ("worktree_inspection_failed", false),
                        };
                        worktree_cli_error(json, code, error.to_string(), refresh)
                    })?,
                )
            } else {
                None
            };
            if worktree_flag && mode == "semantic" {
                repodb::worktree::validate_indexed_head(
                    &env::current_dir()?,
                    commits.first().map(String::as_str),
                )
                .map_err(|error| {
                    let (code, refresh) = match &error {
                        repodb::worktree::WorktreeError::MissingHead => ("missing_head", false),
                        repodb::worktree::WorktreeError::StaleHead { .. }
                        | repodb::worktree::WorktreeError::UnindexedHead { .. } => {
                            ("stale_or_unindexed_head", true)
                        }
                        _ => ("worktree_inspection_failed", false),
                    };
                    worktree_cli_error(json, code, error.to_string(), refresh)
                })?;
            }
            let mut semantic_worktree_report = None;
            let matches_history_path = |candidate: &SearchResult| {
                let Some(paths) = &history_path_filter else {
                    return true;
                };
                let Some(commit_id) = candidate.commit_id.as_ref() else {
                    return true;
                };
                if candidate.repo_id.is_some() {
                    return true;
                }
                paths.contains(&(commit_id.clone(), candidate.path.clone()))
            };
            let include_result = |candidate: &SearchResult| {
                result_scope.matches(candidate) && matches_history_path(candidate)
            };
            let wants_content = selected_types
                .components()
                .contains(&EvidenceComponent::Content);
            let mut result = if !wants_content {
                Vec::new()
            } else if history::is_current(&c)? {
                if explore {
                    history::search_all_occurrences(
                        &c,
                        &q,
                        mode,
                        usize::MAX,
                        &commits,
                        include_result,
                    )?
                } else {
                    history::search(&c, &q, mode, semantic_limit, &commits, include_result)?
                }
            } else if mode == "exact" {
                exact(&c, &q, &commits)?
            } else if mode == "combined" {
                let v = query_vector(&c, &q)?;
                let mut candidates = semantic_with_vector(
                    &c,
                    &v,
                    if explore { usize::MAX } else { semantic_limit },
                    &commits,
                    include_result,
                )?;
                let mut seen = std::collections::HashSet::new();
                for x in &candidates {
                    seen.insert((x.path.clone(), x.line_start));
                }
                for mut x in exact(&c, &q, &commits)? {
                    if include_result(&x) && seen.insert((x.path.clone(), x.line_start)) {
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
                semantic(
                    &c,
                    &q,
                    if explore { usize::MAX } else { semantic_limit },
                    &commits,
                    include_result,
                )?
            };
            if wants_content {
                result.extend(imports::search(
                    &c,
                    &q,
                    mode,
                    if explore { usize::MAX } else { semantic_limit },
                    |result| result_scope.matches(result),
                )?);
            }
            result.retain(|candidate| include_result(candidate));
            if let Some(report) = worktree_report.as_ref() {
                let updated_paths = report.updated_paths.iter().cloned().collect::<HashSet<_>>();
                result.retain(|candidate| {
                    let is_indexed_local_content = candidate.repo_id.is_none()
                        && candidate.commit_id.as_deref() == Some(&report.indexed_head)
                        && candidate.path != "[commit message]";
                    !is_indexed_local_content
                        || mode != "exact"
                        || !updated_paths.contains(&evidence_path_bytes(&candidate.path))
                });
                for found in &report.matches {
                    let path = match String::from_utf8(found.path.clone()) {
                        Ok(path) => path,
                        Err(error) => encode_git_path(error.into_bytes()),
                    };
                    let candidate = SearchResult {
                        path,
                        line_start: found.line_start,
                        line_end: found.line_end,
                        text: found.text.clone(),
                        commit_id: None,
                        distance: None,
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
                        worktree: Some(WorktreeEvidence {
                            origin: "live_worktree",
                            indexed_head: report.indexed_head.clone(),
                            provenance: found.provenance.clone(),
                            head_line_ranges: found.head_line_ranges.clone(),
                            staged_line_ranges: found.staged_line_ranges.clone(),
                            head_to_staged_changed: found.head_to_staged_changed,
                            staged_to_live_changed: found.staged_to_live_changed,
                            head_line_mapping_status: "same_path_query_occurrences_unmapped",
                            untracked: found.untracked,
                        }),
                        semantic_worktree: None,
                    };
                    if include_result(&candidate) {
                        result.push(candidate);
                    }
                }
            }
            result.retain(|candidate| include_result(candidate));
            if worktree_flag && mode != "exact" {
                let indexed_head = commits.first().map(String::as_str).ok_or_else(|| {
                    worktree_cli_error(
                        json,
                        "stale_or_unindexed_head",
                        "--worktree requires an indexed active HEAD",
                        true,
                    )
                })?;
                let result_indices = result
                    .iter()
                    .enumerate()
                    .filter_map(|(index, candidate)| {
                        (candidate.worktree.is_none()
                            && candidate.repo_id.is_none()
                            && candidate.commit_id.as_deref() == Some(indexed_head)
                            && candidate.path != "[commit message]")
                            .then_some(index)
                    })
                    .collect::<Vec<_>>();
                let ranges = result_indices
                    .iter()
                    .map(|index| {
                        let candidate = &result[*index];
                        repodb::worktree::IndexedRange {
                            path: evidence_path_bytes(&candidate.path),
                            line_start: candidate.line_start,
                            line_end: candidate.line_end,
                        }
                    })
                    .collect::<Vec<_>>();
                let mapping_report = repodb::worktree::map_semantic_ranges(
                    &env::current_dir()?,
                    Some(indexed_head),
                    &ranges,
                )
                .map_err(|error| {
                    let (code, refresh) = match error {
                        repodb::worktree::WorktreeError::MissingHead => ("missing_head", false),
                        repodb::worktree::WorktreeError::StaleHead { .. }
                        | repodb::worktree::WorktreeError::UnindexedHead { .. } => {
                            ("stale_or_unindexed_head", true)
                        }
                        _ => ("worktree_inspection_failed", false),
                    };
                    worktree_cli_error(json, code, error.to_string(), refresh)
                })?;
                for (index, mapped) in result_indices.into_iter().zip(&mapping_report.mappings) {
                    let candidate = &mut result[index];
                    let indexed_exact_match =
                        candidate.text.to_lowercase().contains(&q.to_lowercase());
                    candidate.semantic_worktree = Some(SemanticWorktreeEvidence {
                        origin: "indexed_head_semantic",
                        score_provenance: "similarity_to_indexed_head_text",
                        indexed_head: indexed_head.to_owned(),
                        head_line_start: mapped.head_line_start,
                        head_line_end: mapped.head_line_end,
                        head_text: mapped.head_text.clone(),
                        staged_line_start: mapped.staged_line_start,
                        staged_line_end: mapped.staged_line_end,
                        live_line_start: mapped.live_line_start,
                        live_line_end: mapped.live_line_end,
                        mapping_status: mapped.mapping_status,
                        changed_live_text: mapped.changed_live_text,
                        indexed_exact_match,
                    });
                    if mapped.mapping_status == repodb::worktree::MappingStatus::Mapped {
                        candidate.line_start = mapped.live_line_start.ok_or_else(|| {
                            "mapped semantic range omitted its live line start".to_owned()
                        })?;
                        candidate.line_end = mapped.live_line_end.ok_or_else(|| {
                            "mapped semantic range omitted its live line end".to_owned()
                        })?;
                        candidate.text = mapped
                            .live_text
                            .clone()
                            .ok_or_else(|| "mapped semantic range omitted live text".to_owned())?;
                    } else {
                        candidate.line_start = mapped.head_line_start;
                        candidate.line_end = mapped.head_line_end;
                        candidate.text = mapped.head_text.clone();
                    }
                }
                semantic_worktree_report = Some(mapping_report);
            }
            let has_commit_message_index: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='commit_message_units')",
                [],
                |row| row.get(0),
            )?;
            if selected_types
                .components()
                .contains(&EvidenceComponent::CommitMessage)
                && history::is_current(&c)?
                && has_commit_message_index
                && history_path_filter.is_none()
            {
                result.extend(history::search_commit_messages(
                    &c,
                    &q,
                    mode,
                    if explore { usize::MAX } else { semantic_limit },
                    &commits,
                )?);
            }
            result.retain(|candidate| include_result(candidate));
            if mode == "semantic" || mode == "combined" {
                result.sort_by(|a, b| {
                    a.distance
                        .unwrap_or(f64::INFINITY)
                        .total_cmp(&b.distance.unwrap_or(f64::INFINITY))
                        .then((a.path == "[commit message]").cmp(&(b.path == "[commit message]")))
                        .then(a.path.cmp(&b.path))
                        .then(a.line_start.cmp(&b.line_start))
                });
                if mode == "semantic" {
                    if !explore {
                        result.truncate(semantic_limit);
                    }
                } else {
                    let mut rank = 0;
                    result.retain(|candidate| {
                        let keep = explore
                            || rank < semantic_limit
                            || candidate.text.to_lowercase().contains(&q.to_lowercase());
                        rank += 1;
                        keep
                    });
                }
            }
            let mut diff_results = Vec::new();
            let mut include_diffs = selected_types
                .components()
                .contains(&EvidenceComponent::Diff);
            let include_diff = |reference: &history::DiffReference| {
                let (path, commit_id) = match reference.side {
                    history::DiffSide::Added => {
                        (reference.new_path.as_deref(), &reference.child_commit_id)
                    }
                    history::DiffSide::Removed => {
                        (reference.old_path.as_deref(), &reference.parent_commit_id)
                    }
                };
                path.is_some_and(|path| {
                    result_scope.matches_diff(path)
                        && history_path_filter.as_ref().is_none_or(|paths| {
                            paths.contains(&(commit_id.clone(), internal_git_path(path)))
                        })
                })
            };
            if include_diffs && !history::is_current(&c)? {
                if type_set_selected || diff_flag {
                    return Err(
                        "diff search requires a current history index; run `repodb refresh`".into(),
                    );
                }
                include_diffs = false;
            }
            if include_diffs {
                let has_diff_index: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='diff_references')",
                    [],
                    |row| row.get(0),
                )?;
                if !has_diff_index {
                    if type_set_selected || diff_flag {
                        return Err("diff search is not indexed; run `repodb refresh`".into());
                    }
                    include_diffs = false;
                }
                if include_diffs {
                    let search_diff_results =
                        |search_mode, search_limit| -> Result<Vec<history::DiffSearchResult>> {
                            if matches!(result_scope, SearchScope::All)
                                && history_path_filter.is_none()
                            {
                                history::search_diffs(&c, &q, search_mode, search_limit, &commits)
                            } else {
                                history::search_diffs_filtered(
                                    &c,
                                    &q,
                                    search_mode,
                                    search_limit,
                                    &commits,
                                    include_diff,
                                )
                            }
                        };
                    diff_results = match mode {
                        "exact" => search_diff_results(history::DiffSearchMode::Exact, usize::MAX)?,
                        "semantic" => search_diff_results(
                            history::DiffSearchMode::Semantic,
                            if explore { usize::MAX } else { semantic_limit },
                        )?,
                        "combined" => {
                            let semantic = search_diff_results(
                                history::DiffSearchMode::Semantic,
                                if explore { usize::MAX } else { semantic_limit },
                            )?;
                            let exact =
                                search_diff_results(history::DiffSearchMode::Exact, usize::MAX)?;
                            let mut seen = std::collections::HashSet::new();
                            let mut hits = Vec::new();
                            for hit in semantic.into_iter().chain(exact) {
                                let reference = &hit.reference;
                                let identity = (
                                    reference.parent_commit_id.clone(),
                                    reference.child_commit_id.clone(),
                                    reference.hunk_id,
                                    reference.unit_id,
                                );
                                if seen.insert(identity) {
                                    hits.push(hit);
                                }
                            }
                            history::score_diff_results(&c, &q, &mut hits)?;
                            hits
                        }
                        _ => Vec::new(),
                    };
                }
            }
            let mut diff_metadata_cache = DiffMetadataCache::default();
            let mut displays = HashMap::<String, EvidenceDisplay>::new();
            let mut candidates = Vec::new();
            for (order, item) in result.iter().enumerate() {
                let candidate = search_evidence(item, &q, mode, order)?;
                displays.insert(
                    candidate.reference_id.clone(),
                    EvidenceDisplay::Search(Box::new(item.clone())),
                );
                candidates.push(candidate);
            }
            for hit in &diff_results {
                let reference = &hit.reference;
                let side = match reference.side {
                    history::DiffSide::Added => "added",
                    history::DiffSide::Removed => "removed",
                };
                let path = match reference.side {
                    history::DiffSide::Added => reference.new_path.as_deref(),
                    history::DiffSide::Removed => reference.old_path.as_deref(),
                };
                let commit_id = match reference.side {
                    history::DiffSide::Added => &reference.child_commit_id,
                    history::DiffSide::Removed => &reference.parent_commit_id,
                };
                let reference_id = serde_json::to_string(&(
                    &reference.parent_commit_id,
                    &reference.child_commit_id,
                    reference.hunk_id,
                    reference.unit_id,
                    side,
                ))?;
                let payload = diff_search_json(&c, hit, &mut diff_metadata_cache)?;
                candidates.push(EvidenceCandidate {
                    component: EvidenceComponent::Diff,
                    source_id: "local".to_owned(),
                    reference_id: reference_id.clone(),
                    commit_id: Some(commit_id.clone()),
                    change_commit_id: Some(reference.child_commit_id.clone()),
                    parent_commit_id: Some(reference.parent_commit_id.clone()),
                    path: path.map(<[u8]>::to_vec),
                    side: Some(side.to_owned()),
                    line_start: Some(reference.line_start),
                    line_end: Some(reference.line_end),
                    exact: mode == "exact" || hit.text.to_lowercase().contains(&q.to_lowercase()),
                    distance: hit.distance,
                    payload,
                });
                displays.insert(reference_id, EvidenceDisplay::Diff(Box::new(hit.clone())));
            }
            let mut related_diffs = HashMap::new();
            if selected_types == TypeSet::All && include_diffs {
                let mut seen_messages = HashSet::new();
                for item in result.iter().filter(|item| item.path == "[commit message]") {
                    let Some(commit_id) = item.commit_id.as_ref() else {
                        continue;
                    };
                    if !seen_messages.insert(commit_id.clone()) {
                        continue;
                    }
                    let references = history::diff_references(&c, None, Some(commit_id))?
                        .into_iter()
                        .filter(&include_diff)
                        .map(|reference| diff_reference_json(&reference))
                        .collect();
                    related_diffs.insert(("local".to_owned(), commit_id.clone()), references);
                }
            }
            let search_mode = match mode {
                "exact" => SearchMode::Exact,
                "semantic" => SearchMode::Semantic,
                "combined" => SearchMode::Combined,
                _ => unreachable!(),
            };
            // `--semantic-limit` bounds semantic candidate retrieval; exact-mode
            // references remain uncapped, and combined mode retains all exact hits.
            let ranking_limit = if mode == "semantic" && !explore {
                semantic_limit
            } else {
                candidates.len().max(1)
            };
            let ranked = rank_evidence(
                candidates,
                search_mode,
                selected_types,
                ranking_limit,
                &related_diffs,
            )
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
            let mut values = Vec::new();
            for (group_id, group) in ranked.iter().enumerate() {
                values.extend(group.components.iter().map(|candidate| {
                    annotated_ranked_payload(candidate, group, group_id, &history_path_links)
                }));
            }
            if let Some(reference_id) = fetch_reference.as_deref() {
                values.retain(|value| value["reference_id"].as_str() == Some(reference_id));
                if values.is_empty() {
                    return Err(reference_cli_error(json, reference_id));
                }
            }
            let worktree_summary = if let Some(report) = worktree_report.as_ref() {
                let mut summary = worktree_report_json(report);
                if let Some(semantic) = semantic_worktree_report.as_ref() {
                    summary["semantic_mapping"] = semantic_worktree_report_json(semantic);
                }
                Some(summary)
            } else {
                semantic_worktree_report
                    .as_ref()
                    .map(semantic_worktree_report_json)
            };
            let serialized_budget = byte_budget.saturating_sub(1);
            let mut page = if explore {
                repodb::pagination::page_references_with_metadata(
                    &values,
                    result_limit,
                    serialized_budget,
                    cursor.as_deref(),
                    worktree_summary,
                )
            } else {
                repodb::pagination::page_results_with_metadata(
                    &values,
                    result_limit,
                    serialized_budget,
                    cursor.as_deref(),
                    worktree_summary,
                )
            }
            .map_err(|error| pagination_cli_error(json, error))?;
            if !explore && (mode == "semantic" || mode == "combined") {
                page.total_count = None;
            }
            if json {
                let output = if !explore
                    && cursor.is_none()
                    && page.cursor.is_none()
                    && !page.truncated
                    && page.worktree.is_none()
                {
                    serde_json::Value::Array(page.results.clone())
                } else {
                    serde_json::to_value(&page)?
                };
                println!("{}", serde_json::to_string(&output)?);
            } else {
                print_ranked_human(
                    &ranked,
                    &displays,
                    &c,
                    &history_path_links,
                    &mut diff_metadata_cache,
                    &page,
                    explore,
                )?;
                if let Some(report) = worktree_report.as_ref() {
                    print_worktree_summary(report);
                }
                if let Some(report) = semantic_worktree_report.as_ref() {
                    print_semantic_worktree_summary(report);
                }
                if page.cursor.is_some() || page.truncated {
                    println!(
                        "Page: {} results; total {}; truncated={}; snapshot={}",
                        page.results.len(),
                        page.total_count
                            .map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
                        page.truncated,
                        page.snapshot_id,
                    );
                    if let Some(next) = page.cursor.as_deref() {
                        println!("Next cursor: {next}");
                    }
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

#[cfg(test)]
mod tests {
    use super::{ENCODED_GIT_PATH_PREFIX, encode_git_path, evidence_path_bytes};

    #[test]
    fn evidence_path_bytes_decodes_even_length_hex_and_falls_back_for_invalid_hex() {
        assert_eq!(
            evidence_path_bytes(&encode_git_path(vec![0xff, b'a'])),
            [0xff, b'a']
        );
        assert_eq!(
            evidence_path_bytes(&format!("{ENCODED_GIT_PATH_PREFIX}ff0")),
            [0xff]
        );
        assert_eq!(
            evidence_path_bytes(&format!("{ENCODED_GIT_PATH_PREFIX}gg")),
            format!("{ENCODED_GIT_PATH_PREFIX}gg").as_bytes()
        );
    }
}
