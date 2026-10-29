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
use serde::Deserialize;

use crate::fetch::fetch;
use crate::source::github;

/// The files of a release's download, by its URL, with their SHA-256: what
/// an earlier run found, since a release's download never changes.
pub type Known = BTreeMap<String, Vec<(String, String)>>;

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

/// A file attached to a release.
#[derive(Debug, Deserialize)]
pub struct Asset {
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
/// or else downloaded and looked into.
pub async fn files(
    client: &reqwest::Client,
    known: &Known,
    url: &str,
) -> anyhow::Result<Vec<(String, String)>> {
    if let Some(files) = known.get(url) {
        return Ok(files.clone());
    }

    let archive = fetch(client, url).await?;

    Ok(tokio::task::spawn_blocking(move || mods::inspect(&archive)).await??)
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
}
