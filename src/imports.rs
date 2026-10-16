use crate::{
    Result, SearchResult, as_bytes,
    budget::{Budget, MAX_REFRESH_MEMORY_BYTES},
    embed, process,
};
use rusqlite::{Connection, params};
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
};

const MAX_COMMAND_STDERR_BYTES: usize = 64 * 1024;

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
        bounded_command_output_with_input(&mut child, text.as_bytes(), output_limit, used)?;
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

fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS imported_sources(id TEXT PRIMARY KEY, locator TEXT NOT NULL UNIQUE, kind TEXT NOT NULL); CREATE TABLE IF NOT EXISTS imported_versions(source_id TEXT NOT NULL, version TEXT NOT NULL, content TEXT NOT NULL, fetched_order INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(source_id,version)); CREATE TABLE IF NOT EXISTS imported_units(source_id TEXT NOT NULL, version TEXT NOT NULL, line INTEGER NOT NULL, text TEXT NOT NULL, vector BLOB, page INTEGER, PRIMARY KEY(source_id,version,line)); CREATE TABLE IF NOT EXISTS import_config(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS imported_git_units(source_id TEXT NOT NULL,commit_id TEXT NOT NULL,path TEXT NOT NULL,line INTEGER NOT NULL,text TEXT NOT NULL,vector BLOB,PRIMARY KEY(source_id,commit_id,path,line));")?;
    if !column_exists(c, "imported_units", "page")? {
        c.execute("ALTER TABLE imported_units ADD COLUMN page INTEGER", [])?;
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

fn column_exists(c: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(names
        .collect::<std::result::Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column))
}

fn checksum(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for block in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

fn cache_pages(cache: &Path, used: &mut Budget) -> Result<Option<Vec<String>>> {
    let file = match fs::File::open(cache) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let file_size = usize::try_from(file.metadata()?.len())?;
    let input_reservation = file_size.max(64 * 1024);
    used.charge(input_reservation, "PDF cache input reservation")?;
    let bytes = match read_all(file) {
        Ok(bytes) => bytes,
        Err(_) => {
            used.release(input_reservation);
            fs::remove_file(cache)?;
            return Ok(None);
        }
    };
    let input_capacity = bytes.capacity();
    if input_capacity > input_reservation {
        return Err("PDF cache input capacity exceeded its reservation".into());
    }
    used.release(input_reservation - input_capacity);

    // Each JSON string needs at least two bytes, and Vec growth may reserve
    // nearly twice the number of decoded pages. Reserve before serde allocates.
    let page_bound = bytes.len() / 2 + 1;
    let page_reservation = page_bound
        .checked_mul(2)
        .and_then(|count| count.checked_mul(std::mem::size_of::<String>()))
        .ok_or("PDF cache page capacity exceeds refresh budget")?;
    let string_reservation = bytes
        .len()
        .checked_mul(2)
        .ok_or("PDF cache string capacity exceeds refresh budget")?;
    let reservation = page_reservation
        .checked_add(string_reservation)
        .ok_or("PDF cache decoded capacity exceeds refresh budget")?;
    if let Err(error) = used.charge(reservation, "PDF cache decoded capacity") {
        used.release(input_capacity);
        return Err(error);
    }
    match serde_json::from_slice::<Vec<String>>(&bytes) {
        Ok(pages) => {
            let actual = pages.capacity() * std::mem::size_of::<String>()
                + pages.iter().map(String::capacity).sum::<usize>();
            used.release(reservation - actual);
            let bytes_capacity = bytes.capacity();
            drop(bytes);
            used.release(bytes_capacity);
            Ok(Some(pages))
        }
        Err(_) => {
            used.release(reservation + input_capacity);
            fs::remove_file(cache)?;
            Ok(None)
        }
    }
}

fn write_cache(cache: &Path, pages: &[String]) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = cache.parent().ok_or("cache path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        cache
            .file_name()
            .ok_or("cache path has no filename")?
            .to_string_lossy(),
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
    ));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        serde_json::to_writer(&mut file, pages)?;
        file.sync_all()?;
        fs::rename(&temp, cache)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
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

pub fn register(c: &Connection, locator: &str) -> Result<()> {
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
        "INSERT OR IGNORE INTO imported_sources VALUES(?1,?2,?3)",
        params![identity(locator), locator, kind],
    )?;
    Ok(())
}

