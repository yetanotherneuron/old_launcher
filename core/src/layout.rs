//! Locating addon folders inside an extracted archive.
//!
//! Archives come in every shape: `ACP/ACP.toc` at the root, GitHub branch
//! snapshots wrapping several addons in `repo-main/`, or a repository whose
//! root *is* the addon (`Foo-master/Foo.toc`), sometimes with its modules
//! inside it (`Foo-master/Foo_Options/`). The client only loads
//! `AddOns/<Name>/<Name>.toc`, so each folder holding a `.toc` is found and
//! given the name its `.toc` declares.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::error::io_at;
use crate::toc::Toc;

/// An addon folder found inside an extracted archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Where the folder currently is.
    pub path: PathBuf,
    /// The name it must have under `Interface/AddOns`: its `.toc` file's stem.
    pub name: String,
    /// That `.toc` file, spelled as the archive spells it: `Foo.TOC` loads
    /// just as well, since the client ignores case.
    pub toc: PathBuf,
}

/// Walks `root` breadth-first and returns every addon folder in it.
///
/// A directory holding a `.toc` file is an addon folder, and the walk does not
/// descend into it: `.toc` files deeper down belong to libraries the addon
/// embeds, but for its modules (see [`modules`]), which come after every
/// other folder. When several addons claim the same name, the shallowest
/// wins.
pub fn find(root: &Path) -> Result<Vec<Folder>, Error> {
    let mut folders: Vec<Folder> = Vec::new();
    let mut pending = vec![root.to_path_buf()];

    while !pending.is_empty() {
        let mut next = Vec::new();

        for directory in pending {
            let (subdirectories, tocs) = read(&directory)?;

            match pick(&directory, tocs) {
                Some((name, toc)) => {
                    push(
                        &mut folders,
                        Folder {
                            path: directory,
                            name,
                            toc,
                        },
                    );
                }
                None => next.extend(subdirectories),
            }
        }

        pending = next;
    }

    // Looked for once the walk is over, so that a module shipped beside its
    // addon as well wins over the copy inside it.
    for addon in folders.clone() {
        modules(&addon, &mut folders)?;
    }

    Ok(folders)
}

/// Adds to `folders` the folders nested in `addon` whose `.toc` requires
/// it and that it loads nothing from, when `addon` is a repository rather
/// than a folder named for its addon: the modules a repository keeps inside
/// its addon (`Foo-master/Foo_Options/`), which the client only loads
/// beside it. A folder named for its addon was packaged for
/// `Interface/AddOns` as it is, so what is nested in it stays: the addon
/// may load it itself (WIM's skins) or leave it to be moved out by hand
/// (Prat's optional modules). So does a folder the addon loads files from
/// (`options\config.lua`), and the libraries an addon embeds, which don't
/// require it, with whatever is inside them.
fn modules(addon: &Folder, folders: &mut Vec<Folder>) -> Result<(), Error> {
    let named_for_it = addon
        .path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(&addon.name));

    if named_for_it {
        return Ok(());
    }

    let (subdirectories, _) = read(&addon.path)?;
    let mut pending: Vec<PathBuf> = subdirectories.into_iter().rev().collect();

    while let Some(directory) = pending.pop() {
        let (subdirectories, tocs) = read(&directory)?;

        let Some((name, toc)) = pick(&directory, tocs) else {
            pending.extend(subdirectories.into_iter().rev());
            continue;
        };

        let requires = Toc::parse(&fs::read(&toc).map_err(io_at(&toc))?)
            .dependencies
            .iter()
            .any(|dependency| dependency.eq_ignore_ascii_case(&addon.name));

        if !requires || loads_from(addon, &directory)? {
            continue;
        }

        let module = Folder {
            path: directory,
            name,
            toc,
        };

        if push(folders, module.clone()) {
            modules(&module, folders)?;
        }
    }

    Ok(())
}

/// Whether `addon` loads files from `folder`, nested in it: whether its own
/// `.toc` files, or the XML files it holds outside `folder`, name a path
/// through a folder of that name (`options\config.lua`,
/// `<Include file="Foo_Bar\Embed.xml"/>`).
fn loads_from(addon: &Folder, folder: &Path) -> Result<bool, Error> {
    let name = folder
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let paths = [format!("{name}\\"), format!("{name}/")];

    let mut files: Vec<PathBuf> = read(&addon.path)?
        .1
        .into_iter()
        .map(|(_, toc)| toc)
        .collect();
    let mut pending = vec![addon.path.clone()];

    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(io_at(&directory))? {
            let path = entry.map_err(io_at(&directory))?.path();

            if path.is_dir() {
                if path != folder {
                    pending.push(path);
                }
            } else if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
            {
                files.push(path);
            }
        }
    }

    for file in files {
        let text = String::from_utf8_lossy(&fs::read(&file).map_err(io_at(&file))?).to_lowercase();

        if paths.iter().any(|path| text.contains(path.as_str())) {
            return Ok(true);
        }
    }

    Ok(false)
}

/// `.toc` files, by stem.
type Tocs = Vec<(String, PathBuf)>;

