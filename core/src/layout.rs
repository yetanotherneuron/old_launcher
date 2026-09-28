//! Locating addon folders inside an extracted archive.
//!
//! Archives come in every shape: `ACP/ACP.toc` at the root, GitHub branch
//! snapshots wrapping several addons in `repo-main/`, or a repository whose
//! root *is* the addon (`Foo-master/Foo.toc`). The client only loads
//! `AddOns/<Name>/<Name>.toc`, so each folder holding a `.toc` is found and
//! given the name its `.toc` declares.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::error::io_at;

/// An addon folder found inside an extracted archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Where the folder currently is.
    pub path: PathBuf,
    /// The name it must have under `Interface/AddOns`: its `.toc` file's stem.
    pub name: String,
}

/// Walks `root` breadth-first and returns every addon folder in it.
///
/// A directory holding a `.toc` file is an addon folder, and the walk does not
/// descend into it: `.toc` files deeper down belong to libraries the addon
/// embeds. When several addons claim the same name, the shallowest wins.
pub fn find(root: &Path) -> Result<Vec<Folder>, Error> {
    let mut folders: Vec<Folder> = Vec::new();
    let mut pending = vec![root.to_path_buf()];

    while !pending.is_empty() {
        let mut next = Vec::new();

        for directory in pending {
            let mut subdirectories = Vec::new();
            let mut tocs = Vec::new();

            for entry in fs::read_dir(&directory).map_err(io_at(&directory))? {
                let entry = entry.map_err(io_at(&directory))?;
                let path = entry.path();
                let file_name = entry.file_name().to_string_lossy().into_owned();
                let file_type = entry.file_type().map_err(io_at(&path))?;

                if file_type.is_dir() {
                    // macOS resource forks and dot-folders (`.git`) are never addons.
                    if !file_name.starts_with('.') && file_name != "__MACOSX" {
                        subdirectories.push(path);
                    }
                } else if let Some(stem) = toc_stem(&file_name) {
                    tocs.push(stem.to_owned());
                }
            }

            if tocs.is_empty() {
                subdirectories.sort();
                next.extend(subdirectories);
                continue;
            }

            let directory_name = directory
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();

            // Prefer the `.toc` named like its folder; otherwise the shortest
            // stem, so `Foo.toc` beats flavor variants like `Foo_Wrath.toc`.
            tocs.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
            let name = tocs
                .iter()
                .find(|stem| stem.eq_ignore_ascii_case(&directory_name))
                .unwrap_or(&tocs[0])
                .clone();

            if !folders
                .iter()
                .any(|folder| folder.name.eq_ignore_ascii_case(&name))
            {
                folders.push(Folder {
                    path: directory,
                    name,
                });
            }
        }

        pending = next;
    }

    Ok(folders)
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
        let root = tempfile::tempdir().unwrap();

        for file in files {
            let path = root.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
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
    fn the_toc_matching_the_folder_beats_flavor_variants() {
        let root = tree(&["DalaranAH/DalaranAH_Wrath.toc", "DalaranAH/DalaranAH.toc"]);

        assert_eq!(names(root.path()), ["DalaranAH"]);
    }

    #[test]
    fn toc_casing_wins_over_folder_casing() {
        let root = tree(&["acp/ACP.toc"]);

        assert_eq!(names(root.path()), ["ACP"]);
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
