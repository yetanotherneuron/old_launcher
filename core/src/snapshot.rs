//! What an installation holds, read straight from disk.
//!
//! The folders and data files are read at once; the addons inside the
//! game's archives come from the look at its patches, which takes longer
//! (see [`Bundled`]). Until they are known, nothing that depends on them is
//! said: a dependency no folder has isn't missing yet, a folder without its
//! `.toc` isn't broken yet (nor listed, unless the launcher installed it),
//! and data no folder owns isn't left over yet.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::catalog::Entry;
use crate::{Addon, Bundled, Marker, SavedVariables, Toc};

/// The addons and addon data of an installation at one point in time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Every folder under `Interface/AddOns`, sorted by name.
    pub addons: Vec<Addon>,
    /// Every addon data file under `WTF/Account`.
    pub saved_variables: SavedVariables,
    /// The addons inside the archives the game loads. Not known when read
    /// from disk: the launcher fills it in once its look at them is over.
    pub bundled: Bundled,
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

    /// The files of its folders that changed since the launcher installed
    /// them (see [`crate::marker::changed`]), as `folder/path`.
    pub fn changed(&self) -> Vec<String> {
        self.addons
            .iter()
            .flat_map(|addon| {
                addon
                    .changed
                    .iter()
                    .map(move |path| format!("{}/{path}", addon.name))
            })
            .collect()
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
    /// Whether the folder `addon` is listed as an addon of its own. Not when
    /// it is named after an addon of the archives the game loads, which it
    /// is part of (Blizzard's folders that hold only their `.pub`, say), nor,
    /// until the archives are known, when it has no `.toc` and so may be.
    /// The folders the launcher installed always are.
    pub fn listed(&self, addon: &Addon) -> bool {
        addon.marker.is_some()
            || match self.bundled.hold(&addon.name) {
                Some(held) => !held,
                None => addon.toc.is_some(),
            }
    }

    /// Whether the client can't load the folder `addon`: it has no `.toc`,
    /// and the archives the game loads don't hold one for it either. Not
    /// until the archives are known.
    pub fn broken(&self, addon: &Addon) -> bool {
        addon.toc.is_none() && self.bundled.hold(&addon.name) == Some(false)
    }

    /// Whether the game has the addon called `name`, in any case: a folder
    /// of that name with its `.toc`, or one inside the archives it loads.
    /// `None` when no folder has it and the archives aren't known yet.
    pub fn has(&self, name: &str) -> Option<bool> {
        let folder = self
            .addons
            .iter()
            .any(|addon| addon.toc.is_some() && addon.name.eq_ignore_ascii_case(name));

        if folder {
            Some(true)
        } else {
            self.bundled.hold(name)
        }
    }

    /// The installed folders grouped by the catalog entry that installed
    /// them, sorted by title: those [listed](Self::listed) alone.
    pub fn groups(&self) -> Vec<Group<'_>> {
        let mut groups: Vec<Group<'_>> = Vec::new();

        for addon in self.addons.iter().filter(|addon| self.listed(addon)) {
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
    /// addon name: no folder has their name, and the archives the game
    /// loads don't hold them. None until the archives are known.
    pub fn orphaned_data(&self) -> Vec<(String, &[PathBuf])> {
        let installed: HashSet<String> = self
            .addons
            .iter()
            .map(|addon| addon.name.to_lowercase())
            .collect();

        self.saved_variables
            .iter()
            .filter(|(name, _)| {
                !installed.contains(&name.to_lowercase()) && self.bundled.hold(name) == Some(false)
            })
            .collect()
    }

    /// The required dependencies of `addon` that the game doesn't have (see
    /// [`Snapshot::has`]); none that may be in the archives while they
    /// aren't known.
    pub fn missing_dependencies<'a>(&self, addon: &'a Addon) -> Vec<&'a str> {
        let Some(toc) = &addon.toc else {
            return Vec::new();
        };

        toc.dependencies
            .iter()
            .filter(|dependency| self.has(dependency) == Some(false))
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
            changed: Vec::new(),
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
        dependent.toc.as_mut().unwrap().dependencies =
            vec!["ace3".into(), "Missing".into(), "Broken".into()];
        let snapshot = Snapshot {
            addons: vec![
                addon("Ace3", None),
                Addon {
                    toc: None,
                    ..addon("Broken", None)
                },
                dependent.clone(),
            ],
            bundled: Bundled::none(),
            ..Snapshot::default()
        };

        // A folder without its `.toc` is no addon to the game.
        assert_eq!(
            snapshot.missing_dependencies(&dependent),
            ["Missing", "Broken"]
        );
    }

    #[test]
    fn orphaned_data_belongs_to_uninstalled_addons_only() {
        let wtf = tempfile::tempdir().unwrap();
        let folder = wtf.path().join("Account/ME/SavedVariables");
        std::fs::create_dir_all(&folder).unwrap();
        for file in [
            "ACP.lua",
            "Recount.lua",
            "Recount.lua.bak",
            "Blizzard_CombatText.lua",
            "Blizzard_Gone.lua",
        ] {
            std::fs::write(folder.join(file), "").unwrap();
        }
        let mut snapshot = Snapshot {
            addons: vec![addon("acp", None)],
            saved_variables: SavedVariables::scan(wtf.path()),
            bundled: Bundled::default(),
        };

        assert!(
            snapshot.orphaned_data().is_empty(),
            "the archives may hold any of them"
        );

        snapshot.bundled = archives();
        let orphaned = snapshot.orphaned_data();

        let names: Vec<&str> = orphaned.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["Blizzard_Gone", "Recount"]);
        assert_eq!(orphaned[1].1.len(), 2);
    }

    /// A `.toc` inside the archives, as Blizzard's are.
    fn toc(name: &str) -> String {
        format!("Interface\\AddOns\\{name}\\{name}.toc")
    }

    /// The archives of a client with Blizzard's `Blizzard_CombatText` and
    /// `Blizzard_RaidUI`, and a mod's `!PYAndre` in an archive without a list
    /// of its files.
    fn archives() -> Bundled {
        crate::testing::bundled(&[
            (
                "Data/patch-y.mpq",
                &crate::mpq::writer::Writer::new().file(&toc("!PYAndre"), b"x"),
            ),
            (
                "Data/enUS/locale-enUS.MPQ",
                &crate::testing::archive(&[&toc("Blizzard_CombatText"), &toc("Blizzard_RaidUI")]),
            ),
        ])
    }

    /// KPack, requiring addons of the archives and others, a folder of
    /// Blizzard's holding only its `.pub`, one named like Blizzard's with
    /// its own `.toc`, and a folder without one.
    fn kpack() -> Snapshot {
        let mut kpack = addon("KPack", None);
        kpack.toc.as_mut().unwrap().dependencies = vec![
            "Blizzard_CombatText".into(),
            "!pyandre".into(),
            "Blizzard_HelloThisIsSomethingElse".into(),
            "Blizzard_Something".into(),
        ];
        let tocless = |name: &str| Addon {
            toc: None,
            ..addon(name, None)
        };

        Snapshot {
            addons: vec![
                tocless("Bartender4-master"),
                tocless("blizzard_combattext"),
                addon("Blizzard_Something", None),
                kpack,
            ],
            ..Snapshot::default()
        }
    }

    #[test]
    fn a_dependency_the_archives_hold_is_there_and_any_other_is_missing() {
        let mut snapshot = kpack();
        snapshot.bundled = archives();
        let kpack = &snapshot.addons[3];

        assert_eq!(
            snapshot.missing_dependencies(kpack),
            ["Blizzard_HelloThisIsSomethingElse"]
        );
        assert_eq!(snapshot.has("BLIZZARD_COMBATTEXT"), Some(true));
        assert_eq!(snapshot.has("Blizzard_Something"), Some(true), "its folder");
    }

    #[test]
    fn a_folder_of_an_addon_of_the_archives_is_part_of_it_and_any_other_is_listed() {
        let mut snapshot = kpack();
        snapshot.bundled = archives();

        let groups = snapshot.groups();
        let listed: Vec<&str> = groups.iter().map(Group::title).collect();
        assert_eq!(
            listed,
            [
                "Bartender4-master",
                "Blizzard_Something title",
                "KPack title"
            ]
        );
        let broken: Vec<&str> = snapshot
            .addons
            .iter()
            .filter(|addon| snapshot.broken(addon))
            .map(|addon| addon.name.as_str())
            .collect();
        assert_eq!(broken, ["Bartender4-master"]);
    }

    #[test]
    fn nothing_the_archives_may_hold_is_said_before_they_are_known() {
        let mut snapshot = kpack();
        let mut installed = Addon {
            toc: None,
            ..addon("Skada", Some(marker("Skada")))
        };
        installed.name = "SkadaBroken".into();
        snapshot.addons.push(installed);
        let kpack = &snapshot.addons[3];

        assert!(snapshot.missing_dependencies(kpack).is_empty());
        assert_eq!(snapshot.has("Blizzard_HelloThisIsSomethingElse"), None);
        assert!(snapshot.addons.iter().all(|addon| !snapshot.broken(addon)));
        let groups = snapshot.groups();
        let listed: Vec<&str> = groups.iter().map(Group::title).collect();
        // Those with a `.toc`, and the launcher's own, broken or not.
        assert_eq!(listed, ["Blizzard_Something title", "KPack title", "Skada"]);
    }
}