/// The subdirectories of `directory`, sorted, and its `.toc` files. macOS
/// resource forks and dot-folders (`.git`) are never addons.
fn read(directory: &Path) -> Result<(Vec<PathBuf>, Tocs), Error> {
    let mut subdirectories = Vec::new();
    let mut tocs = Vec::new();

    for entry in fs::read_dir(directory).map_err(io_at(directory))? {
        let entry = entry.map_err(io_at(directory))?;
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry.file_type().map_err(io_at(&path))?;

        if file_type.is_dir() {
            if !file_name.starts_with('.') && file_name != "__MACOSX" {
                subdirectories.push(path);
            }
        } else if let Some(stem) = toc_stem(&file_name) {
            tocs.push((stem.to_owned(), path));
        }
    }

    subdirectories.sort();

    Ok((subdirectories, tocs))
}

/// The name and `.toc` of the addon in `directory`, among its `tocs`, when
/// it holds any: the `.toc` named like the folder; otherwise the shortest
/// stem of those made for 3.3.5a, or of all when none is, so `Foo.toc`
/// beats flavor variants like `Foo_Wrath.toc`. The client loads only the
/// `.toc` named like its folder, so a snapshot named after a commit holding
/// the addon's (`Foo-335.toc`, for 3.3.5a) and a stub for any other folder
/// name (`Foo.toc`, saying to rename it) is the addon's.
fn pick(directory: &Path, mut tocs: Tocs) -> Option<(String, PathBuf)> {
    if tocs.is_empty() {
        return None;
    }

    let directory_name = directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let for_wotlk = |path: &Path| {
        fs::read(path).is_ok_and(|bytes| Toc::parse(&bytes).interface == Some(Toc::WOTLK_INTERFACE))
    };

    tocs.sort_by(|(a, _), (b, _)| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    let index = tocs
        .iter()
        .position(|(stem, _)| stem.eq_ignore_ascii_case(&directory_name))
        .or_else(|| tocs.iter().position(|(_, path)| for_wotlk(path)))
        .unwrap_or(0);

    Some(tocs.swap_remove(index))
}

/// Adds `folder` to `folders` unless an addon of its name is there already,
/// and says whether it did.
fn push(folders: &mut Vec<Folder>, folder: Folder) -> bool {
    let taken = folders
        .iter()
        .any(|other| other.name.eq_ignore_ascii_case(&folder.name));

    if !taken {
        folders.push(folder);
    }

    !taken
}

/// The stem of a `.toc` file name, matching the extension case-insensitively.
fn toc_stem(file_name: &str) -> Option<&str> {
    let (stem, extension) = file_name.rsplit_once('.')?;

    (extension.eq_ignore_ascii_case("toc") && !stem.is_empty()).then_some(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        tree_of(&files.iter().map(|file| (*file, "")).collect::<Vec<_>>())
    }

    /// A tree of files with their contents.
    fn tree_of(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();

        for (file, contents) in files {
            let path = root.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        root
    }

    fn names(root: &Path) -> Vec<String> {
        find(root)
            .unwrap()
            .into_iter()
            .map(|folder| folder.name)
            .collect()
    }

    #[test]
    fn a_top_level_addon_folder_is_found() {
        let root = tree(&["ACP/ACP.toc", "ACP/ACP.lua"]);

        assert_eq!(names(root.path()), ["ACP"]);
    }

    #[test]
    fn a_branch_snapshot_wrapper_is_looked_through() {
        let root = tree(&[
            "Skada-WoTLK-main/Skada/Skada.toc",
            "Skada-WoTLK-main/SkadaImprovement/SkadaImprovement.toc",
            "Skada-WoTLK-main/README.md",
        ]);

        assert_eq!(names(root.path()), ["Skada", "SkadaImprovement"]);
    }

    #[test]
    fn a_repository_root_addon_takes_its_toc_name() {
        let root = tree(&["GearScore-main/GearScore.toc", "GearScore-main/core.lua"]);
        let folders = find(root.path()).unwrap();

        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "GearScore");
        assert_eq!(folders[0].path, root.path().join("GearScore-main"));
    }

    #[test]
    fn embedded_library_tocs_are_not_separate_addons() {
        let root = tree(&[
            "WeakAuras-master/WeakAuras/WeakAuras.toc",
            "WeakAuras-master/WeakAuras/Libs/LibStub/LibStub.toc",
            "WeakAuras-master/WeakAurasOptions/WeakAurasOptions.toc",
        ]);

        assert_eq!(names(root.path()), ["WeakAuras", "WeakAurasOptions"]);
    }

    #[test]
    fn modules_kept_inside_their_addon_are_addons_of_their_own() {
        let root = tree_of(&[
            ("Altoholic-master/Altoholic.toc", ""),
            (
                "Altoholic-master/Altoholic_Tabs/Altoholic_Tabs.toc",
                "## RequiredDeps: Altoholic\n",
            ),
            (
                "Altoholic-master/Modules/Altoholic_Achievements/Altoholic_Achievements.toc",
                "## Dependencies: DataStore, altoholic\n",
            ),
            ("Altoholic-master/Libs/LibStub/LibStub.toc", ""),
            (
                "Altoholic-master/Libs/LibStub/Altoholic_Fake/Altoholic_Fake.toc",
                "## Dependencies: Altoholic\n",
            ),
            ("Other/Other.toc", ""),
        ]);
        let folders = find(root.path()).unwrap();

        assert_eq!(
            names(root.path()),
            [
                "Altoholic",
                "Other",
                "Altoholic_Tabs",
                "Altoholic_Achievements"
            ],
            "a library and what is inside it stay in the addon"
        );
        assert_eq!(
            folders[3].path,
            root.path()
                .join("Altoholic-master/Modules/Altoholic_Achievements")
        );
    }

    #[test]
    fn a_module_shipped_beside_its_addon_too_comes_from_there() {
        let root = tree_of(&[
            ("Foo-master/Foo.toc", ""),
            (
                "Foo-master/Foo_Options/Foo_Options.toc",
                "## Dependencies: Foo\n",
            ),
            ("Foo_Options/Foo_Options.toc", "## Dependencies: Foo\n"),
        ]);
        let folders = find(root.path()).unwrap();

        assert_eq!(names(root.path()), ["Foo", "Foo_Options"]);
        assert_eq!(folders[1].path, root.path().join("Foo_Options"));
    }

    #[test]
    fn a_folder_its_addon_loads_files_from_stays_in_it() {
        let root = tree_of(&[
            (
                "ShadowedUnitFrames-master/ShadowedUnitFrames.toc",
                "ShadowedUnitFrames.lua\r\noptions\\config.lua\r\n",
            ),
            (
                "ShadowedUnitFrames-master/options/ShadowedUF_Options.toc",
                "## Dependencies: ShadowedUnitFrames\n",
            ),
            ("Foo-master/Foo.toc", "Modules\\Modules.xml\n"),
            (
                "Foo-master/Modules/Modules.xml",
                "<Ui><Include file=\"Foo_Bar\\Embed.xml\"/></Ui>",
            ),
            (
                "Foo-master/Modules/Foo_Bar/Foo_Bar.toc",
                "## Dependencies: Foo\n",
            ),
            (
                "Foo-master/Foo_Options/Foo_Options.toc",
                "## Dependencies: Foo\n",
            ),
        ]);

        assert_eq!(
            names(root.path()),
            ["Foo", "ShadowedUnitFrames", "Foo_Options"],
            "only Foo_Options is a module its addon leaves to the client"
        );
    }

    #[test]
    fn what_a_folder_named_for_its_addon_holds_stays_in_it() {
        let root = tree_of(&[
            ("WIM/WIM.toc", ""),
            ("WIM/Skins/Default/skin.toc", "## Dependencies: WIM\n"),
        ]);

        assert_eq!(names(root.path()), ["WIM"], "WIM loads its skins itself");
    }

    #[test]
    fn the_toc_matching_the_folder_beats_flavor_variants() {
        let root = tree(&["DalaranAH/DalaranAH_Wrath.toc", "DalaranAH/DalaranAH.toc"]);

        assert_eq!(names(root.path()), ["DalaranAH"]);
    }

    #[test]
    fn a_snapshot_named_after_a_commit_is_the_addon_made_for_3_3_5a() {
        let root = tree_of(&[
            ("Questie-908a40b/Questie-335.toc", "## Interface: 30300\n"),
            (
                "Questie-908a40b/Questie.toc",
                "## Interface: 00000\n## Title: Questie install folder problem\n",
            ),
        ]);
        assert_eq!(names(root.path()), ["Questie-335"], "not the stub");

        // Named after its branch, the folder still says which.
        let root = tree_of(&[
            ("Questie-335/Questie-335.toc", "## Interface: 30300\n"),
            ("Questie-335/Questie.toc", "## Interface: 00000\n"),
        ]);
        assert_eq!(names(root.path()), ["Questie-335"]);
    }

    #[test]
    fn toc_casing_wins_over_folder_casing() {
        let root = tree(&["acp/ACP.toc"]);

        assert_eq!(names(root.path()), ["ACP"]);
    }

    #[test]
    fn a_toc_is_found_whatever_the_case_of_its_extension() {
        let root = tree(&["The_Sarge_DK/The_Sarge.TOC", "The_Sarge_DK/The_Sarge.LUA"]);
        let folders = find(root.path()).unwrap();

        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "The_Sarge");
        assert_eq!(
            folders[0].toc,
            root.path().join("The_Sarge_DK/The_Sarge.TOC")
        );
    }

    #[test]
    fn macos_metadata_folders_are_ignored() {
        let root = tree(&["__MACOSX/ACP/._ACP.toc", "ACP/ACP.toc"]);

        assert_eq!(names(root.path()), ["ACP"]);
    }

    #[test]
    fn archives_without_tocs_have_no_addons() {
        let root = tree(&["README.md", "docs/index.html"]);

        assert!(names(root.path()).is_empty());
    }
}
