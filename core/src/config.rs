//! The launcher's own settings file: which client folders it manages.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::error::io_at;

/// The saved settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Client folders added by the user, in the order they were added.
    #[serde(default)]
    pub installations: Vec<PathBuf>,
    /// The client folder last selected.
    #[serde(default)]
    pub selected: Option<PathBuf>,
}

impl Config {
    /// Loads the config at `path`; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Self, Error> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(io_at(path)(error)),
        }
    }

    /// The client folders a config file that doesn't load still mentions,
    /// as far as can be told: what its `installations` and `selected` say
    /// when it is still TOML, or else every quoted absolute path in it.
    /// Nothing, when it can't be read at all.
    pub fn salvage(path: &Path) -> Vec<PathBuf> {
        fs::read_to_string(path)
            .map(|text| clients_in(&text))
            .unwrap_or_default()
    }

    /// Saves the config to `path`, creating its directory if needed.
    ///
    /// The file is written beside `path` and renamed over it, so a crash
    /// mid-write never leaves a truncated config.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_at(parent))?;
        }

        let partial = path.with_extension("toml.partial");

        fs::write(&partial, toml::to_string_pretty(self)?).map_err(io_at(&partial))?;
        fs::rename(&partial, path).map_err(io_at(path))
    }
}

/// The client folders `text` mentions; see [`Config::salvage`].
fn clients_in(text: &str) -> Vec<PathBuf> {
    let mut clients: Vec<PathBuf> = Vec::new();
    let mut add = |path: &str| {
        let path = PathBuf::from(path);

        if path.is_absolute() && !clients.contains(&path) {
            clients.push(path);
        }
    };

    match toml::from_str::<toml::Table>(text) {
        Ok(table) => {
            for key in ["installations", "selected"] {
                match table.get(key) {
                    Some(toml::Value::String(path)) => add(path),
                    Some(toml::Value::Array(paths)) => {
                        paths
                            .iter()
                            .filter_map(toml::Value::as_str)
                            .for_each(&mut add);
                    }
                    _ => {}
                }
            }
        }
        Err(_) => quoted(text).iter().for_each(|path| add(path)),
    }

    clients
}

/// Every string quoted in `text` the way TOML quotes them: `"basic"`, with
/// backslash escapes, and `'literal'`. Unterminated quotes are skipped.
fn quoted(text: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut chars = text.chars();

    while let Some(c) = chars.next() {
        let mut string = String::new();
        let terminated = match c {
            '"' => loop {
                match chars.next() {
                    Some('"') => break true,
                    Some('\\') => match chars.next() {
                        Some('n') => string.push('\n'),
                        Some('t') => string.push('\t'),
                        Some(escaped) => string.push(escaped),
                        None => break false,
                    },
                    Some('\n') | None => break false,
                    Some(c) => string.push(c),
                }
            },
            '\'' => loop {
                match chars.next() {
                    Some('\'') => break true,
                    Some('\n') | None => break false,
                    Some(c) => string.push(c),
                }
            },
            _ => continue,
        };

        if terminated {
            strings.push(string);
        }
    }

    strings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_configs_still_tell_which_clients_they_list() {
        // Valid TOML, wrong shape.
        assert_eq!(
            clients_in("installations = \"/games/wotlk\"\nselected = \"/games/other\"\n"),
            [PathBuf::from("/games/wotlk"), PathBuf::from("/games/other")]
        );
        // Not TOML anymore: an unclosed list, and an escaped quote.
        assert_eq!(
            clients_in(
                "installations = [\"/games/wotlk\", '/games/war mane'\nselected = \"/games/\\\"q\\\"\""
            ),
            [
                PathBuf::from("/games/wotlk"),
                PathBuf::from("/games/war mane"),
                PathBuf::from("/games/\"q\""),
            ]
        );
        // Relative paths and other strings are not client folders.
        assert!(clients_in("name = \"wotlk\"\ninstallations = [\"games\"]\n").is_empty());
        assert!(clients_in("\u{0}\u{1}garbage").is_empty());
    }

    #[test]
    fn a_saved_config_loads_back_identically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/config.toml");
        let config = Config {
            installations: vec!["/games/wotlk".into(), "/games/warmane".into()],
            selected: Some("/games/warmane".into()),
        };

        config.save(&path).unwrap();

        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn a_missing_config_is_empty() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(
            Config::load(&directory.path().join("config.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn a_corrupt_config_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "installations = 7").unwrap();

        assert!(matches!(Config::load(&path), Err(Error::ConfigRead(_))));
    }
}
