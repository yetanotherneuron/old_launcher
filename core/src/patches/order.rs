//! The order the game loads a client's archives in, which decides what a
//! file is when several archives hold it: the one loaded last wins.
//!
//! This follows the loader of `Wow.exe` (build 12340) as it was read there.
//! The base archives have fixed places, 40 to 49. The patches are found by
//! listing `Data` for `patch-?.MPQ` and the language's folder for
//! `patch-<language>-?.MPQ`, where `?` is exactly one character of any kind
//! and names are compared in ASCII case-insensitively, whole. The paths
//! (`Data\…`) are sorted descending and case-insensitively, `patch.MPQ` and
//! `patch-<language>.MPQ` are added at the end, and the list is opened from
//! last to first, counting priorities up from 64 and skipping what doesn't
//! open. So a later letter beats an earlier one, a digit loses to `_` and
//! letters, and since `Data\enUS\…` sorts before `Data\patch…`, a language's
//! patches load *below* every `Data\patch-?` (for the languages whose folder
//! sorts before `patch`; the others load above).
//!
//! Only the language `WTF/Config.wtf` sets is loaded: the files of another
//! language's folder are never listed.
//!
//! Windows is the environment the design assumes: two paths that differ only
//! in case are one file there, so archives whose paths do are **case-twins**,
//! put in one [`Slot`] (the game leaves their order undefined, so the
//! launcher doesn't pick one) and reported by [`Order::twins`].
//!
//! Paths are as the folder spells them, from the game folder, with `/`
//! between folders: `Data/enUS/patch-enUS-M.MPQ`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::locale::Locale;
use crate::patches::BLIZZARD;

/// The priority of the first patch that opens.
pub const FIRST_PATCH: u32 = 64;

/// The base archives, with the priority each has: whether it is in the
/// language's folder, its name without extension (`{l}` standing for the
/// language), and the priority. The game's own table, in the retail layout.
const BASE: &[(bool, &str, u32)] = &[
    (false, "expansion", 49),
    (false, "lichking", 48),
    (false, "common", 47),
    (false, "common-2", 46),
    (true, "locale-{l}", 45),
    (true, "speech-{l}", 44),
    (true, "expansion-locale-{l}", 43),
    (true, "lichking-locale-{l}", 42),
    (true, "expansion-speech-{l}", 41),
    (true, "lichking-speech-{l}", 40),
];

/// Where an archive sits in the load order, without a number: the number
/// depends on which archives open.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Rank {
    /// A base archive, with its fixed priority.
    Base(u32),
    /// `patch-<language>.MPQ`, the first of the patches.
    Locale,
    /// `patch.MPQ`, right above it.
    Plain,
    /// A patch the enumerations found, by its path sorted as the game sorts
    /// it: lower case, with `\`. The bigger, the later it loads.
    Wild(Vec<u8>),
}

/// Where `path` (from the game folder, with either slash, in any case) sits
/// in the load order of a client that plays in `locale`, or `None` if the
/// game never loads an archive of that name.
pub(super) fn rank_of(path: &str, locale: Locale) -> Option<Rank> {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    let code = locale.code().to_ascii_lowercase();
    let (local, file) = match lower.split('/').collect::<Vec<_>>().as_slice() {
        ["data", file] => (false, (*file).to_owned()),
        ["data", folder, file] if *folder == code => (true, (*file).to_owned()),
        _ => return None,
    };
    let stem = file.strip_suffix(".mpq")?;

    for &(in_folder, name, priority) in BASE {
        if in_folder == local && name.replace("{l}", &code) == stem {
            return Some(Rank::Base(priority));
        }
    }

    if !local && stem == "patch" {
        return Some(Rank::Plain);
    }

    if local && stem == format!("patch-{code}") {
        return Some(Rank::Locale);
    }

    let prefix = if local {
        format!("patch-{code}-")
    } else {
        "patch-".to_owned()
    };
    let mut rest = stem.strip_prefix(&prefix)?.chars();

    // `?` is exactly one character.
    (rest.next().is_some() && rest.next().is_none())
        .then(|| Rank::Wild(lower.replace('/', "\\").into_bytes()))
}

