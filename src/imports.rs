use crate::{
    Result, SearchResult, as_bytes,
    budget::{Budget, MAX_REFRESH_ACCOUNTING_BYTES},
    embed,
    git_objects::GitBlobReader,
    git_ranges, process, segmentation,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

mod cache;
mod sources;
use cache::{checksum, visit_cache_pages, write_cache};
use sources::{column_exists, init};
pub(crate) use sources::{preview, preview_url, register, set_retention};

pub(crate) fn same_file(left: &Path, right: &Path) -> Result<bool> {
    let left = match fs::canonicalize(left) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let right = match fs::canonicalize(right) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let left = fs::metadata(left)?;
        let right = fs::metadata(right)?;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }
    #[cfg(not(unix))]
    {
        Ok(left == right)
    }
}

pub(crate) fn is_database_file(c: &Connection, path: &Path) -> Result<bool> {
    let database_path: String = c.query_row("PRAGMA database_list", [], |row| row.get(2))?;
    if database_path.is_empty() {
        return Ok(false);
    }
    same_file(path, Path::new(&database_path))
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn create_temporary_file(label: &str) -> Result<(TemporaryFile, fs::File)> {
    static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(0);
    for _ in 0..100 {
        let id = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("repodb-{label}-{}-{id}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((TemporaryFile(path), file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err("could not create a unique temporary file".into())
}

const MAX_COMMAND_STDERR_BYTES: usize = 64 * 1024;
const MAX_URL_HEADER_BYTES: u64 = 1024 * 1024;
const DEFAULT_URL_MAX_REDIRECTS: usize = 5;
const HARD_MAX_URL_REDIRECTS: usize = 20;
const MAX_RETAINED_VERSIONS: usize = 100;
const CURL_METADATA_MARKER: &str = "__REPODB_CURL_META__";

struct FetchedUrl {
    body: Vec<u8>,
    final_url: String,
    content_type: String,
    redirect_chain: Vec<String>,
}

fn fetch_url(locator: &str, max_redirects: usize, used: &mut Budget) -> Result<FetchedUrl> {
    let (headers_file, file) = create_temporary_file("url-headers")?;
    drop(file);
    let marker = CURL_METADATA_MARKER;
    let write_out = format!(
        "\\n{marker}%{{url_effective}}\\n%{{content_type}}\\n%{{num_redirects}}\\n%{{http_code}}\\n"
    );
    let mut child = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--max-time",
            "30",
            "--max-redirs",
        ])
        .arg(max_redirects.to_string())
        .args(["--proto-redir", "=http,https", "--dump-header"])
        .arg(&headers_file.0)
        .args(["--write-out", &write_out])
        .arg(locator)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (status, mut output, stderr) = bounded_command_output(
        &mut child,
        used.remaining().saturating_sub(MAX_COMMAND_STDERR_BYTES),
        used,
    )?;
    if !status.success() {
        return Err(format!(
            "URL fetch failed for {locator}: {}",
            String::from_utf8_lossy(&stderr)
        )
        .into());
    }
    let marker = marker.as_bytes();
    let metadata_start = output
        .windows(marker.len())
        .rposition(|window| window == marker)
        .ok_or("curl did not return URL response metadata")?;
    let metadata = std::str::from_utf8(&output[metadata_start + marker.len()..])?;
    let mut fields = metadata.lines();
    let final_url = fields
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(locator)
        .to_owned();
    let content_type = fields.next().unwrap_or_default().to_owned();
    let redirects: usize = fields.next().unwrap_or("0").parse()?;
    let response_code: u16 = fields.next().unwrap_or("0").parse()?;
    if !(200..300).contains(&response_code) {
        return Err(format!("URL fetch returned HTTP status {response_code} for {locator}").into());
    }
    if redirects > max_redirects {
        return Err("URL redirect count exceeded the configured limit".into());
    }
    let body_end = metadata_start
        .checked_sub(1)
        .ok_or("curl response is missing its metadata separator")?;
    output.truncate(body_end);

    let headers_size = fs::metadata(&headers_file.0)?.len();
    if headers_size > MAX_URL_HEADER_BYTES {
        return Err("URL response headers exceeded the 1 MiB limit".into());
    }
    let headers = fs::read(&headers_file.0)?;
    let header_text = String::from_utf8_lossy(&headers);
    let redirect_chain: Vec<String> = header_text
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("location")
                .then(|| value.trim().to_owned())
        })
        .collect();
    if redirect_chain.len() != redirects {
        return Err("URL redirect provenance did not match curl's redirect count".into());
    }
    used.charge(final_url.len(), "URL final address")?;
    used.charge(content_type.len(), "URL content type")?;
    for address in &redirect_chain {
        used.charge(address.len(), "URL redirect provenance")?;
    }
    Ok(FetchedUrl {
        body: output,
        final_url,
        content_type,
        redirect_chain,
    })
}

const LARGE_IMPORT_WARNING_THRESHOLD_BYTES: u64 = 50_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SizeLimit {
    Error,
    Warn,
    Ignore,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportSizeAssessment {
    pub source_bytes: u64,
    pub estimated_net_db_growth_bytes: Option<u64>,
    pub warning_basis_bytes: u64,
    pub warning_basis: &'static str,
    pub warn_large: bool,
}

fn html_text(source: &str) -> String {
    let lowercase = source.to_ascii_lowercase();
    let mut visible_source = String::with_capacity(source.len());
    let mut cursor = 0;
    loop {
        let next_element = ["<script", "<style"]
            .iter()
            .filter_map(|tag| {
                lowercase[cursor..]
                    .find(tag)
                    .map(|offset| (cursor + offset, *tag))
            })
            .filter(|(start, tag)| {
                lowercase
                    .as_bytes()
                    .get(start + tag.len())
                    .is_some_and(|byte| {
                        byte.is_ascii_whitespace() || *byte == b'>' || *byte == b'/'
                    })
            })
            .min_by_key(|(start, _)| *start);
        let Some((start, tag)) = next_element else {
            visible_source.push_str(&source[cursor..]);
            break;
        };
        let Some(open_end) = lowercase[start..].find('>') else {
            visible_source.push_str(&source[cursor..start]);
            break;
        };
        let close_tag = format!("</{}", &tag[1..]);
        let close_start = start + open_end + 1;
        let Some(close_offset) = lowercase[close_start..].find(&close_tag) else {
            visible_source.push_str(&source[cursor..start]);
            break;
        };
        let close_start = close_start + close_offset;
        let Some(close_end) = lowercase[close_start..].find('>') else {
            visible_source.push_str(&source[cursor..start]);
            break;
        };
        visible_source.push_str(&source[cursor..start]);
        cursor = close_start + close_end + 1;
    }

    let mut text = String::with_capacity(visible_source.len());
    let mut cursor = 0;
    let mut link_target: Option<String> = None;
    while cursor < visible_source.len() {
        if visible_source.as_bytes()[cursor] == b'<' {
            let end = visible_source[cursor..]
                .find('>')
                .map(|offset| cursor + offset + 1);
            let Some(end) = end else {
                break;
            };
            let tag = visible_source[cursor + 1..end - 1].trim();
            let closing = tag.starts_with('/');
            let name = tag
                .trim_start_matches('/')
                .split(|character: char| character.is_ascii_whitespace() || character == '/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if closing {
                if name == "a"
                    && let Some(target) = link_target.take()
                {
                    text.push_str("] (");
                    text.push_str(&target);
                    text.push(')');
                }
                text.push(' ');
                if matches!(name.as_str(), "td" | "th") {
                    text.push_str(" | ");
                }
            } else {
                if name == "a" {
                    let attributes = &tag[name.len()..];
                    link_target = html_attribute(attributes, "href");
                    text.push('[');
                } else {
                    text.push(' ');
                }
            }
            cursor = end;
        } else {
            let next = visible_source[cursor..]
                .find('<')
                .map_or(visible_source.len(), |offset| cursor + offset);
            text.push_str(&visible_source[cursor..next]);
            cursor = next;
        }
    }
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn html_attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let start = lower.find(name)? + name.len();
    let remainder = tag[start..].trim_start();
    let remainder = remainder.strip_prefix('=')?.trim_start();
    let quote = remainder.chars().next()?;
    if quote == '\'' || quote == '\"' {
        let value = &remainder[1..];
        Some(value[..value.find(quote)?].to_owned())
    } else {
        Some(remainder.split_ascii_whitespace().next()?.to_owned())
    }
}

fn pdf_representation(pages: &[String]) -> Result<String> {
    let pages_json = serde_json::to_string(pages)?;
    Ok(serde_json::to_string(&serde_json::json!({
        "sha256": checksum(pages_json.as_bytes()),
        "pages": pages,
    }))?)
}

fn table_exists(c: &Connection, table: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )?)
}

