//! Builds the launcher's list of mods: each mod's module says where its
//! releases are and what the launcher can't read from them, and every file of
//! every release is hashed.

pub mod dxvk;
pub mod lumenveil;

use std::collections::BTreeMap;
use std::path::Path;

use old_launcher_core::mods::{self, Mod};
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

/// Every mod, with the files this run found in their releases' downloads
/// (for the next run to reuse), and the releases left out, with why.
pub async fn gather(
    client: &reqwest::Client,
    known: &Known,
) -> anyhow::Result<(Vec<Mod>, Known, Vec<String>)> {
    let mut found = Known::new();
    let mut skipped = Vec::new();

    let lumenveil = lumenveil::gather(client, known, &mut found, &mut skipped).await?;
    let dxvk = dxvk::gather(client, known, &mut found, &mut skipped).await?;

    Ok((vec![lumenveil, dxvk], found, skipped))
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
