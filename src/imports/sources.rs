use super::cache::checksum;
use crate::{Result, budget::Budget};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const LARGE_SOURCE_WARNING_THRESHOLD_BYTES: u64 = 50_000_000;

#[derive(Serialize)]
pub(crate) struct Preview {
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_url: Option<String>,
    selected_supported_local_files: Vec<String>,
    unsupported_regular_files: Vec<UnsupportedFile>,
    total_selected_source_bytes: u64,
    current_db_bytes: Option<u64>,
    estimated_net_db_growth_bytes: Option<u64>,
    warning_basis_bytes: u64,
    warn_large: bool,
}

#[derive(Serialize)]
struct UnsupportedFile {
    path: String,
    reason: String,
}

fn preview_size_fields(total: u64, database: &Path) -> (Option<u64>, u64, bool) {
    let current_db_bytes = fs::metadata(database)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len());
    let warn_large = total >= LARGE_SOURCE_WARNING_THRESHOLD_BYTES
        || current_db_bytes.is_some_and(|size| total > size.saturating_mul(2));
    (current_db_bytes, total, warn_large)
}

pub(crate) fn preview(path: &Path, database: &Path) -> Result<Preview> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
        return Err(format!(
            "preview path must be a regular file or directory: {}",
            path.display()
        )
        .into());
    }
    let mut files = Vec::new();
    if metadata.is_file() {
        if super::same_file(path, database)? {
            return Err("cannot preview RepoDB's own database file".into());
        }
        files.push(path.to_path_buf());
    } else {
        collect_preview_files(path, &mut files)?;
    }
    files.sort();
    let mut supported = Vec::new();
    let mut unsupported = Vec::new();
    let mut total = 0u64;
    for file in files {
        if super::same_file(&file, database)? {
            continue;
        }
        let metadata = fs::symlink_metadata(&file)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        if preview_supported(&file) {
            total = total
                .checked_add(metadata.len())
                .ok_or("selected source byte total overflow")?;
            supported.push(file.to_string_lossy().into_owned());
        } else {
            unsupported.push(UnsupportedFile {
                path: file.to_string_lossy().into_owned(),
                reason: "unsupported local document format".into(),
            });
        }
    }
    let (current_db_bytes, warning_basis_bytes, warn_large) = preview_size_fields(total, database);
    Ok(Preview {
        selected_url: None,
        selected_supported_local_files: supported,
        unsupported_regular_files: unsupported,
        total_selected_source_bytes: total,
        current_db_bytes,
        estimated_net_db_growth_bytes: None,
        warning_basis_bytes,
        warn_large,
    })
}

pub(crate) fn preview_url(url: &str, database: &Path) -> Result<Preview> {
    let deadline = std::time::Instant::now() + crate::url_security::URL_FETCH_TIMEOUT;
    let endpoint = crate::url_security::parse_http_url(url)?;
    let allowlist = crate::url_security::private_address_allowlist_from_env()?;
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err("URL preview exceeded its 30-second total deadline".into());
    }
    let addresses = crate::url_security::resolve_and_validate(&endpoint, &allowlist, deadline)?;
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err("URL preview exceeded its 30-second total deadline".into());
    }
    let timeout = remaining.as_secs_f64().max(0.001);
    let mut command = super::configured_url_curl(&endpoint, &addresses);
    command
        .arg("--max-time")
        .arg(format!("{timeout:.3}"))
        .args([
            "--output",
            "/dev/null",
            "--write-out",
            "%{size_download}\\n%{http_code}",
        ])
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = super::process::spawn_in_process_group(&mut command)?;
    let mut budget = Budget::new(
        64 + super::MAX_COMMAND_STDERR_BYTES,
        "URL preview subprocess output",
    );
    let (status, output, stderr) = super::bounded_command_output(&mut child, 64, &mut budget)?;
    if !status.success() {
        return Err(format!(
            "URL preview fetch failed for {url}: {}",
            String::from_utf8_lossy(&stderr)
        )
        .into());
    }
    let output = String::from_utf8(output)?;
    let mut fields = output.lines();
    let total = fields.next().unwrap_or_default().parse::<u64>()?;
    let response_code = fields.next().unwrap_or_default().parse::<u16>()?;
    if !(200..300).contains(&response_code) {
        return Err(format!(
            "URL preview returned HTTP status {response_code}; redirects are not followed"
        )
        .into());
    }
    let (current_db_bytes, warning_basis_bytes, warn_large) = preview_size_fields(total, database);
    Ok(Preview {
        selected_url: Some(url.to_owned()),
        selected_supported_local_files: Vec::new(),
        unsupported_regular_files: Vec::new(),
        total_selected_source_bytes: total,
        current_db_bytes,
        estimated_net_db_growth_bytes: None,
        warning_basis_bytes,
        warn_large,
    })
}

