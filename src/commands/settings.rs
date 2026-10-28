use std::{error::Error, path::Path};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub(crate) fn retention(db_path: &Path, arguments: &[String]) -> Result<()> {
    let value = arguments
        .first()
        .ok_or("retention requires latest or a version limit")?;
    if arguments.len() != 1 {
        return Err("unexpected arguments".into());
    }
    let pruned = crate::imports::set_retention(&crate::open_db(db_path)?, value)?;
    if pruned > 0 {
        println!("pruned {pruned} stored imported version(s)");
    }
    Ok(())
}

pub(crate) fn redirect_limit(db_path: &Path, arguments: &[String]) -> Result<()> {
    let value: usize = arguments
        .first()
        .ok_or("redirect-limit requires a nonnegative integer")?
        .parse()?;
    if arguments.len() != 1 {
        return Err("unexpected arguments".into());
    }
    crate::imports::set_url_max_redirects(&crate::open_db(db_path)?, value)?;
    println!("URL redirect limit set to {value}");
    Ok(())
}
