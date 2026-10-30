//! Addon folders under `Interface/AddOns`, and the addons that come inside
//! the game's archives instead.
//!
//! The game finds an addon in the archives it loads as it does on disk: as
//! `Interface\AddOns\<name>\<name>.toc`. Blizzard's own come that way, and
//! so may a mod's. Their folders on disk, when they have any, hold only what
//! the archive doesn't, like Blizzard's `<name>.pub`: they belong to that
//! addon, not to the player.

use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use crate::error::io_at;
use crate::mpq::key;
use crate::patches::Graph;
use crate::{Error, Marker, Toc, marker};

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
    /// The files that changed since the launcher installed it, by path
    /// within the folder (see [`marker::changed`]): none when it is as
    /// installed, or installed by hand.
    pub changed: Vec<String>,
}

/// The addons inside the archives the game loads, as far as they are known:
/// what the last look at the client's patches found (see
/// [`patches`](crate::patches)), unknown until it is over.
///
/// It is asked by name, and answers from the archives' hash tables, which
/// key every file by its name hashed in any case and with either slash: an
/// archive without a list of its files answers too, and so does one whose
/// list spells its paths in lowercase. Archives set aside (`.backup`,
/// `.disabled`) aren't loaded, so they hold nothing here.
#[derive(Clone, Default)]
pub struct Bundled {
    /// The archives the game loads, put together; none while not known.
    graph: Option<Arc<Graph>>,
    /// Whether every archive the game loads could be read. When one
    /// couldn't, an addon the others don't hold may be in it: not known.
    whole: bool,
}

impl Bundled {
    /// The addons inside the archives that `graph` puts together, `whole`
    /// unless one the game loads couldn't be read.
    pub fn of(graph: Arc<Graph>, whole: bool) -> Self {
        Self {
            graph: Some(graph),
            whole,
        }
    }

    /// Archives known to hold no addon at all.
    pub fn none() -> Self {
        Self::of(Arc::new(Graph::build(&[], crate::Locale::EnUs)), true)
    }

    /// Whether what the archives hold is known yet.
    pub fn known(&self) -> bool {
        self.graph.is_some()
    }

    /// Whether the archives hold the addon called `name`, in any case: its
    /// `.toc`, not hidden by a delete marker above. `None` while that isn't
    /// known.
    pub fn hold(&self, name: &str) -> Option<bool> {
        let graph = self.graph.as_ref()?;
        let toc = key(&format!("Interface\\AddOns\\{name}\\{name}.toc"));

        match graph.provider(toc) {
            Some(_) => Some(true),
            None => self.whole.then_some(false),
        }
    }
}

impl fmt::Debug for Bundled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Bundled")
            .field("known", &self.known())
            .field("whole", &self.whole)
            .finish()
    }
}

/// Two are the same when they come from the same look, or are both unknown.
impl PartialEq for Bundled {
    fn eq(&self, other: &Self) -> bool {
        let same = match (&self.graph, &other.graph) {
            (Some(one), Some(other)) => Arc::ptr_eq(one, other),
            (None, None) => true,
            _ => false,
        };

        same && self.whole == other.whole
    }
}

impl Eq for Bundled {}

impl Addon {
    /// Reads every addon folder in `directory`, sorted by name.
    ///
    /// Dot-folders are skipped (the launcher's own staging, for one); a
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
            if !path.is_dir() || name.starts_with('.') {
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

            let marker = Marker::read(&path);
            let changed = if marker.is_some() {
                marker::changed(&path)
            } else {
                Vec::new()
            };

            addons.push(Self {
                marker,
                changed,
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
    use crate::mpq::writer::Writer;

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
        assert_eq!(scanned[0].changed, Vec::<String>::new());

        // Changed by hand since: the scan says which files.
        fs::write(acp.join("acp.toc"), "## Title: ACP\n## Version: dev\n").unwrap();
        let scanned = Addon::scan(addons.path()).unwrap();
        assert_eq!(scanned[0].changed, ["acp.toc"]);
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
    fn hidden_folders_are_skipped_and_every_other_is_read() {
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

        // The name says nothing: whether the archives hold it does.
        assert_eq!(names, ["alpha", "Blizzard_AuctionUI", "Zeta"]);
    }

    #[test]
    fn the_archives_answer_for_their_addons_in_any_case_with_or_without_a_list() {
        let toc = |name: &str| format!("Interface\\AddOns\\{name}\\{name}.toc");
        let bundled = crate::testing::bundled(&[
            // As the HD client spells its paths, and without a list of its
            // files.
            (
                "Data/patch-y.mpq",
                &Writer::new()
                    .file(&toc("!pyandre").to_lowercase(), b"x")
                    .delete_marker(&toc("Blizzard_RaidUI")),
            ),
            (
                "Data/enUS/locale-enUS.MPQ",
                &crate::testing::archive(&[
                    &toc("Blizzard_CombatText"),
                    &toc("Blizzard_RaidUI"),
                    "Interface\\AddOns\\Blizzard_Lonely\\Blizzard_Lonely.lua",
                ]),
            ),
        ]);

        assert_eq!(bundled.hold("!PYAndre"), Some(true));
        assert_eq!(bundled.hold("blizzard_combattext"), Some(true));
        assert_eq!(
            bundled.hold("Blizzard_HelloThisIsSomethingElse"),
            Some(false)
        );
        // Hidden by the delete marker of an archive that loads later.
        assert_eq!(bundled.hold("Blizzard_RaidUI"), Some(false));
        // Files of an addon's folder without its `.toc` are no addon.
        assert_eq!(bundled.hold("Blizzard_Lonely"), Some(false));

        assert_eq!(Bundled::default().hold("Blizzard_CombatText"), None);
        assert!(!Bundled::default().known());
        assert_eq!(Bundled::none().hold("Blizzard_CombatText"), Some(false));
    }

    #[test]
    fn an_addon_none_of_the_archives_read_holds_isnt_known_when_one_couldnt_be() {
        let toc = "Interface\\AddOns\\KPack\\KPack.toc";
        let read =
            crate::testing::bundled(&[("Data/patch-k.mpq", &crate::testing::archive(&[toc]))]);
        let partial = Bundled::of(read.graph.unwrap(), false);

        assert_eq!(partial.hold("KPack"), Some(true));
        assert_eq!(partial.hold("Blizzard_CombatText"), None);
    }

    #[test]
    fn a_missing_addons_directory_has_no_addons() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(Addon::scan(&root.path().join("nope")).unwrap(), []);
    }
}
