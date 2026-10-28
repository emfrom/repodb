use repodb::ranking::{
    EvidenceCandidate, EvidenceComponent, RankedEvidence, RankingMethod, SearchMode, TypeSet,
    rank_evidence,
};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    env,
    error::Error,
    path::PathBuf,
    process::{Command, Stdio},
};

mod budget;
mod commands;
mod config;
mod git_objects;
mod git_ranges;
mod history;
mod imports;
mod process;
mod refresh_storage;
mod segmentation;
mod semgrep_languages;
mod sqlite_vec_extension;
mod url_security;

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

pub(crate) fn git(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process::spawn_in_process_group(&mut command)?;
    let output = process::capture_with_input_timeout(
        &mut child,
        &[],
        256 * 1024 * 1024,
        64 * 1024,
        process::refresh_subprocess_timeout()?,
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
pub(crate) fn open_db(path: &std::path::Path) -> Result<Connection> {
    open_db_with_refresh_limits(path, false)
}

pub(crate) fn open_db_for_refresh(path: &std::path::Path) -> Result<Connection> {
    open_db_with_refresh_limits(path, true)
}

fn open_db_with_refresh_limits(
    path: &std::path::Path,
    apply_refresh_limits: bool,
) -> Result<Connection> {
    sqlite_vec_extension::register()?;
    let c = Connection::open(path)?;
    if apply_refresh_limits {
        refresh_storage::configure(&c)?;
    }
    if !history::is_current(&c)? {
        c.execute_batch("CREATE TABLE IF NOT EXISTS indexed_files(path TEXT PRIMARY KEY NOT NULL,commit_id TEXT NOT NULL,content TEXT NOT NULL); CREATE TABLE IF NOT EXISTS embeddings(path TEXT NOT NULL,line INTEGER NOT NULL,text TEXT NOT NULL,commit_id TEXT NOT NULL,vector BLOB NOT NULL,PRIMARY KEY(path,line)); CREATE TABLE IF NOT EXISTS embedding_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    }
    Ok(c)
}
pub(crate) fn resolved_executable(command: &str) -> Option<PathBuf> {
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
    const MAX_SINGLE_EMBEDDING_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
    let output_limit = output_limit.unwrap_or(MAX_SINGLE_EMBEDDING_OUTPUT_BYTES);
    let mut child_command = Command::new(command);
    child_command
        .env("REPODB_EMBEDDING_MODEL", model)
        .args(["--json", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process::spawn_in_process_group(&mut child_command)?;
    // Some embedders return no row for empty/whitespace input. Preserve a
    // searchable vector for every indexed unit with a stable sentinel instead.
    let input = if is_builtin_embedding_command(command) && text.trim().is_empty() {
        "<empty RepoDB unit>"
    } else {
        text
    };
    let output = process::capture_with_input_timeout(
        &mut child,
        input.as_bytes(),
        output_limit,
        64 * 1024,
        process::refresh_subprocess_timeout()?,
    )?;
    if output.exceeded {
        return Err(format!(
            "embedding command output could not be captured completely (limit {output_limit})"
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
    let mut child_command = Command::new(command);
    child_command
        .env("REPODB_EMBEDDING_MODEL", model)
        .args(["--json", "-0", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process::spawn_in_process_group(&mut child_command)?;
    let mut vectors = Vec::with_capacity(texts.len());
    let output = process::capture_lines_with_input_timeout(
        &mut child,
        &input,
        output_limit,
        64 * 1024,
        process::refresh_subprocess_timeout()?,
        |line| {
            if line.iter().all(u8::is_ascii_whitespace) {
                return Ok(());
            }
            let vector: Vec<f32> = serde_json::from_slice(line).map_err(std::io::Error::other)?;
            validate_vector(&vector).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
            })?;
            vectors.push(vector);
            Ok(())
        },
    )?;
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
pub(crate) fn reembed(c: &mut Connection, command: &str, model: &str) -> Result<()> {
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
        return commands::help::print_help(None);
    }
    if matches!(arguments[0].as_str(), "--help" | "-h") {
        if arguments.len() > 2 {
            return Err("usage: repodb --help [COMMAND]".into());
        }
        return commands::help::print_help(arguments.get(1).map(String::as_str));
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
        return commands::help::print_help(None);
    }
    let cmd = arguments.remove(0);
    if cmd == "help" {
        if arguments.len() > 1 {
            return Err("usage: repodb help [COMMAND]".into());
        }
        return commands::help::print_help(arguments.first().map(String::as_str));
    }
    if matches!(cmd.as_str(), "--help" | "-h") {
        if arguments.len() > 1 {
            return Err("usage: repodb [--database PATH] --help [COMMAND]".into());
        }
        return commands::help::print_help(arguments.first().map(String::as_str));
    }
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        return commands::help::print_help(Some(&cmd));
    }
    let a = arguments.into_iter();
    let db_path = selected_database(option)?;
    match cmd.as_str() {
        "status" => {
            commands::status::run(&db_path, &a.collect::<Vec<_>>())?;
        }
        "init" | "refresh" => {
            commands::refresh::run(&db_path, &cmd, &a.collect::<Vec<_>>())?;
        }
        "import" => {
            commands::import::run(&db_path, &a.collect::<Vec<_>>())?;
        }
        "retention" => {
            commands::settings::retention(&db_path, &a.collect::<Vec<_>>())?;
        }
        "redirect-limit" => {
            commands::settings::redirect_limit(&db_path, &a.collect::<Vec<_>>())?;
        }
        "reembed" => {
            commands::import::reembed(&db_path, &a.collect::<Vec<_>>())?;
        }
        "search" => {
            commands::search::run(&db_path, &a.collect::<Vec<_>>())?;
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