pub fn set_retention(c: &Connection, value: &str) -> Result<()> {
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
    init(c)?;
    let mut st = c.prepare("SELECT id,locator,kind FROM imported_sources ORDER BY id")?;
    let mut source_rows = st.query([])?;
    let cfg: Option<(String,String)> = c.query_row("SELECT (SELECT value FROM embedding_config WHERE key='command'),(SELECT value FROM embedding_config WHERE key='model')", [], |r| Ok((r.get(0)?,r.get(1)?))).ok();
    let configured_dimension = c
        .query_row(
            "SELECT value FROM embedding_config WHERE key='dimension'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|d| *d != 0);
    let retention: String = c.query_row(
        "SELECT value FROM import_config WHERE key='retention'",
        [],
        |r| r.get(0),
    )?;
    let cache_dir: String = c.query_row("PRAGMA database_list", [], |r| r.get(2))?;
    let cache_dir = Path::new(&cache_dir)
        .parent()
        .ok_or("database has no parent directory")?
        .join("repodb-cache");
    let tx = c.unchecked_transaction()?;
    // This conservative cumulative accounting includes buffers and representations materialized during preparation.
    let mut refresh_memory_bytes = Budget::new(
        MAX_REFRESH_MEMORY_BYTES,
        "import refresh exceeds the aggregate preparation-memory budget",
    );
    while let Some(row) = source_rows.next()? {
        let id: String = row.get(0)?;
        let locator: String = row.get(1)?;
        let kind: String = row.get(2)?;
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
                let mut content = String::new();
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
                    refresh_memory_bytes
                        .charge(path.len() + text.len() + 2, "Git joined content")?;
                    content.push_str(&path);
                    content.push('\n');
                    content.push_str(&text);
                    if !content.ends_with('\n') {
                        content.push('\n');
                    }
                    for (index, line) in text.lines().enumerate() {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let vector = cfg
                            .as_ref()
                            .map(|(cmd, model)| {
                                import_embed(cmd, model, line, &mut refresh_memory_bytes)
                            })
                            .transpose()?;
                        if let Some(vector) = &vector
                            && configured_dimension.is_some_and(|d| d != vector.len())
                        {
                            return Err("embedding dimension mismatch".into());
                        }
                        refresh_memory_bytes.charge(line.len(), "Git indexed unit text")?;
                        refresh_memory_bytes.charge(path.len(), "Git unit path")?;
                        let vector_bytes = vector.as_ref().map_or(0, |v| v.len() * 4);
                        if vector_bytes > 0 {
                            refresh_memory_bytes.charge(vector_bytes, "Git stored vector bytes")?;
                        }
                        tx.execute(
                            "INSERT OR REPLACE INTO imported_git_units(source_id,commit_id,path,line,text,vector) VALUES(?1,?2,?3,?4,?5,?6)",
                            params![id, commit, path, index + 1, line, vector.map(|v| as_bytes(&v))],
                        )?;
                        refresh_memory_bytes.release(line.len() + path.len() + vector_bytes);
                    }
                }
                let fetched_order: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(fetched_order),0)+1 FROM imported_versions",
                    [],
                    |r| r.get(0),
                )?;
                tx.execute("INSERT INTO imported_versions(source_id,version,content,fetched_order) VALUES(?1,?2,?3,?4) ON CONFLICT(source_id,version) DO UPDATE SET content=excluded.content,fetched_order=excluded.fetched_order", params![id, commit, content, fetched_order])?;
            }
            if retention == "latest" {
                tx.execute("DELETE FROM imported_versions WHERE source_id=?1 AND version NOT IN (SELECT version FROM imported_versions WHERE source_id=?1 ORDER BY fetched_order DESC LIMIT 1)", [&id])?;
                tx.execute("DELETE FROM imported_git_units WHERE source_id=?1 AND commit_id NOT IN (SELECT version FROM imported_versions WHERE source_id=?1)", [&id])?;
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
        if !is_pdf {
            refresh_memory_bytes.charge(source_bytes.len(), "decoded local import text")?;
        }
        let pages = if is_pdf {
            let source_version = checksum(&source_bytes);
            let cache = cache_dir.join(format!("{source_version}.json"));
            if let Some(pages) = cache_pages(&cache, &mut refresh_memory_bytes)? {
                pages
            } else {
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
                refresh_memory_bytes.charge(text.len(), "PDF page text copies")?;
                let pages = text.split('\u{c}').map(str::to_owned).collect::<Vec<_>>();
                write_cache(&cache, &pages)?;
                pages
            }
        } else {
            vec![String::from_utf8(source_bytes.clone())?]
        };
        refresh_memory_bytes.charge(pages.iter().map(String::len).sum(), "PDF/cache pages")?;
        let content_size =
            pages.iter().map(String::len).sum::<usize>() + pages.len().saturating_sub(1);
        refresh_memory_bytes.charge(content_size, "joined imported content")?;
        let content = if is_pdf {
            pages.join("\n")
        } else {
            pages[0].clone()
        };
        let version = checksum(content.as_bytes());
        let fetched_order: i64 = tx.query_row(
            "SELECT COALESCE(MAX(fetched_order),0)+1 FROM imported_versions",
            [],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO imported_versions(source_id,version,content,fetched_order) VALUES(?1,?2,?3,?4) ON CONFLICT(source_id,version) DO UPDATE SET content=excluded.content,fetched_order=excluded.fetched_order",
            params![id, version, content, fetched_order],
        )?;
        let mut dimension = None;
        let lines: Box<dyn Iterator<Item = (usize, Option<usize>, &str)>> = if is_pdf {
            Box::new(
                pages
                    .iter()
                    .enumerate()
                    .map(|(page, text)| (page + 1, Some(page + 1), text.as_str())),
            )
        } else {
            Box::new(
                content
                    .lines()
                    .enumerate()
                    .map(|(line, text)| (line + 1, None, text)),
            )
        };
        for (line, page, text) in lines {
            if text.trim().is_empty() {
                continue;
            }
            let vector = cfg
                .as_ref()
                .map(|(cmd, model)| import_embed(cmd, model, text, &mut refresh_memory_bytes))
                .transpose()?;
            if let Some(vector) = &vector {
                if dimension.is_some_and(|d| d != vector.len())
                    || configured_dimension.is_some_and(|d| d != vector.len())
                {
                    return Err("embedding dimension mismatch".into());
                }
                dimension = Some(vector.len());
            }
            refresh_memory_bytes.charge(text.len(), "indexed unit text")?;
            let vector_bytes = vector.as_ref().map_or(0, |v| v.len() * 4);
            if vector_bytes > 0 {
                refresh_memory_bytes.charge(vector_bytes, "stored vector bytes")?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO imported_units(source_id,version,line,text,vector,page) VALUES(?1,?2,?3,?4,?5,?6)",
                params![id, version, line, text, vector.map(|v| as_bytes(&v)), page],
            )?;
            refresh_memory_bytes.release(text.len() + vector_bytes);
        }
        if retention == "latest" {
            tx.execute(
                "DELETE FROM imported_versions WHERE source_id=?1 AND fetched_order<>?2",
                params![id, fetched_order],
            )?;
            tx.execute(
                "DELETE FROM imported_units WHERE source_id=?1 AND version NOT IN (SELECT version FROM imported_versions WHERE source_id=?1)",
                [&id],
            )?;
        }
    }
    drop(source_rows);
    drop(st);
    tx.commit()?;
    Ok(())
}

