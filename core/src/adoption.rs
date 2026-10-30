//! Recognizing addons that were installed without the launcher.
//!
//! The catalog knows which folders each entry installs, so folders without a
//! [`Marker`] that match an entry exactly can be given that entry's marker,
//! as if the launcher had installed them.

use std::collections::{HashMap, HashSet};

use crate::catalog::{Download, Entry};
use crate::{Addon, Marker, Snapshot};

/// Unmarked folders recognized as one catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoption {
    /// The name the catalog lists the entry under.
    pub name: String,
    /// The marker to write.
    pub marker: Marker,
    /// The folders to write it into, as named on disk.
    pub folders: Vec<String>,
}

impl Adoption {
    /// Recognizes catalog entries among the unmarked folders of `snapshot`
    /// that are [listed](Snapshot::listed): not those of an addon inside the
    /// game's archives.
    ///
    /// An entry is recognized when every folder it installs is present,
    /// loadable and unmarked. Entries installing more folders are matched
    /// first, so a bundle wins over an entry that ships one of its folders
    /// alone, and a folder is only ever claimed once.
    ///
    /// The marker names the entry's download that best matches what is
    /// installed: one whose recorded `.toc` versions contradict none of the
    /// installed ones and match the most of them. When that doesn't single
    /// one out, or none matches (the installed version isn't listed), the
    /// preferred download is named.
    pub fn plan(snapshot: &Snapshot, entries: &[Entry]) -> Vec<Self> {
        let unmarked: HashMap<String, &Addon> = snapshot
            .addons
            .iter()
            .filter(|addon| addon.marker.is_none() && addon.toc.is_some() && snapshot.listed(addon))
            .map(|addon| (addon.name.to_lowercase(), addon))
            .collect();

        let mut candidates: Vec<(Vec<String>, &Entry)> = entries
            .iter()
            .filter_map(|entry| {
                let mut folders: Vec<String> = entry
                    .folders()
                    .iter()
                    .map(|folder| folder.name.to_lowercase())
                    .collect();
                folders.sort();
                folders.dedup();

                let present = !folders.is_empty()
                    && folders.iter().all(|folder| unmarked.contains_key(folder));

                present.then_some((folders, entry))
            })
            .collect();

        // Most folders first; the sort is stable, so ties keep catalog order.
        candidates.sort_by_key(|(folders, _)| std::cmp::Reverse(folders.len()));

        let mut claimed: HashSet<String> = HashSet::new();
        let mut adoptions = Vec::new();

        for (folders, entry) in candidates {
            if folders.iter().any(|folder| claimed.contains(folder)) {
                continue;
            }
            claimed.extend(folders.iter().cloned());

            let installed_version = |download: &Download| -> Option<usize> {
                let mut exact = 0;

                for folder in &download.folders {
                    let installed = unmarked
                        .get(&folder.name.to_lowercase())
                        .and_then(|addon| addon.toc.as_ref())
                        .and_then(|toc| toc.version.as_ref());

                    match &folder.version {
                        None => {}
                        Some(version) if Some(version) == installed => exact += 1,
                        Some(_) => return None,
                    }
                }

                Some(exact)
            };

            let mut best: Option<(&Download, usize)> = None;

            for download in &entry.downloads {
                if let Some(exact) = installed_version(download)
                    && best.is_none_or(|(_, most)| exact > most)
                {
                    best = Some((download, exact));
                }
            }

            let Some(download) = best
                .map(|(download, _)| download)
                .or_else(|| entry.downloads.first())
            else {
                continue;
            };

            adoptions.push(Self {
                name: entry.name.clone(),
                marker: download.marker(),
                folders: folders
                    .iter()
                    .map(|folder| unmarked[folder].name.clone())
                    .collect(),
            });
        }

        adoptions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Toc;
    use crate::catalog::Folder;

    fn addon(name: &str, version: Option<&str>) -> Addon {
        Addon {
            name: name.into(),
            toc: Some(Toc {
                version: version.map(str::to_owned),
                ..Toc::default()
            }),
            marker: None,
            changed: Vec::new(),
        }
    }

    fn download(source: &str, folders: &[(&str, Option<&str>)]) -> Download {
        Download {
            source: source.into(),
            name: format!("{} from {source}", folders[0].0),
            url: format!("https://example.com/{source}.zip"),
            updated: None,
            page: None,
            changes: None,
            links: Vec::new(),
            folders: folders
                .iter()
                .map(|(folder, version)| Folder {
                    name: (*folder).into(),
                    title: None,
                    version: version.map(str::to_owned),
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

    fn snapshot(addons: Vec<Addon>) -> Snapshot {
        Snapshot {
            addons,
            ..Snapshot::default()
        }
    }

    fn adopted(adoptions: &[Adoption]) -> Vec<(&str, Vec<&str>)> {
        adoptions
            .iter()
            .map(|adoption| {
                (
                    adoption.marker.source.as_str(),
                    adoption.folders.iter().map(String::as_str).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn complete_folder_sets_are_adopted_under_their_local_names() {
        let snapshot = snapshot(vec![
            addon("skada", None),
            addon("SkadaStorage", None),
            addon("ACP", None),
        ]);
        let entries = [
            entry(
                "Skada",
                vec![download("a", &[("Skada", None), ("SkadaStorage", None)])],
            ),
            entry("ACP", vec![download("b", &[("ACP", None)])]),
        ];

        let adoptions = Adoption::plan(&snapshot, &entries);

        assert_eq!(
            adopted(&adoptions),
            [("a", vec!["skada", "SkadaStorage"]), ("b", vec!["ACP"])]
        );
        assert_eq!(adoptions[0].name, "Skada");
        assert_eq!(adoptions[0].marker, entries[0].downloads[0].marker());
    }

    #[test]
    fn partial_matches_are_left_alone() {
        let snapshot = snapshot(vec![addon("DBM-Core", None)]);
        let entries = [entry(
            "DBM",
            vec![download("a", &[("DBM-Core", None), ("DBM-GUI", None)])],
        )];

        assert!(Adoption::plan(&snapshot, &entries).is_empty());
    }

    #[test]
    fn marked_and_unloadable_folders_are_never_adopted() {
        let mut marked = addon("ACP", None);
        marked.marker = Some(download("x", &[("Other", None)]).marker());
        let broken = Addon {
            toc: None,
            ..addon("Foo", None)
        };
        let snapshot = snapshot(vec![marked, broken]);
        let entries = [
            entry("ACP", vec![download("a", &[("ACP", None)])]),
            entry("Foo", vec![download("a", &[("Foo", None)])]),
        ];

        assert!(Adoption::plan(&snapshot, &entries).is_empty());
    }

    #[test]
    fn bundles_win_over_entries_shipping_one_of_their_folders() {
        let snapshot = snapshot(vec![addon("Questie", None), addon("LibStub", None)]);
        let entries = [
            entry("LibStub", vec![download("lib", &[("LibStub", None)])]),
            entry(
                "Questie",
                vec![download("bundle", &[("Questie", None), ("LibStub", None)])],
            ),
        ];

        assert_eq!(
            adopted(&Adoption::plan(&snapshot, &entries)),
            [("bundle", vec!["LibStub", "Questie"])]
        );
    }

    #[test]
    fn the_download_matching_the_installed_version_is_recorded() {
        let snapshot = snapshot(vec![addon("ACP", Some("3.3.4"))]);
        let entries = [entry(
            "ACP",
            vec![
                download("newest", &[("ACP", Some("3.4.0"))]),
                download("unversioned", &[("ACP", None)]),
                download("matching", &[("ACP", Some("3.3.4"))]),
            ],
        )];

        assert_eq!(
            adopted(&Adoption::plan(&snapshot, &entries)),
            [("matching", vec!["ACP"])]
        );
    }

    #[test]
    fn unlisted_installed_versions_record_the_preferred_download() {
        let snapshot = snapshot(vec![addon("ACP", Some("0.9"))]);
        let entries = [entry(
            "ACP",
            vec![
                download("preferred", &[("ACP", Some("3.4.0"))]),
                download("other", &[("ACP", Some("3.3.4"))]),
            ],
        )];

        assert_eq!(
            adopted(&Adoption::plan(&snapshot, &entries)),
            [("preferred", vec!["ACP"])]
        );
    }

    #[test]
    fn ties_between_downloads_go_to_the_preferred_one() {
        let snapshot = snapshot(vec![addon("Altoholic", Some("3.3.002b"))]);
        let entries = [entry(
            "Altoholic",
            vec![
                download("first", &[("Altoholic", Some("3.3.002b"))]),
                download("second", &[("Altoholic", Some("3.3.002b"))]),
            ],
        )];

        assert_eq!(
            adopted(&Adoption::plan(&snapshot, &entries)),
            [("first", vec!["Altoholic"])]
        );
    }
}
