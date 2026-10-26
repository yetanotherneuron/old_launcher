//! The file the launcher leaves inside every addon folder it installs.
//!
//! There is no database: which catalog entry a folder came from is recorded
//! next to the addon itself, so scanning `Interface/AddOns` recovers it.
//! With it, each file the launcher put there, so that a later scan tells
//! whether the player changed the addon since (see [`changed`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

/// What the marker's file holds: the marker, and each file the launcher put
/// in the folder, by path within it with `/` between folders.
#[derive(Debug, Serialize, Deserialize)]
struct Written {
    #[serde(flatten)]
    marker: Marker,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    files: BTreeMap<String, Recorded>,
}

/// A file as the launcher put it there: its SHA-256, in lowercase
/// hexadecimal, and what tells it changed without reading it, its length
/// and when it was last changed (in nanoseconds since 1970; 0 when the file
/// system doesn't say).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Recorded {
    sha256: String,
    length: u64,
    modified: u64,
}

impl Marker {
    /// The marker's file name inside an addon folder. The client ignores it.
    pub const FILE_NAME: &str = ".old_launcher.toml";

    /// Reads the marker inside `folder`, if there is a valid one.
    pub fn read(folder: &Path) -> Option<Self> {
        let text = fs::read_to_string(folder.join(Self::FILE_NAME)).ok()?;

        toml::from_str(&text).ok()
    }

    /// Writes this marker inside `folder`, with every file there as it is
    /// now: what the launcher put there, which [`changed`] compares the
    /// folder with later.
    pub fn write(&self, folder: &Path) -> Result<(), Error> {
        let mut files = BTreeMap::new();
        for (path, file) in listed(folder)? {
            let bytes = fs::read(&file).map_err(io_at(&file))?;
            let metadata = fs::metadata(&file).map_err(io_at(&file))?;
            let _ = files.insert(
                path,
                Recorded {
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                    length: metadata.len(),
                    modified: modified(&metadata),
                },
            );
        }
        let written = Written {
            marker: self.clone(),
            files,
        };
        let path = folder.join(Self::FILE_NAME);

        fs::write(&path, toml::to_string(&written)?).map_err(io_at(&path))
    }

    /// Whether this marker was written for the given catalog entry.
    pub fn is_for(&self, source: &str, entry: &str) -> bool {
        self.source == source && self.entry == entry
    }
}

/// The files of the addon folder at `folder` that changed since the
/// launcher put them there, by path within it: those whose contents differ,
/// those gone, and those added, sorted. None for a folder as installed, or
/// without a marker. A file whose length and time are as recorded is as
/// installed without being read; one that differs there is read, since a
/// copy may only have touched it.
///
/// A marker from before the files were recorded gets them now, as the
/// folder is: what is there is taken for what was installed. Recording may
/// fail, and nothing is said then: it is tried again next time.
pub fn changed(folder: &Path) -> Vec<String> {
    let Some(written) = fs::read_to_string(folder.join(Marker::FILE_NAME))
        .ok()
        .and_then(|text| toml::from_str::<Written>(&text).ok())
    else {
        return Vec::new();
    };
    if written.files.is_empty() {
        let _ = written.marker.write(folder);
        return Vec::new();
    }
    let Ok(there) = listed(folder) else {
        return Vec::new();
    };

    let mut changed: Vec<String> = Vec::new();
    for (path, recorded) in &written.files {
        let Some(file) = there.get(path) else {
            changed.push(path.clone());
            continue;
        };
        let same = fs::metadata(file).is_ok_and(|metadata| {
            recorded.modified != 0
                && metadata.len() == recorded.length
                && modified(&metadata) == recorded.modified
        }) || fs::read(file)
            .is_ok_and(|bytes| format!("{:x}", Sha256::digest(&bytes)) == recorded.sha256);
        if !same {
            changed.push(path.clone());
        }
    }
    changed.extend(
        there
            .keys()
            .filter(|path| !written.files.contains_key(*path))
            .cloned(),
    );
    changed.sort_unstable();

    changed
}

