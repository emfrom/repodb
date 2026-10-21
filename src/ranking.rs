use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{cmp::Ordering, collections::HashMap};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceComponent {
    Content,
    Diff,
    CommitMessage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeSet {
    All,
    Content,
    Cmdiffs,
    Diffs,
    Cmcommits,
    Commits,
}

impl TypeSet {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "all" => Ok(Self::All),
            "content" => Ok(Self::Content),
            "cmdiffs" => Ok(Self::Cmdiffs),
            "diffs" => Ok(Self::Diffs),
            "cmcommits" => Ok(Self::Cmcommits),
            "commits" => Ok(Self::Commits),
            _ => Err(format!(
                "type must be all, content, cmdiffs, diffs, cmcommits, or commits (got {value})"
            )),
        }
    }

    pub fn components(self) -> &'static [EvidenceComponent] {
        use EvidenceComponent::{CommitMessage as Message, Content, Diff};
        match self {
            Self::All => &[Content, Diff, Message],
            Self::Content => &[Content, Diff],
            Self::Cmdiffs => &[Message, Diff],
            Self::Diffs => &[Diff],
            Self::Cmcommits => &[Message, Content],
            Self::Commits => &[Message],
        }
    }

    fn includes(self, component: EvidenceComponent) -> bool {
        self.components().contains(&component)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchMode {
    Exact,
    Semantic,
    Combined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankingMethod {
    SemanticDistance,
    Deterministic,
}

/// One matching item from content, a diff side, or a commit message.
/// `commit_id` identifies the view/path for overlap grouping; for diffs it is
/// the commit containing the represented side. `change_commit_id` identifies
/// the child commit changed by a diff or described by a commit message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceCandidate {
    pub component: EvidenceComponent,
    /// Origin identity; include the source/version identity for imported evidence.
    pub source_id: String,
    /// Stable identity of this component's reference (unit, hunk, or message unit).
    pub reference_id: String,
    pub commit_id: Option<String>,
    pub change_commit_id: Option<String>,
    pub parent_commit_id: Option<String>,
    pub path: Option<Vec<u8>>,
    pub side: Option<String>,
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
    pub exact: bool,
    pub distance: Option<f64>,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RankedEvidence {
    pub components: Vec<EvidenceCandidate>,
    pub primary: EvidenceComponent,
    /// Which matching component supplied the best available distance, if any.
    pub score_source: Option<EvidenceComponent>,
    pub ranking_method: RankingMethod,
    pub distance: Option<f64>,
    pub exact_match: bool,
    /// For a matching message, references to every indexed parent diff for its commit.
    pub related_diffs: Vec<Value>,
}

#[derive(Clone, Debug)]
struct EvidenceGroup {
    components: Vec<EvidenceCandidate>,
    related_diffs: Vec<Value>,
    primary: EvidenceComponent,
    score_source: Option<EvidenceComponent>,
    ranking_method: RankingMethod,
    distance: Option<f64>,
    exact_match: bool,
}

/// Apply result-type selection, overlap grouping, message-to-change lifting and
/// deterministic ranking to already-filtered backend evidence.
///
/// `related_diffs` is keyed by `(source_id, commit_id)` and should contain all
/// parent-side diff references for that commit, not only references that matched
/// the query. Exclusions/scope filters must be applied before invoking this API.
pub fn rank_evidence(
    candidates: Vec<EvidenceCandidate>,
    mode: SearchMode,
    type_set: TypeSet,
    limit: usize,
    related_diffs: &HashMap<(String, String), Vec<Value>>,
) -> Result<Vec<RankedEvidence>, String> {
    if limit == 0 {
        return Err("result limit must be greater than zero".to_owned());
    }
    let mut candidates = candidates
        .into_iter()
        .filter(|candidate| type_set.includes(candidate.component))
        .filter(|candidate| match mode {
            SearchMode::Exact => candidate.exact,
            SearchMode::Semantic => candidate.distance.is_some(),
            SearchMode::Combined => candidate.exact || candidate.distance.is_some(),
        })
        .collect::<Vec<_>>();
    if candidates
        .iter()
        .filter_map(|candidate| candidate.distance)
        .any(|distance| !distance.is_finite())
    {
        return Err("semantic distance must be finite".to_owned());
    }
    candidates.sort_by(stable_candidate_order);

    let mut groups = Vec::<EvidenceGroup>::new();
    for candidate in candidates {
        let matches = groups
            .iter()
            .enumerate()
            .filter_map(|(index, group)| {
                let duplicate = group
                    .components
                    .iter()
                    .any(|other| same_reference(&candidate, other));
                (duplicate || (type_set == TypeSet::All && group_matches(&candidate, group)))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        if let Some((&target, rest)) = matches.split_first() {
            absorb_candidate(&mut groups[target], candidate);
            for index in rest.iter().rev() {
                let other = groups.remove(*index);
                for candidate in other.components {
                    absorb_candidate(&mut groups[target], candidate);
                }
            }
        } else {
            groups.push(EvidenceGroup {
                primary: candidate.component,
                components: vec![candidate],
                related_diffs: Vec::new(),
                score_source: None,
                ranking_method: RankingMethod::Deterministic,
                distance: None,
                exact_match: false,
            });
        }
    }

    for group in &mut groups {
        group.components.sort_by(stable_candidate_order);
        if let Some(message) = group
            .components
            .iter()
            .find(|candidate| candidate.component == EvidenceComponent::CommitMessage)
        {
            let commit_id = message
                .change_commit_id
                .as_deref()
                .or(message.commit_id.as_deref());
            if let Some(commit_id) = commit_id {
                let key = (message.source_id.clone(), commit_id.to_owned());
                if let Some(references) = related_diffs.get(&key) {
                    group.related_diffs = references.clone();
                    group.related_diffs.sort_by_key(Value::to_string);
                    group.related_diffs.dedup();
                }
            }
        }
        group.primary = if type_set == TypeSet::All
            && (!group.related_diffs.is_empty()
                || group
                    .components
                    .iter()
                    .any(|candidate| candidate.component == EvidenceComponent::Diff))
        {
            EvidenceComponent::Diff
        } else if group
            .components
            .iter()
            .any(|candidate| candidate.component == EvidenceComponent::CommitMessage)
        {
            EvidenceComponent::CommitMessage
        } else if group
            .components
            .iter()
            .any(|candidate| candidate.component == EvidenceComponent::Diff)
        {
            EvidenceComponent::Diff
        } else {
            EvidenceComponent::Content
        };
        let best_score = group
            .components
            .iter()
            .filter_map(|candidate| {
                candidate
                    .distance
                    .map(|distance| (distance, candidate.component))
            })
            .min_by(|left, right| left.0.total_cmp(&right.0).then(left.1.cmp(&right.1)));
        group.distance = best_score.map(|score| score.0);
        group.score_source = best_score.map(|score| score.1);
        group.exact_match = group.components.iter().any(|candidate| candidate.exact);
        group.ranking_method = if group.distance.is_some() {
            RankingMethod::SemanticDistance
        } else {
            RankingMethod::Deterministic
        };
    }

    let scored = mode != SearchMode::Exact || groups.iter().any(|group| group.distance.is_some());
    groups.sort_by(|left, right| group_order(left, right, scored));

    let mut required = Vec::<usize>::new();
    if mode == SearchMode::Combined {
        if let Some(index) = groups.iter().position(|group| group.exact_match) {
            required.push(index);
        }
        if let Some(index) = groups
            .iter()
            .position(|group| !group.exact_match && group.distance.is_some())
            && !required.contains(&index)
        {
            required.push(index);
        }
    }
    if type_set == TypeSet::All
        && let Some(index) = groups.iter().position(|group| {
            group
                .components
                .iter()
                .any(|candidate| candidate.component == EvidenceComponent::Diff)
                && !group
                    .components
                    .iter()
                    .any(|candidate| candidate.component == EvidenceComponent::Content)
        })
        && !required.contains(&index)
    {
        required.push(index);
    }
    if required.len() > limit {
        return Err(format!(
            "result limit {limit} cannot include all required exact, semantic-only, and distinct diff evidence"
        ));
    }

    let mut selected = required;
    for index in 0..groups.len() {
        if selected.len() == limit {
            break;
        }
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    selected.sort_by(|left, right| group_order(&groups[*left], &groups[*right], scored));
    Ok(selected
        .into_iter()
        .map(|index| {
            let group = groups[index].clone();
            RankedEvidence {
                components: group.components,
                primary: group.primary,
                score_source: group.score_source,
                ranking_method: group.ranking_method,
                distance: group.distance,
                exact_match: group.exact_match,
                related_diffs: group.related_diffs,
            }
        })
        .collect())
}

fn same_reference(left: &EvidenceCandidate, right: &EvidenceCandidate) -> bool {
    left.component == right.component
        && left.source_id == right.source_id
        && left.reference_id == right.reference_id
        && left.commit_id == right.commit_id
        && left.change_commit_id == right.change_commit_id
        && left.parent_commit_id == right.parent_commit_id
        && left.path == right.path
        && left.side == right.side
        && left.line_start == right.line_start
        && left.line_end == right.line_end
}

fn absorb_candidate(group: &mut EvidenceGroup, candidate: EvidenceCandidate) {
    if let Some(existing) = group
        .components
        .iter_mut()
        .find(|existing| same_reference(existing, &candidate))
    {
        let distance = match (existing.distance, candidate.distance) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(distance), None) | (None, Some(distance)) => Some(distance),
            (None, None) => None,
        };
        let exact = existing.exact || candidate.exact;
        if candidate.exact && !existing.exact {
            *existing = candidate;
        }
        existing.exact = exact;
        existing.distance = distance;
    } else {
        group.components.push(candidate);
    }
}

fn group_matches(candidate: &EvidenceCandidate, group: &EvidenceGroup) -> bool {
    group.components.iter().any(|other| {
        if candidate.source_id != other.source_id {
            return false;
        }
        match (candidate.component, other.component) {
            (EvidenceComponent::Content, EvidenceComponent::Diff)
            | (EvidenceComponent::Diff, EvidenceComponent::Content) => {
                ranges_overlap(candidate, other)
            }
            (EvidenceComponent::CommitMessage, EvidenceComponent::Diff)
            | (EvidenceComponent::Diff, EvidenceComponent::CommitMessage) => {
                candidate_change_commit(candidate) == candidate_change_commit(other)
            }
            (EvidenceComponent::CommitMessage, EvidenceComponent::CommitMessage) => {
                candidate_change_commit(candidate) == candidate_change_commit(other)
            }
            _ => false,
        }
    })
}

fn candidate_change_commit(candidate: &EvidenceCandidate) -> Option<&str> {
    candidate
        .change_commit_id
        .as_deref()
        .or(candidate.commit_id.as_deref())
}

fn ranges_overlap(left: &EvidenceCandidate, right: &EvidenceCandidate) -> bool {
    left.commit_id == right.commit_id
        && left.path == right.path
        && left.path.is_some()
        && match (
            left.line_start,
            left.line_end,
            right.line_start,
            right.line_end,
        ) {
            (Some(left_start), Some(left_end), Some(right_start), Some(right_end)) => {
                left_start <= right_end && right_start <= left_end
            }
            _ => false,
        }
}

fn stable_candidate_order(left: &EvidenceCandidate, right: &EvidenceCandidate) -> Ordering {
    left.source_id
        .cmp(&right.source_id)
        .then(left.commit_id.cmp(&right.commit_id))
        .then(left.parent_commit_id.cmp(&right.parent_commit_id))
        .then(left.path.cmp(&right.path))
        .then(left.side.cmp(&right.side))
        .then(left.line_start.cmp(&right.line_start))
        .then(left.line_end.cmp(&right.line_end))
        .then(left.component.cmp(&right.component))
        .then(left.reference_id.cmp(&right.reference_id))
        .then(left.payload.to_string().cmp(&right.payload.to_string()))
}

fn group_order(left: &EvidenceGroup, right: &EvidenceGroup, use_distance: bool) -> Ordering {
    let score_order = if use_distance {
        match (left.distance, right.distance) {
            (Some(left), Some(right)) => left.total_cmp(&right),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    } else {
        Ordering::Equal
    };
    score_order.then_with(|| {
        left.components
            .iter()
            .min_by(|a, b| stable_candidate_order(a, b))
            .zip(
                right
                    .components
                    .iter()
                    .min_by(|a, b| stable_candidate_order(a, b)),
            )
            .map_or(Ordering::Equal, |(a, b)| stable_candidate_order(a, b))
    })
}
