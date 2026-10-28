use std::{error::Error, path::Path};

use crate::{config, history, imports};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(crate) fn run(db_path: &Path, arguments: &[String]) -> Result<()> {
    let first = arguments
        .first()
        .ok_or("import requires one local file, URL, or Git repository")?;
    if first == "--preview" {
        let path = arguments
            .get(1)
            .ok_or("import --preview requires a local file, directory, or HTTP(S) page URL")?;
        if arguments.len() != 2 {
            return Err(
                "import --preview accepts one local file, directory, or HTTP(S) page URL".into(),
            );
        }
        let preview = if path.starts_with("http://") || path.starts_with("https://") {
            if path.ends_with(".git") {
                return Err("import --preview does not preview Git repositories".into());
            }
            imports::preview_url(path, db_path)?
        } else {
            imports::preview(Path::new(path), db_path)?
        };
        println!("{}", serde_json::to_string(&preview)?);
        return Ok(());
    }

    let (revision, locator) = if first == "--revision" {
        let revision = arguments
            .get(1)
            .ok_or("import --revision requires a branch, tag, or commit")?;
        let locator = arguments
            .get(2)
            .ok_or("import --revision requires one Git repository")?;
        (Some(revision), locator)
    } else {
        (None, first)
    };
    if arguments.len() != if revision.is_some() { 3 } else { 1 } {
        return Err("import accepts one file, URL, or Git repository".into());
    }
    imports::register(
        &crate::open_db(db_path)?,
        locator,
        revision.map(String::as_str),
    )?;
    Ok(())
}

pub(crate) fn reembed(db_path: &Path, arguments: &[String]) -> Result<()> {
    let mut command = config::embedding_command()?;
    let mut model = config::DEFAULT_EMBEDDING_MODEL.to_owned();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--command" => {
                index += 1;
                command = arguments
                    .get(index)
                    .ok_or("--command requires path")?
                    .clone();
            }
            "--model" => {
                index += 1;
                model = arguments.get(index).ok_or("--model requires id")?.clone();
            }
            other => return Err(format!("unexpected argument {other}").into()),
        }
        index += 1;
    }
    let mut db = crate::open_db(db_path)?;
    if history::is_current(&db)? {
        history::reembed(&mut db, &command, &model)?;
    } else {
        crate::reembed(&mut db, &command, &model)?;
    }
    Ok(())
}
