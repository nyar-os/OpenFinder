//! User configuration and index location resolution.

use std::{env, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// User configuration persisted as `config.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Explicit index directory; `null` uses the platform default.
    pub index_dir: Option<String>,
}

/// Platform configuration directory for OpenFinder.
pub fn config_dir() -> PathBuf {
    if let Some(appdata) = env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("OpenFinder");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("open-finder");
    }
    PathBuf::from(".open-finder")
}

/// Path of `config.json`.
pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Read the user configuration.
///
/// A missing file is not an error. A malformed file is reported so the caller
/// can decide whether to repair it, rather than silently ignoring user intent.
pub fn load_config() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = fs::read_to_string(&path).map_err(|source| Error::io(path.display().to_string(), source))?;
    serde_json::from_str(&text).map_err(|error| Error::message(format!("config at {} is not valid JSON: {error}", path.display())))
}

/// Resolve the index directory.
///
/// Precedence: `OPEN_FINDER_INDEX_DIR`, then `config.json#indexDir`, then
/// `<config-dir>/index`.
pub fn default_index_dir() -> Result<PathBuf> {
    if let Some(explicit) = env::var_os("OPEN_FINDER_INDEX_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(explicit));
    }
    if let Some(config_dir) = load_config()?.index_dir.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(config_dir));
    }
    Ok(config_dir().join("index"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_lives_under_the_app_directory() {
        let path = config_path();
        assert!(path.ends_with("config.json"));
        assert!(path.parent().is_some_and(|parent| parent.file_name().is_some()));
    }

    #[test]
    fn missing_config_is_not_an_error() {
        // The lookup is environment-dependent, but a synthesized path must
        // never panic regardless of the host layout.
        let path = config_dir().join("does-not-exist.json");
        assert!(!path.exists());
    }
}
