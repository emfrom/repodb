use repodb::worktree::{Layer, WorktreeError, search_exact};
use std::{fs, path::Path, process::Command};
use tempfile::tempdir;

fn git(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn git_ok(root: &Path, args: &[&str]) -> String {
    let output = git(root, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repository() -> (tempfile::TempDir, String) {
    let root = tempdir().unwrap();
    git_ok(root.path(), &["init", "-q"]);
    git_ok(root.path(), &["config", "user.name", "RepoDB test"]);
    git_ok(
        root.path(),
        &["config", "user.email", "repodb@example.test"],
    );
    fs::write(root.path().join("sample.rs"), "head marker\n").unwrap();
    git_ok(root.path(), &["add", "sample.rs"]);
    git_ok(root.path(), &["commit", "-qm", "initial"]);
    let head = git_ok(root.path(), &["rev-parse", "HEAD"]);
    (root, head)
}

#[test]
fn exact_worktree_search_reports_head_staged_and_live_provenance() {
    let (root, head) = repository();
    fs::write(root.path().join("sample.rs"), "staged marker\n").unwrap();
    git_ok(root.path(), &["add", "sample.rs"]);
    fs::write(root.path().join("sample.rs"), "live marker\n").unwrap();
    let status_before = git(root.path(), &["status", "--porcelain=v1", "-z"]).stdout;

    let report = search_exact(root.path(), Some(&head), "marker").unwrap();
    let status_after = git(root.path(), &["status", "--porcelain=v1", "-z"]).stdout;
    assert_eq!(
        status_after, status_before,
        "inspection must not mutate the worktree"
    );

    assert_eq!(report.indexed_head, head);
    assert_eq!(report.matches.len(), 1);
    let found = &report.matches[0];
    assert_eq!(found.path, b"sample.rs");
    assert_eq!(found.text, "live marker");
    assert_eq!(found.line_start, 1);
    assert_eq!(found.head_line_ranges[0].start, 1);
    assert_eq!(found.head_line_ranges[0].end, 1);
    assert_eq!(found.staged_line_ranges[0].start, 1);
    assert_eq!(found.staged_line_ranges[0].end, 1);
    assert_eq!(
        found.provenance,
        vec![Layer::Head, Layer::Staged, Layer::Live]
    );
    assert_eq!(found.head_to_staged_changed, Some(true));
    assert_eq!(found.staged_to_live_changed, Some(true));

    assert!(
        search_exact(root.path(), Some(&head), "staged")
            .unwrap()
            .matches
            .is_empty()
    );
    let live = search_exact(root.path(), Some(&head), "live").unwrap();
    assert_eq!(live.matches[0].provenance, vec![Layer::Live]);
    assert!(live.matches[0].head_line_ranges.is_empty());
    assert!(live.matches[0].staged_line_ranges.is_empty());
}

#[test]
fn exact_worktree_search_includes_git_visible_untracked_and_reports_exclusions() {
    let (root, head) = repository();
    fs::write(root.path().join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(root.path().join("new.rs"), "untracked marker\n").unwrap();
    fs::write(root.path().join("ignored.rs"), "ignored marker\n").unwrap();
    fs::write(root.path().join("photo.png"), b"not text marker").unwrap();
    fs::write(root.path().join("binary.rs"), b"binary\0 marker").unwrap();

    let report = search_exact(root.path(), Some(&head), "marker").unwrap();

    assert_eq!(report.matches.len(), 1);
    assert_eq!(report.matches[0].path, b"new.rs");
    assert!(report.matches[0].untracked);
    assert_eq!(report.matches[0].provenance, vec![Layer::Live]);
    assert!(report.coverage.ignored_files_excluded >= 1);
    assert!(report.coverage.unsupported_files_excluded >= 1);
    assert_eq!(report.coverage.binary_files_excluded, 1);
}

#[test]
fn exact_worktree_search_removes_head_matches_when_the_live_file_is_deleted() {
    let (root, head) = repository();
    fs::remove_file(root.path().join("sample.rs")).unwrap();

    let report = search_exact(root.path(), Some(&head), "marker").unwrap();

    assert!(report.matches.is_empty());
    assert_eq!(report.updated_paths, vec![b"sample.rs".to_vec()]);
    assert_eq!(report.coverage.removed_files, 1);
}

#[test]
fn ignored_files_staged_for_removal_are_not_mistaken_for_live_text() {
    let (root, _) = repository();
    fs::write(root.path().join(".gitignore"), "sample.rs\n").unwrap();
    git_ok(root.path(), &["add", ".gitignore"]);
    git_ok(root.path(), &["commit", "-qm", "ignore future paths"]);
    let head = git_ok(root.path(), &["rev-parse", "HEAD"]);
    git_ok(root.path(), &["rm", "--cached", "--quiet", "sample.rs"]);

    let report = search_exact(root.path(), Some(&head), "marker").unwrap();

    assert!(report.matches.is_empty());
    assert!(report.coverage.ignored_files_excluded >= 1);
}

#[test]
fn exact_worktree_search_rejects_unindexed_and_stale_heads() {
    let (root, _head) = repository();

    assert!(matches!(
        search_exact(root.path(), None, "marker"),
        Err(WorktreeError::UnindexedHead { .. })
    ));
    assert!(matches!(
        search_exact(root.path(), Some("another-commit"), "marker"),
        Err(WorktreeError::StaleHead { .. })
    ));
}

#[test]
fn semantic_ranges_map_across_staged_and_live_line_shifts_without_new_embeddings() {
    let (root, _head) = repository();
    fs::write(
        root.path().join("sample.rs"),
        "fn before() {}\nfn target() {}\nfn middle() {}\n",
    )
    .unwrap();
    git_ok(root.path(), &["add", "sample.rs"]);
    git_ok(root.path(), &["commit", "-qm", "semantic fixture"]);
    let head = git_ok(root.path(), &["rev-parse", "HEAD"]);
    fs::write(
        root.path().join("sample.rs"),
        "fn before() {}\n// staged prefix\nfn target() {}\nfn middle() {}\n",
    )
    .unwrap();
    git_ok(root.path(), &["add", "sample.rs"]);
    fs::write(
        root.path().join("sample.rs"),
        "fn before() {}\n// staged prefix\nfn live_change() {}\nfn middle() {}\n",
    )
    .unwrap();
    fs::write(root.path().join("staged-new.rs"), "staged addition\n").unwrap();
    git_ok(root.path(), &["add", "staged-new.rs"]);
    fs::write(root.path().join("untracked-new.rs"), "untracked addition\n").unwrap();

    let report = repodb::worktree::map_semantic_ranges(
        root.path(),
        Some(&head),
        &[repodb::worktree::IndexedRange {
            path: b"sample.rs".to_vec(),
            line_start: 2,
            line_end: 2,
        }],
    )
    .unwrap();

    let mapped = &report.mappings[0];
    assert_eq!(
        mapped.mapping_status,
        repodb::worktree::MappingStatus::Mapped
    );
    assert_eq!(mapped.head_text, "fn target() {}");
    assert_eq!(mapped.staged_line_start, Some(3));
    assert_eq!(mapped.live_line_start, Some(3));
    assert_eq!(mapped.live_text.as_deref(), Some("fn live_change() {}"));
    assert_eq!(mapped.changed_live_text, Some(true));
    assert_eq!(report.coverage.unembedded_addition_files, 2);
}

#[test]
fn semantic_ranges_report_deleted_and_ambiguous_without_guessing() {
    let (root, _) = repository();
    fs::write(
        root.path().join("sample.rs"),
        "fn target() {}\nfn other() {}\n",
    )
    .unwrap();
    git_ok(root.path(), &["add", "sample.rs"]);
    git_ok(root.path(), &["commit", "-qm", "semantic fixture"]);
    let head = git_ok(root.path(), &["rev-parse", "HEAD"]);
    fs::write(root.path().join("sample.rs"), "fn other() {}\n").unwrap();
    let deleted = repodb::worktree::map_semantic_ranges(
        root.path(),
        Some(&head),
        &[repodb::worktree::IndexedRange {
            path: b"sample.rs".to_vec(),
            line_start: 1,
            line_end: 1,
        }],
    )
    .unwrap();
    assert_eq!(
        deleted.mappings[0].mapping_status,
        repodb::worktree::MappingStatus::Deleted
    );
    assert_eq!(deleted.mappings[0].head_text, "fn target() {}");
    assert!(deleted.mappings[0].live_text.is_none());

    fs::write(
        root.path().join("sample.rs"),
        "fn target_part_one() {}\nfn target_part_two() {}\nfn other() {}\n",
    )
    .unwrap();
    let ambiguous = repodb::worktree::map_semantic_ranges(
        root.path(),
        Some(&head),
        &[repodb::worktree::IndexedRange {
            path: b"sample.rs".to_vec(),
            line_start: 1,
            line_end: 1,
        }],
    )
    .unwrap();
    assert_eq!(
        ambiguous.mappings[0].mapping_status,
        repodb::worktree::MappingStatus::Ambiguous
    );
    assert!(ambiguous.mappings[0].live_text.is_none());
}

#[test]
fn semantic_range_restored_after_staged_deletion_is_ambiguous() {
    let (root, _) = repository();
    fs::write(root.path().join("sample.rs"), "fn target() {}\n").unwrap();
    git_ok(root.path(), &["add", "sample.rs"]);
    git_ok(root.path(), &["commit", "-qm", "semantic fixture"]);
    let head = git_ok(root.path(), &["rev-parse", "HEAD"]);
    git_ok(root.path(), &["rm", "sample.rs"]);
    fs::write(root.path().join("sample.rs"), "fn target() {}\n").unwrap();

    let report = repodb::worktree::map_semantic_ranges(
        root.path(),
        Some(&head),
        &[repodb::worktree::IndexedRange {
            path: b"sample.rs".to_vec(),
            line_start: 1,
            line_end: 1,
        }],
    )
    .unwrap();
    assert_eq!(
        report.mappings[0].mapping_status,
        repodb::worktree::MappingStatus::Ambiguous
    );
    assert!(report.mappings[0].live_text.is_none());
}

#[test]
fn worktree_exact_results_have_a_reported_per_file_limit() {
    let (root, head) = repository();
    let changed = (0..1_100).map(|_| "marker\n").collect::<String>();
    fs::write(root.path().join("sample.rs"), changed).unwrap();

    let report = search_exact(root.path(), Some(&head), "marker").unwrap();

    assert_eq!(report.matches.len(), 1_000);
    assert_eq!(report.coverage.match_limit_reached_files, 1);
    assert!(!report.coverage.total_match_limit_reached);
}

#[cfg(unix)]
#[test]
fn a_file_removed_after_git_discovery_is_reported_as_best_effort_coverage() {
    use std::os::unix::fs::PermissionsExt;

    let (root, head) = repository();
    fs::write(root.path().join("sample.rs"), "changed marker\n").unwrap();
    let temp = tempdir().unwrap();
    let shim_dir = temp.path().join("bin");
    fs::create_dir(&shim_dir).unwrap();
    let real_git = Command::new("which").arg("git").output().unwrap();
    let real_git = String::from_utf8(real_git.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let shim = shim_dir.join("git");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\"{real_git}\" \"$@\"\nstatus=$?\ncase \"$*\" in *'diff --no-renames --name-only'*) rm -- \"$REPODB_RACE_FILE\" 2>/dev/null || true ;; esac\nexit $status\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var("PATH").unwrap_or_default();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "worktree_race_child", "--ignored", "--nocapture"])
        .env("PATH", format!("{}:{old_path}", shim_dir.display()))
        .env("REPODB_RACE_ROOT", root.path())
        .env("REPODB_RACE_HEAD", head)
        .env("REPODB_RACE_FILE", root.path().join("sample.rs"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "race test subprocess failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore]
fn worktree_race_child() {
    let (Ok(root), Ok(head)) = (
        std::env::var("REPODB_RACE_ROOT"),
        std::env::var("REPODB_RACE_HEAD"),
    ) else {
        return;
    };
    let report = search_exact(Path::new(&root), Some(&head), "marker").unwrap();
    assert!(report.matches.is_empty());
    assert_eq!(report.coverage.removed_files, 1);
}
