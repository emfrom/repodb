use rusqlite::Connection;

const MIN_STORAGE_LIMIT_BYTES: u64 = 4096;
const MAX_STORAGE_LIMIT_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const DEFAULT_MAX_DATABASE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const DEFAULT_MAX_REFRESH_TEMP_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DEFAULT_MAX_GIT_CACHE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const DEFAULT_MAX_PDF_CACHE_BYTES: u64 = 1024 * 1024 * 1024;

pub(crate) fn configure(c: &Connection) -> crate::Result<()> {
    let database_limit = configured_limit("REPODB_MAX_DATABASE_BYTES", DEFAULT_MAX_DATABASE_BYTES)?;
    set_page_limit(c, "main", "durable database", database_limit)?;
    Ok(())
}

pub(crate) fn configure_temp(c: &Connection) -> crate::Result<()> {
    let temporary_limit = configured_limit(
        "REPODB_MAX_REFRESH_TEMP_BYTES",
        DEFAULT_MAX_REFRESH_TEMP_BYTES,
    )?;
    set_page_limit(c, "temp", "refresh staging database", temporary_limit)
}

pub(crate) fn max_git_cache_bytes() -> crate::Result<u64> {
    configured_limit("REPODB_MAX_GIT_CACHE_BYTES", DEFAULT_MAX_GIT_CACHE_BYTES)
}

pub(crate) fn max_pdf_cache_bytes() -> crate::Result<u64> {
    configured_limit("REPODB_MAX_PDF_CACHE_BYTES", DEFAULT_MAX_PDF_CACHE_BYTES)
}

pub(crate) fn git_cache_bytes(cache_root: &std::path::Path) -> crate::Result<u64> {
    let entries = match std::fs::read_dir(cache_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut total = 0u64;
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("git-") && entry.file_type()?.is_dir() {
            total = total
                .checked_add(directory_bytes(&entry.path())?)
                .ok_or("Git cache size overflow")?;
        }
    }
    Ok(total)
}

pub(crate) fn directory_bytes(root: &std::path::Path) -> crate::Result<u64> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut total = 0u64;
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let bytes = if file_type.is_dir() {
            directory_bytes(&entry.path())?
        } else if file_type.is_file() {
            entry.metadata()?.len()
        } else {
            0
        };
        total = total.checked_add(bytes).ok_or("cache size overflow")?;
    }
    Ok(total)
}

fn configured_limit(name: &str, default: u64) -> crate::Result<u64> {
    let value = match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| format!("{name} must be a byte count"))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error.into()),
    };
    if !(MIN_STORAGE_LIMIT_BYTES..=MAX_STORAGE_LIMIT_BYTES).contains(&value) {
        return Err(format!(
            "{name} must be between {MIN_STORAGE_LIMIT_BYTES} and {MAX_STORAGE_LIMIT_BYTES} bytes"
        )
        .into());
    }
    Ok(value)
}

fn set_page_limit(
    c: &Connection,
    schema: &str,
    description: &str,
    maximum_bytes: u64,
) -> crate::Result<()> {
    let page_size: u64 = c.query_row(&format!("PRAGMA {schema}.page_size"), [], |row| {
        row.get::<_, u64>(0)
    })?;
    let maximum_pages = maximum_bytes / page_size;
    if maximum_pages == 0 {
        return Err(format!(
            "configured {description} limit ({maximum_bytes} bytes) is smaller than one SQLite page ({page_size} bytes)"
        )
        .into());
    }
    let current_pages: u64 = c.query_row(&format!("PRAGMA {schema}.page_count"), [], |row| {
        row.get::<_, u64>(0)
    })?;
    if current_pages > maximum_pages {
        return Err(format!(
            "existing {description} ({current_pages} pages) exceeds configured limit ({maximum_bytes} bytes)"
        )
        .into());
    }
    let effective_pages: u64 = c.query_row(
        &format!("PRAGMA {schema}.max_page_count = {maximum_pages}"),
        [],
        |row| row.get(0),
    )?;
    if effective_pages > maximum_pages {
        return Err(format!(
            "could not enforce the configured {description} limit of {maximum_bytes} bytes"
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_staging_database_cannot_exceed_its_page_limit() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA temp_store=FILE;").unwrap();
        set_page_limit(&c, "temp", "refresh staging database", 8192).unwrap();
        c.execute_batch("CREATE TEMP TABLE staging(value TEXT NOT NULL);")
            .unwrap();
        let large_value = "x".repeat(16 * 1024);
        let result = c.execute("INSERT INTO temp.staging VALUES(?1)", [&large_value]);
        assert!(
            result.is_err(),
            "the temp database page limit must reject growth"
        );
        let pages: u64 = c
            .query_row("PRAGMA temp.page_count", [], |row| row.get(0))
            .unwrap();
        assert!(pages <= 2, "temporary staging used {pages} pages");
    }
}
