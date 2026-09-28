//! What an installation holds, read straight from disk.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::catalog::Entry;
use crate::{Addon, Marker, SavedVariables, Toc};

/// The addons and addon data of an installation at one point in time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Every folder under `Interface/AddOns`, sorted by name.
    pub addons: Vec<Addon>,
    /// Every addon data file under `WTF/Account`.
    pub saved_variables: SavedVariables,
}

/// Addon folders that are installed and removed together: everything one
/// catalog entry installed, or a single folder installed by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group<'a> {
    /// The marker shared by the folders, if the launcher installed them.
    pub marker: Option<&'a Marker>,
    /// The folders, never empty.
    pub addons: Vec<&'a Addon>,
}

impl Group<'_> {
    /// The catalog entry name, or the addon title for hand-installed folders.
    pub fn title(&self) -> &str {
        match self.marker {
            Some(marker) => &marker.entry,
            None => self.addons[0].title(),
        }
    }

    /// The folder names.
    pub fn folders(&self) -> Vec<String> {
        self.addons.iter().map(|addon| addon.name.clone()).collect()
    }

    /// The titles of the other `groups` with a folder whose `.toc` requires
    /// one of this group's folders, in the order of `groups`.
    pub fn required_by<'b>(&self, groups: &'b [Group<'_>]) -> Vec<&'b str> {
        self.listed_by(groups, |toc| &toc.dependencies)
            .into_iter()
            .map(Group::title)
            .collect()
    }

    /// The titles of the other `groups` with a folder whose `.toc` lists one
    /// of this group's folders as optional, and that do not also require it.
    pub fn optionally_used_by<'b>(&self, groups: &'b [Group<'_>]) -> Vec<&'b str> {
        let required = self.listed_by(groups, |toc| &toc.dependencies);

        self.listed_by(groups, |toc| &toc.optional_dependencies)
            .into_iter()
            .filter(|other| !required.iter().any(|group| same(group, other)))
            .map(Group::title)
            .collect()
    }

    /// The other `groups` with a folder whose `.toc` names one of this
    /// group's folders in the dependency list that `list` picks.
    fn listed_by<'b, 'g>(
        &self,
        groups: &'b [Group<'g>],
        list: fn(&Toc) -> &[String],
    ) -> Vec<&'b Group<'g>> {
        groups
            .iter()
            .filter(|other| {
                !same(self, other)
                    && other
                        .addons
                        .iter()
                        .filter_map(|addon| addon.toc.as_ref())
                        .flat_map(list)
                        .any(|dependency| {
                            self.addons
                                .iter()
                                .any(|addon| addon.name.eq_ignore_ascii_case(dependency))
                        })
            })
            .collect()
    }
}

/// Whether two groups are the same one. Groups never share folders, so the
/// first folder identifies them.
fn same(a: &Group<'_>, b: &Group<'_>) -> bool {
    a.addons[0].name == b.addons[0].name
}

impl Snapshot {
    /// The installed folders grouped by the catalog entry that installed
    /// them, sorted by title.
    pub fn groups(&self) -> Vec<Group<'_>> {
        let mut groups: Vec<Group<'_>> = Vec::new();

        for addon in &self.addons {
            let existing = addon
                .marker
                .as_ref()
                .and_then(|marker| groups.iter_mut().find(|group| group.marker == Some(marker)));

            match existing {
                Some(group) => group.addons.push(addon),
                None => groups.push(Group {
                    marker: addon.marker.as_ref(),
                    addons: vec![addon],
                }),
            }
        }

