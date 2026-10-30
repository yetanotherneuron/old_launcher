//! What the launcher lists for a client: every catalog entry, installed or
//! not, and the addons installed without the catalog.

use std::cmp::{Ordering, Reverse};

use crate::Snapshot;
use crate::catalog::{Catalog, Download, Entry};
use crate::search::Query;
use crate::snapshot::Group;
use crate::toc;

/// The addons listed for a client.
#[derive(Debug, Clone)]
pub struct Library<'a> {
    /// Every group of installed folders.
    pub groups: Vec<Group<'a>>,
    /// Every catalog entry, then every group of installed folders no entry
    /// claims.
    pub items: Vec<Item<'a>>,
    snapshot: Option<&'a Snapshot>,
}

/// One addon in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item<'a> {
    /// A catalog entry, with the index in [`Library::groups`] of the folders
    /// installed from it.
    Entry {
        /// The catalog entry.
        entry: &'a Entry,
        /// Its installed folders, if it is installed.
        installed: Option<usize>,
    },
    /// Installed folders no catalog entry offers, by index in
    /// [`Library::groups`]: installed without the launcher, or from an entry
    /// the catalog no longer lists.
    Manual(usize),
}

/// The installed version of a folder against the catalog's, when they
/// differ; see [`Library::update`] and [`Library::ahead`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Versions<'a> {
    /// The installed folder, as named on disk.
    pub folder: &'a str,
    /// The version installed.
    pub installed: &'a str,
    /// The version the catalog offers.
    pub offered: &'a str,
}

/// Which items to show, and in what order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Which items, by whether and how they are installed.
    pub status: Status,
    /// Which items, by where they come from.
    pub origin: Origin,
    /// The order to show them in.
    pub order: Order,
}

/// A filter on whether and how items are installed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Status {
    /// Everything.
    #[default]
    All,
    /// Installed addons.
    Installed,
    /// Installed addons the catalog has another version of; see
    /// [`Library::update_available`].
    Updatable,
    /// Catalog entries that are not installed.
    NotInstalled,
    /// Installed addons the client cannot load properly: a folder without
    /// its `.toc`, or a required dependency missing.
    Attention,
    /// No addons at all: the saved data removed addons left behind.
    LeftoverData,
}

/// A filter on where items come from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Origin {
    /// Anywhere.
    #[default]
    Any,
    /// Offered by the catalog source with this id, or installed from it.
    Source(String),
    /// Installed without the launcher.
    Manual,
}

/// The order items are shown in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Order {
    /// Alphabetical.
    #[default]
    Name,
    /// Most recently updated first; addons without a date last.
    Updated,
}

impl Status {
    /// Every status filter, in the order to offer them.
    pub const ALL: [Self; 6] = [
        Self::All,
        Self::Installed,
        Self::Updatable,
        Self::NotInstalled,
        Self::Attention,
        Self::LeftoverData,
    ];
}

impl Order {
    /// Every order, in the order to offer them.
    pub const ALL: [Self; 2] = [Self::Name, Self::Updated];
}

impl<'a> Library<'a> {
    /// Lists `catalog` against what `snapshot` says is installed (nothing, for
    /// no snapshot).
    pub fn new(catalog: &'a Catalog, snapshot: Option<&'a Snapshot>) -> Self {
        let groups = snapshot.map(Snapshot::groups).unwrap_or_default();
        let mut claimed = vec![false; groups.len()];
        let mut items = Vec::with_capacity(catalog.entries.len() + groups.len());

        for entry in &catalog.entries {
            let installed = groups
                .iter()
                .position(|group| group.addons.iter().any(|addon| entry.installed(addon)));

            if let Some(index) = installed {
                claimed[index] = true;
            }
            items.push(Item::Entry { entry, installed });
        }

        items.extend(
            (0..groups.len())
                .filter(|index| !claimed[*index])
                .map(Item::Manual),
        );

        Self {
            groups,
            items,
            snapshot,
        }
    }

