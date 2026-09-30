//! The launcher's catalog: every addon it can install, with where to get it
//! and the folders it puts in `Interface/AddOns`.
//!
//! The catalog is generated offline by the `indexer` tool, which downloads
//! every archive to record its folders and merges listings of the same addon
//! from different sources, and is published as one JSON document. The app
//! only reads it.

use std::collections::BTreeMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::error::io_at;
use crate::{Addon, Error, Marker, Toc, archive, layout};

/// A published catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    /// The schema version; see [`Catalog::VERSION`].
    pub version: u32,
    /// The upstream collections the downloads come from.
    pub sources: Vec<Source>,
    /// Every installable addon.
    pub entries: Vec<Entry>,
}

/// An upstream collection of addons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Stable identifier, recorded in each installed addon's [`Marker`].
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Where people can browse the collection themselves.
    pub homepage: String,
}

/// An addon the catalog offers, possibly from several sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Display name.
    pub name: String,
    /// What the addon does.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// A screenshot, if a source has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Where to get it, the newest first. They all install the same
    /// folders; the first is installed, unless the user picks another.
    pub downloads: Vec<Download>,
    /// The source the user prefers for it, if they picked one: its download
    /// goes first. Not published; [`Catalog::prefer`] sets it from the
    /// user's settings.
    #[serde(skip)]
    pub preferred: Option<String>,
}

/// One source's download of an addon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    /// The [`Source`] id listing it.
    pub source: String,
    /// The name the source lists it under, unique within that source.
    pub name: String,
    /// Where to download the archive.
    pub url: String,
    /// When its newest file was modified (`YYYY-MM-DD`), if the archive says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// The addon's own page at the source, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    /// The history of its changes at the source (a commit log, a list of
    /// releases), if the source keeps one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<String>,
    /// Other pages about the addon (original author, CurseForge, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// The folders it puts in `Interface/AddOns`.
    pub folders: Vec<Folder>,
}

/// A labelled web link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// What the page is ("Github", "Curseforge", …).
    pub label: String,
    /// The page URL.
    pub url: String,
}

/// An addon folder a download installs, as its `.toc` describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// The folder name under `Interface/AddOns`.
    pub name: String,
    /// The `.toc` title, without UI escape sequences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The `.toc` version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The client interface the `.toc` declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<u32>,
}

/// What an archive installs, as the indexer records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contents {
    /// The addon folders, as [`Installation::install`](crate::Installation::install)
    /// would place them.
    pub folders: Vec<Folder>,
    /// When the newest file in the archive was modified (`YYYY-MM-DD`).
    pub updated: Option<String>,
}

impl Catalog {
    /// The schema version this crate reads and writes.
    pub const VERSION: u32 = 2;

    /// Reads a published catalog, refusing schema versions this build does
    /// not know. Entries without downloads are dropped.
    pub fn parse(json: &[u8]) -> Result<Self, Error> {
        let mut catalog: Self = serde_json::from_slice(json)?;

        if catalog.version != Self::VERSION {
            return Err(Error::CatalogVersion(catalog.version));
        }

        catalog.entries.retain(|entry| !entry.downloads.is_empty());

        Ok(catalog)
    }

    /// The catalog as it is published: sorted, so regenerating it only
    /// changes what actually changed, and pretty-printed for readable diffs.
    /// The order of each entry's downloads is kept: it is their preference.
    pub fn to_json(&self) -> Result<String, Error> {
        let mut catalog = self.clone();

        catalog.sources.sort_by(|a, b| a.id.cmp(&b.id));
        for download in catalog
            .entries
            .iter_mut()
            .flat_map(|entry| &mut entry.downloads)
        {
            download
                .folders
                .sort_by_cached_key(|folder| folder.name.to_lowercase());
        }
        catalog
            .entries
            .sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.name.clone()));

        let mut json = serde_json::to_string_pretty(&catalog)?;
        json.push('\n');

        Ok(json)
    }

    /// The source with the given id.
    pub fn source(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|source| source.id == id)
    }

    /// Notes the source the user prefers for each entry, from their
    /// settings: `sources`, by [`Entry::key`]. Entries not in it follow the
    /// catalog's order again.
    pub fn prefer(&mut self, sources: &BTreeMap<String, String>) {
        for entry in &mut self.entries {
            entry.preferred = sources.get(&entry.key()).cloned();
        }
    }
}

