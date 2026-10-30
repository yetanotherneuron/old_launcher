//! Builds the launcher's list of mods: each mod's module says where its
//! releases are and what the launcher can't read from them, and every file of
//! every release is hashed.

pub mod dungeon_maps;
pub mod dxvk;
pub mod lau;
pub mod lumenveil;

use std::collections::BTreeMap;
use std::path::Path;

use old_launcher_core::mods::{self, Mod, Release};
use serde::{Deserialize, Serialize};

use crate::fetch::fetch;
use crate::source::github;
use crate::uploads;

/// What earlier runs found in releases' downloads, by URL: it holds while
/// the download is the same upload (see [`uploads`]).
pub type Known = BTreeMap<String, Found>;

/// What was found in a release's download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Kept")]
pub struct Found {
    /// Its files, with their SHA-256.
    pub files: Vec<(String, String)>,
    /// The id of the upload they were found in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<u64>,
}

/// What was found, as kept: as [`Found`] is, or as earlier versions of the
/// indexer kept it, the files alone.
#[derive(Deserialize)]
#[serde(untagged)]
enum Kept {
    Found {
        files: Vec<(String, String)>,
        #[serde(default)]
        asset: Option<u64>,
    },
    Files(Vec<(String, String)>),
}

impl From<Kept> for Found {
    fn from(kept: Kept) -> Self {
        match kept {
            Kept::Found { files, asset } => Self { files, asset },
            Kept::Files(files) => Self { files, asset: None },
        }
    }
}

/// A release, as GitHub's API describes it.
#[derive(Debug, Deserialize)]
pub struct Published {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub html_url: String,
    pub assets: Vec<Asset>,
}

/// A file attached to a release: one upload of it.
#[derive(Debug, Deserialize)]
pub struct Asset {
    /// The id of this upload, which another under the same name doesn't
    /// share.
    pub id: u64,
    pub name: String,
    pub browser_download_url: String,
    /// Its checksum (`sha256:<hex>`), which GitHub gives for files attached
    /// since mid-2025.
    #[serde(default)]
    pub digest: Option<String>,
}

impl Published {
    /// The day it came out (`YYYY-MM-DD`), when GitHub says.
    pub fn day(&self) -> Option<String> {
        self.published_at
            .as_deref()
            .and_then(|date| date.get(..10))
            .map(str::to_owned)
    }

    /// The URL of its first file `wanted` takes.
    pub fn asset(&self, wanted: impl Fn(&str) -> bool) -> Option<String> {
        self.assets
            .iter()
            .find(|asset| wanted(&asset.name))
            .map(|asset| asset.browser_download_url.clone())
    }

    /// The id of the upload of its file at `url`, if it has one there.
    pub fn upload(&self, url: &str) -> Option<u64> {
        self.assets
            .iter()
            .find(|asset| asset.browser_download_url == url)
            .map(|asset| asset.id)
    }
}

/// The releases published in the GitHub repository `repository`, the
/// newest first, drafts and pre-releases aside.
pub async fn published(
    client: &reqwest::Client,
    repository: &str,
) -> anyhow::Result<Vec<Published>> {
    let published: Vec<Published> =
        github::api(client, &format!("repos/{repository}/releases?per_page=100")).await?;

    Ok(published
        .into_iter()
        .filter(|published| !published.draft && !published.prerelease)
        .collect())
}

/// What a run gathered: every mod, the files it found in their releases'
/// downloads (for the next run to reuse), and the releases left out, with
/// why: those `missed` are newer than every release of their mod listed, so
/// the launcher offers an older one until the indexer understands them,
/// which someone has to be told; those `skipped` are older, and no loss.
pub struct Gathered {
    pub mods: Vec<Mod>,
    pub found: Known,
    pub missed: Vec<String>,
    pub skipped: Vec<String>,
}

/// The releases a module left out so far, to tell apart those that matter.
#[derive(Default)]
pub struct LeftOut {
    pub missed: Vec<String>,
    pub skipped: Vec<String>,
}

impl LeftOut {
    /// Notes a release left out for `reason`, as missed when `listed`, the
    /// releases of its mod understood so far (the newest first), has none
    /// newer than it, else as skipped.
    pub fn note(&mut self, reason: String, listed: &[Release]) {
        println!("{reason}");

        if listed.is_empty() {
            self.missed.push(reason);
        } else {
            self.skipped.push(reason);
        }
    }
}

