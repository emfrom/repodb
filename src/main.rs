use rusqlite::{Connection, params};
use serde::Serialize;
use std::{env, error::Error, path::PathBuf, process::Command};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Serialize)]
struct SearchResult {
    path: String,
    line_start: usize,
    line_end: usize,
    text: String,
}

fn git(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git").args(args).output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output.stdout)
}

fn root() -> Result<PathBuf> {
    let bytes = git(&["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(String::from_utf8(bytes)?.trim()))
}

fn database() -> Result<PathBuf> {
    let git_dir = String::from_utf8(git(&["rev-parse", "--git-dir"])?)?;
    let path = PathBuf::from(git_dir.trim());
    let path = if path.is_absolute() {
        path
    } else {
        root()?.join(path)
    };
    Ok(path.join("repodb.sqlite"))
}

fn open_db() -> Result<Connection> {
    let path = database()?;
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS indexed_files (
            path TEXT PRIMARY KEY NOT NULL,
            commit_id TEXT NOT NULL,
            content TEXT NOT NULL
        );",
    )?;
    Ok(connection)
}

fn refresh(connection: &mut Connection) -> Result<()> {
    let commit_id = String::from_utf8(git(&["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned();
    let tree = git(&["ls-tree", "-r", "-z", "--full-tree", "HEAD"])?;
    let mut files = Vec::new();
    for record in tree
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let metadata = String::from_utf8_lossy(&record[..tab]);
        let mut fields = metadata.split_whitespace();
        let _mode = fields.next();
        let kind = fields.next();
        let object_id = fields.next();
        let Some(path) = std::str::from_utf8(&record[tab + 1..]).ok() else {
            continue;
        };
        if kind != Some("blob") || path.starts_with(".git/") {
            continue;
        }
        let Some(object_id) = object_id else { continue };
        let blob = git(&["cat-file", "blob", object_id])?;
        if blob.contains(&0) {
            continue;
        }
        let Ok(content) = String::from_utf8(blob) else {
            continue;
        };
        files.push((path.to_owned(), content));
    }

    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM indexed_files", [])?;
    {
        let mut insert = transaction
            .prepare("INSERT INTO indexed_files(path, commit_id, content) VALUES (?1, ?2, ?3)")?;
        for (path, content) in files {
            insert.execute(params![path, commit_id, content])?;
        }
    }
    transaction.commit()?;
    Ok(())
}

fn search(connection: &Connection, query: &str) -> Result<Vec<SearchResult>> {
    let needle = query.to_lowercase();
    let mut statement =
        connection.prepare("SELECT path, content FROM indexed_files ORDER BY path")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut matches = Vec::new();
    for row in rows {
        let (path, content) = row?;
        for (line_index, line) in content.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                matches.push(SearchResult {
                    path: path.clone(),
                    line_start: line_index + 1,
                    line_end: line_index + 1,
                    text: line.to_owned(),
                });
            }
        }
    }
    Ok(matches)
}

fn execute() -> Result<()> {
    let mut arguments = env::args().skip(1);
    let Some(command) = arguments.next() else {
        return Err("usage: repodb <init|refresh|search> [query] [--json]".into());
    };
    match command.as_str() {
        "init" | "refresh" => {
            if arguments.next().is_some() {
                return Err(format!("unexpected arguments for {command}").into());
            }
            let mut connection = open_db()?;
            refresh(&mut connection)?;
        }
        "search" => {
            let mut query = None;
            let mut json = false;
            for argument in arguments {
                if argument == "--json" {
                    json = true;
                } else if query.replace(argument).is_some() {
                    return Err("search accepts one query".into());
                }
            }
            let query = query.ok_or("search requires a query")?;
            let connection = open_db()?;
            let matches = search(&connection, &query)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&matches)?);
            } else {
                for result in matches {
                    println!(
                        "{}:{}-{}: {}",
                        result.path, result.line_start, result.line_end, result.text
                    );
                }
            }
        }
        _ => return Err(format!("unknown command: {command}").into()),
    }
    Ok(())
}

fn main() {
    if let Err(error) = execute() {
        eprintln!("repodb: {error}");
        std::process::exit(2);
    }
}
