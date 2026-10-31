//! Telling apart the addons of an archive that holds several.
//!
//! Some archives are compilations: a repository of unrelated addons, a
//! server's pack of fifty folders. Listed as one download they would install
//! everything or nothing, and be one card in the list. What each folder's
//! `.toc` requires says which of them go together, so each addon of the
//! archive becomes a listing of its own, installing only its folders.
//!
//! Only the archives in [`ARCHIVES`] are split: what a `.toc` requires is
//! not enough to tell the addons of any archive apart.
//!
//! Every folder is grouped by its anchor, the folder it hangs from: a module
//! stays with the addon it sits inside, and a folder that requires others of
//! the archive takes theirs. A folder that requires nothing of the archive is
//! an addon in itself.

use old_launcher_core::catalog::Contents;

/// The listings, by source id and listing name, whose archive is a
/// compilation of unrelated addons, each listed apart, and so is split.
///
/// Splitting every archive by what its `.toc` files require was wrong about
/// as often as right: the zone modules of EveryQuest, the data modules of
/// Carbonite and the parts of RaidWatch declare nothing, and LittleWigs'
/// zones also require BigWigs, which comes from elsewhere. Those are one
/// addon each and stay one listing.
pub const ARCHIVES: &[(&str, &str)] = &[
    ("github-undeadrogue-wow-3.3.5a-addons", "wow-3.3.5a-addons"),
    ("rising-gods", "LUI v3"),
];

/// Whether the listing called `listing` of the source with id `source` is
/// one of the [`ARCHIVES`] to split. Both are matched exactly, so that a
/// renamed listing stays whole instead of being split by a guess.
pub fn wanted(source: &str, listing: &str) -> bool {
    ARCHIVES.iter().any(|&(archive_source, archive_listing)| {
        archive_source == source && archive_listing == listing
    })
}

/// The folders of an archive that make up one addon.
#[derive(Debug, PartialEq, Eq)]
pub struct Group {
    /// The folder the others hang from, as an index into
    /// [`Contents::folders`]: the one that stands alone, which names the
    /// addon. For an archive that isn't split, its first.
    pub anchor: usize,
    /// Every folder of the addon, anchor included, as indices into
    /// [`Contents::folders`], in the archive's order.
    pub folders: Vec<usize>,
}

/// The addons in `contents`, in the order of their first folder.
///
/// An archive whose parts weren't recorded (see [`Contents::parts`]), or
/// that holds a single folder, is one addon: there is nothing to tell apart
/// by.
pub fn groups(contents: &Contents) -> Vec<Group> {
    let count = contents.folders.len();

    if contents.parts.len() != count || count < 2 {
        return vec![Group {
            anchor: 0,
            folders: (0..count).collect(),
        }];
    }

    let mut groups: Vec<Group> = Vec::new();

    for folder in 0..count {
        let anchor = anchor_of(contents, folder, &mut Vec::new());

        match groups.iter_mut().find(|group| group.anchor == anchor) {
            Some(group) => group.folders.push(folder),
            None => groups.push(Group {
                anchor,
                folders: vec![folder],
            }),
        }
    }

    groups
}

/// The anchor of `folder`, while `resolving` are the folders being resolved
/// to get to it. One reached again is its own anchor, so that folders
/// requiring each other don't send this round in circles.
fn anchor_of(contents: &Contents, folder: usize, resolving: &mut Vec<usize>) -> usize {
    if resolving.contains(&folder) {
        return folder;
    }

    resolving.push(folder);
    let anchor = resolve(contents, folder, resolving);
    let _ = resolving.pop();

    anchor
}

