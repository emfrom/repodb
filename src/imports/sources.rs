use crate::Result;
use rusqlite::{Connection, params};
use std::{path::Path, process::Command};

pub(super) fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS imported_sources(id TEXT PRIMARY KEY, locator TEXT NOT NULL UNIQUE, kind TEXT NOT NULL, object_store TEXT); CREATE TABLE IF NOT EXISTS imported_versions(source_id TEXT NOT NULL, version TEXT NOT NULL, content TEXT NOT NULL, fetched_order INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(source_id,version)); CREATE TABLE IF NOT EXISTS imported_units(source_id TEXT NOT NULL, version TEXT NOT NULL, line INTEGER NOT NULL, text TEXT NOT NULL, vector BLOB, page INTEGER, line_end INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(source_id,version,line,text)); CREATE TABLE IF NOT EXISTS import_config(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS imported_git_units(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,blob_id TEXT NOT NULL,segmentation_kind TEXT NOT NULL,byte_start INTEGER NOT NULL,byte_end INTEGER NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,byte_start));")?;
    if !column_exists(c, "imported_versions", "representation_sha256")? {
        c.execute(
            "ALTER TABLE imported_versions ADD COLUMN representation_sha256 TEXT",
            [],
        )?;
        let rows = {
            let mut statement = c.prepare(
                "SELECT source_id,version,content FROM imported_versions WHERE content<>''",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (source, version, content) in rows {
            c.execute("UPDATE imported_versions SET representation_sha256=?3 WHERE source_id=?1 AND version=?2", params![source, version, super::cache::checksum(content.as_bytes())])?;
        }
    }
    {
        let mut statement = c.prepare("SELECT content,representation_sha256 FROM imported_versions WHERE representation_sha256 IS NOT NULL")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (content, expected) = row?;
            if super::cache::checksum(content.as_bytes()) != expected {
                return Err("imported version representation hash mismatch".into());
            }
        }
    }
    if !column_exists(c, "imported_sources", "object_store")? {
        c.execute(
            "ALTER TABLE imported_sources ADD COLUMN object_store TEXT",
            [],
        )?;
    }
    if !column_exists(c, "imported_units", "page")? {
        c.execute("ALTER TABLE imported_units ADD COLUMN page INTEGER", [])?;
    }
    if !column_exists(c, "imported_units", "line_end")? {
        c.execute(
            "ALTER TABLE imported_units ADD COLUMN line_end INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        c.execute("UPDATE imported_units SET line_end=line", [])?;
    }
    if !column_exists(c, "imported_git_units", "blob_id")?
        && !column_exists(c, "imported_git_units", "line_end")?
    {
        c.execute(
            "ALTER TABLE imported_git_units ADD COLUMN line_end INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        c.execute("UPDATE imported_git_units SET line_end=line", [])?;
    }
    if !primary_key_matches(
        c,
        "imported_units",
        &["source_id", "version", "line", "text"],
    )? {
        c.execute_batch("CREATE TABLE imported_units_new(source_id TEXT NOT NULL,version TEXT NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,page INTEGER,PRIMARY KEY(source_id,version,line,text)); INSERT INTO imported_units_new SELECT source_id,version,line,line_end,text,vector,page FROM imported_units; DROP TABLE imported_units; ALTER TABLE imported_units_new RENAME TO imported_units;")?;
    }
    if !column_exists(c, "imported_git_units", "blob_id")?
        && !primary_key_matches(
            c,
            "imported_git_units",
            &["source_id", "commit_id", "path", "line", "text"],
        )?
    {
        c.execute_batch("CREATE TABLE imported_git_units_new(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,line,text)); INSERT INTO imported_git_units_new SELECT source_id,commit_id,path,line,line_end,text,vector FROM imported_git_units; DROP TABLE imported_git_units; ALTER TABLE imported_git_units_new RENAME TO imported_git_units;")?;
    }
    if !column_exists(c, "imported_versions", "fetched_order")? {
        c.execute(
            "ALTER TABLE imported_versions ADD COLUMN fetched_order INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        c.execute("UPDATE imported_versions SET fetched_order=rowid", [])?;
    }
    c.execute(
        "INSERT OR IGNORE INTO import_config VALUES('retention','latest')",
        [],
    )?;
    Ok(())
}

fn primary_key_matches(c: &Connection, table: &str, expected: &[&str]) -> Result<bool> {
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(1)?, row.get::<_, usize>(5)?))
    })?;
    let mut columns = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    columns.retain(|(_, order)| *order > 0);
    columns.sort_by_key(|(_, order)| *order);
    Ok(columns
        .iter()
        .map(|(name, _)| name.as_str())
        .eq(expected.iter().copied()))
}

pub(super) fn column_exists(c: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(names
        .collect::<std::result::Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column))
}

