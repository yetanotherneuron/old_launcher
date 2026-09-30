//! Moving a client's addons and their saved settings to another client,
//! through one zip file.
//!
//! The zip lists the addons (`addons.toml`) and holds their SavedVariables
//! files at the paths they have in the client folder. The addons themselves
//! are not in it: importing installs them from the catalog again.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;

use crate::catalog::{Catalog, Entry};
use crate::error::io_at;
use crate::installation::child;
use crate::{Error, Installation, Marker, Snapshot};

/// The list of addons inside an export.
const MANIFEST: &str = "addons.toml";

/// The list of addons in an export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The format version; see [`Manifest::VERSION`].
    pub version: u32,
    /// Every addon the client had.
    #[serde(default)]
    pub addons: Vec<Listed>,
}

/// An addon in an export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    /// What the user knows it as.
    pub title: String,
    /// Its folders in `Interface/AddOns`.
    pub folders: Vec<String>,
    /// The catalog entry the launcher installed it from; none when it was
    /// installed some other way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<Marker>,
}

/// How much an export holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Addons listed.
    pub addons: usize,
    /// SavedVariables files included.
    pub files: usize,
}

/// An export read back: its list of addons, and the data files in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The export file.
    pub path: PathBuf,
    /// Its list of addons.
    pub manifest: Manifest,
    /// Its SavedVariables files, relative to the client folder. Anything
    /// else in the zip is ignored.
    pub files: Vec<PathBuf>,
}

/// What importing an export into a client would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Catalog entries to install.
    pub install: Vec<Entry>,
    /// Addons the client already has, left as they are.
    pub present: Vec<Listed>,
    /// Addons installed from catalog entries this catalog doesn't offer.
    pub unavailable: Vec<Listed>,
    /// Addons installed by hand, for the user to install themselves.
    pub manual: Vec<Listed>,
    /// The export's data files that exist in the client already, and would
    /// be replaced; relative to the client folder.
    pub replaced: Vec<PathBuf>,
}

impl Manifest {
    /// The format version this crate reads and writes.
    pub const VERSION: u32 = 1;
}

/// Writes an export of `installation`, as `snapshot` saw it, to `path`: the
/// list of its addons, and their SavedVariables files.
///
/// The zip is written beside `path` and renamed over it, so a failed export
/// never leaves a truncated file behind.
pub fn export(
    installation: &Installation,
    snapshot: &Snapshot,
    path: &Path,
) -> Result<Summary, Error> {
    let groups = snapshot.groups();
    let manifest = Manifest {
        version: Manifest::VERSION,
        addons: groups
            .iter()
            .map(|group| Listed {
                title: group.title().to_owned(),
                folders: group.folders(),
                marker: group.marker.cloned(),
            })
            .collect(),
    };
    let files: Vec<PathBuf> = groups
        .iter()
        .flat_map(|group| snapshot.data_files(group))
        .collect();

    let partial = path.with_extension("zip.partial");
    let written = write(&partial, &manifest, installation.root(), &files);

    match written {
        Ok(files) => {
            fs::rename(&partial, path).map_err(io_at(path))?;

            Ok(Summary {
                addons: manifest.addons.len(),
                files,
            })
        }
        Err(error) => {
            // Nothing more to do if the partial file can't go either.
            let _ = fs::remove_file(&partial);
            Err(error)
        }
    }
}

