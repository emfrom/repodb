use repodb::ranking::{
    EvidenceCandidate, EvidenceComponent, RankingMethod, SearchMode, TypeSet, rank_evidence,
};
use serde_json::json;
use std::collections::HashMap;

fn evidence(
    component: EvidenceComponent,
    commit: Option<&str>,
    path: Option<&[u8]>,
    lines: Option<(usize, usize)>,
    exact: bool,
    distance: Option<f64>,
    payload: serde_json::Value,
) -> EvidenceCandidate {
    EvidenceCandidate {
        component,
        source_id: "local".to_owned(),
        reference_id: payload["id"].as_str().unwrap().to_owned(),
        commit_id: commit.map(str::to_owned),
        change_commit_id: None,
        parent_commit_id: None,
        path: path.map(<[u8]>::to_vec),
        side: None,
        line_start: lines.map(|range| range.0),
        line_end: lines.map(|range| range.1),
        exact,
        distance,
        payload,
    }
}

#[test]
fn type_sets_select_the_specified_components() {
    for (name, selector) in [
        ("all", TypeSet::All),
        ("content", TypeSet::Content),
        ("cmdiffs", TypeSet::Cmdiffs),
        ("diffs", TypeSet::Diffs),
        ("cmcommits", TypeSet::Cmcommits),
        ("commits", TypeSet::Commits),
    ] {
        assert_eq!(TypeSet::parse(name).unwrap(), selector);
    }
    assert!(TypeSet::parse("unknown").is_err());
    assert_eq!(
        TypeSet::All.components(),
        [
            EvidenceComponent::Content,
            EvidenceComponent::Diff,
            EvidenceComponent::CommitMessage,
        ]
    );
    assert_eq!(
        TypeSet::Content.components(),
        [EvidenceComponent::Content, EvidenceComponent::Diff]
    );
    assert_eq!(
        TypeSet::Cmdiffs.components(),
        [EvidenceComponent::CommitMessage, EvidenceComponent::Diff]
    );
    assert_eq!(TypeSet::Diffs.components(), [EvidenceComponent::Diff]);
    assert_eq!(
        TypeSet::Cmcommits.components(),
        [EvidenceComponent::CommitMessage, EvidenceComponent::Content]
    );
    assert_eq!(
        TypeSet::Commits.components(),
        [EvidenceComponent::CommitMessage]
    );
    let filtered = rank_evidence(
        vec![
            evidence(
                EvidenceComponent::Content,
                Some("commit"),
                Some(b"a.rs"),
                Some((1, 1)),
                true,
                None,
                json!({"id":"content"}),
            ),
            evidence(
                EvidenceComponent::Diff,
                Some("commit"),
                Some(b"a.rs"),
                Some((1, 1)),
                true,
                None,
                json!({"id":"diff"}),
            ),
            evidence(
                EvidenceComponent::CommitMessage,
                Some("commit"),
                None,
                None,
                true,
                None,
                json!({"id":"message"}),
            ),
        ],
        SearchMode::Exact,
        TypeSet::Commits,
        5,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].primary, EvidenceComponent::CommitMessage);
}

