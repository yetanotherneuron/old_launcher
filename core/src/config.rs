//! The launcher's own settings file: which client folders it manages, and
//! how each one starts.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::io_at;
use crate::{Error, Launch};

/// The saved settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Client folders added by the user, in the order they were added.
    #[serde(default)]
    pub installations: Vec<PathBuf>,
    /// The client folder last selected.
    #[serde(default)]
    pub selected: Option<PathBuf>,
    /// How clients start outside Windows, by client folder. Clients missing
    /// here start with the defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub launch: BTreeMap<PathBuf, Launch>,
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

    /// How the client at `root` starts.
    pub fn launch(&self, root: &Path) -> Launch {
        self.launch.get(root).cloned().unwrap_or_default()
    }

    /// Sets how the client at `root` starts; default settings are not kept.
    pub fn set_launch(&mut self, root: &Path, launch: Launch) {
        if launch.is_default() {
            self.launch.remove(root);
        } else {
            self.launch.insert(root.to_path_buf(), launch);
        }
    }

    /// Forgets the client at `root`, and selects the first one left if it
    /// was the selected one. Its folder is left alone.
    pub fn remove(&mut self, root: &Path) {
        self.installations.retain(|path| path != root);
        self.launch.remove(root);

        if self.selected.as_deref() == Some(root) {
            self.selected = self.installations.first().cloned();
        }
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

    /// A client folder as an absolute path, which is what counts; Windows
    /// needs a drive for that.
    fn games(name: &str) -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            format!("C:/games/{name}")
        } else {
            format!("/games/{name}")
        })
    }

    #[test]
    fn broken_configs_still_tell_which_clients_they_list() {
        let wotlk = games("wotlk").display().to_string();

        // Valid TOML, wrong shape.
        assert_eq!(
            clients_in(&format!(
                "installations = \"{wotlk}\"\nselected = \"{}\"\n",
                games("other").display()
            )),
            [games("wotlk"), games("other")]
        );
        // Not TOML anymore: an unclosed list, and an escaped quote.
        assert_eq!(
            clients_in(&format!(
                "installations = [\"{wotlk}\", '{}'\nselected = \"{}\\\"q\\\"\"",
                games("war mane").display(),
                games("").display()
            )),
            [games("wotlk"), games("war mane"), games("\"q\"")]
        );
        // Relative paths and other strings are not client folders.
        assert!(clients_in("name = \"wotlk\"\ninstallations = [\"games\"]\n").is_empty());
        assert!(clients_in("\u{0}\u{1}garbage").is_empty());
    }

    #[test]
    fn a_saved_config_loads_back_identically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/config.toml");
        let mut config = Config {
            installations: vec!["/games/wotlk".into(), "/games/war mane".into()],
            selected: Some("/games/war mane".into()),
            launch: BTreeMap::new(),
        };
        config.set_launch(
            Path::new("/games/war mane"),
            Launch {
                env: "WINEPREFIX=\"$HOME/wine prefix\"".into(),
                command: "umu-run %command%".into(),
            },
        );

        config.save(&path).unwrap();

        assert_eq!(Config::load(&path).unwrap(), config);
        assert_eq!(config.launch(Path::new("/games/wotlk")), Launch::default());
    }

    #[test]
    fn configs_without_launch_settings_still_load() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "installations = [\"/games/wotlk\"]\n").unwrap();

        let config = Config::load(&path).unwrap();

        assert_eq!(config.installations, [PathBuf::from("/games/wotlk")]);
        assert!(config.launch.is_empty());
        config.save(&path).unwrap();
        assert!(
            !fs::read_to_string(&path).unwrap().contains("launch"),
            "nothing to write"
        );
    }

    #[test]
    fn forgetting_a_client_forgets_how_it_starts() {
        let mut config = Config {
            installations: vec!["/games/wotlk".into(), "/games/warmane".into()],
            selected: Some("/games/wotlk".into()),
            launch: BTreeMap::new(),
        };
        let custom = Launch {
            env: String::new(),
            command: "lutris lutris:rungameid/3".into(),
        };
        config.set_launch(Path::new("/games/wotlk"), custom.clone());
        config.set_launch(Path::new("/games/warmane"), custom);
        config.set_launch(Path::new("/games/warmane"), Launch::default());
        assert_eq!(config.launch.len(), 1, "default settings are not kept");

        config.remove(Path::new("/games/wotlk"));

        assert_eq!(config.installations, [PathBuf::from("/games/warmane")]);
        assert_eq!(config.selected, Some(PathBuf::from("/games/warmane")));
        assert!(config.launch.is_empty());
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
