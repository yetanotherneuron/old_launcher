//! The per-addon data files the client writes under `WTF/Account`.
//!
//! The client saves each addon's variables to `<Addon>.lua` (plus a `.bak`
//! copy), once per account in `WTF/Account/<ACCOUNT>/SavedVariables/` and once
//! per character in `WTF/Account/<ACCOUNT>/<Realm>/<Character>/SavedVariables/`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Every addon data file in an installation, grouped by addon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedVariables {
    /// Keyed by lowercased addon name; the client treats names
    /// case-insensitively.
    files: BTreeMap<String, Vec<PathBuf>>,
}

/// Whose settings a data file holds, as told by where the client keeps it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// `WTF/Account/<account>/SavedVariables`: shared by every character of
    /// the account.
    Account(String),
    /// `WTF/Account/<account>/<realm>/<character>/SavedVariables`: one
    /// character's own settings.
    Character {
        /// The realm the character lives on.
        realm: String,
        /// The character's name.
        name: String,
    },
}

impl Scope {
    /// The scope of the data file at `path`, when it sits where the client
    /// writes data files.
    pub fn of(path: &Path) -> Option<Self> {
        let mut folders = path
            .parent()?
            .components()
            .rev()
            .map(|component| component.as_os_str().to_string_lossy().into_owned());

        if !folders.next()?.eq_ignore_ascii_case("SavedVariables") {
            return None;
        }

        let owner = folders.next()?;
        let above = folders.next()?;

        if above.eq_ignore_ascii_case("Account") {
            return Some(Self::Account(owner));
        }

        let _account = folders.next()?;

        folders
            .next()?
            .eq_ignore_ascii_case("Account")
            .then_some(Self::Character {
                realm: above,
                name: owner,
            })
    }
}

impl SavedVariables {
    /// Indexes the SavedVariables files under `wtf` (the `WTF` directory).
    ///
    /// Unreadable directories are skipped: a partial index is more useful than
    /// none, and nothing is deleted without being listed first.
    pub fn scan(wtf: &Path) -> Self {
        let mut files: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();

        let accounts = subdirectories(wtf)
            .into_iter()
            .filter(|folder| is_named(folder, "Account"))
            .flat_map(|folder| subdirectories(&folder));

        for account in accounts {
            let mut folders = Vec::new();

            for child in subdirectories(&account) {
                if is_named(&child, "SavedVariables") {
                    folders.push(child);
                    continue;
                }
                // Anything else at this level is a realm.
                for character in subdirectories(&child) {
                    folders.extend(
                        subdirectories(&character)
                            .into_iter()
                            .filter(|folder| is_named(folder, "SavedVariables")),
                    );
                }
            }

            for folder in folders {
                let Ok(entries) = fs::read_dir(&folder) else {
                    continue;
                };

                for entry in entries.filter_map(Result::ok) {
                    let file_name = entry.file_name().to_string_lossy().into_owned();
                    let Some(addon) = addon_name(&file_name).map(str::to_lowercase) else {
                        continue;
                    };

                    if !addon.starts_with("blizzard_") {
                        files.entry(addon).or_default().push(entry.path());
                    }
                }
            }
        }

        for paths in files.values_mut() {
            paths.sort();
        }

        Self { files }
    }

    /// The data files of the addon named `addon`.
    pub fn of(&self, addon: &str) -> &[PathBuf] {
        self.files
            .get(&addon.to_lowercase())
            .map_or(&[], Vec::as_slice)
    }

    /// Every addon with data files, with its name as spelled on disk.
    pub fn iter(&self) -> impl Iterator<Item = (String, &[PathBuf])> {
        self.files.iter().map(|(key, paths)| {
            let name = paths
                .iter()
                .filter_map(|path| path.file_name())
                .find_map(|file_name| addon_name(&file_name.to_string_lossy()).map(str::to_owned))
                .unwrap_or_else(|| key.clone());

            (name, paths.as_slice())
        })
    }
}

/// The addon a data file belongs to: its name without `.lua` or `.lua.bak`.
fn addon_name(file_name: &str) -> Option<&str> {
    let lowercase = file_name.to_ascii_lowercase();
    let suffix = [".lua.bak", ".lua"]
        .into_iter()
        .find(|suffix| lowercase.ends_with(suffix))?;

    Some(&file_name[..file_name.len() - suffix.len()]).filter(|name| !name.is_empty())
}

fn subdirectories(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };

    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

fn is_named(path: &Path, name: &str) -> bool {
    path.file_name()
        .is_some_and(|file_name| file_name.to_string_lossy().eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wtf(files: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();

        for file in files {
            let path = root.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }

        root
    }

    #[test]
    fn account_and_character_files_are_grouped_by_addon() {
        let root = wtf(&[
            "Account/ME/SavedVariables/Recount.lua",
            "Account/ME/SavedVariables/Recount.lua.bak",
            "Account/ME/Icecrown/Arthas/SavedVariables/recount.lua",
            "Account/ME/Icecrown/Arthas/SavedVariables/Omen.lua",
            "Account/ME/SavedVariables.lua",
        ]);

        let saved = SavedVariables::scan(root.path());

        assert_eq!(saved.of("Recount").len(), 3);
        assert_eq!(saved.of("omen").len(), 1);
        assert_eq!(saved.of("SavedVariables"), &[] as &[PathBuf]);
    }

    #[test]
    fn blizzard_data_is_never_listed() {
        let root = wtf(&["Account/ME/SavedVariables/Blizzard_CombatLog.lua"]);

        assert_eq!(SavedVariables::scan(root.path()).iter().count(), 0);
    }

    #[test]
    fn iteration_reports_names_as_spelled_on_disk() {
        let root = wtf(&["Account/ME/SavedVariables/DBM-Core.lua"]);
        let saved = SavedVariables::scan(root.path());

        let names: Vec<String> = saved.iter().map(|(name, _)| name).collect();

        assert_eq!(names, ["DBM-Core"]);
    }

    #[test]
    fn scopes_come_from_where_files_live() {
        assert_eq!(
            Scope::of(Path::new("/wow/WTF/Account/ME/SavedVariables/Skada.lua")),
            Some(Scope::Account("ME".into()))
        );
        assert_eq!(
            Scope::of(Path::new(
                "/wow/WTF/account/ME/Icecrown/Arthas/savedvariables/Skada.lua.bak"
            )),
            Some(Scope::Character {
                realm: "Icecrown".into(),
                name: "Arthas".into()
            })
        );
        assert_eq!(
            Scope::of(Path::new("/wow/Interface/AddOns/Skada/Skada.lua")),
            None
        );
    }

    #[test]
    fn dotted_addon_names_keep_their_dots() {
        assert_eq!(addon_name("Foo.Bar.lua"), Some("Foo.Bar"));
        assert_eq!(addon_name("Foo.LUA.BAK"), Some("Foo"));
        assert_eq!(addon_name("notes.txt"), None);
    }

    #[test]
    fn a_client_without_wtf_has_no_data() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(
            SavedVariables::scan(&root.path().join("WTF")),
            SavedVariables::default()
        );
    }
}
