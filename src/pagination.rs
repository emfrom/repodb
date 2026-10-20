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
    let mut used = 0usize;
    for result in results.iter().skip(start).take(count_budget) {
        let mut candidate = result.clone();
        let mut cost = serde_json::to_vec(&candidate)
            .map_err(|error| error.to_string())?
            .len();
        if used + cost > byte_budget {
            if let Some(object) = candidate.as_object_mut() {
                if object.contains_key("text") {
                    object.remove("text");
                    object.insert("text_omitted_for_size".to_owned(), Value::Bool(true));
                    cost = serde_json::to_vec(&candidate)
                        .map_err(|error| error.to_string())?
                        .len();
                }
            }
        }
        if used + cost > byte_budget {
            if page.is_empty() {
                return Err("minimal result reference exceeds byte budget".to_owned());
            }
            break;
        }
        used += cost;
        page.push(candidate);
    }
    let next = start + page.len();
    let truncated = next < results.len()
        || page
            .iter()
            .any(|result| result.get("text_omitted_for_size") == Some(&Value::Bool(true)));
    Ok(SearchPage {
        results: page,
        cursor: truncated.then(|| format!("{snapshot_id}:{next}")),
        snapshot_id,
        truncated,
        total_count: Some(results.len()),
    })
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pages_with_stable_cursor_and_snapshot_and_known_total() {
        let results = vec![
            json!({"path":"a","text":"one"}),
            json!({"path":"b","text":"two"}),
        ];
        let first = page_results(&results, 1, 1024, None).unwrap();
        assert_eq!(first.results, vec![json!({"path":"a","text":"one"})]);
        assert_eq!(first.total_count, Some(2));
        assert!(first.truncated);
        let cursor = first.cursor.as_deref().unwrap();
        let second = page_results(&results, 1, 1024, Some(cursor)).unwrap();
        assert_eq!(second.results, vec![json!({"path":"b","text":"two"})]);
        assert_eq!(second.cursor, None);
        assert_eq!(first.snapshot_id, second.snapshot_id);
        assert_eq!(
            first.cursor,
            page_results(&results, 1, 1024, None).unwrap().cursor
        );
    }

    #[test]
    fn oversized_text_is_omitted_whole_and_minimal_reference_limit_errors() {
        let results = vec![
            json!({"path":"large","text":"a long passage that is far too large to fit in the byte budget"}),
        ];
        let page = page_results(&results, 1, 70, None).unwrap();
        assert_eq!(
            page.results,
            vec![json!({"path":"large","text_omitted_for_size":true})]
        );
        assert!(page.truncated);
        assert!(
            page_results(&results, 1, 1, None)
                .unwrap_err()
                .contains("byte budget")
        );
    }

    #[test]
    fn rejects_changed_snapshot_cursor_and_zero_limits() {
        let results = vec![
            json!({"path":"a","text":"one"}),
            json!({"path":"c","text":"three"}),
        ];
        let cursor = page_results(&results, 1, 1024, None).unwrap().cursor;
        assert!(page_results(&results, 0, 1024, None).is_err());
        assert!(page_results(&results, 1, 0, None).is_err());
        assert!(
            page_results(
                &[json!({"path":"b"}), json!({"path":"d"})],
                1,
                1024,
                cursor.as_deref()
            )
            .is_err()
        );
    }
}
