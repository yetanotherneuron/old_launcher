//! DXVK, which runs the game's Direct3D 9 on Vulkan: its 32-bit `d3d9.dll`
//! beside `Wow.exe`. Listed for Windows only: Proton, Lutris and the like
//! bring their own.
//!
//! Each GitHub release is a gzipped tarball of the DLLs of both
//! architectures, the one the game loads at `dxvk-<version>/x32/d3d9.dll`.

use std::collections::BTreeMap;

use anyhow::Context;
use old_launcher_core::mods::{Mod, Release, Source, VersionIn};

use crate::mods::{Found, Known, LeftOut, Published, files, published};

/// The repository its releases are published in.
const REPOSITORY: &str = "doitsujin/dxvk";

/// The first release with Direct3D 9. Earlier ones are left out without
/// being downloaded.
const FIRST: &[u32] = &[1, 5];

/// The file the game loads, and where it is in a release's tarball.
const DLL: &str = "d3d9.dll";
const FOLDER: &str = "/x32/";

/// DXVK, with every release from [`FIRST`] on, the newest first. Their
/// files go to `found`, and the releases left out to `left_out`, with why.
pub async fn gather(
    client: &reqwest::Client,
    known: &Known,
    found: &mut Known,
    left_out: &mut LeftOut,
) -> anyhow::Result<Mod> {
    let mut releases = Vec::new();

    for published in published(client, REPOSITORY).await? {
        let tag = published.tag_name.clone();
        if number(&tag).as_slice() < FIRST {
            continue;
        }

        match understood(client, known, found, published).await {
            Ok(release) => {
                println!("DXVK {}: {DLL}", release.version);
                releases.push(release);
            }
            Err(error) => left_out.note(format!("DXVK {tag}: left out, {error:#}"), &releases),
        }
    }

    Ok(Mod {
        id: "dxvk".to_owned(),
        name: "DXVK".to_owned(),
        description: "Runs the game's Direct3D 9 graphics on Vulkan, which plays smoother on many \
                      modern graphics cards."
            .to_owned(),
        source: Source {
            name: "DXVK on GitHub".to_owned(),
            page: format!("https://github.com/{REPOSITORY}"),
        },
        releases,
        // Its settings, which no release brings.
        settings: vec!["dxvk.conf".to_owned()],
        // Its log, and the shader cache of releases before 2.0.
        writes: vec!["Wow_d3d9.log".to_owned(), "Wow.dxvk-cache".to_owned()],
        config: BTreeMap::new(),
        platforms: vec!["windows".to_owned()],
        requires: Vec::new(),
        excludes: Vec::new(),
        conflicts: Vec::new(),
        alternatives: Vec::new(),
        // Its DLL says Windows' own version in its version resource, to
        // pass for Windows' DLL, and its own in the line it logs at start.
        version_in: Some(VersionIn {
            file: DLL.to_owned(),
            after: "DXVK: \0v".to_owned(),
        }),
    })
}

/// `published` as the launcher installs it: the 32-bit `d3d9.dll` of its
/// tarball (whose files are kept in `found`).
async fn understood(
    client: &reqwest::Client,
    known: &Known,
    found: &mut Known,
    published: Published,
) -> anyhow::Result<Release> {
    let url = published.asset(tarball).context("it has no tarball")?;
    let asset = published.upload(&url);

    let files = files(client, known, &url, asset).await?;
    let _ = found.insert(
        url.clone(),
        Found {
            files: files.clone(),
            asset,
        },
    );

    release(published, url, &files)
}

/// `published` with its tarball at `url`, which holds `files`: the 32-bit
/// `d3d9.dll`, from the folder it is in.
fn release(
    published: Published,
    url: String,
    files: &[(String, String)],
) -> anyhow::Result<Release> {
    let (path, sum) = files
        .iter()
        .find(|(path, _)| path.ends_with(&format!("{FOLDER}{DLL}")))
        .with_context(|| format!("it has no x32/{DLL}"))?;
    let inside = path
        .strip_suffix(&format!("/{DLL}"))
        .unwrap_or(path)
        .to_owned();

    Ok(Release {
        version: published.tag_name.trim_start_matches('v').to_owned(),
        url,
        updated: published.day(),
        changes: Some(published.html_url),
        files: vec![(DLL.to_owned(), sum.clone())],
        from: BTreeMap::new(),
        inside: Some(inside),
        setup: Vec::new(),
        languages: BTreeMap::new(),
    })
}

/// Whether the release file named `name` is the tarball for Windows games,
/// not DXVK Native's, which is for Linux ones.
fn tarball(name: &str) -> bool {
    name.starts_with("dxvk-") && name.ends_with(".tar.gz") && !name.contains("native")
}

/// The numbers of a version tag: `v1.10.3` is 1, 10, 3. What isn't a number
/// counts as 0.
fn number(tag: &str) -> Vec<u32> {
    tag.trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::Asset;

    fn published(tag: &str) -> Published {
        Published {
            tag_name: tag.to_owned(),
            draft: false,
            prerelease: false,
            published_at: Some("2026-09-15T17:43:25Z".to_owned()),
            html_url: format!("https://github.com/doitsujin/dxvk/releases/tag/{tag}"),
            assets: vec![Asset {
                id: 1,
                name: "dxvk-native-3.1.1-steamrt-sniper.tar.gz".to_owned(),
                browser_download_url: "https://example.com/native.tar.gz".to_owned(),
                digest: None,
            }],
        }
    }

    #[test]
    fn a_release_is_its_32_bit_d3d9_dll_from_its_folder() {
        let files = [
            ("dxvk-3.1.1/x32/d3d11.dll", "a1"),
            ("dxvk-3.1.1/x32/d3d9.dll", "b2"),
            ("dxvk-3.1.1/x64/d3d9.dll", "c3"),
        ]
        .map(|(path, sum)| (path.to_owned(), sum.to_owned()));

        let release = release(
            published("v3.1.1"),
            "https://example.com/dxvk-3.1.1.tar.gz".to_owned(),
            &files,
        )
        .unwrap();

        assert_eq!(release.version, "3.1.1");
        assert_eq!(release.updated.as_deref(), Some("2026-09-15"));
        assert_eq!(release.files, [("d3d9.dll".to_owned(), "b2".to_owned())]);
        assert_eq!(release.inside.as_deref(), Some("dxvk-3.1.1/x32"));
        assert!(release.setup.is_empty());
    }

    #[test]
    fn a_release_without_the_dll_is_left_out_and_older_ones_are_not_looked_at() {
        let files = [("dxvk-1.4/x32/d3d11.dll".to_owned(), "a1".to_owned())];
        assert!(release(published("v1.4"), String::new(), &files).is_err());

        assert!(number("v1.4.6").as_slice() < FIRST);
        assert!(number("v1.5").as_slice() >= FIRST);
        assert!(number("v1.10.3") > number("v1.9.4"));
        assert_eq!(
            published("v3.1.1").asset(tarball),
            None,
            "the native build is for Linux games"
        );
        assert!(tarball("dxvk-3.1.1.tar.gz"));
    }
}