    /// The installed folders of `item`, if any.
    pub fn installed(&self, item: Item<'a>) -> Option<&Group<'a>> {
        match item {
            Item::Entry { installed, .. } => installed.map(|index| &self.groups[index]),
            Item::Manual(index) => Some(&self.groups[index]),
        }
    }

    /// The name `item` is listed under.
    pub fn name(&self, item: Item<'a>) -> &str {
        match item {
            Item::Entry { entry, .. } => &entry.name,
            Item::Manual(index) => self.groups[index].title(),
        }
    }

    /// When `item` was last updated, for catalog entries that know: the
    /// date of the download installed first.
    pub fn updated(&self, item: Item<'a>) -> Option<&'a str> {
        match item {
            Item::Entry { entry, .. } => entry.preferred()?.updated.as_deref(),
            Item::Manual(_) => None,
        }
    }

    /// Whether the catalog offers another version of the installed `item`;
    /// see [`Library::update`].
    pub fn update_available(&self, item: Item<'a>) -> bool {
        self.update(item).is_some()
    }

    /// The newer version the catalog offers for the installed `item`, if
    /// any: the first folder its preferred download installs whose `.toc`
    /// version is newer than the installed folder's. Versions that don't
    /// compare (see [`toc::compare_versions`]) go by the catalog's dates
    /// instead (see [`by_date`]), and when those can't tell either, a
    /// different version counts as newer. Folders without a version on
    /// either side say nothing.
    pub fn update(&self, item: Item<'a>) -> Option<Versions<'a>> {
        self.versions(item)
            .into_iter()
            .find_map(|(versions, ordering)| {
                matches!(ordering, Some(Ordering::Greater) | None).then_some(versions)
            })
    }

    /// The version installed for `item` that is newer than the catalog's,
    /// if any: the first folder whose installed `.toc` version compares
    /// newer than the one the preferred download installs, or, for versions
    /// that don't compare, that the catalog dates later (see [`by_date`]).
    pub fn ahead(&self, item: Item<'a>) -> Option<Versions<'a>> {
        self.versions(item)
            .into_iter()
            .find_map(|(versions, ordering)| (ordering == Some(Ordering::Less)).then_some(versions))
    }

    /// The folders of the installed `item` whose `.toc` version differs from
    /// the one its preferred download installs, in the catalog's order, each
    /// with how the catalog's compares with the installed one.
    fn versions(&self, item: Item<'a>) -> Vec<(Versions<'a>, Option<Ordering>)> {
        let Item::Entry {
            entry,
            installed: Some(index),
        } = item
        else {
            return Vec::new();
        };
        let group = &self.groups[index];
        let dated = by_date(entry, group);

        entry
            .folders()
            .iter()
            .filter_map(|offered| {
                let addon = group
                    .addons
                    .iter()
                    .copied()
                    .find(|addon| addon.name.eq_ignore_ascii_case(&offered.name))?;
                let installed = addon.toc.as_ref()?.version.as_deref()?;
                let offered = offered.version.as_deref()?;
                let ordering = toc::compare_versions(offered, installed).or(dated);

                (ordering != Some(Ordering::Equal)).then_some((
                    Versions {
                        folder: &addon.name,
                        installed,
                        offered,
                    },
                    ordering,
                ))
            })
            .collect()
    }

    /// Whether `item` is installed but the client cannot load it properly:
    /// a folder is [broken](Snapshot::broken), or a dependency missing. Not
    /// for what the game's archives may hold, until they are known.
    pub fn needs_attention(&self, item: Item<'a>) -> bool {
        let (Some(group), Some(snapshot)) = (self.installed(item), self.snapshot) else {
            return false;
        };

        group
            .addons
            .iter()
            .any(|addon| snapshot.broken(addon) || !snapshot.missing_dependencies(addon).is_empty())
    }

    /// The items `filter` and `query` let through, in the filter's order.
    /// Leftover data is not an item, so that status lets none through.
    pub fn shown(&self, query: &Query, filter: &Filter) -> Vec<Item<'a>> {
        let mut shown: Vec<Item<'a>> = self
            .items
            .iter()
            .copied()
            .filter(|&item| self.passes(item, query, filter))
            .collect();

        match filter.order {
            Order::Name => shown.sort_by_cached_key(|&item| self.name(item).to_lowercase()),
            Order::Updated => shown.sort_by_cached_key(|&item| {
                (Reverse(self.updated(item)), self.name(item).to_lowercase())
            }),
        }

        shown
    }

    /// How many items `filter` and `query` let through.
    pub fn count(&self, query: &Query, filter: &Filter) -> usize {
        self.items
            .iter()
            .filter(|&&item| self.passes(item, query, filter))
            .count()
    }

    /// How many items `filter` and `query` would let through under each
    /// status, in the order of [`Status::ALL`], in one pass. Leftover data
    /// is not an item, so its count is zero.
    pub fn counts(&self, query: &Query, filter: &Filter) -> [(Status, usize); 6] {
        let mut counts = Status::ALL.map(|status| (status, 0));

        for &item in &self.items {
            if !self.passes_origin(item, &filter.origin) || !query.matches(&self.searchable(item)) {
                continue;
            }

            for (status, count) in &mut counts {
                if self.passes_status(item, *status) {
                    *count += 1;
                }
            }
        }

        counts
    }

    /// Whether `filter` and `query` let `item` through.
    fn passes(&self, item: Item<'a>, query: &Query, filter: &Filter) -> bool {
        self.passes_status(item, filter.status)
            && self.passes_origin(item, &filter.origin)
            && query.matches(&self.searchable(item))
    }

    /// Whether the `status` filter lets `item` through.
    fn passes_status(&self, item: Item<'a>, status: Status) -> bool {
        let installed = self.installed(item).is_some();

        match status {
            Status::All => true,
            Status::Installed => installed,
            Status::Updatable => self.update_available(item),
            Status::NotInstalled => !installed,
            Status::Attention => self.needs_attention(item),
            Status::LeftoverData => false,
        }
    }

    /// Whether the `origin` filter lets `item` through. Folders the
    /// launcher installed from an entry the catalog no longer lists still
    /// carry their source.
    fn passes_origin(&self, item: Item<'a>, origin: &Origin) -> bool {
        match (origin, item) {
            (Origin::Any, _) => true,
            (Origin::Source(id), Item::Entry { entry, .. }) => entry
                .downloads
                .iter()
                .any(|download| download.source == *id),
            (Origin::Source(id), Item::Manual(index)) => self.groups[index]
                .marker
                .is_some_and(|marker| marker.source == *id),
            (Origin::Manual, Item::Manual(index)) => self.groups[index].marker.is_none(),
            (Origin::Manual, Item::Entry { .. }) => false,
        }
    }

    /// The text a search looks in: name, description and folder names.
    fn searchable(&self, item: Item<'a>) -> Vec<&str> {
        let mut fields = vec![self.name(item)];

        match item {
            Item::Entry { entry, .. } => {
                fields.push(&entry.description);
                fields.extend(entry.folders().iter().map(|folder| folder.name.as_str()));
            }
            Item::Manual(index) => {
                let group = &self.groups[index];
                fields.extend(
                    group
                        .addons
                        .iter()
                        .filter_map(|addon| addon.toc.as_ref()?.notes.as_deref()),
                );
                fields.extend(group.addons.iter().map(|addon| addon.name.as_str()));
            }
        }

        fields
    }
}

/// How the preferred download of `entry` compares with what `group` has
/// installed by the catalog's dates, for versions that don't compare: its
/// date against those of the downloads that hold exactly the installed
/// versions (see [`holds`]). Nothing when the catalog can't tell: no
/// download holds them, a date is missing, or the dates tie or disagree.
/// The files' own dates aren't asked: a copy made by hand dates them when
/// it was made, not when the addon was.
fn by_date(entry: &Entry, group: &Group<'_>) -> Option<Ordering> {
    let offered = entry.preferred()?.updated.as_deref()?;
    let mut orderings = entry
        .downloads
        .iter()
        .filter(|download| holds(download, group))
        .map(|download| Some(offered.cmp(download.updated.as_deref()?)));

    let first = orderings.next()??;
    let agreed = orderings.all(|ordering| ordering == Some(first));

    (agreed && first != Ordering::Equal).then_some(first)
}

/// Whether `download` installs the versions `group` has: it gives a version
/// for at least one folder, and every folder it gives one for is installed
/// at that version.
fn holds(download: &Download, group: &Group<'_>) -> bool {
    let mut versioned = false;

    let installed = download.folders.iter().all(|folder| {
        let Some(version) = folder.version.as_deref() else {
            return true;
        };
        versioned = true;

        group
            .addons
            .iter()
            .find(|addon| addon.name.eq_ignore_ascii_case(&folder.name))
            .and_then(|addon| addon.toc.as_ref()?.version.as_deref())
            .is_some_and(|installed| {
                toc::compare_versions(version, installed) == Some(Ordering::Equal)
            })
    });

    installed && versioned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Download, Folder};
    use crate::{Addon, Bundled, Toc};

    fn entry(name: &str, source: &str, updated: Option<&str>, folders: &[&str]) -> Entry {
        Entry {
            name: name.into(),
            description: format!("{name} does things"),
            image: None,
            preferred: None,
            downloads: vec![Download {
                source: source.into(),
                name: name.into(),
                url: format!("https://example.com/{name}.zip"),
                updated: updated.map(str::to_owned),
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
            }],
        }
    }

    fn catalog() -> Catalog {
        Catalog {
            version: Catalog::VERSION,
            sources: Vec::new(),
            entries: vec![
                entry("Skada", "a", Some("2024-01-01"), &["Skada", "SkadaStorage"]),
                entry("ACP", "b", Some("2026-05-05"), &["ACP"]),
                entry("Omen", "a", None, &["Omen"]),
            ],
        }
    }

    fn addon(name: &str, marker: Option<&Entry>) -> Addon {
        Addon {
            name: name.into(),
            toc: Some(Toc::default()),
            marker: marker.map(|entry| entry.downloads[0].marker()),
            changed: Vec::new(),
        }
    }

    /// Skada installed from the catalog, `Retired` installed from source `a`
    /// before the catalog dropped it, `Plugin` installed by hand and needing
    /// `Ace3`, and a folder the client can't load; the game's archives hold
    /// no addon.
    fn snapshot(catalog: &Catalog) -> Snapshot {
        let skada = &catalog.entries[0];
        let retired = entry("Retired", "a", None, &["Retired"]);
        let mut plugin = addon("Plugin", None);
        plugin.toc.as_mut().unwrap().dependencies = vec!["Ace3".into()];

        Snapshot {
            addons: vec![
                addon("Skada", Some(skada)),
                addon("SkadaStorage", Some(skada)),
                addon("Retired", Some(&retired)),
                plugin,
                Addon {
                    toc: None,
                    ..addon("Broken-master", None)
                },
            ],
            bundled: Bundled::none(),
            ..Snapshot::default()
        }
    }

    fn names(library: &Library<'_>, items: &[Item<'_>]) -> Vec<String> {
        items
            .iter()
            .map(|&item| library.name(item).to_owned())
            .collect()
    }

    fn shown(library: &Library<'_>, filter: Filter) -> Vec<String> {
        names(library, &library.shown(&Query::new(""), &filter))
    }

    #[test]
    fn entries_pair_with_their_installed_folders_and_the_rest_is_manual() {
        let catalog = catalog();
        let snapshot = snapshot(&catalog);
        let library = Library::new(&catalog, Some(&snapshot));

        let skada = library.items[0];
        assert_eq!(
            library.installed(skada).unwrap().folders(),
            ["Skada", "SkadaStorage"]
        );
        assert!(library.installed(library.items[1]).is_none());
        assert_eq!(
            names(&library, &library.items[3..]),
            ["Broken-master", "Plugin", "Retired"]
        );
    }

    #[test]
    fn status_filters_split_installed_missing_and_troubled_addons() {
        let catalog = catalog();
        let snapshot = snapshot(&catalog);
        let library = Library::new(&catalog, Some(&snapshot));
        let status = |status| Filter {
            status,
            ..Filter::default()
        };

        assert_eq!(
            shown(&library, status(Status::Installed)),
            ["Broken-master", "Plugin", "Retired", "Skada"]
        );
        assert_eq!(
            shown(&library, status(Status::NotInstalled)),
            ["ACP", "Omen"]
        );
        assert_eq!(
            shown(&library, status(Status::Attention)),
            ["Broken-master", "Plugin"]
        );
        assert!(shown(&library, status(Status::LeftoverData)).is_empty());
        assert_eq!(
            library.count(&Query::new(""), &status(Status::Installed)),
            4
        );
    }

    #[test]
    fn nothing_needs_attention_for_what_the_archives_may_hold_until_they_are_known() {
        let catalog = catalog();
        let mut snapshot = snapshot(&catalog);
        snapshot.bundled = Bundled::default();
        let library = Library::new(&catalog, Some(&snapshot));
        let status = |status| Filter {
            status,
            ..Filter::default()
        };

        assert!(shown(&library, status(Status::Attention)).is_empty());
        assert_eq!(
            shown(&library, status(Status::Installed)),
            ["Plugin", "Retired", "Skada"],
            "a folder without its .toc may be part of one of theirs"
        );
        let counts = library.counts(&Query::new(""), &Filter::default());
        assert_eq!(counts[1], (Status::Installed, 3));
        assert_eq!(counts[4], (Status::Attention, 0));
    }

    #[test]
    fn origin_filters_pick_a_source_or_manual_installs() {
        let catalog = catalog();
        let snapshot = snapshot(&catalog);
        let library = Library::new(&catalog, Some(&snapshot));
        let origin = |origin| Filter {
            origin,
            ..Filter::default()
        };

        assert_eq!(
            shown(&library, origin(Origin::Source("a".into()))),
            ["Omen", "Retired", "Skada"],
            "what the source offers, and what was installed from it"
        );
        assert_eq!(
            shown(&library, origin(Origin::Manual)),
            ["Broken-master", "Plugin"]
        );
    }

    #[test]
    fn searches_look_in_folder_names_too() {
        let catalog = catalog();
        let library = Library::new(&catalog, None);

        let found = library.shown(&Query::new("skadastorage"), &Filter::default());

        assert_eq!(names(&library, &found), ["Skada"]);
    }

    #[test]
    fn an_update_is_available_when_the_catalog_version_differs() {
        let mut catalog = catalog();
        catalog.entries[0].downloads[0].folders[0].version = Some("2.0".into());
        catalog.entries[1].downloads[0].folders[0].version = Some("1.0".into());
        let mut snapshot = snapshot(&catalog);
        // Skada is at 1.0 on disk, the catalog has 2.0; ACP is not installed;
        // Omen's versions are unknown on both sides.
        snapshot.addons[0].toc.as_mut().unwrap().version = Some("1.0".into());
        let library = Library::new(&catalog, Some(&snapshot));

        assert_eq!(
            shown(
                &library,
                Filter {
                    status: Status::Updatable,
                    ..Filter::default()
                }
            ),
            ["Skada"]
        );
        let counts = library.counts(&Query::new(""), &Filter::default());
        assert_eq!(counts[0], (Status::All, 6));
        assert_eq!(counts[2], (Status::Updatable, 1));
        assert_eq!(counts[5], (Status::LeftoverData, 0));
        assert_eq!(
            library.update(library.items[0]),
            Some(Versions {
                folder: "Skada",
                installed: "1.0",
                offered: "2.0",
            })
        );
        assert_eq!(library.update(library.items[1]), None, "not installed");
    }

    #[test]
    fn an_installed_version_newer_than_the_catalogs_is_ahead_not_behind() {
        let mut catalog = catalog();
        catalog.entries[0].downloads[0].folders[0].version = Some("1.0".into());
        let mut snapshot = snapshot(&catalog);
        // What the catalog offers as an update, and what is installed ahead
        // of it.
        let compared = |snapshot: &Snapshot| -> (Option<String>, Option<String>) {
            let library = Library::new(&catalog, Some(snapshot));
            let item = library.items[0];

            (
                library
                    .update(item)
                    .map(|versions| versions.offered.to_owned()),
                library
                    .ahead(item)
                    .map(|versions| versions.installed.to_owned()),
            )
        };

        // Ahead of the catalog: nothing to update to, and said so.
        snapshot.addons[0].toc.as_mut().unwrap().version = Some("2.0".into());
        assert_eq!(compared(&snapshot), (None, Some("2.0".into())));

        // A version of another shape can't be told apart: offered.
        snapshot.addons[0].toc.as_mut().unwrap().version = Some("r5".into());
        assert_eq!(compared(&snapshot), (Some("1.0".into()), None));

        // The same: neither.
        snapshot.addons[0].toc.as_mut().unwrap().version = Some("1.0".into());
        assert_eq!(compared(&snapshot), (None, None));
    }

    /// SellJunk, with CurseForge's r117 first, dated `curseforge`, and
    /// another source's 1.4.2, dated `other`.
    fn sell_junk(curseforge: Option<&str>, other: Option<&str>) -> Catalog {
        let mut sell_junk = entry("SellJunk", "curseforge", curseforge, &["SellJunk"]);
        sell_junk.downloads[0].folders[0].version = Some("r117".into());
        let mut semver = entry("SellJunk", "other", other, &["SellJunk"])
            .downloads
            .remove(0);
        semver.folders[0].version = Some("1.4.2".into());
        sell_junk.downloads.push(semver);

        Catalog {
            version: Catalog::VERSION,
            sources: Vec::new(),
            entries: vec![sell_junk],
        }
    }

    /// What `catalog` offers as an update for its first entry installed at
    /// `version` from its second download, and what is installed ahead of
    /// it.
    fn compared(catalog: &Catalog, version: &str) -> (Option<String>, Option<String>) {
        let mut installed = addon("SellJunk", None);
        installed.marker = Some(catalog.entries[0].downloads[1].marker());
        installed.toc.as_mut().unwrap().version = Some(version.into());
        let snapshot = Snapshot {
            addons: vec![installed],
            ..Snapshot::default()
        };
        let library = Library::new(catalog, Some(&snapshot));
        let item = library.items[0];

        (
            library
                .update(item)
                .map(|versions| versions.offered.to_owned()),
            library
                .ahead(item)
                .map(|versions| versions.installed.to_owned()),
        )
    }

    #[test]
    fn versions_that_dont_compare_go_by_the_catalogs_dates() {
        // r117 is from 2010, the 1.4.2 installed from 2023: ahead.
        let catalog = sell_junk(Some("2010-09-02"), Some("2023-05-01"));
        assert_eq!(compared(&catalog, "1.4.2"), (None, Some("1.4.2".into())));

        // An r117 newer than it: an update.
        let catalog = sell_junk(Some("2024-01-01"), Some("2023-05-01"));
        assert_eq!(compared(&catalog, "1.4.2"), (Some("r117".into()), None));
    }

    #[test]
    fn when_the_dates_cant_tell_a_different_version_is_offered() {
        let offered = (Some("r117".to_owned()), None);

        // Installed at a version no download holds, so the catalog has no
        // date for it.
        let catalog = sell_junk(Some("2010-09-02"), Some("2023-05-01"));
        assert_eq!(compared(&catalog, "1.5.0"), offered, "not listed");

        assert_eq!(
            compared(&sell_junk(Some("2010-09-02"), None), "1.4.2"),
            offered,
            "the installed version undated"
        );
        assert_eq!(
            compared(&sell_junk(None, Some("2023-05-01")), "1.4.2"),
            offered,
            "the offered one undated"
        );
        assert_eq!(
            compared(&sell_junk(Some("2023-05-01"), Some("2023-05-01")), "1.4.2"),
            offered,
            "the same day"
        );

        // Two downloads hold 1.4.2, one dated before r117 and one after.
        let mut catalog = sell_junk(Some("2020-01-01"), Some("2023-05-01"));
        let mut older = catalog.entries[0].downloads[1].clone();
        older.source = "third".into();
        older.updated = Some("2015-01-01".into());
        catalog.entries[0].downloads.push(older);
        assert_eq!(compared(&catalog, "1.4.2"), offered, "dates that disagree");
    }

    /// ACP as the catalog has it: Rising Gods' 3.3.4 first, the newest,
    /// then locus313's 3.2.0.118, which is installed. Preferring locus313
    /// makes the installed copy current.
    #[test]
    fn a_preferred_source_is_what_updates_are_measured_against() {
        use std::collections::BTreeMap;

        let mut acp = entry("ACP", "rising-gods", Some("2010-08-02"), &["ACP"]);
        acp.downloads[0].folders[0].version = Some("3.3.4".into());
        let mut locus = entry("ACP", "locus313", Some("2026-09-27"), &["ACP"])
            .downloads
            .remove(0);
        locus.folders[0].version = Some("3.2.0.118".into());
        acp.downloads.push(locus);
        let mut catalog = Catalog {
            version: Catalog::VERSION,
            sources: Vec::new(),
            entries: vec![acp],
        };
        let mut installed = addon("ACP", None);
        installed.marker = Some(catalog.entries[0].downloads[1].marker());
        installed.toc.as_mut().unwrap().version = Some("3.2.0.118".into());
        let snapshot = Snapshot {
            addons: vec![installed],
            ..Snapshot::default()
        };

        let library = Library::new(&catalog, Some(&snapshot));
        let item = library.items[0];
        assert_eq!(
            library.update(item).map(|versions| versions.offered),
            Some("3.3.4")
        );
        assert_eq!(library.updated(item), Some("2010-08-02"));

        catalog.prefer(&BTreeMap::from([("acp".to_owned(), "locus313".to_owned())]));
        let library = Library::new(&catalog, Some(&snapshot));
        let item = library.items[0];
        assert_eq!(library.update(item), None);
        assert_eq!(library.ahead(item), None);
        assert_eq!(library.updated(item), Some("2026-09-27"));
    }

    #[test]
    fn an_update_names_the_first_folder_whose_version_differs() {
        let mut catalog = catalog();
        catalog.entries[0].downloads[0].folders[0].version = Some("1.0".into());
        catalog.entries[0].downloads[0].folders[1].version = Some("1.1".into());
        let mut snapshot = snapshot(&catalog);
        // Skada itself is at the catalog's version; SkadaStorage is behind,
        // and spelled differently on disk.
        snapshot.addons[0].toc.as_mut().unwrap().version = Some("1.0".into());
        snapshot.addons[1].name = "skadastorage".into();
        snapshot.addons[1].toc.as_mut().unwrap().version = Some("1.0".into());
        let library = Library::new(&catalog, Some(&snapshot));

        assert_eq!(
            library.update(library.items[0]),
            Some(Versions {
                folder: "skadastorage",
                installed: "1.0",
                offered: "1.1",
            })
        );
    }

    #[test]
    fn recently_updated_comes_first_and_undated_last() {
        let catalog = catalog();
        let library = Library::new(&catalog, None);

        assert_eq!(
            shown(
                &library,
                Filter {
                    order: Order::Updated,
                    ..Filter::default()
                }
            ),
            ["ACP", "Skada", "Omen"]
        );
    }

    #[test]
    fn without_a_client_nothing_is_installed() {
        let catalog = catalog();
        let library = Library::new(&catalog, None);

        assert!(library.groups.is_empty());
        assert!(
            shown(
                &library,
                Filter {
                    status: Status::Installed,
                    ..Filter::default()
                }
            )
            .is_empty()
        );
    }
}
