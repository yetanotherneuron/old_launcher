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
    /// When it was last updated (`YYYY-MM-DD`): as its source says, or as the
    /// newest date in its archive (of the files in its own folders, for one
    /// of the addons of an archive holding several), or, for an addon of a
    /// repository's snapshot, the last commit that changed its folders; none
    /// when nothing says.
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
    /// The folders it puts in `Interface/AddOns`, and the only ones
    /// installing it takes from the archive: an archive holding several
    /// unrelated addons is listed as one download for each.
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

/// What tells an addon folder of an archive apart from the others, as the
/// indexer records it to tell which of them belong together: what its `.toc`
/// requires, which folder it sits inside, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    /// The folder, as in [`Contents::folders`].
    pub name: String,
    /// Where it sits in the archive, its directories joined by `/`
    /// (`addons-main/Addons/ElvUI_MultiBot`), whose last may be named
    /// otherwise than the folder, which takes its `.toc`'s name. Empty for
    /// the archive's root.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// The folders its `.toc` requires (`Dependencies`, `RequiredDeps`),
    /// as written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    /// The `.toc` notes, without UI escape sequences and trimmed; none when
    /// blank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The addon folder of the archive it sits inside, when it is a module
    /// of another (see [`layout::find`]): the closest one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inside: Option<String>,
    /// When the newest file in it, the folders inside it included, was
    /// modified (`YYYY-MM-DD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// When the last commit that changed it was made (`YYYY-MM-DD`), for an
    /// archive that is a snapshot of a repository, whose files all carry the
    /// date of the snapshot's commit; none as inspected, until the indexer
    /// looks it up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed: Option<String>,
}

