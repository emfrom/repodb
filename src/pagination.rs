use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchPage {
    pub results: Vec<Value>,
    pub cursor: Option<String>,
    pub snapshot_id: String,
    pub truncated: bool,
    pub total_count: Option<usize>,
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
    let mut references = results.to_vec();
    for reference in &mut references {
        strip_text(reference);
        if let Some(object) = reference.as_object_mut() {
            object.insert("reference_only".to_owned(), Value::Bool(true));
        }
    }
    page_results(&references, count_budget, byte_budget, cursor)
}

fn strip_text(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for field in ["text", "head_text", "live_text", "text_omitted_for_size"] {
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
    if count_budget == 0 || byte_budget == 0 {
        return Err("count and byte budgets must be positive".to_owned());
    }
    let serialized = serde_json::to_vec(results).map_err(|error| error.to_string())?;
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
        )? > byte_budget
        {
            page.pop();
            if let Some(object) = candidate.as_object_mut() {
                if object.contains_key("text") {
                    object.remove("text");
                    object.insert("text_omitted_for_size".to_owned(), Value::Bool(true));
                    page.push(candidate);
                    omitted_text = true;
                    let next = start + page.len();
                    if serialized_page_size(&page, &snapshot_id, next, results.len(), omitted_text)?
                        <= byte_budget
                    {
                        continue;
                    }
                    page.pop();
                    omitted_text = page.iter().any(|result| {
                        result.get("text_omitted_for_size") == Some(&Value::Bool(true))
                    });
                }
            }
            if page.is_empty() {
                return Err("minimal result reference exceeds byte budget".to_owned());
            }
            break;
        }
    }
    let next = start + page.len();
    let more_results = next < results.len();
    Ok(SearchPage {
        results: page,
        cursor: more_results.then(|| format!("{snapshot_id}:{next}")),
        snapshot_id,
        truncated: more_results || omitted_text,
        total_count: Some(results.len()),
    })
}

fn serialized_page_size(
    results: &[Value],
    snapshot_id: &str,
    next: usize,
    total: usize,
    omitted_text: bool,
) -> Result<usize, String> {
    let more_results = next < total;
    serde_json::to_vec(&SearchPage {
        results: results.to_vec(),
        cursor: more_results.then(|| format!("{snapshot_id}:{next}")),
        snapshot_id: snapshot_id.to_owned(),
        truncated: more_results || omitted_text,
        total_count: Some(total),
    })
    .map(|bytes| bytes.len())
    .map_err(|error| error.to_string())
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}
