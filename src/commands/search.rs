use std::{error::Error, path::Path};

use crate::*;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(crate) fn run(db_path: &Path, raw_arguments: &[String]) -> Result<()> {
    let mut a = raw_arguments.iter().cloned();
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
                result_scope = SearchScope::parse(&a.next().ok_or("--scope requires a value")?)?;
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
                selected_types = TypeSet::parse(&a.next().ok_or("--type requires a value")?)
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
    let mut connection = open_db(db_path)?;
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
            history::search_all_occurrences(&c, &q, mode, usize::MAX, &commits, include_result)?
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
            let indexed_exact_match = candidate.text.to_lowercase().contains(&q.to_lowercase());
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
                candidate.line_end = mapped
                    .live_line_end
                    .ok_or_else(|| "mapped semantic range omitted its live line end".to_owned())?;
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
            history::DiffSide::Added => (reference.new_path.as_deref(), &reference.child_commit_id),
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
                    if matches!(result_scope, SearchScope::All) && history_path_filter.is_none() {
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
                    let exact = search_diff_results(history::DiffSearchMode::Exact, usize::MAX)?;
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
    Ok(())
}
