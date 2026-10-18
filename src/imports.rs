use crate::{
    Result, SearchResult, as_bytes,
    budget::{Budget, MAX_REFRESH_ACCOUNTING_BYTES},
    embed, process, segmentation,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
};

mod cache;
mod sources;
use cache::{checksum, visit_cache_pages, write_cache};
use sources::{column_exists, init};
pub(crate) use sources::{register, set_retention};
const MAX_COMMAND_STDERR_BYTES: usize = 64 * 1024;

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

pub fn refresh(c: &Connection) -> Result<()> {
    c.execute_batch("PRAGMA temp_store=FILE;
        DROP TABLE IF EXISTS temp.import_stage_sources; DROP TABLE IF EXISTS temp.import_stage_versions;
        DROP TABLE IF EXISTS temp.import_stage_units; DROP TABLE IF EXISTS temp.import_stage_git_units;
        CREATE TEMP TABLE import_stage_sources(id TEXT PRIMARY KEY,locator TEXT NOT NULL,kind TEXT NOT NULL);
        CREATE TEMP TABLE import_stage_versions(source_id TEXT NOT NULL,version TEXT NOT NULL,content TEXT NOT NULL,fetched_order INTEGER NOT NULL,PRIMARY KEY(source_id,version));
        CREATE TEMP TABLE import_stage_units(source_id TEXT NOT NULL,version TEXT NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,page INTEGER,PRIMARY KEY(source_id,version,line,text));
        CREATE TEMP TABLE import_stage_git_units(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,line INTEGER NOT NULL,line_end INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,line,text));")?;
    if table_exists(c, "imported_sources")? {
        c.execute(
            "INSERT INTO temp.import_stage_sources SELECT id,locator,kind FROM imported_sources",
            [],
        )?;
    }
    if table_exists(c, "imported_versions")? {
        let order = if column_exists(c, "imported_versions", "fetched_order")? {
            "fetched_order"
        } else {
            "rowid"
        };
        c.execute(&format!("INSERT INTO temp.import_stage_versions SELECT source_id,version,content,{order} FROM imported_versions"), [])?;
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
    if table_exists(c, "imported_git_units")? {
        let line_end = if column_exists(c, "imported_git_units", "line_end")? {
            "line_end"
        } else {
            "line"
        };
        c.execute(&format!("INSERT INTO temp.import_stage_git_units SELECT source_id,commit_id,path,line,{line_end},text,vector FROM imported_git_units"), [])?;
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
    let retention: String = c
        .query_row(
            "SELECT value FROM import_config WHERE key='retention'",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "latest".to_owned());
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
                } else {
                    let status = Command::new("git")
                        .arg("--git-dir")
                        .arg(&mirror)
                        .args(["fetch", "--prune", "origin"])
                        .status()?;
                    if !status.success() {
                        return Err(format!("git fetch failed for {locator}").into());
                    }
                }
                mirror.to_string_lossy().into_owned()
            };
            let commits_bytes = git_output(
                &repository,
                &["rev-list", "--all", "--date-order"],
                refresh_memory_bytes
                    .remaining()
                    .saturating_sub(MAX_COMMAND_STDERR_BYTES),
                &mut refresh_memory_bytes,
            )?;
            let commits = String::from_utf8(commits_bytes)?;
            let mut commit_ids = Vec::new();
            for line in commits.lines() {
                refresh_memory_bytes.charge(line.len(), "Git commit identifiers")?;
                refresh_memory_bytes.reserve_vec(&mut commit_ids, 1, "Git commit vector")?;
                commit_ids.push(line.to_owned());
            }
            let mut commits = commit_ids;
            let selected = if retention == "latest" {
                commits.into_iter().take(1).collect()
            } else {
                commits.reverse();
                commits
            };
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
                    let path = String::from_utf8(path.to_vec())?;
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
                    let mut unit_error = None;
                    let segmentation_result = segmentation::visit(
                        segmentation::kind(path.as_bytes()),
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
                                refresh_memory_bytes
                                    .charge(unit.text.len(), "Git indexed unit text")?;
                                refresh_memory_bytes.charge(path.len(), "Git unit path")?;
                                let vector_bytes = vector.as_ref().map_or(0, |v| v.len() * 4);
                                refresh_memory_bytes
                                    .charge(vector_bytes, "Git stored vector bytes")?;
                                c.execute("INSERT OR REPLACE INTO temp.import_stage_git_units(source_id,commit_id,path,line,line_end,text,vector) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![id, commit, path, unit.line_start, unit.line_end, unit.text, vector.map(|v| as_bytes(&v))])?;
                                refresh_memory_bytes
                                    .release(unit.text.len() + path.len() + vector_bytes);
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
            if retention == "latest" {
                c.execute("DELETE FROM temp.import_stage_versions WHERE source_id=?1 AND version NOT IN (SELECT version FROM temp.import_stage_versions WHERE source_id=?1 ORDER BY fetched_order DESC LIMIT 1)", [&id])?;
                c.execute("DELETE FROM temp.import_stage_git_units WHERE source_id=?1 AND commit_id NOT IN (SELECT version FROM temp.import_stage_versions WHERE source_id=?1)", [&id])?;
            }
            continue;
        }
        if kind == "file" {
            let size = fs::metadata(&locator)?.len();
            refresh_memory_bytes.charge(size as usize, "local import input")?;
        }
        let source_bytes = if kind == "file" {
            read_all(fs::File::open(&locator)?)?
        } else {
            let mut child = Command::new("curl")
                .args([
                    "--fail",
                    "--location",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "30",
                    "--proto-redir",
                    "=http,https",
                    &locator,
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let output = bounded_command_output(
                &mut child,
                refresh_memory_bytes
                    .remaining()
                    .saturating_sub(MAX_COMMAND_STDERR_BYTES),
                &mut refresh_memory_bytes,
            )?;
            if !output.0.success() {
                return Err(format!(
                    "URL fetch failed for {locator}: {}",
                    String::from_utf8_lossy(&output.2)
                )
                .into());
            }
            output.1
        };
        let is_pdf = source_bytes
            .windows(5)
            .take(1024)
            .any(|window| window == b"%PDF-")
            || Path::new(&locator)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
        let (version, content) = if is_pdf {
            let source_version = checksum(&source_bytes);
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
                    .map_err(|error| format!("PDF imports require pdftotext on PATH: {error}"))?;
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
            (source_version, None)
        } else {
            // Transfer ownership into decoded text instead of retaining source bytes too.
            let content = String::from_utf8(source_bytes)?;
            refresh_memory_bytes.charge(content.len(), "decoded local import text")?;
            (checksum(content.as_bytes()), Some(content))
        };
        let fetched_order: i64 = c.query_row(
            "SELECT COALESCE(MAX(fetched_order),0)+1 FROM temp.import_stage_versions",
            [],
            |r| r.get(0),
        )?;
        c.execute(
            "INSERT INTO temp.import_stage_versions(source_id,version,content,fetched_order) VALUES(?1,?2,?3,?4) ON CONFLICT(source_id,version) DO UPDATE SET content=excluded.content,fetched_order=excluded.fetched_order",
            params![id, version, content.as_deref().unwrap_or(""), fetched_order],
        )?;
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
                        refresh_memory_bytes.charge(unit.text.len(), "PDF indexed unit text")?;
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
        if retention == "latest" {
            c.execute(
                "DELETE FROM temp.import_stage_versions WHERE source_id=?1 AND fetched_order<>?2",
                params![id, fetched_order],
            )?;
            c.execute(
                "DELETE FROM temp.import_stage_units WHERE source_id=?1 AND version NOT IN (SELECT version FROM temp.import_stage_versions WHERE source_id=?1)",
                [&id],
            )?;
        }
    }
    if let Some(dimension) = dimension {
        c.execute(
            "INSERT INTO temp.import_refresh_dimension VALUES(?1)",
            [dimension as i64],
        )?;
    }
    Ok(())
}

pub fn validate_dimensions(c: &Connection) -> Result<()> {
    let imported: Option<i64> = c
        .query_row(
            "SELECT value FROM temp.import_refresh_dimension LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let history: Option<i64> = c
        .query_row(
            "SELECT value FROM temp.refresh_dimension LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if imported.zip(history).is_some_and(|(a, b)| a != b) {
        return Err("embedding dimension mismatch".into());
    }
    Ok(())
}

pub fn publish(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    init(tx)?;
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
    for (table, stage) in [
        ("imported_versions", "import_stage_versions"),
        ("imported_units", "import_stage_units"),
        ("imported_git_units", "import_stage_git_units"),
    ] {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
        let columns = match table {
            "imported_versions" => "source_id,version,content,fetched_order",
            "imported_units" => "source_id,version,line,line_end,text,vector,page",
            "imported_git_units" => "source_id,commit_id,path,line,line_end,text,vector",
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
    c.execute_batch("PRAGMA temp_store=FILE; DROP TABLE IF EXISTS temp.import_reembed_vectors; CREATE TEMP TABLE import_reembed_vectors(kind TEXT NOT NULL, source_id TEXT NOT NULL, version TEXT NOT NULL, line INTEGER NOT NULL, text TEXT NOT NULL, path TEXT, vector BLOB NOT NULL);")?;
    let mut dimension = None;
    {
        let mut stmt = c.prepare("SELECT source_id,version,line,text,'file',NULL FROM imported_units UNION ALL SELECT source_id,commit_id,line,text,'git',path FROM imported_git_units ORDER BY 1,2,3")?;
        let mut rows = stmt.query([])?;
        let mut staged =
            c.prepare("INSERT INTO temp.import_reembed_vectors VALUES(?1,?2,?3,?4,?5,?6,?7)")?;
        while let Some(row) = rows.next()? {
            let source: String = row.get(0)?;
            let version: String = row.get(1)?;
            let line: usize = row.get(2)?;
            let text: String = row.get(3)?;
            let kind: String = row.get(4)?;
            let path: Option<String> = row.get(5)?;
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
                as_bytes(&vector)
            ])?;
        }
    }
    c.execute("UPDATE imported_units SET vector=(SELECT vector FROM temp.import_reembed_vectors v WHERE v.kind='file' AND v.source_id=imported_units.source_id AND v.version=imported_units.version AND v.line=imported_units.line AND v.text=imported_units.text)", [])?;
    c.execute("UPDATE imported_git_units SET vector=(SELECT vector FROM temp.import_reembed_vectors v WHERE v.kind='git' AND v.source_id=imported_git_units.source_id AND v.version=imported_git_units.commit_id AND v.line=imported_git_units.line AND v.path=imported_git_units.path AND v.text=imported_git_units.text)", [])?;
    c.execute_batch("DROP TABLE temp.import_reembed_vectors")?;
    Ok(dimension)
}

pub fn search(c: &Connection, query: &str, mode: &str, limit: usize) -> Result<Vec<SearchResult>> {
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
    let mut st = c.prepare("SELECT s.locator,u.line,u.line_end,u.text,u.version,u.vector,u.page FROM imported_units u JOIN imported_sources s ON s.id=u.source_id WHERE s.kind<>'git'")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, usize>(1)?,
            r.get::<_, usize>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<Vec<u8>>>(5)?,
            r.get::<_, Option<usize>>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (locator, line, line_end, text, version, stored, page) = row?;
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
                        path: locator,
                        line_start: line,
                        line_end,
                        text,
                        commit_id: None,
                        distance: Some(d),
                        blob_id: Some(version),
                        page,
                        repo_id: None,
                    },
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
                },
            ));
        }
    }
    let mut git_rows = c.prepare("SELECT s.locator,u.path,u.line,u.line_end,u.text,u.commit_id,u.vector FROM imported_git_units u JOIN imported_sources s ON s.id=u.source_id")?;
    let rows = git_rows.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, usize>(2)?,
            r.get::<_, usize>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<Vec<u8>>>(6)?,
        ))
    })?;
    for row in rows {
        let (repo_id, path, line, line_end, text, commit, stored) = row?;
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
                    },
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
                },
            ));
        }
    }
    if semantic {
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        if mode == "combined" {
            let mut rank = 0;
            out.retain(|(_, result)| {
                let keep =
                    rank < limit || result.text.to_lowercase().contains(&query.to_lowercase());
                rank += 1;
                keep
            });
        } else {
            out.truncate(limit);
        }
    }
    Ok(out.into_iter().map(|x| x.1).collect())
}

#[cfg(test)]
mod tests {
    use super::{bounded_command_output_with_input, checksum, visit_cache_pages, write_cache};
    use crate::budget::Budget;
    use std::process::{Command, Stdio};

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
