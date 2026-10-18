use serde::Deserialize;
use std::{env, ffi::OsString, fs, path::PathBuf, sync::OnceLock};

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    tools: Tools,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Tools {
    semgrep_core: Option<PathBuf>,
    segmenter: Option<PathBuf>,
    embedding_command: Option<PathBuf>,
}

static CONFIG: OnceLock<std::result::Result<Config, String>> = OnceLock::new();

pub fn config_path() -> std::result::Result<PathBuf, String> {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        let path = PathBuf::from(xdg);
        if !path.is_absolute() {
            return Err("XDG_CONFIG_HOME must be an absolute path".into());
        }
        return Ok(path.join("repodb/config.toml"));
    }

    let home = env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .or_else(|| env::var_os("USERPROFILE").filter(|path| !path.is_empty()))
        .ok_or_else(|| "cannot resolve RepoDB config path: HOME is not set".to_owned())?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err("HOME must be an absolute path to resolve RepoDB config".into());
    }
    Ok(home.join(".config/repodb/config.toml"))
}

fn load() -> std::result::Result<Config, String> {
    CONFIG
        .get_or_init(|| {
            let path = config_path()?;
            let contents = match fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Config::default());
                }
                Err(error) => {
                    return Err(format!(
                        "could not read RepoDB config {}: {error}",
                        path.display()
                    ));
                }
            };
            toml::from_str(&contents)
                .map_err(|error| format!("invalid RepoDB config {}: {error}", path.display()))
        })
        .clone()
}

pub fn semgrep_core() -> std::result::Result<Option<OsString>, String> {
    if let Some(command) = env::var_os("REPODB_SEMGREP_CORE") {
        return Ok(Some(command));
    }
    Ok(load()?.tools.semgrep_core.map(PathBuf::into_os_string))
}

pub fn segmenter() -> std::result::Result<Option<OsString>, String> {
    if let Some(command) = env::var_os("REPODB_SEGMENTER") {
        return Ok(Some(command));
    }
    Ok(load()?.tools.segmenter.map(PathBuf::into_os_string))
}

pub const DEFAULT_EMBEDDING_MODEL: &str = "all-MiniLM-L6-v2";

pub fn embedding_command() -> std::result::Result<String, String> {
    if let Some(command) = load()?.tools.embedding_command {
        return Ok(command.to_string_lossy().into_owned());
    }
    let home = env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .or_else(|| env::var_os("USERPROFILE").filter(|path| !path.is_empty()))
        .ok_or_else(|| {
            "set [tools].embedding_command in the RepoDB config; HOME is not set".to_owned()
        })?;
    Ok(PathBuf::from(home)
        .join("bin/generate_embeddings")
        .to_string_lossy()
        .into_owned())
}