pub fn reembed(c: &Connection, command: &str, model: &str) -> Result<Option<usize>> {
    init(c)?;
    c.execute_batch("PRAGMA temp_store=FILE; DROP TABLE IF EXISTS temp.import_reembed_vectors; CREATE TEMP TABLE import_reembed_vectors(kind TEXT NOT NULL, source_id TEXT NOT NULL, version TEXT NOT NULL, line INTEGER NOT NULL, path TEXT, vector BLOB NOT NULL);")?;
    let mut dimension = None;
    {
        let mut stmt = c.prepare("SELECT source_id,version,line,text,'file',NULL FROM imported_units UNION ALL SELECT source_id,commit_id,line,text,'git',path FROM imported_git_units ORDER BY 1,2,3")?;
        let mut rows = stmt.query([])?;
        let mut staged =
            c.prepare("INSERT INTO temp.import_reembed_vectors VALUES(?1,?2,?3,?4,?5,?6)")?;
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
                path,
                as_bytes(&vector)
            ])?;
        }
    }
    c.execute("UPDATE imported_units SET vector=(SELECT vector FROM temp.import_reembed_vectors v WHERE v.kind='file' AND v.source_id=imported_units.source_id AND v.version=imported_units.version AND v.line=imported_units.line)", [])?;
    c.execute("UPDATE imported_git_units SET vector=(SELECT vector FROM temp.import_reembed_vectors v WHERE v.kind='git' AND v.source_id=imported_git_units.source_id AND v.version=imported_git_units.commit_id AND v.line=imported_git_units.line AND v.path=imported_git_units.path)", [])?;
    c.execute_batch("DROP TABLE temp.import_reembed_vectors")?;
    Ok(dimension)
}

pub fn search(c: &Connection, query: &str, mode: &str, limit: usize) -> Result<Vec<SearchResult>> {
    init(c)?;
    let semantic = mode != "exact";
    let mut vector = None;
    if semantic {
        let (cmd,model): (String,String) = c.query_row("SELECT (SELECT value FROM embedding_config WHERE key='command'),(SELECT value FROM embedding_config WHERE key='model')", [], |r| Ok((r.get(0)?,r.get(1)?))).map_err(|_| "no embeddings; run reembed")?;
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
    let mut st = c.prepare("SELECT s.locator,u.line,u.text,u.version,u.vector,u.page FROM imported_units u JOIN imported_sources s ON s.id=u.source_id WHERE s.kind<>'git'")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, usize>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<Vec<u8>>>(4)?,
            r.get::<_, Option<usize>>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (locator, line, text, version, stored, page) = row?;
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
                        line_end: line,
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
                    line_end: line,
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
    let mut git_rows = c.prepare("SELECT s.locator,u.path,u.line,u.text,u.commit_id,u.vector FROM imported_git_units u JOIN imported_sources s ON s.id=u.source_id")?;
    let rows = git_rows.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, usize>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<Vec<u8>>>(5)?,
        ))
    })?;
    for row in rows {
        let (repo_id, path, line, text, commit, stored) = row?;
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
                        line_end: line,
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
                    line_end: line,
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
    use super::{bounded_command_output_with_input, cache_pages, checksum};
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
        let mut budget = Budget::new(usize::MAX, "refresh accounting");

        let pages = cache_pages(&path, &mut budget).unwrap().unwrap();

        assert_eq!(pages.len(), 1000);
        assert!(path.exists(), "a valid cache should be preserved");
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