fn read_all(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut reader = reader;
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn configured_url_curl() -> Command {
    let mut command = Command::new("curl");
    command.args([
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--max-time",
        "30",
        "--max-redirs",
    ]);
    command.arg(DEFAULT_URL_MAX_REDIRECTS.to_string());
    command.args(["--proto-redir", "=http,https"]);
    command
}

fn bounded_command_output(
    child: &mut std::process::Child,
    stdout_limit: usize,
    used: &mut Budget,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    bounded_command_output_with_input(child, &[], stdout_limit, used)
}

fn bounded_command_output_with_input(
    child: &mut std::process::Child,
    input: &[u8],
    stdout_limit: usize,
    used: &mut Budget,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    let remaining = used.remaining();
    let stdout_limit = stdout_limit.min(remaining);
    let stderr_limit = (remaining - stdout_limit).min(MAX_COMMAND_STDERR_BYTES);
    let reservation = stdout_limit + stderr_limit;
    used.charge(reservation, "subprocess output capture")?;

    let output = match process::capture_with_input(child, input, stdout_limit, stderr_limit) {
        Ok(output) => output,
        Err(error) => {
            used.release(reservation);
            return Err(error.into());
        }
    };
    if output.exceeded {
        drop(output);
        used.release(reservation);
        return Err(format!("subprocess output exceeded its bounded capture (stdout: {stdout_limit} bytes, stderr: {stderr_limit} bytes)").into());
    }
    let retained = output.stdout.capacity() + output.stderr.capacity();
    used.release(reservation - retained);
    Ok((output.status, output.stdout, output.stderr))
}

fn import_embed(command: &str, model: &str, text: &str, used: &mut Budget) -> Result<Vec<f32>> {
    let input = if crate::is_builtin_embedding_command(command) && text.trim().is_empty() {
        "<empty RepoDB unit>"
    } else {
        text
    };
    let mut child = Command::new(command)
        .env("REPODB_EMBEDDING_MODEL", model)
        .args(["--json", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // A JSON number needs at least one input byte; Vec growth can reserve up to
    // twice the element count, so budget up to 8 bytes of vector capacity per
    // captured stdout byte while stdout remains live.
    let output_limit = used.remaining().saturating_sub(MAX_COMMAND_STDERR_BYTES) / 9;
    let (status, stdout, stderr) =
        bounded_command_output_with_input(&mut child, input.as_bytes(), output_limit, used)?;
    if !status.success() {
        return Err(format!(
            "embedding command failed: {}",
            String::from_utf8_lossy(&stderr)
        )
        .into());
    }
    let vector_reservation = stdout
        .len()
        .checked_mul(2 * std::mem::size_of::<f32>())
        .ok_or("embedding vector capacity exceeds refresh budget")?;
    used.charge(vector_reservation, "embedding vector capacity")?;
    let vector: Vec<f32> = serde_json::from_slice(&stdout)?;
    let vector_bytes = vector.capacity() * std::mem::size_of::<f32>();
    used.release(vector_reservation - vector_bytes);
    let stdout_bytes = stdout.len();
    drop(stdout);
    used.release(stdout_bytes);
    if vector.is_empty()
        || vector.iter().any(|value| !value.is_finite())
        || vector.iter().all(|value| *value == 0.0)
    {
        return Err("embedding must be a nonempty finite vector with nonzero norm".into());
    }
    Ok(vector)
}

fn git_output(repo: &str, args: &[&str], limit: usize, used: &mut Budget) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .args(["-C", repo])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (status, stdout, stderr) = bounded_command_output(&mut child, limit, used)?;
    if !status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&stderr)
        )
        .into());
    }
    Ok(stdout)
}