impl Entry {
    /// What identifies the entry across catalog refreshes, and names it in
    /// the user's settings: the folders it installs, which the catalog never
    /// gives two entries, lowercase and sorted, joined by `/`, which no
    /// folder name holds.
    pub fn key(&self) -> String {
        let mut folders: Vec<String> = self
            .downloads
            .first()
            .map_or(&[][..], |download| download.folders.as_slice())
            .iter()
            .map(|folder| folder.name.to_lowercase())
            .collect();
        folders.sort();
        folders.dedup();

        folders.join("/")
    }

    /// The download of the source the user picked, when they picked one
    /// and the source still lists the entry.
    fn picked(&self) -> Option<&Download> {
        self.preferred.as_deref().and_then(|source| {
            self.downloads
                .iter()
                .find(|download| download.source == source)
        })
    }

    /// The download installed first, which also describes the entry: the
    /// picked source's, or else the first, the newest.
    pub fn preferred(&self) -> Option<&Download> {
        self.picked().or_else(|| self.downloads.first())
    }

    /// Every download, the preferred one first, then the rest as the
    /// catalog orders them.
    pub fn ranked(&self) -> impl Iterator<Item = &Download> {
        let first = self.preferred();

        first.into_iter().chain(
            self.downloads
                .iter()
                .filter(move |download| first.is_none_or(|first| !std::ptr::eq(*download, first))),
        )
    }

    /// The folders installing it puts in `Interface/AddOns`, as the
    /// preferred download describes them.
    pub fn folders(&self) -> &[Folder] {
        self.preferred()
            .map_or(&[], |download| download.folders.as_slice())
    }

    /// Whether `addon` was installed from this entry, through any download.
    pub fn installed(&self, addon: &Addon) -> bool {
        addon.marker.as_ref().is_some_and(|marker| {
            self.downloads
                .iter()
                .any(|download| marker.is_for(&download.source, &download.name))
        })
    }
}

impl Download {
    /// The marker written into every folder installed from this download.
    pub fn marker(&self) -> Marker {
        Marker {
            source: self.source.clone(),
            entry: self.name.clone(),
            url: self.url.clone(),
        }
    }
}