fn identity(locator: &str) -> String {
    // Stable, reversible identity is not exposed as a filesystem path.
    format!(
        "import:{}",
        locator
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

pub(crate) fn register(c: &Connection, locator: &str) -> Result<()> {
    let is_remote_git = locator.starts_with("ssh://")
        || locator.starts_with("git@")
        || locator.starts_with("git://")
        || locator.starts_with("file://")
        || locator.starts_with("http://") && locator.ends_with(".git")
        || locator.starts_with("https://") && locator.ends_with(".git");
    let is_url =
        !is_remote_git && (locator.starts_with("http://") || locator.starts_with("https://"));
    let path = Path::new(locator);
    let is_git = is_remote_git
        || path.is_dir()
            && Command::new("git")
                .args(["-C", locator, "rev-parse", "--git-dir"])
                .output()
                .is_ok_and(|o| o.status.success());
    if !is_url && !is_git && !path.is_file() {
        return Err(
            "imports must name an individual local file, HTTP(S) URL, or Git repository".into(),
        );
    }
    init(c)?;
    let kind = if is_git {
        "git"
    } else if is_url {
        "url"
    } else {
        "file"
    };
    c.execute(
        "INSERT OR IGNORE INTO imported_sources(id,locator,kind) VALUES(?1,?2,?3)",
        params![identity(locator), locator, kind],
    )?;
    Ok(())
}

pub(crate) fn set_retention(c: &Connection, value: &str) -> Result<()> {
    if value != "latest" && value != "all" {
        return Err("retention must be latest or all".into());
    }
    init(c)?;
    c.execute(
        "UPDATE import_config SET value=?1 WHERE key='retention'",
        [value],
    )?;
    if value == "latest" {
        c.execute("DELETE FROM imported_versions WHERE EXISTS (SELECT 1 FROM imported_versions newer WHERE newer.source_id=imported_versions.source_id AND newer.fetched_order>imported_versions.fetched_order)", [])?;
        c.execute("DELETE FROM imported_units WHERE NOT EXISTS (SELECT 1 FROM imported_versions v WHERE v.source_id=imported_units.source_id AND v.version=imported_units.version)", [])?;
        c.execute("DELETE FROM imported_git_units WHERE NOT EXISTS (SELECT 1 FROM imported_versions v WHERE v.source_id=imported_git_units.source_id AND v.version=imported_git_units.commit_id)", [])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::init;
    use rusqlite::Connection;

    #[test]
    fn init_marks_unrecoverable_legacy_pdf_and_backfills_text_hashes() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE imported_sources(id TEXT PRIMARY KEY,locator TEXT NOT NULL UNIQUE,kind TEXT NOT NULL);
             CREATE TABLE imported_versions(source_id TEXT NOT NULL,version TEXT NOT NULL,content TEXT NOT NULL,fetched_order INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(source_id,version));
             INSERT INTO imported_sources VALUES('pdf','manual.pdf','file');
             INSERT INTO imported_sources VALUES('text','notes.txt','file');
             INSERT INTO imported_versions VALUES('pdf','source-hash','',1);
             INSERT INTO imported_versions VALUES('text','text-hash','durable text',2);",
        ).unwrap();

        init(&connection).unwrap();
        let pdf_hash: Option<String> = connection
            .query_row(
                "SELECT representation_sha256 FROM imported_versions WHERE source_id='pdf'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            pdf_hash, None,
            "a source hash cannot recover legacy PDF text"
        );
        let text_hash: String = connection
            .query_row(
                "SELECT representation_sha256 FROM imported_versions WHERE source_id='text'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(text_hash, super::super::cache::checksum(b"durable text"));

        connection
            .execute(
                "UPDATE imported_versions SET content='tampered' WHERE source_id='text'",
                [],
            )
            .unwrap();
        assert!(
            init(&connection)
                .unwrap_err()
                .to_string()
                .contains("representation hash mismatch")
        );
        let original_rows: i64 = connection
            .query_row("SELECT count(*) FROM imported_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            original_rows, 2,
            "failed integrity migration must not discard versions"
        );
    }

    #[test]
    fn init_migrates_legacy_unit_keys_without_losing_rows() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE imported_units(source_id TEXT NOT NULL,version TEXT NOT NULL,line INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,page INTEGER,PRIMARY KEY(source_id,version,line));
             CREATE TABLE imported_git_units(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,line INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,line));
             INSERT INTO imported_units VALUES('s','v',4,'legacy file',X'0102',7);
             INSERT INTO imported_git_units VALUES('s','c','a.rs',4,'legacy git',X'0304');",
        ).unwrap();

        init(&connection).unwrap();
        connection.execute("INSERT OR REPLACE INTO imported_units(source_id,version,line,line_end,text,vector,page) VALUES('s','v',4,4,'second file',X'0506',NULL)", []).unwrap();
        connection.execute("INSERT OR REPLACE INTO imported_git_units(source_id,commit_id,path,line,line_end,text,vector) VALUES('s','c','a.rs',4,4,'second git',X'0708')", []).unwrap();
        let file_rows: i64 = connection.query_row(
            "SELECT count(*) FROM imported_units WHERE source_id='s' AND version='v' AND line=4",
            [],
            |row| row.get(0),
        ).unwrap();
        let git_rows: i64 = connection.query_row(
            "SELECT count(*) FROM imported_git_units WHERE source_id='s' AND commit_id='c' AND path='a.rs' AND line=4",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!((file_rows, git_rows), (2, 2));
        let old_vector: Vec<u8> = connection
            .query_row(
                "SELECT vector FROM imported_units WHERE text='legacy file'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_vector, [1, 2]);
        let page: i64 = connection
            .query_row(
                "SELECT page FROM imported_units WHERE text='legacy file'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(page, 7);
    }
}
