//! Backport maintainers on GitHub: every repository of theirs that holds a
//! 3.3.5a addon.
//!
//! Each of the maintainer's own repositories (not forks, but for those named
//! in `FORKS`) is listed, and the indexer keeps those whose archive has a
//! folder declaring the 3.3.5a interface. Listing costs one GitHub API call
//! per hundred repositories; archive downloads don't count against the API
//! limits. Setting `GITHUB_TOKEN` raises the limits if they ever get in the
//! way.
//!
//! A single repository of someone who isn't a maintainer is listed by its
//! full name, in `REPOSITORIES`, the same way.

use anyhow::Context;
use old_launcher_core::catalog;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::source::{Listing, Source, Updated};

/// The maintainers indexed, in order of preference. NoM0Re is not one: their
/// repositories are listed with their collection, as one source.
pub const MAINTAINERS: &[&str] = &[
    "ElvUI-WotLK",
    "bkader",
    "Bunny67",
    "Zidras",
    "s0h2x",
    "KhalGH",
    "Sattva-108",
    "Xurkon",
    "Wrath-AddOns",
];

/// Forks listed like the maintainer's own repositories: most forks are
/// copies, but these carry backports of their own.
const FORKS: &[&str] = &[
    "Wrath-AddOns/TradeSkillMaster",
    "Wrath-AddOns/Pawn-WOTLK",
    "Wrath-AddOns/Tale-WotLK",
];

/// Collections of addons, which are read through their own index instead.
const EXCLUDED: &[&str] = &["NoM0Re/WoW-3.3.5a-Addons", "NoM0Re/WoW-3.3.5a-Addons2"];

/// Repositories whose releases attach each addon as a zip of its own:
/// Trimitor's, made for his Dungeon Maps.
pub const COLLECTIONS: &[&str] = &["Trimitor/WDM-addons"];

/// Single repositories, by full name (`owner/name`), of owners who aren't
/// maintainers: the rest of what they keep isn't 3.3.5a addons.
pub const REPOSITORIES: &[&str] = &["UndeadRogue/wow-3.3.5a-addons"];

/// Repositories bigger than this (in KB, as GitHub reports it) are
/// collections or asset dumps, not an addon.
const MAX_SIZE: u64 = 150_000;