/// Whether `path` (from the game folder, in any case) is one of the archives
/// Blizzard ships with the client, as [`BLIZZARD`] lists them, for a client
/// that plays in `locale`. Everything else is custom: the player's, a
/// torrent's or a mod's.
pub fn is_blizzard(path: &str, locale: Locale) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    let code = locale.code().to_ascii_lowercase();
    let (local, file) = match lower.split('/').collect::<Vec<_>>().as_slice() {
        ["data", file] => (false, (*file).to_owned()),
        ["data", folder, file] if *folder == code => (true, (*file).to_owned()),
        _ => return false,
    };
    let Some(stem) = file.strip_suffix(".mpq") else {
        return false;
    };

    BLIZZARD.iter().any(|name| {
        let in_folder = name.contains(crate::mods::LOCALE);
        let name = name
            .replace(crate::mods::LOCALE, &code)
            .to_ascii_lowercase();

        in_folder == local && name == stem
    })
}

/// An archive of the load order, or several that differ only in case.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Slot {
    /// Its priority: the higher, the more it wins. Base archives have 40 to
    /// 49, patches 64 and up.
    pub priority: u32,
    /// The path of each archive, in byte order. More than one means
    /// case-twins, which count as one archive.
    pub paths: Vec<String>,
}

/// Why an MPQ the folder holds isn't loaded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reason {
    /// It was set aside by a backup: `X.mpq.backup`, `X.mpq.backup-1`.
    Backup,
    /// It was switched off: `X.mpq.disabled`.
    Disabled,
    /// The game never lists a name like it, so it never looks.
    NotListed,
    /// It would load, but doesn't open, and why.
    Unreadable(String),
}

/// An MPQ the folder holds that the game doesn't load.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Unloaded {
    /// Its path.
    pub path: String,
    /// Why it isn't loaded.
    pub reason: Reason,
}

/// The archives of a client in the order the game loads them.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Order {
    /// The archives that load, the one that wins first.
    pub loaded: Vec<Slot>,
    /// The MPQs in `Data` and the language's folder that don't, by path.
    pub unloaded: Vec<Unloaded>,
}

impl Order {
    /// The groups of archives that differ only in case, which the game
    /// leaves in no defined order and Windows couldn't hold.
    pub fn twins(&self) -> Vec<&[String]> {
        self.loaded
            .iter()
            .filter(|slot| slot.paths.len() > 1)
            .map(|slot| slot.paths.as_slice())
            .collect()
    }
}

/// The archives of the client at `root` in the order the game loads them
/// when it plays in `locale`, opening each to see that it does.
pub fn order(root: &Path, locale: Locale) -> Order {
    order_with(root, locale, |path| {
        crate::mpq::Archive::open(path)
            .map(drop)
            .map_err(|error| error.to_string())
    })
}

/// [`order`], with `opens` telling whether an archive opens, and if not
/// why: the scan knows that already for the archives it has read before.
pub fn order_with(
    root: &Path,
    locale: Locale,
    mut opens: impl FnMut(&Path) -> Result<(), String>,
) -> Order {
    let mut slots: BTreeMap<Rank, Vec<(String, PathBuf)>> = BTreeMap::new();
    let mut unloaded = Vec::new();

    for (path, full) in candidates(root, locale) {
        match rank_of(&path, locale) {
            Some(rank) => slots.entry(rank).or_default().push((path, full)),
            None => {
                let name = path.rsplit('/').next().unwrap_or(&path);

                if let Some(reason) = why_not_listed(name) {
                    unloaded.push(Unloaded { path, reason });
                }
            }
        }
    }

    let mut loaded = Vec::new();
    let mut next = FIRST_PATCH;

    // Lowest first: the patches count up from the first in the order the
    // game opens them.
    for (rank, mut members) in slots {
        members.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut paths = Vec::new();

        for (path, full) in members {
            match opens(&full) {
                Ok(()) => paths.push(path),
                Err(reason) => unloaded.push(Unloaded {
                    path,
                    reason: Reason::Unreadable(reason),
                }),
            }
        }

        if paths.is_empty() {
            continue;
        }

        let priority = if let Rank::Base(priority) = rank {
            priority
        } else {
            next += 1;
            next - 1
        };
        loaded.push(Slot { priority, paths });
    }

    loaded.sort_by_key(|slot| std::cmp::Reverse(slot.priority));
    unloaded.sort_by(|a, b| a.path.cmp(&b.path));

    Order { loaded, unloaded }
}

