//! Addon folders under `Interface/AddOns`.

use std::fs;
use std::path::Path;

use crate::error::io_at;
use crate::{Error, Marker, Toc};

/// A folder under `Interface/AddOns`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addon {
    /// The folder name, which is also the name the client knows it by.
    pub name: String,
    /// The folder's `<name>.toc`, or `None` if it has none (so the client
    /// ignores the folder).
    pub toc: Option<Toc>,
    /// Which catalog entry installed it, when the launcher did.
    pub marker: Option<Marker>,
}

impl Addon {
    /// Reads every addon folder in `directory`, sorted by name.
    ///
    /// Blizzard's own `Blizzard_*` folders and dot-folders are skipped; a
    /// missing `directory` simply has no addons.
    pub fn scan(directory: &Path) -> Result<Vec<Self>, Error> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_at(directory)(error)),
        };

        let mut addons = Vec::new();

        for entry in entries {
            let entry = entry.map_err(io_at(directory))?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();

            // `is_dir` follows symlinks, which people use to share addons
            // between installations.
            if !path.is_dir() || name.starts_with('.') || name.starts_with("Blizzard_") {
                continue;
            }

            let toc = fs::read_dir(&path)
                .map_err(io_at(&path))?
                .filter_map(Result::ok)
                .find(|file| {
                    let file_name = file.file_name().to_string_lossy().to_lowercase();
                    file_name == format!("{}.toc", name.to_lowercase())
                })
                .and_then(|file| fs::read(file.path()).ok())
                .map(|bytes| Toc::parse(&bytes));

            addons.push(Self {
                marker: Marker::read(&path),
                name,
                toc,
            });
        }

        addons.sort_by_key(|addon| addon.name.to_lowercase());

        Ok(addons)
    }

    /// The name to show: the `.toc` title, or the folder name without one.
    pub fn title(&self) -> &str {
        self.toc
            .as_ref()
            .and_then(|toc| toc.title.as_deref())
            .filter(|title| !title.is_empty())
            .unwrap_or(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanning_reads_tocs_and_markers() {
        let addons = tempfile::tempdir().unwrap();
        let acp = addons.path().join("ACP");
        fs::create_dir_all(&acp).unwrap();
        fs::write(acp.join("acp.toc"), "## Title: |cff00ff00ACP|r\n").unwrap();
        let marker = Marker {
            source: "nom0re".into(),
            entry: "Addon Control Panel".into(),
            url: "https://example.com/ACP.zip".into(),
        };
        marker.write(&acp).unwrap();

        let scanned = Addon::scan(addons.path()).unwrap();

        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].title(), "ACP");
        assert_eq!(scanned[0].marker, Some(marker));
    }

    #[test]
    fn folders_without_a_matching_toc_are_listed_without_one() {
        let addons = tempfile::tempdir().unwrap();
        let broken = addons.path().join("Bartender4-master");
        fs::create_dir_all(broken.join("Bartender4")).unwrap();
        fs::write(broken.join("Bartender4/Bartender4.toc"), "").unwrap();

        let scanned = Addon::scan(addons.path()).unwrap();

        assert_eq!(scanned[0].name, "Bartender4-master");
        assert_eq!(scanned[0].toc, None);
        assert_eq!(scanned[0].title(), "Bartender4-master");
    }

    #[test]
    fn blizzard_and_hidden_folders_are_skipped() {
        let addons = tempfile::tempdir().unwrap();
        for folder in [
            "Blizzard_AuctionUI",
            ".old_launcher-staging",
            "Zeta",
            "alpha",
        ] {
            fs::create_dir_all(addons.path().join(folder)).unwrap();
        }

        let names: Vec<_> = Addon::scan(addons.path())
            .unwrap()
            .into_iter()
            .map(|addon| addon.name)
            .collect();

        assert_eq!(names, ["alpha", "Zeta"]);
    }

    #[test]
    fn a_missing_addons_directory_has_no_addons() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(Addon::scan(&root.path().join("nope")).unwrap(), []);
    }
}