fn stage_legacy_git_units(
    c: &Connection,
    source_id: &str,
    repository: &str,
    retention_limit: usize,
    memory: &mut Budget,
) -> Result<()> {
    let legacy = {
        let mut stmt = c.prepare("SELECT commit_id,path,line,line_end,text,vector FROM imported_git_units WHERE source_id=?1 AND commit_id IN (SELECT version FROM temp.import_stage_versions WHERE source_id=?1 ORDER BY fetched_order DESC LIMIT ?2)")?;
        stmt.query_map(params![source_id, retention_limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, usize>(2)?,
                row.get::<_, usize>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<Vec<u8>>>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (commit, path, line, line_end, legacy_text, vector) in legacy {
        let spec = format!("{commit}:{path}");
        let text = String::from_utf8(git_output(
            repository,
            &["show", &spec],
            memory.remaining().saturating_sub(MAX_COMMAND_STDERR_BYTES),
            memory,
        )?)?;
        memory.charge(text.len(), "legacy Git blob text")?;
        let blob_id =
            String::from_utf8(git_output(repository, &["rev-parse", &spec], 128, memory)?)?
                .trim()
                .to_owned();
        let kind = segmentation::kind(path.as_bytes());
        let mut found = None;
        segmentation::visit(kind, &text, |unit| {
            if unit.text == legacy_text && unit.line_start == line && unit.line_end == line_end {
                found = Some((unit.byte_start, unit.byte_end));
                false
            } else {
                true
            }
        })?;
        let (start, end) =
            found.ok_or_else(|| format!("legacy imported Git unit not found in {spec}"))?;
        c.execute("INSERT OR REPLACE INTO temp.import_stage_git_units(source_id,commit_id,path,blob_id,segmentation_kind,byte_start,byte_end,line,line_end,vector) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![source_id, commit, path, blob_id, kind, start, end, line, line_end, vector])?;
        memory.release(text.len());
    }
    Ok(())
}

fn utc_rfc3339_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_secs() as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // gmtime_r writes a complete UTC calendar value without using process-global state.
    let tm = unsafe { libc::gmtime_r(&seconds, tm.as_mut_ptr()) };
    let tm = unsafe { tm.as_ref().expect("gmtime_r supports current timestamp") };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

pub struct PrunedVersions {
    pub source: String,
    pub count: usize,
}

pub fn pruning_report(c: &Connection) -> Result<Vec<PrunedVersions>> {
    let table_exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE type='table' AND name='import_refresh_pruned')",
        [],
        |row| row.get(0),
    )?;
    if !table_exists {
        return Ok(Vec::new());
    }
    let mut statement = c.prepare(
        "SELECT locator,versions_pruned FROM temp.import_refresh_pruned WHERE versions_pruned>0 ORDER BY locator",
    )?;
    Ok(statement
        .query_map([], |row| {
            Ok(PrunedVersions {
                source: row.get(0)?,
                count: row.get(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

fn retention_limit(c: &Connection) -> Result<usize> {
    let value: String = c.query_row(
        "SELECT value FROM import_config WHERE key='history_limit'",
        [],
        |row| row.get(0),
    )?;
    let limit: usize = value.parse()?;
    if !(1..=MAX_RETAINED_VERSIONS).contains(&limit) {
        return Err("stored import history limit is invalid".into());
    }
    Ok(limit)
}

fn prune_staged_versions(c: &Connection, source_id: &str, limit: usize) -> Result<usize> {
    let before: i64 = c.query_row(
        "SELECT count(*) FROM temp.import_stage_versions WHERE source_id=?1",
        [source_id],
        |row| row.get(0),
    )?;
    c.execute(
        "DELETE FROM temp.import_stage_versions AS older WHERE older.source_id=?1 AND EXISTS (SELECT 1 FROM temp.import_stage_versions newer WHERE newer.source_id=older.source_id AND (newer.fetched_order>older.fetched_order OR (newer.fetched_order=older.fetched_order AND newer.version>older.version)) ORDER BY newer.fetched_order DESC,newer.version DESC LIMIT 1 OFFSET ?2)",
        params![source_id, limit.saturating_sub(1) as i64],
    )?;
    c.execute(
        "DELETE FROM temp.import_stage_units WHERE source_id=?1 AND version NOT IN (SELECT version FROM temp.import_stage_versions WHERE source_id=?1)",
        [source_id],
    )?;
    c.execute(
        "DELETE FROM temp.import_stage_git_units WHERE source_id=?1 AND commit_id NOT IN (SELECT version FROM temp.import_stage_versions WHERE source_id=?1)",
        [source_id],
    )?;
    let after: i64 = c.query_row(
        "SELECT count(*) FROM temp.import_stage_versions WHERE source_id=?1",
        [source_id],
        |row| row.get(0),
    )?;
    Ok((before - after).max(0) as usize)
}

fn record_pruned_versions(
    c: &Connection,
    source_id: &str,
    locator: &str,
    count: usize,
) -> Result<()> {
    if count > 0 {
        c.execute(
            "INSERT INTO temp.import_refresh_pruned VALUES(?1,?2,?3) ON CONFLICT(source_id) DO UPDATE SET versions_pruned=versions_pruned+excluded.versions_pruned",
            params![source_id, locator, count as i64],
        )?;
    }
    Ok(())
}

pub fn set_url_max_redirects(c: &Connection, max_redirects: usize) -> Result<()> {
    if max_redirects > HARD_MAX_URL_REDIRECTS {
        return Err(format!("URL redirect limit must not exceed {HARD_MAX_URL_REDIRECTS}").into());
    }
    init(c)?;
    c.execute(
        "UPDATE import_config SET value=?1 WHERE key='url_max_redirects'",
        [max_redirects.to_string()],
    )?;
    Ok(())
}

fn url_max_redirects(c: &Connection) -> Result<usize> {
    let value: String = c.query_row(
        "SELECT value FROM import_config WHERE key='url_max_redirects'",
        [],
        |row| row.get(0),
    )?;
    let max_redirects: usize = value.parse()?;
    if max_redirects > HARD_MAX_URL_REDIRECTS {
        return Err(format!("stored URL redirect limit exceeds {HARD_MAX_URL_REDIRECTS}").into());
    }
    Ok(max_redirects)
}

pub fn refresh_with_size_limit(
    c: &Connection,
    size_limit: SizeLimit,
) -> Result<Vec<ImportSizeAssessment>> {
    init(c)?;
    let url_max_redirects = url_max_redirects(c)?;
    let history_limit = retention_limit(c)?;
    let mut size_assessments = Vec::new();
    c.execute_batch("PRAGMA temp_store=FILE;
        DROP TABLE IF EXISTS temp.import_refresh_pruned;
        DROP TABLE IF EXISTS temp.import_stage_sources; DROP TABLE IF EXISTS temp.import_stage_versions;
        DROP TABLE IF EXISTS temp.import_stage_units; DROP TABLE IF EXISTS temp.import_stage_git_units;
        CREATE TEMP TABLE import_refresh_pruned(source_id TEXT PRIMARY KEY,locator TEXT NOT NULL,versions_pruned INTEGER NOT NULL);
        CREATE TEMP TABLE import_stage_sources(id TEXT PRIMARY KEY,locator TEXT NOT NULL,kind TEXT NOT NULL,object_store TEXT,fetch_status TEXT,last_fetch_error TEXT,last_fetch_attempt_at TEXT,requested_revision TEXT,pinned_commit TEXT);
        CREATE TEMP TABLE import_stage_versions(source_id TEXT NOT NULL,version TEXT NOT NULL,content TEXT NOT NULL,fetched_order INTEGER NOT NULL,first_retrieved_at TEXT,last_retrieved_at TEXT,final_url TEXT,redirect_chain TEXT,PRIMARY KEY(source_id,version));
        CREATE TEMP TABLE import_stage_units(source_id TEXT NOT NULL,version TEXT NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,page INTEGER,PRIMARY KEY(source_id,version,line,text));
        CREATE TEMP TABLE import_stage_git_units(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,blob_id TEXT NOT NULL,segmentation_kind TEXT NOT NULL,byte_start INTEGER NOT NULL,byte_end INTEGER NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,byte_start));")?;
    if table_exists(c, "imported_sources")? {
        c.execute(
            "INSERT INTO temp.import_stage_sources SELECT id,locator,kind,object_store,fetch_status,last_fetch_error,last_fetch_attempt_at,requested_revision,pinned_commit FROM imported_sources",
            [],
        )?;
    }
    if table_exists(c, "imported_versions")? {
        let order = if column_exists(c, "imported_versions", "fetched_order")? {
            "fetched_order"
        } else {
            "rowid"
        };
        let first = if column_exists(c, "imported_versions", "first_retrieved_at")? {
            "first_retrieved_at"
        } else {
            "NULL"
        };
        let last = if column_exists(c, "imported_versions", "last_retrieved_at")? {
            "last_retrieved_at"
        } else {
            "NULL"
        };
        c.execute(&format!("INSERT INTO temp.import_stage_versions SELECT source_id,version,content,{order},{first},{last},final_url,redirect_chain FROM imported_versions"), [])?;
    }
    if table_exists(c, "imported_units")? {
        let line_end = if column_exists(c, "imported_units", "line_end")? {
            "line_end"
        } else {
            "line"
        };
        let page = if column_exists(c, "imported_units", "page")? {
            "page"
        } else {
            "NULL"
        };
        c.execute(&format!("INSERT INTO temp.import_stage_units SELECT source_id,version,line,{line_end},text,vector,{page} FROM imported_units"), [])?;
    }
    let legacy_git_units = table_exists(c, "imported_git_units")?
        && !column_exists(c, "imported_git_units", "blob_id")?;
    if table_exists(c, "imported_git_units")? && !legacy_git_units {
        c.execute("INSERT INTO temp.import_stage_git_units SELECT source_id,commit_id,path,blob_id,segmentation_kind,byte_start,byte_end,line,line_end,vector FROM imported_git_units", [])?;
    }
    let source_rows = {
        let mut st =
            c.prepare("SELECT id,locator,kind FROM temp.import_stage_sources ORDER BY id")?;
        st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    c.execute_batch("DROP TABLE IF EXISTS temp.import_refresh_dimension; CREATE TEMP TABLE import_refresh_dimension(value INTEGER);")?;
    let (command, model, configured_dimension) = crate::history::configuration(c)?;
    let cfg = Some((command, model));
    let configured_dimension = (configured_dimension != 0).then_some(configured_dimension);
    let mut dimension = None;
    {
        let mut stmt = c.prepare("SELECT DISTINCT length(vector)/4 FROM temp.import_stage_units WHERE vector IS NOT NULL UNION SELECT DISTINCT length(vector)/4 FROM temp.import_stage_git_units WHERE vector IS NOT NULL")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let staged_dimension: usize = row.get(0)?;
            if dimension.is_some_and(|d| d != staged_dimension) {
                return Err("embedding dimension mismatch".into());
            }
            dimension = Some(staged_dimension);
        }
    }
    let cache_dir: String = c.query_row("PRAGMA database_list", [], |r| r.get(2))?;
    let cache_dir = Path::new(&cache_dir)
        .parent()
        .ok_or("database has no parent directory")?
        .join("repodb-cache");
    c.execute_batch("DROP TABLE IF EXISTS temp.import_pdf_pages; CREATE TEMP TABLE import_pdf_pages(page INTEGER PRIMARY KEY,text TEXT NOT NULL);")?;
    // This conservative cumulative accounting includes buffers and representations materialized during preparation.
    let mut refresh_memory_bytes = Budget::new(
        MAX_REFRESH_ACCOUNTING_BYTES,
        "import refresh preparation accounting",
    );
    for (id, locator, kind) in source_rows {
        if kind == "git" {
            let repository = if Path::new(&locator).is_dir() {
                locator.clone()
            } else {
                let mirror = cache_dir.join(format!("git-{}", checksum(locator.as_bytes())));
                fs::create_dir_all(&cache_dir)?;
                if !mirror.exists() {
                    let status = Command::new("git")
                        .args(["clone", "--mirror", &locator])
                        .arg(&mirror)
                        .status()?;
                    if !status.success() {
                        return Err(format!("git clone failed for {locator}").into());
                    }
                }
                mirror.to_string_lossy().into_owned()
            };
            c.execute(
                "UPDATE temp.import_stage_sources SET object_store=?1 WHERE id=?2",
                params![repository, id],
            )?;
            let (requested_revision, pinned_commit): (Option<String>, Option<String>) = c
                .query_row(
                    "SELECT requested_revision,pinned_commit FROM temp.import_stage_sources WHERE id=?1",
                    [&id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
            let revision = requested_revision.unwrap_or_else(|| "HEAD".to_owned());
            let commit = match pinned_commit {
                Some(commit) => commit,
                None => {
                    let spec = format!("{revision}^{{commit}}");
                    let resolved = String::from_utf8(git_output(
                        &repository,
                        &["rev-parse", "--verify", &spec],
                        128,
                        &mut refresh_memory_bytes,
                    )?)?
                    .trim()
                    .to_owned();
                    c.execute(
                        "UPDATE temp.import_stage_sources SET requested_revision=?1,pinned_commit=?2 WHERE id=?3",
                        params![revision, resolved, id],
                    )?;
                    resolved
                }
            };
            let commits_bytes = commit.into_bytes();
            let commits = String::from_utf8(commits_bytes)?;
            let mut commit_ids = Vec::new();
            for line in commits.lines() {
                refresh_memory_bytes.charge(line.len(), "Git commit identifiers")?;
                refresh_memory_bytes.reserve_vec(&mut commit_ids, 1, "Git commit vector")?;
                commit_ids.push(line.to_owned());
            }
            let mut commits = commit_ids;
            let selected = if history_limit == 1 {
                commits.into_iter().take(1).collect()
            } else {
                commits.reverse();
                commits
            };
            if history_limit == 1 && selected.is_empty() {
                c.execute(
                    "DELETE FROM temp.import_stage_versions WHERE source_id=?1",
                    [&id],
                )?;
                c.execute(
                    "DELETE FROM temp.import_stage_git_units WHERE source_id=?1",
                    [&id],
                )?;
            }
            for commit in selected {
                let paths = git_output(
                    &repository,
                    &["ls-tree", "-r", "--name-only", "-z", &commit],
                    refresh_memory_bytes
                        .remaining()
                        .saturating_sub(MAX_COMMAND_STDERR_BYTES),
                    &mut refresh_memory_bytes,
                )?;
                for path in paths.split(|b| *b == 0).filter(|p| !p.is_empty()) {
                    let Ok(path) = String::from_utf8(path.to_vec()) else {
                        continue;
                    };
                    let spec = format!("{commit}:{path}");
                    let bytes = git_output(
                        &repository,
                        &["show", &spec],
                        refresh_memory_bytes
                            .remaining()
                            .saturating_sub(MAX_COMMAND_STDERR_BYTES),
                        &mut refresh_memory_bytes,
                    )?;
                    let Ok(text) = String::from_utf8(bytes) else {
                        continue;
                    };
                    refresh_memory_bytes.charge(text.len(), "Git blob text")?;
                    let blob_id = String::from_utf8(git_output(
                        &repository,
                        &["rev-parse", &spec],
                        128,
                        &mut refresh_memory_bytes,
                    )?)?
                    .trim()
                    .to_owned();
                    let segmentation_kind = segmentation::kind(path.as_bytes());
                    let mut unit_error = None;
                    let segmentation_result = segmentation::visit(
                        segmentation_kind,
                        &text,
                        |unit| {
                            let result = (|| -> Result<()> {
                                let vector = cfg
                                    .as_ref()
                                    .map(|(cmd, model)| {
                                        import_embed(
                                            cmd,
                                            model,
                                            unit.text,
                                            &mut refresh_memory_bytes,
                                        )
                                    })
                                    .transpose()?;
                                if let Some(vector) = &vector {
                                    if configured_dimension.is_some_and(|d| d != vector.len())
                                        || dimension.is_some_and(|d| d != vector.len())
                                    {
                                        return Err("embedding dimension mismatch".into());
                                    }
                                    dimension = Some(vector.len());
                                }
                                refresh_memory_bytes.charge(path.len(), "Git unit path")?;
                                let vector_bytes = vector.as_ref().map_or(0, |v| v.len() * 4);
                                refresh_memory_bytes
                                    .charge(vector_bytes, "Git stored vector bytes")?;
                                c.execute("INSERT OR REPLACE INTO temp.import_stage_git_units(source_id,commit_id,path,blob_id,segmentation_kind,byte_start,byte_end,line,line_end,vector) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![id, commit, path, blob_id, segmentation_kind, unit.byte_start, unit.byte_end, unit.line_start, unit.line_end, vector.map(|v| as_bytes(&v))])?;
                                refresh_memory_bytes.release(path.len() + vector_bytes);
                                Ok(())
                            })();
                            if let Err(error) = result {
                                unit_error = Some(error);
                                return false;
                            }
                            true
                        },
                    );
                    segmentation_result?;
                    if let Some(error) = unit_error {
                        return Err(error);
                    }
                    refresh_memory_bytes.release(text.len());
                }
                let fetched_order: i64 = c.query_row(
                    "SELECT COALESCE(MAX(fetched_order),0)+1 FROM temp.import_stage_versions",
                    [],
                    |r| r.get(0),
                )?;
                c.execute("INSERT INTO temp.import_stage_versions(source_id,version,content,fetched_order) VALUES(?1,?2,'',?3) ON CONFLICT(source_id,version) DO UPDATE SET content=excluded.content,fetched_order=excluded.fetched_order", params![id, commit, fetched_order])?;
            }
            if legacy_git_units {
                stage_legacy_git_units(
                    c,
                    &id,
                    &repository,
                    history_limit,
                    &mut refresh_memory_bytes,
                )?;
            }
            let pruned = prune_staged_versions(c, &id, history_limit)?;
            record_pruned_versions(c, &id, &locator, pruned)?;
            continue;
        }
        let source_result = (|| -> Result<()> {
            if kind == "file" {
                if is_database_file(c, Path::new(&locator))? {
                    return Err(
                        "cannot refresh an import that points to RepoDB's own database file".into(),
                    );
                }
                let size = fs::metadata(&locator)?.len();
                refresh_memory_bytes.charge(size as usize, "local import input")?;
            }
            let (source_bytes, final_url, content_type, redirect_chain) = if kind == "file" {
                (read_all(fs::File::open(&locator)?)?, None, None, Vec::new())
            } else {
                let response = fetch_url(&locator, url_max_redirects, &mut refresh_memory_bytes)?;
                (
                    response.body,
                    Some(response.final_url),
                    Some(response.content_type),
                    response.redirect_chain,
                )
            };
            let source_size = source_bytes.len() as u64;
            if kind == "url" {
                let media_type = content_type
                    .as_deref()
                    .unwrap_or_default()
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase();
                let is_supported_type = media_type.is_empty()
                    || media_type.starts_with("text/")
                    || matches!(
                        media_type.as_str(),
                        "application/xhtml+xml"
                            | "application/json"
                            | "application/xml"
                            | "application/pdf"
                    )
                    || source_bytes.starts_with(b"%PDF-");
                if !is_supported_type {
                    return Err(
                        format!("unsupported URL response content type {media_type}").into(),
                    );
                }
            }
            let source_version = checksum(&source_bytes);
            let is_pdf = source_bytes
                .windows(5)
                .take(1024)
                .any(|window| window == b"%PDF-")
                || Path::new(&locator)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
            let mut url_version = None;
            let (version, content) = if is_pdf {
                let cache = cache_dir.join(format!("{source_version}.json"));
                c.execute("DELETE FROM temp.import_pdf_pages", [])?;
                let cached = visit_cache_pages(&cache, |page, text| {
                    c.execute(
                        "INSERT INTO temp.import_pdf_pages(page,text) VALUES(?1,?2)",
                        params![page, text],
                    )?;
                    Ok(())
                })?;
                if cached.is_none() {
                    c.execute("DELETE FROM temp.import_pdf_pages", [])?;
                    let mut child = Command::new("pdftotext")
                        .args(["-layout", "-enc", "UTF-8", "-", "-"])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .map_err(|error| {
                            format!("PDF imports require pdftotext on PATH: {error}")
                        })?;
                    let output = bounded_command_output_with_input(
                        &mut child,
                        &source_bytes,
                        refresh_memory_bytes
                            .remaining()
                            .saturating_sub(MAX_COMMAND_STDERR_BYTES),
                        &mut refresh_memory_bytes,
                    )?;
                    if !output.0.success() {
                        return Err(format!(
                            "PDF conversion failed for {locator}: {}",
                            String::from_utf8_lossy(&output.2)
                        )
                        .into());
                    }
                    let text = String::from_utf8(output.1)?;
                    refresh_memory_bytes.charge(text.len(), "PDF conversion output")?;
                    let pages = text.split('\u{c}');
                    write_cache(&cache, pages.clone())?;
                    for (page, text) in pages.enumerate() {
                        c.execute(
                            "INSERT INTO temp.import_pdf_pages(page,text) VALUES(?1,?2)",
                            params![page + 1, text],
                        )?;
                    }
                }
                // Keep the converted snapshot alongside the units. The conversion cache is only
                // an optimization; it may be deleted without losing the imported representation.
                let pages = {
                    let mut statement =
                        c.prepare("SELECT text FROM temp.import_pdf_pages ORDER BY page")?;
                    statement
                        .query_map([], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?
                };
                if kind == "url" {
                    let extracted_text = pages.join("\u{c}");
                    url_version = Some(checksum(extracted_text.as_bytes()));
                }
                (source_version, Some(pdf_representation(&pages)?))
            } else {
                // Imported version identity is the original source, not its decoded representation.
                let source_text = String::from_utf8(source_bytes)?;
                let content_type_is_html = content_type.as_deref().is_some_and(|content_type| {
                    let media_type = content_type.split(';').next().unwrap_or_default().trim();
                    media_type.eq_ignore_ascii_case("text/html")
                        || media_type.eq_ignore_ascii_case("application/xhtml+xml")
                });
                let path_is_html = Path::new(&locator)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        matches!(extension.to_ascii_lowercase().as_str(), "html" | "htm")
                    });
                let is_html = content_type_is_html
                    || content_type.as_deref().is_none_or(str::is_empty) && path_is_html;
                let content = if is_html {
                    html_text(&source_text)
                } else {
                    source_text
                };
                if content.trim().is_empty() {
                    return Err(
                        format!("no searchable text found in local document {locator}").into(),
                    );
                }
                refresh_memory_bytes.charge(content.len(), "decoded local import text")?;
                if kind == "url" {
                    url_version = Some(checksum(content.as_bytes()));
                }
                (source_version, Some(content))
            };
            let version = if kind == "url" {
                url_version.ok_or("URL response did not produce searchable text")?
            } else {
                version
            };
            let existing_version: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM temp.import_stage_versions WHERE source_id=?1 AND version=?2 UNION SELECT 1 FROM imported_versions WHERE source_id=?1 AND version=?2)",
            params![id, version],
            |row| row.get(0),
        )?;
            let estimated_growth = existing_version.then_some(0);
            let database_path: String =
                c.query_row("PRAGMA database_list", [], |row| row.get(2))?;
            let database_size = fs::metadata(database_path)
                .ok()
                .map(|metadata| metadata.len());
            let warning_basis_bytes = estimated_growth.unwrap_or(source_size);
            let warn_large = warning_basis_bytes >= LARGE_IMPORT_WARNING_THRESHOLD_BYTES
                || database_size.is_some_and(|size| warning_basis_bytes > size.saturating_mul(2));
            let assessment = ImportSizeAssessment {
                source_bytes: source_size,
                estimated_net_db_growth_bytes: estimated_growth,
                warning_basis_bytes,
                warning_basis: if estimated_growth.is_some() {
                    "estimated_net_db_growth"
                } else {
                    "source_bytes_fallback"
                },
                warn_large,
            };
            if warn_large && size_limit == SizeLimit::Error {
                return Err(format!(
                "import size limit exceeded: source bytes={}, estimated net database growth={}, warning basis={} ({})",
                assessment.source_bytes,
                assessment.estimated_net_db_growth_bytes.map_or_else(|| "unknown".into(), |value| value.to_string()),
                assessment.warning_basis_bytes,
                assessment.warning_basis,
            ).into());
            }
            if size_limit != SizeLimit::Ignore {
                size_assessments.push(assessment);
            }
            let fetched_order: i64 = c.query_row(
                "SELECT COALESCE(MAX(fetched_order),0)+1 FROM temp.import_stage_versions",
                [],
                |r| r.get(0),
            )?;
            if kind == "url" {
                let observed_at = utc_rfc3339_now();
                let redirect_chain_json = serde_json::to_string(&redirect_chain)?;
                c.execute(
                "INSERT INTO temp.import_stage_versions(source_id,version,content,fetched_order,first_retrieved_at,last_retrieved_at,final_url,redirect_chain) VALUES(?1,?2,?3,?4,?5,?5,?6,?7) ON CONFLICT(source_id,version) DO UPDATE SET content=excluded.content,fetched_order=excluded.fetched_order,first_retrieved_at=import_stage_versions.first_retrieved_at,last_retrieved_at=excluded.last_retrieved_at,final_url=excluded.final_url,redirect_chain=excluded.redirect_chain",
                params![id, version, content.as_deref().unwrap_or(""), fetched_order, observed_at, final_url.as_deref().unwrap_or(&locator), redirect_chain_json],
            )?;
                c.execute(
                "UPDATE temp.import_stage_sources SET fetch_status='success',last_fetch_error=NULL,last_fetch_attempt_at=?1 WHERE id=?2",
                params![observed_at, id],
            )?;
            } else {
                c.execute(
                "INSERT INTO temp.import_stage_versions(source_id,version,content,fetched_order) VALUES(?1,?2,?3,?4) ON CONFLICT(source_id,version) DO UPDATE SET content=excluded.content,fetched_order=excluded.fetched_order",
                params![id, version, content.as_deref().unwrap_or(""), fetched_order],
            )?;
            }
            if is_pdf {
                let mut stmt =
                    c.prepare("SELECT page,text FROM temp.import_pdf_pages ORDER BY page")?;
                let mut rows = stmt.query([])?;
                let mut last_line = 0;
                while let Some(row) = rows.next()? {
                    let page: usize = row.get(0)?;
                    let text: String = row.get(1)?;
                    let mut failure = None;
                    crate::segmentation::visit("paragraph", &text, |unit| {
                        let result = (|| -> Result<()> {
                            refresh_memory_bytes
                                .charge(unit.text.len(), "PDF indexed unit text")?;
                            let vector = cfg
                                .as_ref()
                                .map(|(cmd, model)| {
                                    import_embed(cmd, model, unit.text, &mut refresh_memory_bytes)
                                })
                                .transpose()?;
                            if let Some(vector) = &vector {
                                if dimension.is_some_and(|d| d != vector.len())
                                    || configured_dimension.is_some_and(|d| d != vector.len())
                                {
                                    return Err("embedding dimension mismatch".into());
                                }
                                dimension = Some(vector.len());
                            }
                            let vector_bytes = vector.as_ref().map_or(0, |v| v.len() * 4);
                            refresh_memory_bytes.charge(vector_bytes, "PDF stored vector bytes")?;
                            let line = unit.line_start.max(last_line + 1);
                            let line_end = line + unit.line_end.saturating_sub(unit.line_start);
                            last_line = line_end;
                            c.execute(
                            "INSERT OR REPLACE INTO temp.import_stage_units(source_id,version,line,line_end,text,vector,page) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                            params![id, version, line, line_end, unit.text, vector.map(|v| as_bytes(&v)), page],
                        )?;
                            refresh_memory_bytes.release(unit.text.len() + vector_bytes);
                            Ok(())
                        })();
                        if let Err(error) = result {
                            failure = Some(error);
                            false
                        } else {
                            true
                        }
                    })?;
                    if let Some(error) = failure {
                        return Err(error);
                    }
                }
            } else {
                let content = content.expect("non-PDF content exists");
                let kind = segmentation::kind(Path::new(&locator).as_os_str().as_encoded_bytes());
                let mut unit_error = None;
                let segmentation_result = segmentation::visit(kind, &content, |unit| {
                    let result = (|| -> Result<()> {
                        let vector = cfg
                            .as_ref()
                            .map(|(cmd, model)| {
                                import_embed(cmd, model, unit.text, &mut refresh_memory_bytes)
                            })
                            .transpose()?;
                        if let Some(vector) = &vector {
                            if dimension.is_some_and(|d| d != vector.len())
                                || configured_dimension.is_some_and(|d| d != vector.len())
                            {
                                return Err("embedding dimension mismatch".into());
                            }
                            dimension = Some(vector.len());
                        }
                        let vector_bytes = vector.as_ref().map_or(0, |v| v.len() * 4);
                        refresh_memory_bytes.charge(vector_bytes, "stored vector bytes")?;
                        c.execute("INSERT OR REPLACE INTO temp.import_stage_units(source_id,version,line,line_end,text,vector,page) VALUES(?1,?2,?3,?4,?5,?6,NULL)", params![id, version, unit.line_start, unit.line_end, unit.text, vector.map(|v| as_bytes(&v))])?;
                        refresh_memory_bytes.release(vector_bytes);
                        Ok(())
                    })();
                    if let Err(error) = result {
                        unit_error = Some(error);
                        return false;
                    }
                    true
                });
                segmentation_result?;
                if let Some(error) = unit_error {
                    return Err(error);
                }
            }
            let pruned = prune_staged_versions(c, &id, history_limit)?;
            record_pruned_versions(c, &id, &locator, pruned)?;
            Ok(())
        })();
        if let Err(error) = source_result {
            if kind == "url" {
                // Fetch errors are attempt-state; unlike Git pins, they describe this failed attempt.
                let message: String = error.to_string().chars().take(1024).collect();
                c.execute(
                    "UPDATE imported_sources SET fetch_status='stale',last_fetch_error=?1,last_fetch_attempt_at=?2 WHERE id=?3",
                    params![message, utc_rfc3339_now(), id],
                )?;
            }
            return Err(error);
        }
    }
    if let Some(dimension) = dimension {
        c.execute(
            "INSERT INTO temp.import_refresh_dimension VALUES(?1)",
            [dimension as i64],
        )?;
    }
    Ok(size_assessments)
}

pub fn validate_dimensions(c: &Connection) -> Result<()> {
    let imported: Option<i64> = c
        .query_row(
            "SELECT value FROM temp.import_refresh_dimension LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let history_dimension_table: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE type='table' AND name='refresh_dimension')",
        [],
        |row| row.get(0),
    )?;
    let history: Option<i64> = if history_dimension_table {
        c.query_row(
            "SELECT value FROM temp.refresh_dimension LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?
    } else {
        let configured_dimension = crate::history::configuration(c)?.2;
        (configured_dimension != 0).then_some(configured_dimension as i64)
    };
    if imported.zip(history).is_some_and(|(a, b)| a != b) {
        return Err("embedding dimension mismatch".into());
    }
    Ok(())
}

pub fn publish(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    init(tx)?;
    if !column_exists(tx, "imported_git_units", "blob_id")? {
        tx.execute_batch("DROP TABLE imported_git_units; CREATE TABLE imported_git_units(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,blob_id TEXT NOT NULL,segmentation_kind TEXT NOT NULL,byte_start INTEGER NOT NULL,byte_end INTEGER NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,byte_start));")?;
    }
    if let Some(dimension) = tx
        .query_row(
            "SELECT value FROM temp.import_refresh_dimension LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    {
        tx.execute(
            "UPDATE embedding_config SET value=?1 WHERE key='dimension'",
            [dimension.to_string()],
        )?;
    }
    tx.execute("UPDATE imported_sources SET object_store=(SELECT object_store FROM temp.import_stage_sources s WHERE s.id=imported_sources.id),fetch_status=(SELECT fetch_status FROM temp.import_stage_sources s WHERE s.id=imported_sources.id),last_fetch_error=(SELECT last_fetch_error FROM temp.import_stage_sources s WHERE s.id=imported_sources.id),last_fetch_attempt_at=(SELECT last_fetch_attempt_at FROM temp.import_stage_sources s WHERE s.id=imported_sources.id),requested_revision=(SELECT requested_revision FROM temp.import_stage_sources s WHERE s.id=imported_sources.id),pinned_commit=(SELECT pinned_commit FROM temp.import_stage_sources s WHERE s.id=imported_sources.id)",
 [])?;
    for (table, stage) in [
        ("imported_versions", "import_stage_versions"),
        ("imported_units", "import_stage_units"),
        ("imported_git_units", "import_stage_git_units"),
    ] {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
        if table == "imported_versions" {
            tx.execute("INSERT INTO imported_versions(source_id,version,content,fetched_order,first_retrieved_at,last_retrieved_at,final_url,redirect_chain) SELECT source_id,version,content,fetched_order,first_retrieved_at,last_retrieved_at,final_url,redirect_chain FROM temp.import_stage_versions", [])?;
            let representations = {
                let mut statement = tx.prepare(
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
            for (source, version, content) in representations {
                tx.execute("UPDATE imported_versions SET representation_sha256=?3 WHERE source_id=?1 AND version=?2", params![source, version, checksum(content.as_bytes())])?;
            }
            continue;
        }
        let columns = match table {
            "imported_versions" => unreachable!(),
            "imported_units" => "source_id,version,line,line_end,text,vector,page",
            "imported_git_units" => {
                "source_id,commit_id,path,blob_id,segmentation_kind,byte_start,byte_end,line,line_end,vector"
            }
            _ => unreachable!(),
        };
        tx.execute(
            &format!("INSERT INTO {table}({columns}) SELECT {columns} FROM temp.{stage}"),
            [],
        )?;
    }
    Ok(())
}

pub fn reembed(c: &Connection, command: &str, model: &str) -> Result<Option<usize>> {
    init(c)?;
    c.execute_batch("PRAGMA temp_store=FILE; DROP TABLE IF EXISTS temp.import_reembed_vectors; CREATE TEMP TABLE import_reembed_vectors(kind TEXT NOT NULL, source_id TEXT NOT NULL, version TEXT NOT NULL, line INTEGER NOT NULL, text TEXT NOT NULL, path TEXT, byte_start INTEGER, vector BLOB NOT NULL);")?;
    let mut dimension = None;
    {
        let current = column_exists(c, "imported_git_units", "blob_id")?;
        let sql = if current {
            "SELECT source_id,version,line,text,'file',NULL,NULL,NULL,NULL FROM imported_units UNION ALL SELECT u.source_id,u.commit_id,u.line,NULL,'git',u.path,u.blob_id,u.byte_start,u.byte_end FROM imported_git_units u ORDER BY 1,2,3"
        } else {
            "SELECT source_id,version,line,text,'file',NULL,NULL,NULL,NULL FROM imported_units UNION ALL SELECT source_id,commit_id,line,text,'git',path,NULL,NULL,NULL FROM imported_git_units ORDER BY 1,2,3"
        };
        let mut stmt = c.prepare(sql)?;
        let mut rows = stmt.query([])?;
        let mut staged =
            c.prepare("INSERT INTO temp.import_reembed_vectors VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
        let mut batch_readers: std::collections::HashMap<String, GitBlobReader> =
            std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let source: String = row.get(0)?;
            let version: String = row.get(1)?;
            let line: usize = row.get(2)?;
            let kind: String = row.get(4)?;
            let path: Option<String> = row.get(5)?;
            let blob_id: Option<String> = row.get(6)?;
            let byte_start: Option<usize> = row.get(7)?;
            let text: String = if let (Some(blob), Some(start), Some(end)) = (
                blob_id.as_deref(),
                byte_start,
                row.get::<_, Option<usize>>(8)?,
            ) {
                let locator: String = c.query_row(
                    "SELECT COALESCE(object_store,locator) FROM imported_sources WHERE id=?1",
                    [&source],
                    |r| r.get(0),
                )?;
                let reader = batch_reader(&mut batch_readers, &locator)?;
                hydrate_git_unit_with_reader(reader, blob, start, end)?
            } else {
                row.get::<_, String>(3)?
            };
            let vector = embed(command, model, &text)?;
            if dimension.is_some_and(|d| d != vector.len()) {
                return Err("embedding dimension mismatch".into());
            }
            dimension = Some(vector.len());
            staged.execute(params![
                kind,
                source,
                version,
                line,
                text,
                path,
                byte_start,
                as_bytes(&vector)
            ])?;
        }
        for reader in batch_readers.values_mut() {
            reader.finish()?;
        }
    }
    c.execute("UPDATE imported_units SET vector=(SELECT vector FROM temp.import_reembed_vectors v WHERE v.kind='file' AND v.source_id=imported_units.source_id AND v.version=imported_units.version AND v.line=imported_units.line AND v.text=imported_units.text)", [])?;
    if column_exists(c, "imported_git_units", "blob_id")? {
        c.execute("UPDATE imported_git_units SET vector=(SELECT vector FROM temp.import_reembed_vectors v WHERE v.kind='git' AND v.source_id=imported_git_units.source_id AND v.version=imported_git_units.commit_id AND v.line=imported_git_units.line AND v.path=imported_git_units.path AND v.byte_start=imported_git_units.byte_start)", [])?;
    } else {
        c.execute("UPDATE imported_git_units SET vector=(SELECT vector FROM temp.import_reembed_vectors v WHERE v.kind='git' AND v.source_id=imported_git_units.source_id AND v.version=imported_git_units.commit_id AND v.line=imported_git_units.line AND v.path=imported_git_units.path AND v.text=imported_git_units.text)", [])?;
    }
    c.execute_batch("DROP TABLE temp.import_reembed_vectors")?;
    Ok(dimension)
}

fn slice_git_unit(blob_bytes: &[u8], blob_id: &str, start: usize, end: usize) -> Result<String> {
    git_ranges::slice_utf8(blob_bytes, blob_id, start, end, "imported")
}

fn hydrate_git_unit_with_reader(
    reader: &mut GitBlobReader,
    blob_id: &str,
    start: usize,
    end: usize,
) -> Result<String> {
    let bytes = reader.blob(blob_id)?;
    slice_git_unit(&bytes, blob_id, start, end)
}

fn batch_reader<'a>(
    readers: &'a mut std::collections::HashMap<String, GitBlobReader>,
    object_store: &str,
) -> Result<&'a mut GitBlobReader> {
    if !readers.contains_key(object_store) {
        readers.insert(
            object_store.to_owned(),
            GitBlobReader::new(Some(object_store))?,
        );
    }
    Ok(readers.get_mut(object_store).unwrap())
}

pub fn search(
    c: &Connection,
    query: &str,
    mode: &str,
    limit: usize,
    include: impl Fn(&SearchResult) -> bool,
) -> Result<Vec<SearchResult>> {
    init(c)?;
    let semantic = mode != "exact";
    let mut vector = None;
    if semantic {
        let (cmd,model): (String,String) = c.query_row("SELECT (SELECT value FROM embedding_config WHERE key='command'),(SELECT value FROM embedding_config WHERE key='model')", [], |r| Ok((r.get(0)?,r.get(1)?))).map_err(|_| "no embeddings; run refresh")?;
        vector = Some(as_bytes(&embed(&cmd, &model, query)?));
        let dimension: i64 = c
            .query_row(
                "SELECT value FROM embedding_config WHERE key='dimension'",
                [],
                |r| r.get::<_, String>(0),
            )?
            .parse()?;
        if dimension != 0 && vector.as_ref().unwrap().len() != dimension as usize * 4 {
            return Err("query embedding dimension mismatch".into());
        }
    }
    let mut st = c.prepare("SELECT s.locator,u.line,u.line_end,u.text,u.version,u.vector,u.page,v.first_retrieved_at,v.last_retrieved_at,s.kind,v.final_url,v.redirect_chain,s.fetch_status,s.last_fetch_error FROM imported_units u JOIN imported_sources s ON s.id=u.source_id LEFT JOIN imported_versions v ON v.source_id=u.source_id AND v.version=u.version WHERE s.kind<>'git'")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, usize>(1)?,
            r.get::<_, usize>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<Vec<u8>>>(5)?,
            r.get::<_, Option<usize>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, Option<String>>(8)?,
            r.get::<_, String>(9)?,
            r.get::<_, Option<String>>(10)?,
            r.get::<_, Option<String>>(11)?,
            r.get::<_, Option<String>>(12)?,
            r.get::<_, Option<String>>(13)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (
            locator,
            line,
            line_end,
            text,
            version,
            stored,
            page,
            first_retrieved_at,
            last_retrieved_at,
            source_kind,
            final_url,
            redirect_chain_json,
            fetch_status,
            fetch_error,
        ) = row?;
        let requested_url = (source_kind == "url").then(|| locator.clone());
        let redirect_chain = redirect_chain_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        if semantic {
            if let (Some(v), Some(q)) = (stored.as_deref(), vector.as_deref()) {
                if v.len() != q.len() {
                    return Err("imported embedding dimension mismatch".into());
                }
                let d: f64 =
                    c.query_row("SELECT vec_distance_cosine(?1,?2)", params![v, q], |r| {
                        r.get(0)
                    })?;
                out.push((
                    d,
                    SearchResult {
                        path: locator.clone(),
                        line_start: line,
                        line_end,
                        text,
                        commit_id: None,
                        distance: Some(d),
                        blob_id: Some(version),
                        page,
                        repo_id: None,
                        first_retrieved_at: first_retrieved_at.clone(),
                        last_retrieved_at: last_retrieved_at.clone(),
                        requested_url: requested_url.clone(),
                        final_url: final_url.clone(),
                        redirect_chain: redirect_chain.clone(),
                        fetch_status: fetch_status.clone(),
                        fetch_error: fetch_error.clone(),
                        worktree: None,
                        semantic_worktree: None,
                    },
                    None,
                ));
            }
        } else if text.to_lowercase().contains(&query.to_lowercase()) {
            out.push((
                0.0,
                SearchResult {
                    path: locator,
                    line_start: line,
                    line_end,
                    text,
                    commit_id: None,
                    distance: None,
                    blob_id: Some(version),
                    page,
                    repo_id: None,
                    first_retrieved_at,
                    last_retrieved_at,
                    requested_url,
                    final_url,
                    redirect_chain,
                    fetch_status,
                    fetch_error,
                    worktree: None,
                    semantic_worktree: None,
                },
                None,
            ));
        }
    }
    let git_current = column_exists(c, "imported_git_units", "blob_id")?;
    let sql = if git_current {
        "SELECT s.locator,COALESCE(s.object_store,s.locator),u.path,u.line,u.line_end,u.blob_id,u.byte_start,u.byte_end,u.commit_id,u.vector,NULL FROM imported_git_units u JOIN imported_sources s ON s.id=u.source_id"
    } else {
        "SELECT s.locator,COALESCE(s.object_store,s.locator),u.path,u.line,u.line_end,NULL,NULL,NULL,u.commit_id,u.vector,u.text FROM imported_git_units u JOIN imported_sources s ON s.id=u.source_id"
    };
    let mut git_rows = c.prepare(sql)?;
    let rows = git_rows.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, usize>(3)?,
            r.get::<_, usize>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<usize>>(6)?,
            r.get::<_, Option<usize>>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, Option<Vec<u8>>>(9)?,
            r.get::<_, Option<String>>(10)?,
        ))
    })?;
    let mut batch_readers: std::collections::HashMap<String, GitBlobReader> =
        std::collections::HashMap::new();
    for row in rows {
        let (
            repo_id,
            object_store,
            path,
            line,
            line_end,
            blob_id,
            byte_start,
            byte_end,
            commit,
            stored,
            legacy_text,
        ) = row?;
        let deferred = semantic && mode != "combined" && blob_id.is_some();
        let deferred_payload = if deferred {
            let (Some(blob), Some(start), Some(end)) = (blob_id.clone(), byte_start, byte_end)
            else {
                return Err("invalid imported Git byte range".into());
            };
            Some((object_store.clone(), blob, start, end))
        } else {
            None
        };
        let text = if deferred {
            String::new()
        } else {
            match (blob_id.as_deref(), byte_start, byte_end) {
                (Some(blob), Some(start), Some(end)) => {
                    let reader = batch_reader(&mut batch_readers, &object_store)?;
                    hydrate_git_unit_with_reader(reader, blob, start, end)?
                }
                (None, None, None) => legacy_text.ok_or("missing legacy imported Git text")?,
                _ => return Err("invalid imported Git byte range".into()),
            }
        };
        if semantic {
            if let (Some(stored), Some(query_vector)) = (stored.as_deref(), vector.as_deref()) {
                let distance: f64 = c.query_row(
                    "SELECT vec_distance_cosine(?1,?2)",
                    params![stored, query_vector],
                    |r| r.get(0),
                )?;
                out.push((
                    distance,
                    SearchResult {
                        path,
                        line_start: line,
                        line_end,
                        text,
                        commit_id: Some(commit),
                        distance: Some(distance),
                        blob_id: None,
                        page: None,
                        repo_id: Some(repo_id),
                        first_retrieved_at: None,
                        last_retrieved_at: None,
                        requested_url: None,
                        final_url: None,
                        redirect_chain: None,
                        fetch_status: None,
                        fetch_error: None,
                        worktree: None,
                        semantic_worktree: None,
                    },
                    deferred_payload,
                ));
            }
        } else if text.to_lowercase().contains(&query.to_lowercase()) {
            out.push((
                0.0,
                SearchResult {
                    path,
                    line_start: line,
                    line_end,
                    text,
                    commit_id: Some(commit),
                    distance: None,
                    blob_id: None,
                    page: None,
                    repo_id: Some(repo_id),
                    first_retrieved_at: None,
                    last_retrieved_at: None,
                    requested_url: None,
                    final_url: None,
                    redirect_chain: None,
                    fetch_status: None,
                    fetch_error: None,
                    worktree: None,
                    semantic_worktree: None,
                },
                None,
            ));
        }
    }
    out.retain(|(_, result, _)| include(result));
    if semantic {
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        if mode == "combined" {
            let mut rank = 0;
            out.retain(|(_, result, _)| {
                let keep =
                    rank < limit || result.text.to_lowercase().contains(&query.to_lowercase());
                rank += 1;
                keep
            });
        } else {
            out.truncate(limit);
        }
    }
    let results = out
        .into_iter()
        .map(|(_, mut result, deferred)| {
            if let Some((store, blob, start, end)) = deferred {
                let reader = batch_reader(&mut batch_readers, &store)?;
                result.text = hydrate_git_unit_with_reader(reader, &blob, start, end)?;
            }
            Ok(result)
        })
        .collect::<Result<Vec<_>>>()?;
    for reader in batch_readers.values_mut() {
        reader.finish()?;
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::{
        SizeLimit, batch_reader, bounded_command_output_with_input, checksum,
        hydrate_git_unit_with_reader, pdf_representation, refresh_with_size_limit, register,
        set_url_max_redirects, slice_git_unit, visit_cache_pages, write_cache,
    };
    use crate::budget::Budget;
    use rusqlite::Connection;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
    };

    fn size_policy_database(root: &std::path::Path) -> (Connection, std::path::PathBuf) {
        let database_path = root.join("repodb.sqlite");
        let connection = Connection::open(&database_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE embedding_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);",
            )
            .unwrap();
        let embedder = root.join("embedder.sh");
        std::fs::write(&embedder, "#!/bin/sh\ncat >/dev/null\nprintf '[1.0]'\n").unwrap();
        std::fs::set_permissions(&embedder, std::fs::Permissions::from_mode(0o755)).unwrap();
        connection
            .execute(
                "INSERT INTO embedding_config VALUES('command',?1)",
                [embedder.to_string_lossy().as_ref()],
            )
            .unwrap();
        connection
            .execute("INSERT INTO embedding_config VALUES('model','test')", [])
            .unwrap();
        connection
            .execute("INSERT INTO embedding_config VALUES('dimension','0')", [])
            .unwrap();
        (connection, database_path)
    }

    #[test]
    fn callable_url_refresh_enforces_a_configured_redirect_limit() {
        let root = tempfile::tempdir().unwrap();
        let (connection, _) = size_policy_database(root.path());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (path, target) in [("/one", "/two"), ("/two", "/three")] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                let count = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                assert!(request.starts_with(&format!("GET {path} ")), "{request}");
                write!(
                    stream,
                    "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
        });
        let url = format!("http://{address}/one");
        register(&connection, &url, None).unwrap();
        set_url_max_redirects(&connection, 1).unwrap();
        assert!(set_url_max_redirects(&connection, 21).is_err());

        let error = refresh_with_size_limit(&connection, SizeLimit::Ignore).unwrap_err();

        assert!(error.to_string().contains("URL fetch failed"));
        server.join().unwrap();
        let (status, error_message): (String, String) = connection
            .query_row(
                "SELECT fetch_status,last_fetch_error FROM imported_sources WHERE locator=?1",
                [&url],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "stale");
        assert!(error_message.contains("URL fetch failed"));
        let versions: i64 = connection
            .query_row("SELECT count(*) FROM imported_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(versions, 0);
    }

    #[test]
    fn callable_size_policy_rejects_atomically_warns_and_ignores_without_temp_files() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("document.txt");
        std::fs::write(&source, "initial").unwrap();
        let (connection, database_path) = size_policy_database(root.path());
        register(&connection, source.to_str().unwrap(), None).unwrap();
        let database_size = std::fs::metadata(&database_path).unwrap().len();
        let source_bytes = database_size.saturating_mul(2) + 1;
        std::fs::write(&source, vec![b'x'; source_bytes as usize]).unwrap();
        let source_size = std::fs::metadata(&source).unwrap().len();

        let rejected = refresh_with_size_limit(&connection, SizeLimit::Error).unwrap_err();
        assert!(rejected.to_string().contains("import size limit exceeded"));
        let published_versions: i64 = connection
            .query_row("SELECT count(*) FROM imported_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            published_versions, 0,
            "rejection must not publish staged data"
        );

        let warned = refresh_with_size_limit(&connection, SizeLimit::Warn).unwrap();
        assert_eq!(warned.len(), 1);
        assert_eq!(warned[0].source_bytes, source_size);
        assert_eq!(warned[0].estimated_net_db_growth_bytes, None);
        assert_eq!(warned[0].warning_basis_bytes, source_size);
        assert_eq!(warned[0].warning_basis, "source_bytes_fallback");
        assert!(warned[0].warn_large);

        let ignored = refresh_with_size_limit(&connection, SizeLimit::Ignore).unwrap();
        assert!(
            ignored.is_empty(),
            "ignore continues without reporting warnings"
        );

        let source_id: String = connection
            .query_row(
                "SELECT id FROM imported_sources WHERE locator=?1",
                [source.to_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        let source_hash = checksum(&std::fs::read(&source).unwrap());
        connection
            .execute(
                "INSERT INTO imported_versions(source_id,version,content,fetched_order) VALUES(?1,?2,'',1)",
                rusqlite::params![source_id, source_hash],
            )
            .unwrap();
        let deduplicated = refresh_with_size_limit(&connection, SizeLimit::Error).unwrap();
        assert_eq!(deduplicated[0].estimated_net_db_growth_bytes, Some(0));
        assert_eq!(deduplicated[0].warning_basis, "estimated_net_db_growth");
        assert!(!deduplicated[0].warn_large);

        assert!(source.exists());
        assert!(!root.path().join("repodb-cache").exists());
        assert_eq!(
            std::fs::metadata(&database_path).unwrap().len(),
            database_size
        );
    }

    #[test]
    fn size_policy_does_not_claim_zero_growth_for_a_different_source() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.txt");
        let second = root.path().join("second.txt");
        let content = "identical imported text";
        std::fs::write(&first, content).unwrap();
        std::fs::write(&second, content).unwrap();
        let (connection, _) = size_policy_database(root.path());
        register(&connection, first.to_str().unwrap(), None).unwrap();
        register(&connection, second.to_str().unwrap(), None).unwrap();
        let first_id: String = connection
            .query_row(
                "SELECT id FROM imported_sources WHERE locator=?1",
                [first.to_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        let version = checksum(content.as_bytes());
        connection
            .execute(
                "INSERT INTO imported_versions(source_id,version,content,fetched_order) VALUES(?1,?2,'',1)",
                rusqlite::params![first_id, version],
            )
            .unwrap();

        let assessments = refresh_with_size_limit(&connection, SizeLimit::Warn).unwrap();
        assert_eq!(assessments.len(), 2);
        assert_eq!(
            assessments
                .iter()
                .filter(|assessment| assessment.estimated_net_db_growth_bytes == Some(0))
                .count(),
            1,
            "only the source with the retained identical version can claim zero growth"
        );
        assert_eq!(
            assessments
                .iter()
                .filter(|assessment| assessment.estimated_net_db_growth_bytes.is_none())
                .count(),
            1
        );
    }

    #[test]
    fn size_error_policy_never_gates_git_import_content() {
        let root = tempfile::tempdir().unwrap();
        let (connection, database_path) = size_policy_database(root.path());
        let database_size = std::fs::metadata(&database_path).unwrap().len();
        let repository = root.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "RepoDB test"]);
        git(&["config", "user.email", "test@example.invalid"]);
        std::fs::write(
            repository.join("large.txt"),
            vec![b'x'; database_size.saturating_mul(2) as usize + 1],
        )
        .unwrap();
        git(&["add", "large.txt"]);
        git(&["commit", "-qm", "large Git content"]);
        register(&connection, repository.to_str().unwrap(), None).unwrap();

        let assessment = refresh_with_size_limit(&connection, SizeLimit::Error).unwrap();

        assert!(
            assessment.is_empty(),
            "Git commits are outside document-size policy"
        );
    }

    #[test]
    fn repeated_reembed_units_from_one_blob_share_an_operation_scoped_reader() {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(root.path())
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        let content = "first unit\nsecond unit\n";
        std::fs::write(root.path().join("units.txt"), content).unwrap();
        git(&["add", "units.txt"]);
        let blob = git(&["hash-object", "units.txt"]);
        let mut readers = std::collections::HashMap::new();
        let reader = batch_reader(&mut readers, root.path().to_str().unwrap()).unwrap();

        assert_eq!(
            hydrate_git_unit_with_reader(reader, &blob, 0, 10).unwrap(),
            "first unit"
        );
        assert_eq!(
            hydrate_git_unit_with_reader(reader, &blob, 11, 22).unwrap(),
            "second unit"
        );
        assert_eq!(readers.len(), 1, "one persistent reader serves both units");
    }

    #[test]
    fn git_unit_slice_validates_utf8_byte_ranges() {
        assert_eq!(
            slice_git_unit("héllo".as_bytes(), "blob", 1, 3).unwrap(),
            "é"
        );
        assert!(
            slice_git_unit(b"text", "blob", 3, 2)
                .unwrap_err()
                .to_string()
                .contains("invalid imported Git byte range 3..2")
        );
        assert!(
            slice_git_unit(b"text", "blob", 0, 99)
                .unwrap_err()
                .to_string()
                .contains("invalid imported Git byte range 0..99 for blob blob")
        );
        assert!(
            slice_git_unit(b"\xff", "blob", 0, 1)
                .unwrap_err()
                .to_string()
                .contains("no longer valid UTF-8")
        );
    }

    #[test]
    fn pdf_representation_keeps_pages_and_detects_tampering() {
        let pages = vec!["first page".to_owned(), "second\npage".to_owned()];
        let representation = pdf_representation(&pages).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&representation).unwrap();
        let canonical_pages = serde_json::to_string(&pages).unwrap();
        assert_eq!(parsed["pages"], serde_json::json!(pages));
        assert_eq!(parsed["sha256"], checksum(canonical_pages.as_bytes()));
        assert_ne!(
            parsed["sha256"],
            checksum(
                serde_json::to_string(&vec!["changed page"])
                    .unwrap()
                    .as_bytes()
            )
        );
        assert_eq!(representation, pdf_representation(&pages).unwrap());
    }

    #[test]
    fn command_stdout_and_stderr_capture_fit_remaining_budget_together() {
        let mut budget = Budget::new(10, "refresh limit exceeded");
        budget.charge(3, "existing refresh data").unwrap();
        let mut child = Command::new("sh")
            .args(["-c", "printf 1234; printf abc >&2"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let (status, stdout, stderr) =
            bounded_command_output_with_input(&mut child, &[], 4, &mut budget).unwrap();

        assert!(status.success());
        assert_eq!(stdout, b"1234");
        assert_eq!(stderr, b"abc");
        assert_eq!(
            budget.used(),
            3 + stdout.capacity() + stderr.capacity(),
            "aggregate accounting retains capture capacities"
        );
        assert!(
            budget.used() <= 10,
            "capture capacities fit the aggregate limit"
        );
    }

    #[test]
    fn command_stderr_over_capture_size_is_drained_without_rejection() {
        let mut budget = Budget::new(10, "refresh limit exceeded");
        budget.charge(7, "existing refresh data").unwrap();
        let mut child = Command::new("sh")
            .args(["-c", "printf 1234 >&2"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let (status, stdout, stderr) =
            bounded_command_output_with_input(&mut child, &[], 8, &mut budget).unwrap();

        assert!(status.success());
        assert!(stdout.is_empty());
        assert!(
            stderr.len() <= 3,
            "stderr excerpt fits available capture space"
        );
        assert!(budget.used() >= 7);
    }

    #[test]
    fn cached_pages_above_the_old_aggregate_budget_are_read() {
        let path =
            std::env::temp_dir().join(format!("repodb-cache-budget-{}.json", std::process::id()));
        let json = format!("[{}]", vec!["\"\""; 1000].join(","));
        std::fs::write(&path, &json).unwrap();
        let mut pages = 0;
        let count = visit_cache_pages(&path, |_, page| {
            assert!(page.is_empty());
            pages += 1;
            Ok(())
        })
        .unwrap()
        .unwrap();

        assert_eq!(count, 1000);
        assert_eq!(pages, 1000);
        assert!(path.exists(), "a valid cache should be preserved");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cache_writer_streams_page_iterator_as_compatible_json() {
        let path =
            std::env::temp_dir().join(format!("repodb-cache-write-{}.json", std::process::id()));
        let pages = vec!["first page".to_owned(), "second\\npage".to_owned()];

        write_cache(&path, pages.iter().map(String::as_str)).unwrap();

        let decoded: Vec<String> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(decoded, pages);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn checksum_is_sha256_and_distinguishes_cache_inputs() {
        assert_eq!(
            checksum(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(checksum(b"source one"), checksum(b"source two"));
        assert_eq!(
            checksum(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