/// Every file of the `Data` folder at `root` and of the folder of `locale`
/// in it, whatever their case, as (path, where). Folders that differ only in
/// case are all read: on Windows they would be one.
fn candidates(root: &Path, locale: Locale) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();

    for (data, data_path) in folders(root, "", "Data") {
        found.extend(files(&data_path, &data));

        for (language, language_path) in folders(&data_path, &data, locale.code()) {
            found.extend(files(&language_path, &language));
        }
    }

    found
}

/// The folders of `parent` (at `spelled`, in paths) called `name` in any
/// case, in byte order.
fn folders(parent: &Path, spelled: &str, name: &str) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = fs::read_dir(parent)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(name)
        })
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let own = entry.file_name().to_string_lossy().into_owned();
            let path = if spelled.is_empty() {
                own
            } else {
                format!("{spelled}/{own}")
            };

            (path, entry.path())
        })
        .collect();
    found.sort();

    found
}

/// The files of the folder at `folder` (spelled `spelled` in paths).
fn files(folder: &Path, spelled: &str) -> Vec<(String, PathBuf)> {
    fs::read_dir(folder)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();

            (format!("{spelled}/{name}"), entry.path())
        })
        .collect()
}

/// Why the game never loads the file called `name`, if it looks like an MPQ
/// at all.
fn why_not_listed(name: &str) -> Option<Reason> {
    let lower = name.to_ascii_lowercase();

    if !lower.contains("mpq") {
        return None;
    }

    match lower.split_once(".mpq.") {
        Some((_, rest)) if rest.starts_with("backup") => Some(Reason::Backup),
        Some((_, rest)) if rest.starts_with("disabled") => Some(Reason::Disabled),
        _ => Some(Reason::NotListed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpq::writer::Writer;
    use crate::patches::fixtures;
    use crate::testing::client;

    /// A tiny archive at `path` under `root`.
    fn put(root: &Path, path: &str) {
        fixtures::put(root, path, &fixtures::tiny());
    }

    fn paths(order: &Order) -> Vec<(u32, Vec<&str>)> {
        order
            .loaded
            .iter()
            .map(|slot| {
                (
                    slot.priority,
                    slot.paths.iter().map(String::as_str).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn patches_load_in_the_games_order() {
        let (root, _) = client(&[]);
        let root = root.path();

        for path in [
            "Data/common.MPQ",
            "Data/enUS/locale-enUS.MPQ",
            "Data/patch.MPQ",
            "Data/patch-2.MPQ",
            "Data/patch-_.MPQ",
            "Data/patch-A.MPQ",
            "Data/patch-b.mpq",
            "Data/enUS/patch-enUS.MPQ",
            "Data/enUS/patch-enUS-2.MPQ",
            "Data/enUS/patch-enUS-M.MPQ",
            // Not loaded.
            "Data/patch-F.mpq.backup",
            "Data/patch-F.mpq.backup-1",
            "Data/patch-G.mpq.disabled",
            "Data/patch-10.MPQ",
            "Data/patch-W.Ompq",
            "Data/deDE/patch-deDE-2.MPQ",
            "Data/base-enUS.MPQ",
        ] {
            put(root, path);
        }

        // One that doesn't open.
        fs::write(root.join("Data/patch-Z.MPQ"), b"not an archive").unwrap();
        fs::write(root.join("Data/readme.txt"), b"x").unwrap();

        let order = order(root, Locale::EnUs);

        assert_eq!(
            paths(&order),
            vec![
                (71, vec!["Data/patch-b.mpq"]),
                (70, vec!["Data/patch-A.MPQ"]),
                (69, vec!["Data/patch-_.MPQ"]),
                (68, vec!["Data/patch-2.MPQ"]),
                (67, vec!["Data/enUS/patch-enUS-M.MPQ"]),
                (66, vec!["Data/enUS/patch-enUS-2.MPQ"]),
                (65, vec!["Data/patch.MPQ"]),
                (64, vec!["Data/enUS/patch-enUS.MPQ"]),
                (47, vec!["Data/common.MPQ"]),
                (45, vec!["Data/enUS/locale-enUS.MPQ"]),
            ]
        );

        let unloaded: Vec<(&str, &Reason)> = order
            .unloaded
            .iter()
            .map(|archive| (archive.path.as_str(), &archive.reason))
            .collect();
        let broken = Reason::Unreadable(
            crate::mpq::Archive::open(&root.join("Data/patch-Z.MPQ"))
                .unwrap_err()
                .to_string(),
        );

        assert_eq!(
            unloaded,
            vec![
                ("Data/base-enUS.MPQ", &Reason::NotListed),
                ("Data/patch-10.MPQ", &Reason::NotListed),
                ("Data/patch-F.mpq.backup", &Reason::Backup),
                ("Data/patch-F.mpq.backup-1", &Reason::Backup),
                ("Data/patch-G.mpq.disabled", &Reason::Disabled),
                ("Data/patch-W.Ompq", &Reason::NotListed),
                ("Data/patch-Z.MPQ", &broken),
            ]
        );
        assert!(order.twins().is_empty());
    }

    #[test]
    fn a_languages_patches_load_above_the_others_when_its_folder_sorts_later() {
        let (root, _) = client(&[]);

        for path in [
            "Data/ruRU/patch-ruRU-3.MPQ",
            "Data/patch-4.MPQ",
            "Data/ruRU/locale-ruRU.MPQ",
        ] {
            put(root.path(), path);
        }

        let order = order(root.path(), Locale::RuRu);

        // `data\rur…` sorts after `data\patch…`.
        assert_eq!(
            paths(&order),
            vec![
                (65, vec!["Data/ruRU/patch-ruRU-3.MPQ"]),
                (64, vec!["Data/patch-4.MPQ"]),
                (45, vec!["Data/ruRU/locale-ruRU.MPQ"]),
            ]
        );
    }

    #[test]
    fn a_languages_folder_is_found_whatever_its_case_and_others_are_not_loaded() {
        let (root, _) = client(&[]);

        for path in [
            "data/eses/patch-esES-2.mpq",
            "data/enus/patch-enUS-2.mpq",
            "data/patch-a.mpq",
        ] {
            put(root.path(), path);
        }

        let order = order(root.path(), Locale::EsEs);

        assert_eq!(
            paths(&order),
            vec![
                (65, vec!["data/patch-a.mpq"]),
                (64, vec!["data/eses/patch-esES-2.mpq"]),
            ]
        );
        assert!(order.unloaded.is_empty());
    }

    #[test]
    fn two_names_differing_only_in_case_are_one_slot_and_reported() {
        let (root, _) = client(&[]);
        let data = root.path().join("Data");

        put(root.path(), "Data/patch-w.mpq");
        put(root.path(), "Data/patch-x.mpq");
        put(root.path(), "Data/patch-v.mpq");

        // A file system that ignores case can't hold the second.
        Writer::new()
            .file("y.txt", b"y")
            .write(&data.join("patch-W.mpq"));
        if fs::read_dir(&data).unwrap().count() < 4 {
            return;
        }

        let order = order(root.path(), Locale::EnUs);

        assert_eq!(
            paths(&order),
            vec![
                (66, vec!["Data/patch-x.mpq"]),
                (65, vec!["Data/patch-W.mpq", "Data/patch-w.mpq"]),
                (64, vec!["Data/patch-v.mpq"]),
            ]
        );
        assert_eq!(
            order.twins(),
            vec![["Data/patch-W.mpq".to_owned(), "Data/patch-w.mpq".to_owned()].as_slice()]
        );
    }

    #[test]
    fn blizzards_archives_are_told_in_any_case_and_only_where_the_game_looks() {
        let blizzard = |path| is_blizzard(path, Locale::EnUs);

        assert!(blizzard("Data/patch-3.MPQ"));
        assert!(blizzard("data/COMMON.mpq"));
        assert!(blizzard("Data/enUS/patch-enUS-2.MPQ"));
        assert!(blizzard("data\\enus\\expansion-locale-enUS.MPQ"));
        assert!(!blizzard("Data/patch-4.MPQ"));
        assert!(!blizzard("Data/patch-Y.mpq"));
        assert!(!blizzard("Data/enUS/patch-enUS-M.MPQ"));
        assert!(!blizzard("Data/enUS/common.MPQ"));
        assert!(!blizzard("Data/patch-enUS-2.MPQ"));
        assert!(!blizzard("Data/deDE/patch-deDE-2.MPQ"));
        assert!(!blizzard("Data/patch-3.mpq.backup"));
        assert!(is_blizzard("Data/deDE/patch-deDE-2.MPQ", Locale::DeDe));
    }
}