/// What an archive installs, as the indexer records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contents {
    /// The addon folders, as [`Installation::install`](crate::Installation::install)
    /// would place them.
    pub folders: Vec<Folder>,
    /// What tells each of the folders apart, one for each in the same
    /// order. Empty when recorded by an indexer from before parts were.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// What the addon says it does: the `.toc` notes of the first folder
    /// that has some, without UI escape sequences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// When the newest entry in the archive, folders included, was modified
    /// (`YYYY-MM-DD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

        let dates = archive::extract(archive, staging.path())?;

        let found = layout::find(staging.path())?;
        let mut folders = Vec::new();
        let mut parts = Vec::new();
        let mut notes = None;

        for folder in &found {
            let toc = Toc::parse(&fs::read(&folder.toc).map_err(io_at(&folder.toc))?);
            let folder_notes = toc
                .notes
                .map(|notes| notes.trim().to_owned())
                .filter(|notes| !notes.is_empty());
            // A module sits inside the folder of its addon, and of the
            // addon's own addon when that is a module too: the closest is
            // the one it belongs to.
            let inside = found
                .iter()
                .filter(|other| other.path != folder.path && folder.path.starts_with(&other.path))
                .max_by_key(|other| other.path.components().count())
                .map(|other| other.name.clone());
            let path = folder
                .path
                .strip_prefix(staging.path())
                .unwrap_or(&folder.path)
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");

            if notes.is_none() {
                notes.clone_from(&folder_notes);
            }

            parts.push(Part {
                name: folder.name.clone(),
                updated: dates.newest_in(&path),
                path,
                dependencies: toc.dependencies,
                notes: folder_notes,
                inside,
                committed: None,
            });
            folders.push(Folder {
                name: folder.name.clone(),
                title: toc.title,
                version: toc.version,
                interface: toc.interface,
            });
        }

        Ok(Self {
            folders,
            parts,
            notes,
            updated: dates.newest(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{dated_zip_of, zip_of};

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
            changed: Vec::new(),
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

    #[test]
    fn an_archive_says_what_it_does_with_the_first_notes_of_its_folders() {
        let archive = zip_of(&[
            (
                "Kekator-main/addon/Kekator/Kekator.toc",
                "## Title: Kekator\n",
            ),
            (
                "Kekator-main/addon/Kekator_NPC/Kekator_NPC.toc",
                "## Notes: |cff00ff00Scripted NPC lines.|r \n",
            ),
            (
                "Kekator-main/addon/Kekator_Zone/Kekator_Zone.toc",
                "## Notes: Zones.\n",
            ),
        ]);

        assert_eq!(
            Contents::inspect(&archive).unwrap().notes.as_deref(),
            Some("Scripted NPC lines."),
            "the first folder has none, the second's lose their escapes"
        );
        assert_eq!(
            Contents::inspect(&zip_of(&[("A/A.toc", "## Notes:  \n")]))
                .unwrap()
                .notes,
            None,
            "blank notes say nothing"
        );
    }

    #[test]
    fn an_archive_says_what_each_folder_requires_and_sits_inside() {
        let archive = zip_of(&[
            (
                "Pack-main/Foo-main/Foo.toc",
                "## Title: Foo\n## Notes: |cff00ff00Does foo.|r \n",
            ),
            (
                "Pack-main/Foo-main/Foo_Options/Foo_Options.toc",
                "## Dependencies: Foo\n",
            ),
            (
                "Pack-main/Bar/Bar.toc",
                "## Dependencies: Blizzard_RaidUI, Foo\n## Notes:\n",
            ),
        ]);

        let contents = Contents::inspect(&archive).unwrap();

        assert_eq!(
            contents
                .folders
                .iter()
                .map(|folder| folder.name.as_str())
                .collect::<Vec<_>>(),
            ["Bar", "Foo", "Foo_Options"]
        );
        assert_eq!(
            contents.parts,
            [
                Part {
                    name: "Bar".into(),
                    path: "Pack-main/Bar".into(),
                    dependencies: vec!["Blizzard_RaidUI".into(), "Foo".into()],
                    notes: None,
                    inside: None,
                    updated: None,
                    committed: None,
                },
                Part {
                    name: "Foo".into(),
                    path: "Pack-main/Foo-main".into(),
                    dependencies: Vec::new(),
                    notes: Some("Does foo.".into()),
                    inside: None,
                    updated: None,
                    committed: None,
                },
                Part {
                    name: "Foo_Options".into(),
                    path: "Pack-main/Foo-main/Foo_Options".into(),
                    dependencies: vec!["Foo".into()],
                    notes: None,
                    inside: Some("Foo".into()),
                    updated: None,
                    committed: None,
                },
            ]
        );
        assert_eq!(contents.notes.as_deref(), Some("Does foo."));
    }

    #[test]
    fn each_folder_of_an_archive_is_dated_by_its_newest_file() {
        let archive = dated_zip_of(&[
            // When the pack was put together.
            ("Pack/Foo-main/", "", (2010, 8, 1)),
            ("Pack/Foo-main/Foo.toc", "", (2009, 12, 16)),
            (
                "Pack/Foo-main/Foo_Options/Foo_Options.toc",
                "## Dependencies: Foo\n",
                (2010, 1, 18),
            ),
            ("Pack/Bar/Bar.toc", "", (2010, 7, 4)),
        ]);

        let contents = Contents::inspect(&archive).unwrap();

        assert_eq!(
            contents
                .parts
                .iter()
                .map(|part| (part.name.as_str(), part.updated.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("Bar", Some("2010-07-04")),
                ("Foo", Some("2010-01-18")),
                ("Foo_Options", Some("2010-01-18")),
            ]
        );
        assert_eq!(contents.updated.as_deref(), Some("2010-08-01"));
    }

    #[test]
    fn a_catalog_from_before_parts_reads_without_them() {
        let json = br#"{ "folders": [{ "name": "A" }] }"#;

        let contents: Contents = serde_json::from_slice(json).unwrap();

        assert_eq!(contents.folders.len(), 1);
        assert!(contents.parts.is_empty());
    }

    #[test]
    fn an_upper_case_toc_extension_is_read_too() {
        // As Group Inviter is packaged on WoWInterface.
        let archive = zip_of(&[
            (
                "Groupinviter/Groupinviter.TOC",
                "## Title: Group Inviter\n## Interface: 30300\n",
            ),
            ("Groupinviter/Groupinviter.LUA", ""),
        ]);

        let contents = Contents::inspect(&archive).unwrap();

        assert_eq!(
            contents.folders,
            [Folder {
                name: "Groupinviter".into(),
                title: Some("Group Inviter".into()),
                version: None,
                interface: Some(30300),
            }]
        );
    }
}
