//! The file the launcher leaves inside every addon folder it installs.
//!
//! There is no database: which catalog entry a folder came from is recorded
//! next to the addon itself, so scanning `Interface/AddOns` recovers it.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::error::io_at;

/// Where an installed addon folder came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Marker {
    /// The catalog [`Source`](crate::catalog::Source) id.
    pub source: String,
    /// The catalog entry name within that source.
    pub entry: String,
    /// The URL the archive was downloaded from.
    pub url: String,
}

impl Marker {
    /// The marker's file name inside an addon folder. The client ignores it.
    pub const FILE_NAME: &str = ".old_launcher.toml";

    /// Reads the marker inside `folder`, if there is a valid one.
    pub fn read(folder: &Path) -> Option<Self> {
        let text = fs::read_to_string(folder.join(Self::FILE_NAME)).ok()?;

        toml::from_str(&text).ok()
    }

    /// Writes this marker inside `folder`.
    pub fn write(&self, folder: &Path) -> Result<(), Error> {
        let path = folder.join(Self::FILE_NAME);

        fs::write(&path, toml::to_string(self)?).map_err(io_at(&path))
    }

    /// Whether this marker was written for the given catalog entry.
    pub fn is_for(&self, source: &str, entry: &str) -> bool {
        self.source == source && self.entry == entry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_marker_reads_back_identically() {
        let folder = tempfile::tempdir().unwrap();
        let marker = Marker {
            source: "nom0re".into(),
            entry: "Addon Control Panel".into(),
            url: "https://example.com/ACP.zip".into(),
        };

        marker.write(folder.path()).unwrap();

        assert_eq!(Marker::read(folder.path()), Some(marker));
    }

    #[test]
    fn folders_without_a_marker_have_none() {
        let folder = tempfile::tempdir().unwrap();

        assert_eq!(Marker::read(folder.path()), None);
    }
}