/// The anchor of `folder`:
///
/// - the module's addon's, for a module of another folder of the archive,
///   which stays with it whatever its `.toc` says;
/// - its own, when it requires nothing of the archive: nothing but what the
///   game ships (`Blizzard_` addons), or only things the archive lacks. An
///   addon needing one that comes from elsewhere is a plugin or a bridge for
///   it, and an addon of its own: the launcher offers to install what it is
///   missing;
/// - the one the folders it requires all have, when they do: a folder
///   requiring an addon and its other modules stays with them;
/// - its own otherwise: a bridge between two of the archive's addons
///   belongs to neither.
fn resolve(contents: &Contents, folder: usize, resolving: &mut Vec<usize>) -> usize {
    let part = &contents.parts[folder];
    let find = |name: &str| {
        contents
            .folders
            .iter()
            .position(|other| other.name.eq_ignore_ascii_case(name))
    };

    if let Some(addon) = part.inside.as_deref().and_then(find) {
        return anchor_of(contents, addon, resolving);
    }

    let mut anchors = Vec::new();

    for name in part
        .dependencies
        .iter()
        .filter(|name| !shipped_with_the_game(name))
    {
        let Some(dependency) = find(name) else {
            return folder;
        };

        anchors.push(anchor_of(contents, dependency, resolving));
    }

    anchors.sort_unstable();
    anchors.dedup();

    match anchors.as_slice() {
        [only] => *only,
        _ => folder,
    }
}

/// Whether `name` is one of the game's own addons (`Blizzard_RaidUI`),
/// which every client has and no archive needs to hold.
fn shipped_with_the_game(name: &str) -> bool {
    const PREFIX: &[u8] = b"Blizzard_";

    name.as_bytes()
        .get(..PREFIX.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(PREFIX))
}

#[cfg(test)]
mod tests {
    use old_launcher_core::catalog::{Folder, Part};

    use super::*;
    use crate::source::{Source, github, rising_gods};

    /// An archive of the folders given, each with what its `.toc` requires
    /// and the folder it sits inside.
    fn archive(folders: &[(&str, &[&str], Option<&str>)]) -> Contents {
        Contents {
            folders: folders
                .iter()
                .map(|(name, _, _)| Folder {
                    name: (*name).to_owned(),
                    title: None,
                    version: None,
                    interface: None,
                })
                .collect(),
            parts: folders
                .iter()
                .map(|(name, dependencies, inside)| Part {
                    name: (*name).to_owned(),
                    path: String::new(),
                    dependencies: dependencies.iter().map(|name| (*name).to_owned()).collect(),
                    notes: None,
                    inside: inside.map(str::to_owned),
                    updated: None,
                })
                .collect(),
            notes: None,
            updated: None,
        }
    }

    /// The folders of each addon of `contents`.
    fn split(contents: &Contents) -> Vec<Vec<usize>> {
        groups(contents)
            .into_iter()
            .map(|group| group.folders)
            .collect()
    }

    #[test]
    fn only_the_listed_archives_are_wanted_and_by_exact_name() {
        assert!(wanted("rising-gods", "LUI v3"));
        assert!(wanted(
            "github-undeadrogue-wow-3.3.5a-addons",
            "wow-3.3.5a-addons"
        ));

        assert!(!wanted("rising-gods", "lui v3"), "the name is exact");
        assert!(!wanted("rising-gods", "LUI v3 "), "the name is exact");
        assert!(
            !wanted("rising-gods", "wow-3.3.5a-addons"),
            "another source's"
        );
        assert!(!wanted("github-undeadrogue-wow-3.3.5a-addons", "LUI v3"));
        assert!(!wanted("Rising-Gods", "LUI v3"), "the id is exact");
        assert!(!wanted("rising-gods", "EveryQuest"), "not a compilation");
        assert!(!wanted("", ""));
    }

    #[test]
    fn the_archives_listed_use_the_ids_their_sources_give() {
        // Spelled out here, as the source's id is built from it.
        let repository = "UndeadRogue/wow-3.3.5a-addons";
        let id = github::Repository(repository).info().id;

        assert!(github::REPOSITORIES.contains(&repository));
        assert!(
            wanted(&id, "wow-3.3.5a-addons"),
            "{id} isn't in ARCHIVES: {ARCHIVES:?}"
        );
        assert!(
            wanted(&rising_gods::Collection.info().id, "LUI v3"),
            "Rising Gods isn't in ARCHIVES: {ARCHIVES:?}"
        );
    }

