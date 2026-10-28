use std::{error::Error, path::Path};

use rusqlite::Connection;
use serde::Serialize;

use crate::{config, git, resolved_executable};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Serialize)]
struct CapabilityReadiness {
    ready: bool,
    reason: Option<String>,
}

fn capability(ready: bool, reason: Option<String>) -> CapabilityReadiness {
    CapabilityReadiness { ready, reason }
}

pub(crate) fn run(db_path: &Path, arguments: &[String]) -> Result<()> {
    let json = match arguments {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err("usage: repodb status [--json]".into()),
    };
    let report = report(db_path)?;
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
    Ok(())
}

fn report(db_path: &Path) -> Result<serde_json::Value> {
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
    let live_head = std::process::Command::new("git")
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
            let units = c.query_row("SELECT count(*) FROM indexed_units", [], |r| {
                r.get::<_, i64>(0)
            })?;
            let unsupported = c.query_row(
                "SELECT count(*) FROM blob_contents WHERE status != 'text'",
                [],
                |r| r.get::<_, i64>(0),
            )?;
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
