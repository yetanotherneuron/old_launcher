//! What the launcher lists for a client: every catalog entry, installed or
//! not, and the addons installed without the catalog.

use std::cmp::Reverse;

use crate::Snapshot;
use crate::catalog::{Catalog, Entry};
use crate::search::Query;
use crate::snapshot::Group;

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

    /// When `item` was last updated, for catalog entries that know.
    pub fn updated(&self, item: Item<'a>) -> Option<&'a str> {
        match item {
            Item::Entry { entry, .. } => entry.downloads.first()?.updated.as_deref(),
            Item::Manual(_) => None,
        }
    }

    /// Whether the catalog offers another version of the installed `item`:
    /// one of the folders its preferred download installs declares a `.toc`
    /// version other than the installed folder's. Folders without a version
    /// on either side say nothing.
    pub fn update_available(&self, item: Item<'a>) -> bool {
        let Item::Entry {
            entry,
            installed: Some(index),
        } = item
        else {
            return false;
        };
        let group = &self.groups[index];

        entry.folders().iter().any(|offered| {
            let installed = group
                .addons
                .iter()
                .find(|addon| addon.name.eq_ignore_ascii_case(&offered.name))
                .and_then(|addon| addon.toc.as_ref()?.version.as_deref());

            match (offered.version.as_deref(), installed) {
                (Some(offered), Some(installed)) => offered != installed,
                _ => false,
            }
        })
    }

    /// Whether `item` is installed but the client cannot load it properly.
    pub fn needs_attention(&self, item: Item<'a>) -> bool {
        let Some(group) = self.installed(item) else {
            return false;
        };

        group.addons.iter().any(|addon| {
            addon.toc.is_none()
                || self
                    .snapshot
                    .is_some_and(|snapshot| !snapshot.missing_dependencies(addon).is_empty())
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Download, Folder};
    use crate::{Addon, Toc};

    fn entry(name: &str, source: &str, updated: Option<&str>, folders: &[&str]) -> Entry {
        Entry {
            name: name.into(),
            description: format!("{name} does things"),
            image: None,
            downloads: vec![Download {
                source: source.into(),
                name: name.into(),
                url: format!("https://example.com/{name}.zip"),
                updated: updated.map(str::to_owned),
                page: None,
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
        }
    }

    /// Skada installed from the catalog, `Retired` installed from source `a`
    /// before the catalog dropped it, `Plugin` installed by hand and needing
    /// `Ace3`, and a folder the client can't load.
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
