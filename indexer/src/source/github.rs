//! Backport maintainers on GitHub: every repository of theirs that holds a
//! 3.3.5a addon.
//!
//! Each of the maintainer's own repositories (not forks) is listed, and the
//! indexer keeps those whose archive has a folder declaring the 3.3.5a
//! interface. Listing costs one GitHub API call per hundred repositories;
//! archive downloads don't count against the API limits. Setting
//! `GITHUB_TOKEN` raises the limits if they ever get in the way.

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
];

/// Collections of addons, which are read through their own index instead.
const EXCLUDED: &[&str] = &["NoM0Re/WoW-3.3.5a-Addons", "NoM0Re/WoW-3.3.5a-Addons2"];

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

/// Every repository of `owner`'s that may hold an addon: their own (not
/// forks), minus collections and oversized ones. Only those whose archive
/// has a folder declaring the 3.3.5a interface are kept by the indexer.
pub async fn repositories(client: &reqwest::Client, owner: &str) -> anyhow::Result<Vec<Listing>> {
    #[derive(Deserialize)]
    struct Repository {
        name: String,
        full_name: String,
        description: Option<String>,
        fork: bool,
        size: u64,
        default_branch: String,
        html_url: String,
    }

    let mut listings = Vec::new();

    for page in 1.. {
        let repositories: Vec<Repository> = api(
            client,
            &format!("users/{owner}/repos?type=owner&per_page=100&page={page}"),
        )
        .await?;
        let last = repositories.len() < 100;

        listings.extend(
            repositories
                .into_iter()
                .filter(|repository| {
                    !repository.fork
                        && repository.size <= MAX_SIZE
                        && !EXCLUDED.contains(&repository.full_name.as_str())
                })
                .map(|repository| Listing {
                    download: format!(
                        "https://github.com/{}/archive/refs/heads/{}.zip",
                        repository.full_name, repository.default_branch
                    ),
                    name: repository.name,
                    description: repository.description.unwrap_or_default().trim().to_owned(),
                    image: None,
                    page: Some(repository.html_url),
                    links: Vec::new(),
                    // A branch snapshot's files carry the last commit's date.
                    updated: Updated::FromArchive,
                    require_wotlk: true,
                }),
        );

        if last {
            break;
        }
    }

    Ok(listings)
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
