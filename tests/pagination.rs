use repodb::pagination::page_results;
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
    let page = page_results(&results, 1, 140, None).unwrap();
    assert!(serde_json::to_vec(&page).unwrap().len() <= 140);
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

#[test]
fn reference_only_pages_enumerate_duplicate_refs_without_text() {
    let results = vec![
        json!({"path":"a.rs", "commit_id":"one", "line_start":1, "text":"same text"}),
        json!({"path":"a.rs", "commit_id":"two", "line_start":1, "text":"same text"}),
        json!({"path":"b.rs", "commit_id":"two", "line_start":1, "text":"same text"}),
    ];
    let first = repodb::pagination::page_references(&results, 1, 1024, None).unwrap();
    let second =
        repodb::pagination::page_references(&results, 1, 1024, first.cursor.as_deref()).unwrap();
    let third =
        repodb::pagination::page_references(&results, 1, 1024, second.cursor.as_deref()).unwrap();

    assert_eq!(first.total_count, Some(3));
    assert_eq!(first.results[0]["path"], "a.rs");
    assert_eq!(second.results[0]["commit_id"], "two");
    assert_eq!(third.results[0]["path"], "b.rs");
    for page in [&first, &second, &third] {
        assert!(page.results[0].get("text").is_none());
        assert!(page.results[0]["reference_only"].as_bool().unwrap());
    }
}

#[test]
fn preserves_each_duplicate_eligible_occurrence_across_pages() {
    let results = vec![
        json!({"blob":"same","path":"first","text":"match"}),
        json!({"blob":"same","path":"second","text":"match"}),
    ];
    let first = page_results(&results, 1, 1024, None).unwrap();
    let second = page_results(&results, 1, 1024, first.cursor.as_deref()).unwrap();
    assert_eq!(first.results[0]["path"], "first");
    assert_eq!(second.results[0]["path"], "second");
}