fn preview_supported(path: &Path) -> bool {
    path.extension().is_none_or(|extension| {
        extension.to_str().is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "txt" | "pdf" | "html" | "htm" | "org" | "rs"
            )
        })
    })
}

fn collect_preview_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if entry.file_name() != ".git" {
                collect_preview_files(&path, files)?;
            }
        } else if metadata.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

pub(super) fn init(c: &Connection) -> Result<()> {
    const SAVEPOINT: &str = "repodb_import_schema_init";
    c.execute_batch(&format!("SAVEPOINT {SAVEPOINT}"))?;
    match init_schema(c) {
        Ok(()) => match c.execute_batch(&format!("RELEASE SAVEPOINT {SAVEPOINT}")) {
            Ok(()) => Ok(()),
            Err(error) => rollback_schema_savepoint(c, SAVEPOINT, error.into()),
        },
        Err(error) => rollback_schema_savepoint(c, SAVEPOINT, error),
    }
}

fn rollback_schema_savepoint(
    c: &Connection,
    savepoint: &str,
    error: Box<dyn std::error::Error>,
) -> Result<()> {
    match c.execute_batch(&format!(
        "ROLLBACK TO SAVEPOINT {savepoint}; RELEASE SAVEPOINT {savepoint}"
    )) {
        Ok(()) => Err(error),
        Err(rollback_error) => Err(format!(
            "{error}; additionally, import schema rollback failed: {rollback_error}"
        )
        .into()),
    }
}