    #[test]
    fn plugins_for_an_addon_from_elsewhere_are_addons_of_their_own() {
        // UndeadRogue/wow-3.3.5a-addons: ElvUI isn't in it.
        let contents = archive(&[
            ("DynamicFarclip", &[], None),
            ("ElvUI_AuctionatorTabs", &["ElvUI"], None),
            ("ElvUI_InlineAuraBridge", &["ElvUI", "InlineAura"], None),
            ("ElvUI_MultiBot", &["ElvUI", "MultiBot"], None),
            ("ElvUI_QuestieTracker", &["ElvUI"], None),
            ("InlineAura", &[], None),
            ("InlineAura_Config", &["InlineAura"], None),
            ("MouselookBinder", &[], None),
            ("MouselookBinder_ElvUI", &["ElvUI", "MouselookBinder"], None),
        ]);

        assert_eq!(
            split(&contents),
            [
                vec![0],
                vec![1],
                vec![2],
                vec![3],
                vec![4],
                vec![5, 6],
                vec![7],
                vec![8]
            ]
        );
        assert_eq!(groups(&contents)[5].anchor, 5, "InlineAura names it");
    }

    #[test]
    fn modules_requiring_each_other_stay_with_the_addon_they_all_require() {
        let contents = archive(&[
            ("Forte_Core", &[], None),
            ("Forte_Casting", &["Forte_Core"], None),
            ("Forte_Timer", &["Forte_Core", "Forte_Casting"], None),
            ("Forte_Mage", &["Forte_Core"], None),
        ]);

        assert_eq!(split(&contents), [vec![0, 1, 2, 3]]);
        assert_eq!(groups(&contents)[0].anchor, 0);
    }

    #[test]
    fn a_module_inside_an_addon_stays_with_it_and_names_are_matched_in_any_case() {
        let contents = archive(&[
            ("Foo", &[], None),
            ("Bar", &[], None),
            ("Foo_Options", &[], Some("Foo")),
            ("Bar_Plugin", &["bar"], None),
        ]);

        assert_eq!(split(&contents), [vec![0, 2], vec![1, 3]]);
    }

    #[test]
    fn what_the_game_ships_is_not_a_requirement() {
        let contents = archive(&[
            ("A", &["blizzard_raidui"], None),
            ("B", &["BLIZZARD_Calendar", "a"], None),
            ("C", &["Blizzard"], None),
        ]);

        assert_eq!(
            split(&contents),
            [vec![0, 1], vec![2]],
            "B requires only A; C requires something the archive lacks"
        );
    }

    #[test]
    fn a_bridge_between_two_addons_of_the_archive_joins_neither() {
        let contents = archive(&[
            ("X", &[], None),
            ("Y", &[], None),
            ("X_Y", &["X", "Y"], None),
        ]);

        assert_eq!(split(&contents), [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn folders_requiring_each_other_do_not_send_it_in_circles() {
        let contents = archive(&[("A", &["B"], None), ("B", &["A"], None)]);

        let mut folders: Vec<usize> = split(&contents).into_iter().flatten().collect();
        folders.sort_unstable();

        assert_eq!(folders, [0, 1], "each folder is in exactly one addon");
    }

    #[test]
    fn an_archive_is_one_addon_when_there_is_nothing_to_tell_them_apart_by() {
        let mut contents = archive(&[("A", &[], None), ("B", &[], None), ("C", &["B"], None)]);
        assert_eq!(split(&contents), [vec![0], vec![1, 2]]);

        contents.parts.clear();
        assert_eq!(split(&contents), [vec![0, 1, 2]], "parts not recorded");

        let single = archive(&[("A", &[], None)]);
        assert_eq!(split(&single), [vec![0]], "a single folder");
    }
}