/// Writes the zip of an export to `path`, with `files` stored relative to
/// `root`. Returns how many files went in.
fn write(path: &Path, manifest: &Manifest, root: &Path, files: &[PathBuf]) -> Result<usize, Error> {
    let list = toml::to_string_pretty(manifest).map_err(|error| Error::NotAnExport {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let mut zip = zip::ZipWriter::new(File::create(path).map_err(io_at(path))?);
    let options = SimpleFileOptions::default();

    zip.start_file(MANIFEST, options)?;
    zip.write_all(list.as_bytes()).map_err(io_at(path))?;

    let mut written = 0;

    for file in files {
        let Ok(relative) = file.strip_prefix(root) else {
            continue;
        };
        let name: Vec<String> = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();

        zip.start_file(name.join("/"), options)?;
        zip.write_all(&fs::read(file).map_err(io_at(file))?)
            .map_err(io_at(path))?;
        written += 1;
    }

    zip.finish()?;

    Ok(written)
}

impl Import {
    /// Reads the export at `path`.
    pub fn read(path: &Path) -> Result<Self, Error> {
        let mut zip = zip::ZipArchive::new(File::open(path).map_err(io_at(path))?)?;
        let not_an_export = |reason: String| Error::NotAnExport {
            path: path.to_path_buf(),
            reason,
        };

        let mut list = String::new();
        zip.by_name(MANIFEST)
            .map_err(|_| not_an_export(format!("it has no {MANIFEST}")))?
            .read_to_string(&mut list)
            .map_err(|error| not_an_export(error.to_string()))?;

        let manifest: Manifest =
            toml::from_str(&list).map_err(|error| not_an_export(error.to_string()))?;

        if manifest.version != Manifest::VERSION {
            return Err(Error::ExportVersion(manifest.version));
        }

        let mut files: Vec<PathBuf> = zip.file_names().filter_map(saved_variables).collect();
        files.sort();

        Ok(Self {
            path: path.to_path_buf(),
            manifest,
            files,
        })
    }

    /// What importing into `installation` would do, given the `catalog` and
    /// what `snapshot` says is installed.
    pub fn plan(
        &self,
        catalog: &Catalog,
        installation: &Installation,
        snapshot: &Snapshot,
    ) -> Plan {
        let installed: HashSet<String> = snapshot
            .addons
            .iter()
            .map(|addon| addon.name.to_lowercase())
            .collect();
        let mut plan = Plan::default();

        for listed in &self.manifest.addons {
            let present = listed
                .folders
                .iter()
                .all(|folder| installed.contains(&folder.to_lowercase()));

            if present {
                plan.present.push(listed.clone());
                continue;
            }

            let Some(marker) = &listed.marker else {
                plan.manual.push(listed.clone());
                continue;
            };

            // The entry the addon came from, or else one installing the same
            // folders, in case the catalog renamed it since.
            let entry = catalog
                .entries
                .iter()
                .find(|entry| {
                    entry
                        .downloads
                        .iter()
                        .any(|download| marker.is_for(&download.source, &download.name))
                })
                .or_else(|| {
                    catalog
                        .entries
                        .iter()
                        .find(|entry| same_folders(entry, &listed.folders))
                });

            match entry {
                Some(entry) if !plan.install.contains(entry) => plan.install.push(entry.clone()),
                Some(_) => {}
                None => plan.unavailable.push(listed.clone()),
            }
        }

        plan.replaced = self
            .files
            .iter()
            .filter(|file| resolve(installation.root(), file).exists())
            .cloned()
            .collect();

        plan
    }

    /// Writes the export's data files into `installation`, replacing those
    /// at the same paths, and returns how many were written.
    pub fn restore(&self, installation: &Installation) -> Result<usize, Error> {
        let mut zip = zip::ZipArchive::new(File::open(&self.path).map_err(io_at(&self.path))?)?;

        for file in &self.files {
            let name: Vec<String> = file
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect();

            let mut contents = Vec::new();
            zip.by_name(&name.join("/"))?
                .read_to_end(&mut contents)
                .map_err(io_at(&self.path))?;

            let destination = resolve(installation.root(), file);

            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(io_at(parent))?;
            }

            // Written beside and renamed over, so a failure never leaves a
            // truncated settings file for the game to read.
            let partial = destination.with_extension("partial");
            fs::write(&partial, &contents).map_err(io_at(&partial))?;
            fs::rename(&partial, &destination).map_err(io_at(&destination))?;
        }

        Ok(self.files.len())
    }
}

/// `name`, a zip entry, as a path relative to the client folder, when it is
/// a SavedVariables file where the client keeps them:
/// `WTF/Account/<account>/SavedVariables/<file>` or
/// `WTF/Account/<account>/<realm>/<character>/SavedVariables/<file>`.
fn saved_variables(name: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = name.split('/').collect();

    let plain = parts.iter().all(|part| {
        !part.is_empty() && *part != "." && *part != ".." && !part.contains(['\\', ':', '\0'])
    });
    let file = parts.last()?.to_ascii_lowercase();
    let data = file.ends_with(".lua") || file.ends_with(".lua.bak");
    let placed = match parts.as_slice() {
        [wtf, account, _, folder, _] | [wtf, account, _, _, _, folder, _] => {
            wtf.eq_ignore_ascii_case("WTF")
                && account.eq_ignore_ascii_case("Account")
                && folder.eq_ignore_ascii_case("SavedVariables")
        }
        _ => false,
    };

    (plain && data && placed).then(|| parts.iter().collect())
}

/// Where `relative` lands under `root`, following the spelling of the
/// folders that exist already.
fn resolve(root: &Path, relative: &Path) -> PathBuf {
    relative
        .components()
        .fold(root.to_path_buf(), |path, part| {
            child(&path, &part.as_os_str().to_string_lossy())
        })
}

/// Whether `entry` installs exactly `folders`, ignoring case.
fn same_folders(entry: &Entry, folders: &[String]) -> bool {
    let names = |folders: &mut dyn Iterator<Item = &str>| {
        let mut names: Vec<String> = folders.map(str::to_lowercase).collect();
        names.sort();
        names
    };

    names(&mut entry.folders().iter().map(|folder| folder.name.as_str()))
        == names(&mut folders.iter().map(String::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Download, Folder};

    /// A 3.3.5a client in a temporary directory, with `files` each holding
    /// its own path.
    fn client(files: &[&str]) -> (tempfile::TempDir, Installation) {
        let contents: Vec<String> = files.iter().map(|file| format!("-- {file}")).collect();
        let with_contents: Vec<(&str, &str)> = files
            .iter()
            .zip(&contents)
            .map(|(file, contents)| (*file, contents.as_str()))
            .collect();

        crate::testing::client_with(&with_contents)
    }

    fn entry(name: &str, source: &str, folders: &[&str]) -> Entry {
        Entry {
            name: name.into(),
            description: String::new(),
            image: None,
            preferred: None,
            downloads: vec![Download {
                source: source.into(),
                name: name.into(),
                url: format!("https://example.com/{name}.zip"),
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
            }],
        }
    }

    fn catalog() -> Catalog {
        Catalog {
            version: Catalog::VERSION,
            sources: Vec::new(),
            entries: vec![
                entry("Skada", "a", &["Skada", "SkadaStorage"]),
                entry("Omen", "a", &["Omen"]),
                entry("Recount", "b", &["Recount"]),
            ],
        }
    }

    /// A client with Skada and Omen from the catalog, a hand-installed
    /// addon, and settings for all of them plus a removed addon.
    fn source() -> (tempfile::TempDir, Installation) {
        let catalog = catalog();
        let (root, installation) = client(&[
            "Interface/AddOns/Skada/Skada.toc",
            "Interface/AddOns/SkadaStorage/SkadaStorage.toc",
            "Interface/AddOns/Omen/Omen.toc",
            "Interface/AddOns/Mine/Mine.toc",
            "WTF/Account/ME/SavedVariables/Skada.lua",
            "WTF/Account/ME/SavedVariables/Mine.lua",
            "WTF/Account/ME/Icecrown/Arthas/SavedVariables/Omen.lua",
            "WTF/Account/ME/SavedVariables/Removed.lua",
            "WTF/Config.wtf",
        ]);
        let addons = installation.addons_directory();

        for (index, folders) in [(0, &["Skada", "SkadaStorage"][..]), (1, &["Omen"][..])] {
            for folder in folders {
                catalog.entries[index].downloads[0]
                    .marker()
                    .write(&addons.join(folder))
                    .unwrap();
            }
        }

        (root, installation)
    }

    #[test]
    fn exports_list_the_addons_and_carry_only_their_data() {
        let (_root, installation) = source();
        let snapshot = installation.scan().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.zip");

        let summary = export(&installation, &snapshot, &path).unwrap();
        let import = Import::read(&path).unwrap();

        assert_eq!(
            summary,
            Summary {
                addons: 3,
                files: 3
            }
        );
        let titles: Vec<&str> = import
            .manifest
            .addons
            .iter()
            .map(|listed| listed.title.as_str())
            .collect();
        assert_eq!(titles, ["Mine", "Omen", "Skada"]);
        assert!(
            import.manifest.addons[0].marker.is_none(),
            "installed by hand"
        );
        assert_eq!(
            import.files,
            [
                PathBuf::from("WTF/Account/ME/Icecrown/Arthas/SavedVariables/Omen.lua"),
                PathBuf::from("WTF/Account/ME/SavedVariables/Mine.lua"),
                PathBuf::from("WTF/Account/ME/SavedVariables/Skada.lua"),
            ],
            "no data of removed addons, no game settings"
        );
        assert!(!directory.path().join("export.zip.partial").exists());
    }

    #[test]
    fn importing_plans_what_to_install_keep_and_replace() {
        let (_source_root, source) = source();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.zip");
        export(&source, &source.scan().unwrap(), &path).unwrap();

        // Omen is installed already, and Skada left the catalog under that
        // name but still installs the same folders.
        let (_root, target) = client(&[
            "Interface/AddOns/Omen/Omen.toc",
            "WTF/Account/ME/SavedVariables/Skada.lua",
        ]);
        let mut catalog = catalog();
        catalog.entries[0].downloads[0].name = "Skada Revisited".into();

        let import = Import::read(&path).unwrap();
        let plan = import.plan(&catalog, &target, &target.scan().unwrap());

        let names = |listed: &[Listed]| -> Vec<String> {
            listed.iter().map(|listed| listed.title.clone()).collect()
        };
        assert_eq!(plan.install, [catalog.entries[0].clone()]);
        assert_eq!(names(&plan.present), ["Omen"]);
        assert_eq!(names(&plan.manual), ["Mine"]);
        assert!(plan.unavailable.is_empty());
        assert_eq!(
            plan.replaced,
            [PathBuf::from("WTF/Account/ME/SavedVariables/Skada.lua")]
        );
    }

    #[test]
    fn restoring_writes_the_data_where_the_client_keeps_it() {
        let (_source_root, source) = source();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.zip");
        export(&source, &source.scan().unwrap(), &path).unwrap();

        // The target spells its folders differently.
        let (root, target) = client(&["wtf/Account/ME/SavedVariables/Skada.lua"]);

        let restored = Import::read(&path).unwrap().restore(&target).unwrap();

        assert_eq!(restored, 3);
        assert_eq!(
            fs::read_to_string(root.path().join("wtf/Account/ME/SavedVariables/Skada.lua"))
                .unwrap(),
            "-- WTF/Account/ME/SavedVariables/Skada.lua",
            "replaced with the exported copy"
        );
        assert!(
            root.path()
                .join("wtf/Account/ME/Icecrown/Arthas/SavedVariables/Omen.lua")
                .exists()
        );
        let wtf_folders = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("WTF")
            })
            .count();
        assert_eq!(wtf_folders, 1, "no second WTF folder");
    }

    #[test]
    fn only_saved_variables_paths_are_ever_restored() {
        for (name, restored) in [
            ("WTF/Account/ME/SavedVariables/Skada.lua", true),
            (
                "WTF/Account/ME/Icecrown/Arthas/SavedVariables/Omen.lua.bak",
                true,
            ),
            ("wtf/account/ME/savedvariables/Skada.lua", true),
            ("WTF/Config.wtf", false),
            ("WTF/Account/ME/SavedVariables/../../../../Wow.exe", false),
            ("Interface/AddOns/Evil/Evil.lua", false),
            ("WTF/Account/ME/SavedVariables/notes.txt", false),
            ("/WTF/Account/ME/SavedVariables/Skada.lua", false),
            ("WTF/Account/ME/SavedVariables/C:\\Skada.lua", false),
        ] {
            assert_eq!(saved_variables(name).is_some(), restored, "{name}");
        }
    }

    #[test]
    fn zips_without_a_list_of_addons_are_not_exports() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("other.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file("readme.txt", SimpleFileOptions::default())
            .unwrap();
        zip.finish().unwrap();

        assert!(matches!(
            Import::read(&path),
            Err(Error::NotAnExport { .. })
        ));
    }
}