fn init_schema(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS imported_sources(id TEXT PRIMARY KEY, locator TEXT NOT NULL UNIQUE, kind TEXT NOT NULL, object_store TEXT, fetch_status TEXT, last_fetch_error TEXT, last_fetch_attempt_at TEXT); CREATE TABLE IF NOT EXISTS imported_versions(source_id TEXT NOT NULL, version TEXT NOT NULL, content TEXT NOT NULL, fetched_order INTEGER NOT NULL DEFAULT 0, first_retrieved_at TEXT, last_retrieved_at TEXT, final_url TEXT, redirect_chain TEXT, PRIMARY KEY(source_id,version)); CREATE TABLE IF NOT EXISTS imported_units(source_id TEXT NOT NULL, version TEXT NOT NULL, line INTEGER NOT NULL, text TEXT NOT NULL, vector BLOB, page INTEGER, line_end INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(source_id,version,line,text)); CREATE TABLE IF NOT EXISTS import_config(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS imported_git_units(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,blob_id TEXT NOT NULL,segmentation_kind TEXT NOT NULL,byte_start INTEGER NOT NULL,byte_end INTEGER NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,byte_start));")?;
    if !column_exists(c, "imported_versions", "representation_sha256")? {
        c.execute(
            "ALTER TABLE imported_versions ADD COLUMN representation_sha256 TEXT",
            [],
        )?;
    }
    {
        let mut statement = c.prepare(
            "SELECT source_id,version,content FROM imported_versions WHERE content<>'' AND representation_sha256 IS NULL",
        )?;
        let mut rows = statement.query([])?;
        let mut update = c.prepare_cached(
            "UPDATE imported_versions SET representation_sha256=?3 WHERE source_id=?1 AND version=?2",
        )?;
        while let Some(row) = rows.next()? {
            let source: String = row.get(0)?;
            let version: String = row.get(1)?;
            let content = row.get_ref(2)?.as_bytes()?;
            std::str::from_utf8(content)?;
            let hash = super::cache::checksum(content);
            update.execute(params![source, version, hash])?;
        }
    }
    {
        let mut statement = c.prepare("SELECT content,representation_sha256 FROM imported_versions WHERE representation_sha256 IS NOT NULL")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let content = row.get_ref(0)?.as_bytes()?;
            std::str::from_utf8(content)?;
            let expected: String = row.get(1)?;
            if super::cache::checksum(content) != expected {
                return Err("imported version representation hash mismatch".into());
            }
        }
    }
    if !column_exists(c, "imported_sources", "requested_revision")? {
        c.execute(
            "ALTER TABLE imported_sources ADD COLUMN requested_revision TEXT",
            [],
        )?;
    }
    if !column_exists(c, "imported_sources", "pinned_commit")? {
        c.execute(
            "ALTER TABLE imported_sources ADD COLUMN pinned_commit TEXT",
            [],
        )?;
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
    if !column_exists(c, "imported_versions", "first_retrieved_at")? {
        c.execute(
            "ALTER TABLE imported_versions ADD COLUMN first_retrieved_at TEXT",
            [],
        )?;
    }
    if !column_exists(c, "imported_versions", "last_retrieved_at")? {
        c.execute(
            "ALTER TABLE imported_versions ADD COLUMN last_retrieved_at TEXT",
            [],
        )?;
    }
    if !column_exists(c, "imported_versions", "final_url")? {
        c.execute(
            "ALTER TABLE imported_versions ADD COLUMN final_url TEXT",
            [],
        )?;
    }
    if !column_exists(c, "imported_versions", "redirect_chain")? {
        c.execute(
            "ALTER TABLE imported_versions ADD COLUMN redirect_chain TEXT",
            [],
        )?;
    }
    if !column_exists(c, "imported_sources", "fetch_status")? {
        c.execute(
            "ALTER TABLE imported_sources ADD COLUMN fetch_status TEXT",
            [],
        )?;
    }
    if !column_exists(c, "imported_sources", "last_fetch_error")? {
        c.execute(
            "ALTER TABLE imported_sources ADD COLUMN last_fetch_error TEXT",
            [],
        )?;
    }
    if !column_exists(c, "imported_sources", "last_fetch_attempt_at")? {
        c.execute(
            "ALTER TABLE imported_sources ADD COLUMN last_fetch_attempt_at TEXT",
            [],
        )?;
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
    let default_history_limit = if c.query_row(
        "SELECT value FROM import_config WHERE key='retention'",
        [],
        |row| row.get::<_, String>(0),
    )? == "all"
    {
        "2"
    } else {
        "1"
    };
    c.execute(
        "INSERT OR IGNORE INTO import_config VALUES('history_limit',?1)",
        [default_history_limit],
    )?;
    c.execute(
        "INSERT OR IGNORE INTO import_config VALUES('url_max_redirects',?1)",
        [super::DEFAULT_URL_MAX_REDIRECTS.to_string()],
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

fn git_remote_has_embedded_credentials(locator: &str) -> bool {
    let Some((scheme, rest)) = locator.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let Some((userinfo, _)) = authority.rsplit_once('@') else {
        return false;
    };
    match scheme {
        "http" | "https" | "git" | "file" => true,
        "ssh" => userinfo.contains(':'),
        _ => false,
    }
}

pub(crate) fn register(c: &Connection, locator: &str, revision: Option<&str>) -> Result<()> {
    let is_remote_git = locator.starts_with("ssh://")
        || locator.starts_with("git@")
        || locator.starts_with("git://")
        || locator.starts_with("file://")
        || locator.starts_with("http://") && locator.ends_with(".git")
        || locator.starts_with("https://") && locator.ends_with(".git");
    if is_remote_git && git_remote_has_embedded_credentials(locator) {
        return Err(
            "Git remote URLs must not contain embedded credentials; use a Git credential helper instead"
                .into(),
        );
    }
    let is_url =
        !is_remote_git && (locator.starts_with("http://") || locator.starts_with("https://"));
    if is_url {
        crate::url_security::parse_http_url(locator)?;
    }
    let path = Path::new(locator);
    let is_git = is_remote_git
        || path.is_dir()
            && Command::new("git")
                .args(["-C", locator, "rev-parse", "--show-toplevel"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .is_some_and(|root| {
                    Path::new(root.trim()).canonicalize().ok() == path.canonicalize().ok()
                });
    if !is_url && !is_git && !path.is_file() {
        return Err(
            "imports must name an individual local file, HTTP(S) URL, or Git repository".into(),
        );
    }
    if !is_url && !is_git && super::is_database_file(c, path)? {
        return Err("cannot import RepoDB's own database file".into());
    }
    if !is_url && !is_git {
        let supported = path.extension().is_none()
            || path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "md" | "markdown" | "txt" | "pdf" | "html" | "htm" | "org" | "rs"
                    )
                });
        if !supported {
            return Err(format!(
                "unsupported local document format for {}; supported formats are extensionless plain text, .md, .markdown, .txt, .pdf, .html, .htm, .org, and .rs",
                path.display()
            )
            .into());
        }
    }
    init(c)?;
    let kind = if is_git {
        "git"
    } else if is_url {
        "url"
    } else {
        "file"
    };
    let _remote_git_deadline = (is_git && !path.is_dir())
        .then(crate::budget::RefreshDeadline::start)
        .transpose()?;
    let requested_revision = if is_git {
        let revision = revision.unwrap_or("HEAD");
        if revision.is_empty() || revision.starts_with('-') || revision.contains('\0') {
            return Err("invalid Git revision".into());
        }
        Some(revision)
    } else {
        if revision.is_some() {
            return Err("--revision requires a Git repository".into());
        }
        None
    };
    let pinned_commit = if let Some(revision) = requested_revision {
        let git_dir = if path.is_dir() {
            locator.to_owned()
        } else {
            let database_path: String =
                c.query_row("PRAGMA database_list", [], |row| row.get(2))?;
            let cache = Path::new(&database_path)
                .parent()
                .ok_or("database has no parent directory")?
                .join("repodb-cache");
            fs::create_dir_all(&cache)?;
            let mirror = cache.join(format!("git-{}", checksum(locator.as_bytes())));
            if !mirror.exists() {
                let mut budget = Budget::refresh("Git import registration")?;
                super::clone_git_mirror(locator, &mirror, &cache, &mut budget)?;
            } else {
                let status = Command::new("git")
                    .arg("--git-dir")
                    .arg(&mirror)
                    .args(["fetch", "--prune", "origin"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?;
                if !status.success() {
                    return Err(
                        "Git fetch failed; check remote access and Git authentication configuration"
                            .into(),
                    );
                }
            }
            mirror.to_string_lossy().into_owned()
        };
        let output = Command::new("git")
            .args([
                "-C",
                &git_dir,
                "rev-parse",
                "--verify",
                &format!("{revision}^{{commit}}"),
            ])
            .output()?;
        if !output.status.success() {
            return Err(format!("could not resolve Git revision {revision}").into());
        }
        Some(String::from_utf8(output.stdout)?.trim().to_owned())
    } else {
        None
    };
    c.execute(
        "INSERT INTO imported_sources(id,locator,kind,requested_revision,pinned_commit) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(locator) DO UPDATE SET requested_revision=excluded.requested_revision,pinned_commit=excluded.pinned_commit",
        params![identity(locator), locator, kind, requested_revision, pinned_commit],
    )?;
    Ok(())
}

pub(crate) fn set_retention(c: &Connection, value: &str) -> Result<usize> {
    let limit = match value {
        "latest" => 1,
        "all" => 2,
        numeric => numeric.parse::<usize>().map_err(
            |_| "retention must be latest or a version limit between 2 and 100 (all is 2)",
        )?,
    };
    if !(1..=super::MAX_RETAINED_VERSIONS).contains(&limit) || (limit == 1 && value != "latest") {
        return Err(format!(
            "retention must be latest or a version limit between 2 and {} (all is 2)",
            super::MAX_RETAINED_VERSIONS
        )
        .into());
    }
    init(c)?;
    let retention = if limit == 1 { "latest" } else { "all" };
    c.execute(
        "UPDATE import_config SET value=?1 WHERE key='retention'",
        [retention],
    )?;
    c.execute(
        "UPDATE import_config SET value=?1 WHERE key='history_limit'",
        [limit.to_string()],
    )?;
    let before: i64 = c.query_row("SELECT count(*) FROM imported_versions", [], |row| {
        row.get(0)
    })?;
    c.execute("DELETE FROM imported_versions AS older WHERE EXISTS (SELECT 1 FROM imported_versions newer WHERE newer.source_id=older.source_id AND (newer.fetched_order>older.fetched_order OR (newer.fetched_order=older.fetched_order AND newer.version>older.version)) ORDER BY newer.fetched_order DESC,newer.version DESC LIMIT 1 OFFSET ?1)", [limit.saturating_sub(1) as i64])?;
    c.execute("DELETE FROM imported_units WHERE NOT EXISTS (SELECT 1 FROM imported_versions v WHERE v.source_id=imported_units.source_id AND v.version=imported_units.version)", [])?;
    c.execute("DELETE FROM imported_git_units WHERE NOT EXISTS (SELECT 1 FROM imported_versions v WHERE v.source_id=imported_git_units.source_id AND v.version=imported_git_units.commit_id)", [])?;
    let after: i64 = c.query_row("SELECT count(*) FROM imported_versions", [], |row| {
        row.get(0)
    })?;
    Ok((before - after).max(0) as usize)
}

#[cfg(test)]
mod tests {
    use super::{git_remote_has_embedded_credentials, init};
    use rusqlite::Connection;

    #[test]
    fn git_remote_credential_check_rejects_uri_userinfo_without_blocking_ssh_usernames() {
        for (locator, expected) in [
            ("https://user:token@example.com/repo.git", true),
            ("https://token@example.com/repo.git", true),
            ("http://user:token@example.com/repo.git", true),
            ("git://user@example.com/repo.git", true),
            ("file://user@example.com/repo.git", true),
            ("ssh://user:token@example.com/repo.git", true),
            ("https://example.com/repo.git", false),
            ("ssh://git@example.com/repo.git", false),
            ("git@example.com:owner/repo.git", false),
        ] {
            assert_eq!(
                git_remote_has_embedded_credentials(locator),
                expected,
                "{locator}"
            );
        }
    }

    #[test]
    fn init_marks_unrecoverable_legacy_pdf_and_backfills_text_hashes() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE imported_sources(id TEXT PRIMARY KEY,locator TEXT NOT NULL UNIQUE,kind TEXT NOT NULL);
             CREATE TABLE imported_versions(source_id TEXT NOT NULL,version TEXT NOT NULL,content TEXT NOT NULL,fetched_order INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(source_id,version));
             INSERT INTO imported_sources VALUES('pdf','manual.pdf','file');
             INSERT INTO imported_sources VALUES('text','notes.txt','file');
             INSERT INTO imported_sources VALUES('url','http://127.0.0.1:1/legacy','url');
             INSERT INTO imported_versions VALUES('pdf','source-hash','',1);
             INSERT INTO imported_versions VALUES('text','text-hash','durable text',2);
             INSERT INTO imported_versions VALUES('url','legacy-hash','legacy page',3);",
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
        let legacy_url_timestamps: (Option<String>, Option<String>) = connection
            .query_row(
                "SELECT first_retrieved_at,last_retrieved_at FROM imported_versions WHERE source_id='url'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(legacy_url_timestamps, (None, None));
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
                "UPDATE imported_versions SET representation_sha256=NULL WHERE source_id='text'",
                [],
            )
            .unwrap();
        init(&connection).unwrap();
        let repaired_hash: String = connection
            .query_row(
                "SELECT representation_sha256 FROM imported_versions WHERE source_id='text'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repaired_hash, text_hash);

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
            original_rows, 3,
            "failed integrity migration must not discard versions"
        );
    }

    #[test]
    fn failed_import_schema_migration_rolls_back_and_can_be_retried() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE imported_versions(source_id TEXT,version TEXT,content TEXT);
                 INSERT INTO imported_versions VALUES('legacy','v1','legacy content');
                 CREATE TRIGGER fail_representation_backfill
                 BEFORE UPDATE ON imported_versions
                 BEGIN SELECT RAISE(ABORT, 'injected migration failure'); END;",
            )
            .unwrap();

        let error = init(&connection).unwrap_err().to_string();
        assert!(error.contains("injected migration failure"));
        assert!(
            !super::column_exists(&connection, "imported_versions", "representation_sha256")
                .unwrap()
        );
        assert!(!super::column_exists(&connection, "imported_sources", "id").unwrap());
        assert_eq!(
            connection
                .query_row("SELECT content FROM imported_versions", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "legacy content"
        );

        connection
            .execute_batch("DROP TRIGGER fail_representation_backfill")
            .unwrap();
        init(&connection).unwrap();
        let expected = super::super::cache::checksum(b"legacy content");
        let actual: String = connection
            .query_row(
                "SELECT representation_sha256 FROM imported_versions WHERE source_id='legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(actual, expected);
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