        groups.sort_by_cached_key(|group| group.title().to_lowercase());
        groups
    }

    /// The folders installed from `entry`.
    pub fn installed(&self, entry: &Entry) -> Vec<&Addon> {
        self.addons
            .iter()
            .filter(|addon| entry.installed(addon))
            .collect()
    }

    /// The data files of every folder in `group`.
    pub fn data_files(&self, group: &Group<'_>) -> Vec<PathBuf> {
        group
            .addons
            .iter()
            .flat_map(|addon| self.saved_variables.of(&addon.name))
            .cloned()
            .collect()
    }

    /// Data files left behind by addons that are no longer installed, by
    /// addon name.
    pub fn orphaned_data(&self) -> Vec<(String, &[PathBuf])> {
        let installed: HashSet<String> = self
            .addons
            .iter()
            .map(|addon| addon.name.to_lowercase())
            .collect();

        self.saved_variables
            .iter()
            .filter(|(name, _)| !installed.contains(&name.to_lowercase()))
            .collect()
    }

    /// The required dependencies of `addon` that are not installed.
    pub fn missing_dependencies<'a>(&self, addon: &'a Addon) -> Vec<&'a str> {
        let Some(toc) = &addon.toc else {
            return Vec::new();
        };

        toc.dependencies
            .iter()
            .filter(|dependency| {
                !self
                    .addons
                    .iter()
                    .any(|installed| installed.name.eq_ignore_ascii_case(dependency))
            })
            .map(String::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Toc;

    fn marker(entry: &str) -> Marker {
        Marker {
            source: "nom0re".into(),
            entry: entry.into(),
            url: format!("https://example.com/{entry}.zip"),
        }
    }

    fn addon(name: &str, marker: Option<Marker>) -> Addon {
        Addon {
            name: name.into(),
            toc: Some(Toc {
                title: Some(format!("{name} title")),
                ..Toc::default()
            }),
            marker,
        }
    }

    #[test]
    fn folders_from_one_entry_form_one_group() {
        let snapshot = Snapshot {
            addons: vec![
                addon("Skada", Some(marker("Skada"))),
                addon("ACP", None),
                addon("SkadaImprovement", Some(marker("Skada"))),
            ],
            ..Snapshot::default()
        };

        let groups = snapshot.groups();

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].title(), "ACP title");
        assert_eq!(groups[1].title(), "Skada");
        assert_eq!(groups[1].folders(), ["Skada", "SkadaImprovement"]);
    }

    #[test]
    fn groups_know_which_other_groups_require_them() {
        let mut plugin = addon("Plugin", None);
        plugin.toc.as_mut().unwrap().dependencies = vec!["ace3".into()];
        let mut options = addon("SkadaOptions", Some(marker("Skada")));
        options.toc.as_mut().unwrap().dependencies = vec!["Skada".into(), "Ace3".into()];
        let snapshot = Snapshot {
            addons: vec![
                addon("Ace3", None),
                plugin,
                addon("Skada", Some(marker("Skada"))),
                options,
            ],
            ..Snapshot::default()
        };

        let groups = snapshot.groups();
        let [ace3, plugin, skada] = groups.as_slice() else {
            panic!("expected three groups, got {groups:?}");
        };

        assert_eq!(ace3.required_by(&groups), ["Plugin title", "Skada"]);
        assert!(plugin.required_by(&groups).is_empty());
        // SkadaOptions requiring Skada is internal to the Skada entry.
        assert!(skada.required_by(&groups).is_empty());
    }

    #[test]
    fn optional_users_exclude_groups_that_require_it_anyway() {
        let mut both = addon("Both", None);
        both.toc.as_mut().unwrap().dependencies = vec!["Lib".into()];
        both.toc.as_mut().unwrap().optional_dependencies = vec!["Lib".into()];
        let mut optional = addon("Optional", None);
        optional.toc.as_mut().unwrap().optional_dependencies = vec!["lib".into()];
        let snapshot = Snapshot {
            addons: vec![addon("Lib", None), both, optional],
            ..Snapshot::default()
        };

        let groups = snapshot.groups();
        let lib = groups
            .iter()
            .find(|group| group.title() == "Lib title")
            .unwrap();

        assert_eq!(lib.required_by(&groups), ["Both title"]);
        assert_eq!(lib.optionally_used_by(&groups), ["Optional title"]);
    }

    #[test]
    fn missing_dependencies_ignore_case() {
        let mut dependent = addon("Plugin", None);
        dependent.toc.as_mut().unwrap().dependencies = vec!["ace3".into(), "Missing".into()];
        let snapshot = Snapshot {
            addons: vec![addon("Ace3", None), dependent.clone()],
            ..Snapshot::default()
        };

        assert_eq!(snapshot.missing_dependencies(&dependent), ["Missing"]);
    }

    #[test]
    fn orphaned_data_belongs_to_uninstalled_addons_only() {
        let wtf = tempfile::tempdir().unwrap();
        let folder = wtf.path().join("Account/ME/SavedVariables");
        std::fs::create_dir_all(&folder).unwrap();
        for file in ["ACP.lua", "Recount.lua", "Recount.lua.bak"] {
            std::fs::write(folder.join(file), "").unwrap();
        }
        let snapshot = Snapshot {
            addons: vec![addon("acp", None)],
            saved_variables: SavedVariables::scan(wtf.path()),
        };

        let orphaned = snapshot.orphaned_data();

        assert_eq!(orphaned.len(), 1);
        assert_eq!(orphaned[0].0, "Recount");
        assert_eq!(orphaned[0].1.len(), 2);
    }
}
