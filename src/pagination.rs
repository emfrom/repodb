use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchPage {
    pub results: Vec<Value>,
    pub cursor: Option<String>,
    pub snapshot_id: String,
    pub truncated: bool,
    pub total_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Value>,
}

/// Paginate references without returning any indexed or live text.
///
/// Reference identity, source, line/page anchors, and ranking metadata are kept;
/// text fields are removed recursively before cursor identity and byte
/// accounting are calculated.
pub fn page_references(
    results: &[Value],
    count_budget: usize,
    byte_budget: usize,
    cursor: Option<&str>,
) -> Result<SearchPage, String> {
    page_references_with_metadata(results, count_budget, byte_budget, cursor, None)
}

pub fn page_references_with_metadata(
    results: &[Value],
    count_budget: usize,
    byte_budget: usize,
    cursor: Option<&str>,
    worktree: Option<Value>,
) -> Result<SearchPage, String> {
    let mut references = results.to_vec();
    for reference in &mut references {
        strip_text(reference);
        if let Some(object) = reference.as_object_mut() {
            object.insert("reference_only".to_owned(), Value::Bool(true));
        }
    }
    page_results_with_metadata(&references, count_budget, byte_budget, cursor, worktree)
}

fn strip_text(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for field in [
                "text",
                "head_text",
                "live_text",
                "text_omitted_for_size",
                "head_text_omitted_for_size",
                "live_text_omitted_for_size",
            ] {
                object.remove(field);
            }
            for nested in object.values_mut() {
                strip_text(nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                strip_text(nested);
            }
        }
        _ => {}
    }
}

/// Paginate a deterministically ordered set of complete search results.
pub fn page_results(
    results: &[Value],
    count_budget: usize,
    byte_budget: usize,
    cursor: Option<&str>,
) -> Result<SearchPage, String> {
    page_results_with_metadata(results, count_budget, byte_budget, cursor, None)
}

pub fn page_results_with_metadata(
    results: &[Value],
    count_budget: usize,
    byte_budget: usize,
    cursor: Option<&str>,
    worktree: Option<Value>,
) -> Result<SearchPage, String> {
    if count_budget == 0 || byte_budget == 0 {
        return Err("count and byte budgets must be positive".to_owned());
    }
    let stable_metadata = worktree.as_ref().map(stable_worktree_metadata);
    let serialized =
        serde_json::to_vec(&(results, stable_metadata)).map_err(|error| error.to_string())?;
    let snapshot_id = format!("{:016x}", stable_hash(&serialized));
    let start = match cursor {
        None => 0,
        Some(value) => {
            let (identity, offset) = value
                .split_once(':')
                .ok_or_else(|| "invalid cursor".to_owned())?;
            if identity != snapshot_id {
                return Err("cursor snapshot does not match results".to_owned());
            }
            let offset = offset
                .parse::<usize>()
                .map_err(|_| "invalid cursor".to_owned())?;
            if offset == 0 || offset >= results.len() {
                return Err("cursor offset is out of range".to_owned());
            }
            offset
        }
    };
    let mut page = Vec::new();
    let mut omitted_text = false;
    for result in results.iter().skip(start).take(count_budget) {
        let mut candidate = result.clone();
        page.push(candidate.clone());
        let next = start + page.len();
        if serialized_page_size(
            &page,
            snapshot_id.as_str(),
            next,
            results.len(),
            omitted_text,
            worktree.as_ref(),
        )? > byte_budget
        {
            page.pop();
            if omit_text_fields(&mut candidate) {
                page.push(candidate);
                omitted_text = true;
                let next = start + page.len();
                if serialized_page_size(
                    &page,
                    &snapshot_id,
                    next,
                    results.len(),
                    omitted_text,
                    worktree.as_ref(),
                )? <= byte_budget
                {
                    continue;
                }
                page.pop();
                omitted_text = page.iter().any(|result| contains_text_omission(result));
            }
            if page.is_empty() {
                return Err("minimal result reference exceeds byte budget".to_owned());
            }
            break;
        }
    }
    let next = start + page.len();
    let more_results = next < results.len();
    if serialized_page_size(
        &page,
        &snapshot_id,
        next,
        results.len(),
        omitted_text,
        worktree.as_ref(),
    )? > byte_budget
    {
        return Err("minimum pagination metadata exceeds byte budget".to_owned());
    }
    Ok(SearchPage {
        results: page,
        cursor: more_results.then(|| format!("{snapshot_id}:{next}")),
        snapshot_id,
        truncated: more_results || omitted_text,
        total_count: Some(results.len()),
        worktree,
    })
}

fn serialized_page_size(
    results: &[Value],
    snapshot_id: &str,
    next: usize,
    total: usize,
    omitted_text: bool,
    worktree: Option<&Value>,
) -> Result<usize, String> {
    let more_results = next < total;
    serde_json::to_vec(&SearchPage {
        results: results.to_vec(),
        cursor: more_results.then(|| format!("{snapshot_id}:{next}")),
        snapshot_id: snapshot_id.to_owned(),
        truncated: more_results || omitted_text,
        total_count: Some(total),
        worktree: worktree.cloned(),
    })
    .map(|bytes| bytes.len())
    .map_err(|error| error.to_string())
}

fn stable_worktree_metadata(metadata: &Value) -> Value {
    let mut stable = metadata.clone();
    strip_observation_time(&mut stable);
    stable
}

fn strip_observation_time(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("observed_at_unix_ms");
            for nested in object.values_mut() {
                strip_observation_time(nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                strip_observation_time(nested);
            }
        }
        _ => {}
    }
}

fn omit_text_fields(value: &mut Value) -> bool {
    match value {
        Value::Object(object) => {
            let mut omitted = false;
            for field in ["text", "head_text", "live_text"] {
                if object.remove(field).is_some() {
                    object.insert(format!("{field}_omitted_for_size"), Value::Bool(true));
                    omitted = true;
                }
            }
            for nested in object.values_mut() {
                omitted |= omit_text_fields(nested);
            }
            if omitted {
                object.insert("text_omitted_for_size".to_owned(), Value::Bool(true));
            }
            omitted
        }
        Value::Array(values) => values
            .iter_mut()
            .fold(false, |omitted, nested| omit_text_fields(nested) || omitted),
        _ => false,
    }
}

fn contains_text_omission(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.get("text_omitted_for_size") == Some(&Value::Bool(true))
                || object.values().any(contains_text_omission)
        }
        Value::Array(values) => values.iter().any(contains_text_omission),
        _ => false,
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}