impl Contents {
    /// Lists what `archive` installs, reading each addon folder's `.toc`.
    pub fn inspect(archive: &[u8]) -> Result<Self, Error> {
        let temporary = std::env::temp_dir();
        let staging = tempfile::tempdir().map_err(io_at(&temporary))?;

        let updated = archive::extract(archive, staging.path())?;

        let folders = layout::find(staging.path())?
            .into_iter()
            .map(|folder| {
                // The name is the stem of a `.toc` file inside the folder.
                let path = folder.path.join(format!("{}.toc", folder.name));
                let toc = Toc::parse(&fs::read(&path).map_err(io_at(&path))?);

                Ok(Folder {
                    name: folder.name,
                    title: toc.title,
                    version: toc.version,
                    interface: toc.interface,
                })
            })
            .collect::<Result<_, Error>>()?;

        Ok(Self { folders, updated })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::zip_of;

    fn download(source: &str, name: &str, folders: &[&str]) -> Download {
        Download {
            source: source.into(),
            name: name.into(),
            url: format!("https://example.com/{source}/{name}.zip"),
            updated: None,
            page: None,
            changes: None,
            links: Vec::new(),
            folders: folders
                .iter()
                .map(|folder| Folder {
                    name: (*folder).into(),
                    title: None,
                    version: None,
                    interface: None,
                })
                .collect(),
        }
    }

    fn entry(name: &str, downloads: Vec<Download>) -> Entry {
        Entry {
            name: name.into(),
            description: String::new(),
            image: None,
            downloads,
            preferred: None,
        }
    }

    #[test]
    fn a_preferred_source_goes_first_and_describes_the_entry() {
        let mut acp = entry(
            "ACP",
            vec![
                download("rising-gods", "ACP", &["ACP"]),
                download("locus313", "ACP", &["ACP"]),
            ],
        );
        acp.downloads[1].folders[0].version = Some("3.2".into());
        let mut catalog = Catalog {
            version: Catalog::VERSION,
            sources: Vec::new(),
            entries: vec![acp],
        };
        let ranked = |catalog: &Catalog| -> Vec<String> {
            catalog.entries[0]
                .ranked()
                .map(|download| download.source.clone())
                .collect()
        };
        let preferred = |catalog: &Catalog| catalog.entries[0].preferred().unwrap().source.clone();

        assert_eq!(catalog.entries[0].key(), "acp");
        assert_eq!(preferred(&catalog), "rising-gods");
        assert_eq!(ranked(&catalog), ["rising-gods", "locus313"]);
        assert_eq!(catalog.entries[0].folders()[0].version, None);

        catalog.prefer(&BTreeMap::from([("acp".to_owned(), "locus313".to_owned())]));
        assert_eq!(preferred(&catalog), "locus313");
        assert_eq!(ranked(&catalog), ["locus313", "rising-gods"]);
        assert_eq!(
            catalog.entries[0].folders()[0].version.as_deref(),
            Some("3.2")
        );

        // A source that stopped listing it: the catalog's order again.
        catalog.prefer(&BTreeMap::from([("acp".to_owned(), "gone".to_owned())]));
        assert_eq!(preferred(&catalog), "rising-gods");
        assert_eq!(ranked(&catalog), ["rising-gods", "locus313"]);

        catalog.prefer(&BTreeMap::new());
        assert_eq!(catalog.entries[0].preferred, None);
    }

    #[test]
    fn folders_carrying_any_download_marker_count_as_installed() {
        let entry = entry(
            "Skada",
            vec![
                download("a", "Skada", &["Skada"]),
                download("b", "Skada!", &["Skada"]),
            ],
        );
        let installed = Addon {
            name: "Skada".into(),
            toc: None,
            marker: Some(entry.downloads[1].marker()),
        };
        let manual = Addon {
            marker: None,
            ..installed.clone()
        };

        assert!(entry.installed(&installed));
        assert!(!entry.installed(&manual));
    }

    #[test]
    fn published_catalogs_read_back_identically() {
        let mut acp = download("a", "ACP", &["ACP"]);
        acp.changes = Some("https://example.com/a/ACP/commits".into());
        let catalog = Catalog {
            version: Catalog::VERSION,
            sources: vec![Source {
                id: "test".into(),
                name: "Test".into(),
                homepage: "https://example.com".into(),
            }],
            entries: vec![
                entry(
                    "Skada",
                    vec![
                        download("b", "Skada", &["SkadaStorage", "Skada"]),
                        download("a", "Skada", &["Skada", "SkadaStorage"]),
                    ],
                ),
                entry("ACP", vec![acp]),
            ],
        };

        let published = Catalog::parse(catalog.to_json().unwrap().as_bytes()).unwrap();

        assert_eq!(published.entries[0].name, "ACP");
        assert_eq!(
            published.entries[0].downloads[0].changes.as_deref(),
            Some("https://example.com/a/ACP/commits")
        );
        assert_eq!(published.entries[1].key(), "skada/skadastorage");
        assert_eq!(published.entries[1].folders()[0].name, "Skada");
        // Download order is a preference, not sorted away.
        assert_eq!(published.entries[1].downloads[0].source, "b");
        assert_eq!(published.source("test").unwrap().name, "Test");
    }

    #[test]
    fn unknown_schema_versions_are_refused() {
        let json = br#"{ "version": 1, "sources": [], "entries": [] }"#;

        assert!(matches!(
            Catalog::parse(json),
            Err(Error::CatalogVersion(1))
        ));
    }

    #[test]
    fn entries_without_downloads_are_dropped() {
        let json = br#"{ "version": 2, "sources": [], "entries": [{ "name": "Ghost", "downloads": [] }] }"#;

        assert!(Catalog::parse(json).unwrap().entries.is_empty());
    }

    #[test]
    fn inspecting_an_archive_lists_its_folders_and_tocs() {
        let archive = zip_of(&[
            (
                "Skada-main/Skada/Skada.toc",
                "## Title: |cffff0000Skada|r\n## Version: 1.8\n## Interface: 30300\n",
            ),
            ("Skada-main/SkadaStorage/SkadaStorage.toc", ""),
            ("Skada-main/README.md", ""),
        ]);

        let contents = Contents::inspect(&archive).unwrap();

        assert_eq!(
            contents.folders,
            [
                Folder {
                    name: "Skada".into(),
                    title: Some("Skada".into()),
                    version: Some("1.8".into()),
                    interface: Some(30300),
                },
                Folder {
                    name: "SkadaStorage".into(),
                    title: None,
                    version: None,
                    interface: None,
                },
            ]
        );
        assert_eq!(contents.updated, None);
    }
}