/// Every file under `folder` but the marker and whatever starts with a dot
/// (`.git`, `.DS_Store`), by path within it with `/` between folders.
fn listed(folder: &Path) -> Result<BTreeMap<String, PathBuf>, Error> {
    fn walk(
        folder: &Path,
        directory: &Path,
        files: &mut BTreeMap<String, PathBuf>,
    ) -> Result<(), Error> {
        for entry in fs::read_dir(directory).map_err(io_at(directory))? {
            let path = entry.map_err(io_at(directory))?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') {
                continue;
            }

            if path.is_dir() {
                walk(folder, &path, files)?;
            } else {
                let relative = path
                    .strip_prefix(folder)
                    .unwrap_or(&path)
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                let _ = files.insert(relative, path);
            }
        }

        Ok(())
    }

    let mut files = BTreeMap::new();
    walk(folder, folder, &mut files)?;

    Ok(files)
}

/// When the file of `metadata` was last changed, in nanoseconds since 1970;
/// 0 when the file system doesn't say.
fn modified(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker() -> Marker {
        Marker {
            source: "nom0re".into(),
            entry: "Addon Control Panel".into(),
            url: "https://example.com/ACP.zip".into(),
        }
    }

    #[test]
    fn a_written_marker_reads_back_identically() {
        let folder = tempfile::tempdir().unwrap();
        let marker = marker();

        marker.write(folder.path()).unwrap();

        assert_eq!(Marker::read(folder.path()), Some(marker));
    }

    #[test]
    fn folders_without_a_marker_have_none() {
        let folder = tempfile::tempdir().unwrap();

        assert_eq!(Marker::read(folder.path()), None);
        assert_eq!(changed(folder.path()), Vec::<String>::new());
    }

    #[test]
    fn the_files_installed_are_recorded_and_those_changed_since_are_told() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        fs::create_dir_all(root.join("modules")).unwrap();
        fs::write(root.join("ACP.toc"), "## Title: ACP\n").unwrap();
        fs::write(root.join("ACP.lua"), "print(1)\n").unwrap();
        fs::write(root.join("modules/bars.lua"), "bars\n").unwrap();
        fs::write(root.join(".DS_Store"), "mac\n").unwrap();

        marker().write(root).unwrap();
        assert_eq!(changed(root), Vec::<String>::new());

        // Touched, or copied again: read, and found the same.
        fs::write(root.join("ACP.lua"), "print(1)\n").unwrap();
        assert_eq!(changed(root), Vec::<String>::new());

        // Changed, added, and gone.
        fs::write(root.join("ACP.lua"), "print(2)\n").unwrap();
        fs::write(root.join("modules/new.lua"), "new\n").unwrap();
        fs::remove_file(root.join("modules/bars.lua")).unwrap();
        fs::write(root.join(".git"), "gitdir: elsewhere\n").unwrap();
        assert_eq!(
            changed(root),
            ["ACP.lua", "modules/bars.lua", "modules/new.lua"]
        );
        assert_eq!(
            Marker::read(root),
            Some(marker()),
            "the marker is as it was"
        );

        // The same bytes back: as installed again.
        fs::write(root.join("ACP.lua"), "print(1)\n").unwrap();
        fs::write(root.join("modules/bars.lua"), "bars\n").unwrap();
        fs::remove_file(root.join("modules/new.lua")).unwrap();
        assert_eq!(changed(root), Vec::<String>::new());
    }

    #[test]
    fn a_marker_from_before_the_files_were_recorded_takes_them_as_they_are() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        fs::write(root.join("ACP.toc"), "## Title: ACP\n").unwrap();
        fs::write(
            root.join(Marker::FILE_NAME),
            "source = \"nom0re\"\nentry = \"Addon Control Panel\"\nurl = \"https://example.com/ACP.zip\"\n",
        )
        .unwrap();

        assert_eq!(
            changed(root),
            Vec::<String>::new(),
            "nothing is known to differ"
        );
        assert!(
            fs::read_to_string(root.join(Marker::FILE_NAME))
                .unwrap()
                .contains("[files.\"ACP.toc\"]"),
            "recorded now"
        );

        fs::write(root.join("ACP.toc"), "## Title: ACP\n## Version: 2\n").unwrap();
        assert_eq!(changed(root), ["ACP.toc"]);
    }
}
