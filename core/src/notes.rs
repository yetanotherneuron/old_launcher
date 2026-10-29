//! What the launcher notes in a client folder, all in one file beside
//! `Wow.exe`, `.old_launcher.toml`: the torrent the client comes from, what
//! was last found as that torrent has it, and the mods installed. It
//! travels with the folder, so a client forgotten by the launcher and added
//! back has it all still.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::error::io_at;
use crate::mods::Marker;
use crate::torrent::{Origin, Verified};

/// The file, beside `Wow.exe`.
pub(crate) const FILE: &str = ".old_launcher.toml";

/// Where a new version of it is written, and renamed over it once whole.
const PARTIAL: &str = ".old_launcher.toml.partial";

/// One change to a folder's notes at a time: the mods and the torrent
/// change them from different threads.
static CHANGING: Mutex<()> = Mutex::new(());

/// What a client folder notes.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Notes {
    /// The torrent the client comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) torrent: Option<Origin>,
    /// The mods installed, by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) mods: BTreeMap<String, Marker>,
    /// What was last found as that torrent has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) verified: Option<Verified>,
}

impl Notes {
    /// What the client folder at `root` notes: nothing, when there is no
    /// file, or one that can't be read.
    pub(crate) fn read(root: &Path) -> Self {
        fs::read_to_string(root.join(FILE))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Changes what the client folder at `root` notes with `change`. The
    /// file is written beside it and renamed over it, never seen half
    /// written, and goes when nothing is left to note.
    pub(crate) fn change(root: &Path, change: impl FnOnce(&mut Self)) -> Result<(), Error> {
        let _changing = CHANGING.lock().unwrap_or_else(PoisonError::into_inner);
        let mut notes = Self::read(root);
        change(&mut notes);

        let path = root.join(FILE);
        if notes.torrent.is_none() && notes.mods.is_empty() && notes.verified.is_none() {
            return match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io_at(&path)(error)),
                _ => Ok(()),
            };
        }

        let partial = root.join(PARTIAL);
        fs::write(&partial, toml::to_string(&notes)?).map_err(io_at(&partial))?;
        fs::rename(&partial, &path).map_err(io_at(&path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_change_one_at_a_time_and_go_when_empty() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        assert!(Notes::read(root).mods.is_empty(), "none yet");

        let threads: Vec<_> = (0..8)
            .map(|index| {
                let root = root.to_path_buf();
                std::thread::spawn(move || {
                    Notes::change(&root, |notes| {
                        let _ = notes.mods.insert(
                            format!("mod{index}"),
                            Marker {
                                version: "1.0".into(),
                                url: "https://example.com".into(),
                                set_up: true,
                                instead: None,
                            },
                        );
                    })
                    .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(Notes::read(root).mods.len(), 8, "no change lost");
        assert!(!root.join(PARTIAL).exists());

        Notes::change(root, |notes| notes.mods.clear()).unwrap();
        assert!(!root.join(FILE).exists(), "nothing left to note");
    }
}
