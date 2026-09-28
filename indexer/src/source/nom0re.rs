//! NoM0Re: their curated collection,
//! <https://github.com/NoM0Re/WoW-3.3.5a-Addons>, and their own addon
//! repositories, as one source.
//!
//! The collection's website is generated from a JSON index in the
//! repository, which lists each addon with a download link, a screenshot and
//! pages about it.

use anyhow::Context;
use old_launcher_core::catalog;
use serde::Deserialize;

use crate::fetch::fetch;
use crate::source::{Listing, Source, Updated, github};

const INDEX: &str =
    "https://raw.githubusercontent.com/NoM0Re/WoW-3.3.5a-Addons/main/docs/wotlk_addons.json";

/// The GitHub account behind the collection.
const OWNER: &str = "NoM0Re";

/// NoM0Re's curated collection and repositories.
pub struct Collection;

impl Source for Collection {
    fn info(&self) -> catalog::Source {
        catalog::Source {
            id: "nom0re".into(),
            name: "NoM0Re".into(),
            homepage: "https://github.com/NoM0Re/WoW-3.3.5a-Addons".into(),
        }
    }

    /// Fetches and reads the collection's index, then lists NoM0Re's own
    /// repositories after it.
    ///
    /// Some entries link to a GitHub release page rather than a file; those
    /// are resolved to the release's zip (or its source archive, if it has
    /// none).
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>> {
        #[derive(Deserialize)]
        struct Release {
            assets: Vec<Asset>,
            zipball_url: String,
        }

        #[derive(Deserialize)]
        struct Asset {
            name: String,
            browser_download_url: String,
        }

        let mut listings = parse(&fetch(client, INDEX).await?)?;

        for listing in &mut listings {
            let Some(release) = release_path(&listing.download) else {
                continue;
            };
            let release: Release = github::api(client, &release)
                .await
                .with_context(|| format!("resolving {}", listing.download))?;

            listing.download = release
                .assets
                .into_iter()
                .find(|asset| asset.name.to_lowercase().ends_with(".zip"))
                .map_or(release.zipball_url, |asset| asset.browser_download_url);
        }

        // A repository named like a collection entry is left out by the
        // indexer, which keeps names unique within a source: the entry
        // already describes it.
        listings.extend(github::repositories(client, OWNER).await?);

        Ok(listings)
    }
}

/// The GitHub API path of the release a release page shows:
/// `github.com/{owner}/{repo}/releases/latest` or `…/releases/tag/{tag}`.
fn release_path(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("https://github.com/")?
        .trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').collect();

    match parts.as_slice() {
        [owner, repository, "releases", "latest"] => {
            Some(format!("repos/{owner}/{repository}/releases/latest"))
        }
        [owner, repository, "releases", "tag", tag] => {
            Some(format!("repos/{owner}/{repository}/releases/tags/{tag}"))
        }
        _ => None,
    }
}

fn parse(index: &[u8]) -> anyhow::Result<Vec<Listing>> {
    #[derive(Deserialize)]
    struct Raw {
        name: String,
        #[serde(default)]
        description_text: String,
        primary_download: String,
        #[serde(default)]
        primary_image: Option<String>,
        #[serde(default)]
        source: Vec<RawLink>,
    }

    #[derive(Deserialize)]
    struct RawLink {
        label: String,
        url: String,
    }

    let raw: Vec<Raw> = serde_json::from_slice(index).context("reading NoM0Re's index")?;

    Ok(raw
        .into_iter()
        .map(|raw| Listing {
            name: raw.name.trim().to_owned(),
            description: raw.description_text.trim().to_owned(),
            download: download_url(raw.primary_download.trim()),
            image: raw
                .primary_image
                .filter(|image| !image.trim().is_empty())
                .map(|image| raw_url(image.trim())),
            // The collection has no page per addon; its links are upstream's.
            page: None,
            links: raw
                .source
                .into_iter()
                .map(|link| catalog::Link {
                    label: link.label,
                    url: link.url,
                })
                .collect(),
            updated: Updated::FromArchive,
            require_wotlk: false,
        })
        .collect())
}

/// Turns a bare `https://github.com/owner/repo` link, which is a web page,
/// into a download of the repository's default branch.
fn download_url(url: &str) -> String {
    let Some(path) = url.strip_prefix("https://github.com/") else {
        return url.to_owned();
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);

    if path.split('/').count() == 2 {
        format!("https://github.com/{path}/archive/HEAD.zip")
    } else {
        url.to_owned()
    }
}

/// Turns a GitHub `blob` page URL into the raw file it shows.
fn raw_url(url: &str) -> String {
    let Some(path) = url.strip_prefix("https://github.com/") else {
        return url.to_owned();
    };
    let mut parts = path.splitn(4, '/');

    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(repository), Some("blob"), Some(rest)) => {
            format!("https://raw.githubusercontent.com/{owner}/{repository}/{rest}")
        }
        _ => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_index_is_read_into_listings() {
        let index = br#"[{
            "name": "Addon Control Panel",
            "description_text": "Toggle addons in game. ",
            "primary_download": "https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/main/src/Addons/ACP.zip",
            "primary_image": "https://github.com/NoM0Re/WoW-3.3.5a-Addons/blob/main/src/Pictures/ACP.jpg",
            "image_urls": [],
            "source": [{ "label": "Curseforge", "url": "https://www.curseforge.com/wow/addons/acp" }]
        }]"#;

        let listings = parse(index).unwrap();

        assert_eq!(listings.len(), 1);
        assert_eq!(listings[0].name, "Addon Control Panel");
        assert_eq!(listings[0].description, "Toggle addons in game.");
        assert_eq!(
            listings[0].image.as_deref(),
            Some(
                "https://raw.githubusercontent.com/NoM0Re/WoW-3.3.5a-Addons/main/src/Pictures/ACP.jpg"
            )
        );
        assert_eq!(listings[0].links[0].label, "Curseforge");
    }

    #[test]
    fn missing_optional_fields_are_tolerated() {
        let index = br#"[{ "name": "Bare", "primary_download": "https://example.com/Bare.zip" }]"#;

        let listings = parse(index).unwrap();

        assert_eq!(listings[0].description, "");
        assert_eq!(listings[0].image, None);
        assert!(listings[0].links.is_empty());
    }

    #[test]
    fn repository_page_links_become_branch_downloads() {
        assert_eq!(
            download_url("https://github.com/Zendevve/CartoMapper"),
            "https://github.com/Zendevve/CartoMapper/archive/HEAD.zip"
        );
        assert_eq!(
            download_url("https://github.com/owner/repo.git/"),
            "https://github.com/owner/repo/archive/HEAD.zip"
        );
    }

    #[test]
    fn release_pages_map_to_their_api_paths() {
        assert_eq!(
            release_path("https://github.com/Stradios/Emblem-of-Gold/releases/latest").as_deref(),
            Some("repos/Stradios/Emblem-of-Gold/releases/latest")
        );
        assert_eq!(
            release_path("https://github.com/o/r/releases/tag/v1.0.1/").as_deref(),
            Some("repos/o/r/releases/tags/v1.0.1")
        );
        assert_eq!(
            release_path("https://github.com/o/r/releases/download/v1/r.zip"),
            None
        );
    }

    #[test]
    fn direct_downloads_are_left_alone() {
        for url in [
            "https://github.com/bkader/Dominos/archive/refs/heads/main.zip",
            "https://gitlab.com/Tsoukie/clique-3.3.5/-/archive/main/clique-3.3.5-main.zip",
        ] {
            assert_eq!(download_url(url), url);
        }
    }
}
