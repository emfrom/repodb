use std::{error::Error, path::Path};

use crate::{budget, history, imports, open_db_for_refresh};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(crate) fn run(db_path: &Path, cmd: &str, raw_arguments: &[String]) -> Result<()> {
    let a = raw_arguments.iter().cloned();
    let mut arguments = Vec::new();
    let mut size_limit = imports::SizeLimit::Error;
    let mut size_limit_supplied = false;
    for argument in a {
        if let Some(value) = argument.strip_prefix("--size-limit=") {
            if size_limit_supplied {
                return Err("--size-limit may be specified only once".into());
            }
            size_limit_supplied = true;
            size_limit = match value {
                "error" => imports::SizeLimit::Error,
                "warn" => imports::SizeLimit::Warn,
                "ignore" => imports::SizeLimit::Ignore,
                _ => return Err("--size-limit must be error, warn, or ignore".into()),
            };
        } else {
            arguments.push(argument);
        }
    }
    let imports_only = arguments.len() == 1 && arguments[0] == "--imports";
    let changed = arguments.len() == 1 && arguments[0] == "--changed";
    let update = if arguments.len() == 2 && arguments[0] == "--update" {
        Some(history::resolve_update_commit(&arguments[1])?)
    } else {
        None
    };
    if (!arguments.is_empty() && !imports_only && !changed && update.is_none())
        || (cmd == "init" && (imports_only || changed || update.is_some()))
        || (changed && update.is_some())
        || (imports_only && (changed || update.is_some()))
        || (size_limit_supplied && (changed || update.is_some()))
    {
        return Err(
        "usage: repodb refresh [--imports | --changed | --update COMMIT] [--size-limit=error|warn|ignore]".into(),
    );
    }
    let _refresh_deadline = budget::RefreshDeadline::start()?;
    let mut db = open_db_for_refresh(db_path)?;
    let mut size_assessments = Vec::new();
    let excluded_database_path = if imports_only {
        None
    } else {
        history::database_git_path(db_path)?
    };
    if imports_only {
        let version = history::snapshot_version(&db)?;
        size_assessments = imports::refresh_with_size_limit(&db, size_limit)?;
        budget::check_refresh_deadline()?;
        imports::validate_dimensions(&db)?;
        let tx = history::publish_transaction(&mut db, version)?;
        imports::publish(&tx)?;
        budget::check_refresh_deadline()?;
        tx.commit()?;
    } else if changed {
        history::refresh_changed_excluding(&mut db, excluded_database_path.as_deref())?;
        budget::check_refresh_deadline()?;
    } else if let Some(commit_id) = update {
        history::refresh_update_excluding(&mut db, &commit_id, excluded_database_path.as_deref())?;
        budget::check_refresh_deadline()?;
    } else {
        let version = history::snapshot_version(&db)?;
        size_assessments = imports::refresh_with_size_limit(&db, size_limit)?;
        budget::check_refresh_deadline()?;
        history::prepare_refresh_excluding(&db, excluded_database_path.as_deref())?;
        budget::check_refresh_deadline()?;
        imports::validate_dimensions(&db)?;
        let tx = history::publish_transaction(&mut db, version)?;
        imports::publish(&tx)?;
        history::publish_refresh(&tx)?;
        budget::check_refresh_deadline()?;
        tx.commit()?;
    }
    imports::print_pruning_report(&db)?;
    for assessment in size_assessments
        .into_iter()
        .filter(|assessment| assessment.warn_large)
    {
        println!(
            "large import warning: source bytes={}, estimated net database growth={}, warning basis={} ({})",
            assessment.source_bytes,
            assessment
                .estimated_net_db_growth_bytes
                .map_or_else(|| "unknown".into(), |value| value.to_string()),
            assessment.warning_basis_bytes,
            assessment.warning_basis,
        );
    }
    Ok(())
}