/// Every mod, with what was found and what was left out.
pub async fn gather(client: &reqwest::Client, known: &Known) -> anyhow::Result<Gathered> {
    let mut found = Known::new();
    let mut left_out = LeftOut::default();

    let lumenveil = lumenveil::gather(client, known, &mut found, &mut left_out).await?;
    let dxvk = dxvk::gather(client, known, &mut found, &mut left_out).await?;
    let lau = lau::gather(client, known, &mut found, &mut left_out).await?;
    let maps = dungeon_maps::gather(client, known, &mut found, &mut left_out).await?;

    Ok(Gathered {
        mods: [lumenveil, dxvk]
            .into_iter()
            .chain(lau)
            .chain([maps])
            .collect(),
        found,
        missed: left_out.missed,
        skipped: left_out.skipped,
    })
}

/// The files of the download at `url`, with their SHA-256: as `known` says,
/// when it is still the upload `asset` (see [`earlier`]), or else downloaded
/// and looked into.
pub async fn files(
    client: &reqwest::Client,
    known: &Known,
    url: &str,
    asset: Option<u64>,
) -> anyhow::Result<Vec<(String, String)>> {
    if let Some(files) = earlier(known, url, asset) {
        return Ok(files.to_vec());
    }

    let archive = fetch(client, url).await?;

    Ok(tokio::task::spawn_blocking(move || mods::inspect(&archive)).await??)
}

/// The files `known` says the release's download at `url` holds, if it is
/// still the upload they were found in: `asset`, the id of the one there
/// now.
pub fn earlier<'a>(
    known: &'a Known,
    url: &str,
    asset: Option<u64>,
) -> Option<&'a [(String, String)]> {
    known
        .get(url)
        .filter(|found| uploads::same(found.asset, asset))
        .map(|found| found.files.as_slice())
}

/// What earlier runs found in releases' downloads, as kept at `path`.
/// Without the file, or when it doesn't read, every download is looked into
/// again, which costs time and nothing else.
pub fn known(path: &Path) -> Known {
    let json = match std::fs::read(path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Known::new(),
        Err(error) => {
            println!("Not reusing {}: {error}", path.display());
            return Known::new();
        }
    };

    serde_json::from_slice(&json).unwrap_or_else(|error| {
        println!("Not reusing {}: {error}", path.display());
        Known::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_left_out_is_missed_until_a_newer_one_is_listed() {
        let listed = |version: &str| Release {
            version: version.to_owned(),
            url: String::new(),
            updated: None,
            changes: None,
            files: Vec::new(),
            from: BTreeMap::new(),
            inside: None,
            setup: Vec::new(),
            languages: BTreeMap::new(),
        };
        let mut left_out = LeftOut::default();

        left_out.note("Mod v2.0: its scripts are new".to_owned(), &[]);
        left_out.note("Mod v0.9: it has no zip".to_owned(), &[listed("1.0")]);

        assert_eq!(left_out.missed, ["Mod v2.0: its scripts are new"]);
        assert_eq!(left_out.skipped, ["Mod v0.9: it has no zip"]);
    }

    #[test]
    fn what_was_found_holds_only_for_the_same_upload() {
        let url = "https://github.com/doitsujin/dxvk/releases/download/v3.1/dxvk-3.1.tar.gz";
        let files = vec![("dxvk-3.1/x32/d3d9.dll".to_owned(), "b2".to_owned())];
        let known = Known::from([(
            url.to_owned(),
            Found {
                files: files.clone(),
                asset: Some(7),
            },
        )]);

        assert_eq!(earlier(&known, url, Some(7)), Some(files.as_slice()));
        assert_eq!(earlier(&known, url, Some(8)), None, "uploaded again");
        assert_eq!(earlier(&known, url, None), None, "unknown now");
        assert_eq!(earlier(&known, "https://example.com/other", Some(7)), None);
    }

    #[test]
    fn what_earlier_runs_found_is_read_back_as_it_was_kept_before_too() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mod-archives.json");
        let known = Known::from([(
            "https://example.com/a.zip".to_owned(),
            Found {
                files: vec![("a.txt".to_owned(), "a1".to_owned())],
                asset: Some(7),
            },
        )]);

        std::fs::write(&path, serde_json::to_string_pretty(&known).unwrap()).unwrap();
        assert_eq!(super::known(&path), known);

        // As earlier versions of the indexer kept it: the files alone.
        std::fs::write(
            &path,
            r#"{ "https://example.com/a.zip": [["a.txt", "a1"]] }"#,
        )
        .unwrap();
        assert_eq!(
            super::known(&path)["https://example.com/a.zip"],
            Found {
                files: vec![("a.txt".to_owned(), "a1".to_owned())],
                asset: None,
            }
        );
    }
}