#[test]
fn combined_mode_reserves_exact_semantic_only_and_distinct_diff_evidence() {
    let candidates = vec![
        evidence(
            EvidenceComponent::Content,
            Some("exact"),
            Some(b"src/a.rs"),
            Some((1, 1)),
            true,
            Some(0.7),
            json!({"id":"exact"}),
        ),
        evidence(
            EvidenceComponent::Content,
            Some("semantic"),
            Some(b"src/b.rs"),
            Some((2, 2)),
            false,
            Some(0.05),
            json!({"id":"semantic"}),
        ),
        evidence(
            EvidenceComponent::Diff,
            Some("change"),
            Some(b"src/c.rs"),
            Some((3, 3)),
            true,
            Some(0.8),
            json!({"id":"distinct-diff"}),
        ),
        evidence(
            EvidenceComponent::Content,
            Some("other"),
            Some(b"src/d.rs"),
            Some((4, 4)),
            false,
            Some(0.3),
            json!({"id":"other"}),
        ),
        {
            let mut message = evidence(
                EvidenceComponent::CommitMessage,
                Some("message-only"),
                None,
                None,
                false,
                Some(0.01),
                json!({"id":"message-only"}),
            );
            message.change_commit_id = Some("message-only".to_owned());
            message
        },
    ];
    assert!(
        rank_evidence(
            candidates.clone(),
            SearchMode::Combined,
            TypeSet::All,
            2,
            &HashMap::new(),
        )
        .is_err()
    );
    let ranked = rank_evidence(
        candidates,
        SearchMode::Combined,
        TypeSet::All,
        4,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(ranked.len(), 4);
    let ids = ranked
        .iter()
        .flat_map(|group| group.components.iter())
        .map(|candidate| candidate.payload["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"exact"));
    assert!(ids.contains(&"semantic"));
    assert!(ids.contains(&"distinct-diff"));
    assert!(ids.contains(&"message-only"));
}

#[test]
fn exact_mode_uses_available_semantic_scores_or_deterministic_fallback() {
    let candidates = vec![
        evidence(
            EvidenceComponent::Content,
            Some("b"),
            Some(b"b.rs"),
            Some((1, 1)),
            true,
            Some(0.5),
            json!({"id":"b"}),
        ),
        evidence(
            EvidenceComponent::Content,
            Some("a"),
            Some(b"a.rs"),
            Some((1, 1)),
            true,
            Some(0.1),
            json!({"id":"a"}),
        ),
    ];
    let scored = rank_evidence(
        candidates.clone(),
        SearchMode::Exact,
        TypeSet::Content,
        2,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(scored[0].components[0].payload["id"], "a");
    assert_eq!(scored[0].ranking_method, RankingMethod::SemanticDistance);
    assert_eq!(scored[0].score_source, Some(EvidenceComponent::Content));

    let mut fallback_candidates = candidates;
    for candidate in &mut fallback_candidates {
        candidate.distance = None;
    }
    let fallback = rank_evidence(
        fallback_candidates,
        SearchMode::Exact,
        TypeSet::Content,
        2,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(fallback[0].components[0].payload["id"], "a");
    assert_eq!(fallback[0].ranking_method, RankingMethod::Deterministic);
    assert_eq!(fallback[0].score_source, None);
    assert_eq!(fallback[0].distance, None);
}

#[test]
fn combined_mode_merges_exact_and_semantic_views_of_the_same_reference() {
    let exact = evidence(
        EvidenceComponent::Content,
        Some("commit"),
        Some(b"src/main.rs"),
        Some((4, 4)),
        true,
        None,
        json!({"id":"same-reference"}),
    );
    let semantic = evidence(
        EvidenceComponent::Content,
        Some("commit"),
        Some(b"src/main.rs"),
        Some((4, 4)),
        false,
        Some(0.12),
        json!({"id":"same-reference"}),
    );
    let ranked = rank_evidence(
        vec![exact, semantic],
        SearchMode::Combined,
        TypeSet::Content,
        5,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].components.len(), 1);
    assert!(ranked[0].exact_match);
    assert_eq!(ranked[0].distance, Some(0.12));
}

#[test]
fn all_mode_groups_overlapping_content_and_diff_with_diff_primary() {
    let content = evidence(
        EvidenceComponent::Content,
        Some("commit"),
        Some(b"src/main.rs"),
        Some((10, 12)),
        true,
        Some(0.4),
        json!({"id":"content"}),
    );
    let diff = evidence(
        EvidenceComponent::Diff,
        Some("commit"),
        Some(b"src/main.rs"),
        Some((11, 11)),
        true,
        Some(0.2),
        json!({"id":"diff"}),
    );
    let ranked = rank_evidence(
        vec![content, diff],
        SearchMode::Exact,
        TypeSet::All,
        5,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].primary, EvidenceComponent::Diff);
    assert_eq!(ranked[0].components.len(), 2);
}

#[test]
fn commit_message_lifts_all_parent_diffs_and_keeps_message_only_changes() {
    let mut message = evidence(
        EvidenceComponent::CommitMessage,
        Some("merge"),
        None,
        None,
        true,
        Some(0.3),
        json!({"id":"merge-message"}),
    );
    message.change_commit_id = Some("merge".to_owned());
    let mut merge_diff = evidence(
        EvidenceComponent::Diff,
        Some("merge"),
        Some(b"src/merged.rs"),
        Some((7, 8)),
        true,
        Some(0.25),
        json!({"id":"merge-diff"}),
    );
    merge_diff.change_commit_id = Some("merge".to_owned());
    merge_diff.parent_commit_id = Some("left".to_owned());
    let mut imported_message = evidence(
        EvidenceComponent::CommitMessage,
        Some("merge"),
        None,
        None,
        true,
        Some(0.1),
        json!({"id":"imported-message"}),
    );
    imported_message.source_id = "imported".to_owned();
    imported_message.change_commit_id = Some("merge".to_owned());
    let mut message_only = evidence(
        EvidenceComponent::CommitMessage,
        Some("empty-change"),
        None,
        None,
        true,
        Some(0.2),
        json!({"id":"message-only"}),
    );
    message_only.change_commit_id = Some("empty-change".to_owned());
    let related = HashMap::from([
        (
            ("local".to_owned(), "merge".to_owned()),
            vec![json!({"parent":"left"}), json!({"parent":"right"})],
        ),
        (("local".to_owned(), "empty-change".to_owned()), Vec::new()),
    ]);
    let ranked = rank_evidence(
        vec![message, merge_diff, message_only, imported_message],
        SearchMode::Exact,
        TypeSet::All,
        5,
        &related,
    )
    .unwrap();
    let merge = ranked
        .iter()
        .find(|group| {
            group
                .components
                .iter()
                .any(|item| item.payload["id"] == "merge-message")
        })
        .unwrap();
    assert_eq!(merge.primary, EvidenceComponent::Diff);
    assert_eq!(merge.components.len(), 2);
    assert_eq!(
        merge.related_diffs,
        [json!({"parent":"left"}), json!({"parent":"right"})]
    );
    assert!(ranked.iter().any(|group| {
        group
            .components
            .iter()
            .any(|item| item.payload["id"] == "message-only")
            && group.primary == EvidenceComponent::CommitMessage
    }));
    let imported = ranked
        .iter()
        .find(|group| {
            group
                .components
                .iter()
                .any(|item| item.payload["id"] == "imported-message")
        })
        .unwrap();
    assert_eq!(imported.primary, EvidenceComponent::CommitMessage);
    assert!(imported.related_diffs.is_empty());
}

#[test]
fn stable_ties_use_source_commit_parent_path_side_and_range() {
    let mut later = evidence(
        EvidenceComponent::Diff,
        Some("b"),
        Some(b"z.rs"),
        Some((4, 5)),
        false,
        Some(0.5),
        json!({"id":"later"}),
    );
    later.parent_commit_id = Some("parent-b".to_owned());
    later.side = Some("removed".to_owned());
    let mut earlier = evidence(
        EvidenceComponent::Diff,
        Some("a"),
        Some(b"a.rs"),
        Some((1, 2)),
        false,
        Some(0.5),
        json!({"id":"earlier"}),
    );
    earlier.parent_commit_id = Some("parent-a".to_owned());
    earlier.side = Some("added".to_owned());
    let mut line_later = evidence(
        EvidenceComponent::Diff,
        Some("a"),
        Some(b"a.rs"),
        Some((3, 4)),
        false,
        Some(0.5),
        json!({"id":"line-later"}),
    );
    line_later.parent_commit_id = Some("parent-a".to_owned());
    line_later.side = Some("added".to_owned());
    let mut side_later = evidence(
        EvidenceComponent::Diff,
        Some("a"),
        Some(b"a.rs"),
        Some((1, 2)),
        false,
        Some(0.5),
        json!({"id":"side-later"}),
    );
    side_later.parent_commit_id = Some("parent-a".to_owned());
    side_later.side = Some("removed".to_owned());
    let mut path_later = evidence(
        EvidenceComponent::Diff,
        Some("a"),
        Some(b"b.rs"),
        Some((1, 2)),
        false,
        Some(0.5),
        json!({"id":"path-later"}),
    );
    path_later.parent_commit_id = Some("parent-a".to_owned());
    path_later.side = Some("added".to_owned());
    let mut parent_later = evidence(
        EvidenceComponent::Diff,
        Some("a"),
        Some(b"a.rs"),
        Some((1, 2)),
        false,
        Some(0.5),
        json!({"id":"parent-later"}),
    );
    parent_later.parent_commit_id = Some("parent-b".to_owned());
    parent_later.side = Some("added".to_owned());
    let ranked = rank_evidence(
        vec![
            later,
            parent_later,
            path_later,
            side_later,
            line_later,
            earlier,
        ],
        SearchMode::Semantic,
        TypeSet::Diffs,
        6,
        &HashMap::new(),
    )
    .unwrap();
    let ids = ranked
        .iter()
        .map(|group| group.components[0].payload["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        [
            "earlier",
            "line-later",
            "side-later",
            "path-later",
            "parent-later",
            "later",
        ]
    );
}