/// A backport maintainer, by GitHub user or organization name.
pub struct Maintainer(pub &'static str);

impl Source for Maintainer {
    fn info(&self) -> catalog::Source {
        let Self(owner) = self;

        catalog::Source {
            id: format!("github-{}", owner.to_lowercase()),
            name: format!("{owner} on GitHub"),
            homepage: format!("https://github.com/{owner}"),
        }
    }

    /// Lists the maintainer's own repositories.
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>> {
        let Self(owner) = self;

        repositories(client, owner).await
    }
}

/// A repository whose releases attach each addon as a zip of its own, by
/// its full name (`owner/name`): those of its latest stable release are
/// listed.
pub struct Collection(pub &'static str);

impl Source for Collection {
    fn info(&self) -> catalog::Source {
        let Self(repository) = self;
        let owner = repository.split('/').next().unwrap_or(repository);

        catalog::Source {
            id: format!("github-{}", repository.to_lowercase().replace('/', "-")),
            name: format!("{owner} on GitHub"),
            homepage: format!("https://github.com/{repository}"),
        }
    }

    /// Lists the zips of the latest stable release.
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>> {
        let Self(repository) = self;
        let latest: Latest = api(client, &format!("repos/{repository}/releases/latest")).await?;

        Ok(listings(repository, latest))
    }
}

/// A single repository, by its full name (`owner/name`), whose branch
/// snapshot is listed like a maintainer's.
pub struct Repository(pub &'static str);

impl Source for Repository {
    fn info(&self) -> catalog::Source {
        let Self(repository) = self;
        let owner = repository.split('/').next().unwrap_or(repository);

        catalog::Source {
            id: format!("github-{}", repository.to_lowercase().replace('/', "-")),
            name: format!("{owner} on GitHub"),
            homepage: format!("https://github.com/{repository}"),
        }
    }

    /// Lists the repository, whatever its size or whether it is a fork: it
    /// was picked by name.
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>> {
        let Self(repository) = self;
        let described: Described = api(client, &format!("repos/{repository}")).await?;

        Ok(vec![listing(described)])
    }
}

/// A release, as GitHub's API describes it, with what a listing needs.
#[derive(Deserialize)]
struct Latest {
    html_url: String,
    published_at: Option<String>,
    assets: Vec<Attached>,
}

/// A file attached to a release.
#[derive(Deserialize)]
struct Attached {
    name: String,
    browser_download_url: String,
}

/// The addons of the `repository`'s release `latest`: its zips, each named
/// after its file, with the release's date. A release's files keep their
/// URLs: one uploaded again under its name is told apart by the indexer
/// (see `uploads`).
fn listings(repository: &str, latest: Latest) -> Vec<Listing> {
    let day = latest
        .published_at
        .as_deref()
        .and_then(|date| date.get(..10))
        .map(str::to_owned);

    latest
        .assets
        .into_iter()
        .filter_map(|asset| {
            let name = asset.name.strip_suffix(".zip")?.to_owned();

            Some(Listing {
                name,
                description: String::new(),
                download: asset.browser_download_url,
                image: None,
                page: Some(format!("https://github.com/{repository}")),
                links: Vec::new(),
                changes: Some(latest.html_url.clone()),
                updated: Updated::Given(day.clone()),
                require_wotlk: true,
                immutable: true,
            })
        })
        .collect()
}

/// Every repository of `owner`'s that may hold an addon: their own (not
/// forks, but for those in `FORKS`), minus collections and oversized ones.
/// Only those whose archive has a folder declaring the 3.3.5a interface are
/// kept by the indexer.
pub async fn repositories(client: &reqwest::Client, owner: &str) -> anyhow::Result<Vec<Listing>> {
    let mut listings = Vec::new();

    for page in 1.. {
        let repositories: Vec<Described> = api(
            client,
            &format!("users/{owner}/repos?type=owner&per_page=100&page={page}"),
        )
        .await?;
        let last = repositories.len() < 100;

        listings.extend(
            repositories
                .into_iter()
                .filter(|repository| {
                    (!repository.fork || FORKS.contains(&repository.full_name.as_str()))
                        && repository.size <= MAX_SIZE
                        && !EXCLUDED.contains(&repository.full_name.as_str())
                })
                .map(listing),
        );

        if last {
            break;
        }
    }

    Ok(listings)
}

/// A repository, as GitHub's API describes it, with what a listing needs.
#[derive(Deserialize)]
struct Described {
    name: String,
    full_name: String,
    description: Option<String>,
    fork: bool,
    size: u64,
    default_branch: String,
    html_url: String,
}

/// The listing of `repository`: a snapshot of its default branch.
fn listing(repository: Described) -> Listing {
    Listing {
        download: format!(
            "https://github.com/{}/archive/refs/heads/{}.zip",
            repository.full_name, repository.default_branch
        ),
        name: repository.name,
        description: repository.description.unwrap_or_default().trim().to_owned(),
        image: None,
        changes: Some(format!(
            "{}/commits/{}",
            repository.html_url, repository.default_branch
        )),
        page: Some(repository.html_url),
        links: Vec::new(),
        // A branch snapshot's files carry the last commit's date.
        updated: Updated::FromArchive,
        require_wotlk: true,
        // The branch moves on under the same URL, until the indexer pins it
        // to its commit (see `pin`).
        immutable: false,
    }
}

/// Calls the GitHub REST API at `path` (relative to `https://api.github.com/`),
/// authenticated when `GITHUB_TOKEN` is set.
pub async fn api<T: DeserializeOwned>(client: &reqwest::Client, path: &str) -> anyhow::Result<T> {
    let url = format!("https://api.github.com/{path}");
    let mut request = client
        .get(&url)
        .header("Accept", "application/vnd.github+json");

    let authenticated = match std::env::var("GITHUB_TOKEN") {
        Ok(token) if !token.is_empty() => {
            request = request.bearer_auth(token);
            true
        }
        _ => false,
    };

    let response = request
        .send()
        .await
        .with_context(|| format!("calling {url}"))?;
    let status = response.status();

    if !authenticated && (status.as_u16() == 403 || status.as_u16() == 429) {
        anyhow::bail!(
            "GitHub's API limit for anonymous calls was reached ({status} for {url}); \
             set GITHUB_TOKEN, for example: GITHUB_TOKEN=$(gh auth token)"
        );
    }

    let body = response
        .error_for_status()
        .with_context(|| format!("calling {url}"))?
        .bytes()
        .await?;

    serde_json::from_slice(&body).with_context(|| format!("reading {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collection_lists_each_zip_of_its_latest_release() {
        let latest: Latest = serde_json::from_str(
            r#"{
                "html_url": "https://github.com/Trimitor/WDM-addons/releases/tag/1.0.9-stable",
                "published_at": "2026-06-11T09:30:00Z",
                "assets": [
                    { "name": "WDM.zip", "browser_download_url": "https://example.com/WDM.zip" },
                    { "name": "Astrolabe.zip", "browser_download_url": "https://example.com/Astrolabe.zip" },
                    { "name": "notes.txt", "browser_download_url": "https://example.com/notes.txt" }
                ]
            }"#,
        )
        .unwrap();

        let listed = listings("Trimitor/WDM-addons", latest);

        assert_eq!(
            listed
                .iter()
                .map(|listing| listing.name.as_str())
                .collect::<Vec<_>>(),
            ["WDM", "Astrolabe"]
        );
        assert_eq!(listed[0].download, "https://example.com/WDM.zip");
        assert_eq!(
            listed[0].updated,
            Updated::Given(Some("2026-06-11".to_owned()))
        );
        assert!(listed[0].immutable && listed[0].require_wotlk);
        assert_eq!(
            Collection("Trimitor/WDM-addons").info().id,
            "github-trimitor-wdm-addons"
        );
    }

    #[test]
    fn a_single_repository_is_listed_as_a_maintainers_is() {
        let source = Repository("UndeadRogue/wow-3.3.5a-addons").info();
        let described: Described = serde_json::from_str(
            r#"{
                "name": "wow-3.3.5a-addons",
                "full_name": "UndeadRogue/wow-3.3.5a-addons",
                "description": " Addons and ElvUI plugins. ",
                "fork": false,
                "size": 4000,
                "default_branch": "main",
                "html_url": "https://github.com/UndeadRogue/wow-3.3.5a-addons"
            }"#,
        )
        .unwrap();

        let listed = listing(described);

        assert_eq!(source.id, "github-undeadrogue-wow-3.3.5a-addons");
        assert_eq!(source.name, "UndeadRogue on GitHub");
        assert_eq!(
            listed.download,
            "https://github.com/UndeadRogue/wow-3.3.5a-addons/archive/refs/heads/main.zip"
        );
        assert_eq!(listed.description, "Addons and ElvUI plugins.");
        assert!(!listed.immutable && listed.require_wotlk);
    }
}
