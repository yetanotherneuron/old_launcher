//! Builds the launcher's catalog from every upstream source: lists their
//! addons, downloads every archive to check it and record the folders it
//! installs, merges the listings of the same addon from different sources,
//! and writes the result.

mod fetch;
mod source;

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use old_launcher_core::catalog::Contents;
use old_launcher_core::{Catalog, Toc, catalog};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::fetch::fetch;
use crate::source::{Listing, Source, Updated, github, locus313, nom0re, rising_gods};

/// Builds the launcher's addon catalog from every upstream source.
#[derive(Debug, Parser)]
struct Arguments {
    /// Where to write the catalog.
    #[arg(long, default_value = "index/wotlk.json")]
    output: PathBuf,
    /// How many archives to download at once.
    #[arg(long, default_value_t = 8)]
    jobs: usize,
}

/// A listing whose archive was downloaded and looked into.
struct Inspected {
    /// The source's position in the order of preference: lower is preferred.
    priority: usize,
    source: String,
    listing: Listing,
    contents: Contents,
    /// When the addon was last updated, wherever the listing says to look.
    updated: Option<String>,
}

/// Hosts upstream sources link to that no longer serve those pages: gone,
/// or taken over by someone else.
const DEAD_LINK_HOSTS: &[&str] = &["wotlkaddons.com", "felbite.com"];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();

    // Fails only when a provider is already installed, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let client = reqwest::Client::builder()
        .user_agent(concat!("old_launcher-indexer/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()?;

    // Every source, in order of preference: when several list the same addon,
    // the first describes it (so English descriptions come before German
    // ones).
    let mut gathered = vec![
        gather(&client, &nom0re::Collection).await?,
        gather(&client, &locus313::Collection).await?,
    ];
    for owner in github::MAINTAINERS {
        gathered.push(gather(&client, &github::Maintainer(owner)).await?);
    }
    gathered.push(gather(&client, &rising_gods::Collection).await?);

    let mut sources = Vec::new();
    let mut listings = Vec::new();

    for (priority, (info, listed)) in gathered.into_iter().enumerate() {
        listings.extend(
            listed
                .into_iter()
                .map(|listing| (priority, info.id.clone(), listing)),
        );
        sources.push(info);
    }

    // Markers identify downloads by source and name, so those must be unique.
    let mut seen = HashSet::new();
    listings.retain(|(_, source, listing)| seen.insert((source.clone(), listing.name.clone())));

    let total = listings.len();
    let downloads = Arc::new(Semaphore::new(arguments.jobs.max(1)));
    let mut inspections = JoinSet::new();

    for (priority, source, listing) in listings {
        let client = client.clone();
        let downloads = Arc::clone(&downloads);

        let _ = inspections.spawn(async move {
            let contents = async {
                let _permit = downloads.acquire_owned().await?;

                contents(&client, &listing.download).await
            }
            .await;

            (priority, source, listing, contents)
        });
    }

    let mut inspected = Vec::new();
    let mut skipped = Vec::new();

    while let Some(inspection) = inspections.join_next().await {
        let (priority, source, listing, contents) = inspection?;
        let done = inspected.len() + skipped.len() + 1;

        let reason = match contents {
            Ok(contents) if contents.folders.is_empty() => "the archive holds no addon".to_owned(),
            Ok(contents)
                if listing.require_wotlk
                    && !contents
                        .folders
                        .iter()
                        .any(|folder| folder.interface == Some(Toc::WOTLK_INTERFACE)) =>
            {
                "no folder declares the 3.3.5a interface".to_owned()
            }
            Ok(contents) => {
                println!(
                    "[{done}/{total}] {}: {} folders",
                    listing.name,
                    contents.folders.len()
                );
                let updated = match &listing.updated {
                    Updated::FromArchive => contents.updated.clone(),
                    Updated::Given(date) => date.clone(),
                };

                inspected.push(Inspected {
                    priority,
                    source,
                    listing,
                    contents,
                    updated,
                });
                continue;
            }
            Err(error) => format!("{error:#}"),
        };

        println!("[{done}/{total}] {}: skipped, {reason}", listing.name);
        skipped.push(format!("{source} / {}: {reason}", listing.name));
    }

    // Listings that install the same folders are the same addon, whichever
    // source lists them.
    let mut addons: BTreeMap<Vec<String>, Vec<Inspected>> = BTreeMap::new();

    for item in inspected {
        let mut folders: Vec<String> = item
            .contents
            .folders
            .iter()
            .map(|folder| folder.name.to_lowercase())
            .collect();
        folders.sort();

        addons.entry(folders).or_default().push(item);
    }

    let merged = addons
        .values()
        .filter(|listings| listings.len() > 1)
        .count();

    let entries: Vec<catalog::Entry> = addons
        .into_values()
        .map(|mut listings| {
            // Sources are listed in order of preference, and the preferred
            // one describes the addon.
            listings.sort_by_key(|item| item.priority);

            let name = listings[0].listing.name.clone();
            let description = listings
                .iter()
                .map(|item| &item.listing.description)
                .find(|description| !description.is_empty())
                .cloned()
                .unwrap_or_default();
            let image = listings.iter().find_map(|item| item.listing.image.clone());

            // The most recently updated download is installed first; undated
            // ones come last.
            listings.sort_by_key(|item| {
                (
                    item.updated.is_none(),
                    Reverse(item.updated.clone()),
                    item.priority,
                )
            });

            let mut urls = HashSet::new();
            let downloads = listings
                .into_iter()
                .filter(|item| urls.insert(item.listing.download.clone()))
                .map(|item| catalog::Download {
                    source: item.source,
                    name: item.listing.name,
                    url: item.listing.download,
                    updated: item.updated,
                    page: item.listing.page,
                    links: item
                        .listing
                        .links
                        .into_iter()
                        .filter(|link| {
                            let host = link
                                .url
                                .split_once("://")
                                .and_then(|(_, rest)| rest.split('/').next())
                                .unwrap_or_default()
                                .to_ascii_lowercase();

                            !DEAD_LINK_HOSTS
                                .iter()
                                .any(|dead| host == *dead || host.ends_with(&format!(".{dead}")))
                        })
                        .collect(),
                    folders: item.contents.folders,
                })
                .collect();

            catalog::Entry {
                name,
                description,
                image,
                downloads,
            }
        })
        .collect();

    if entries.is_empty() {
        bail!(
            "no entry survived inspection; not overwriting {}",
            arguments.output.display()
        );
    }

    let catalog = Catalog {
        version: Catalog::VERSION,
        sources,
        entries,
    };

    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&arguments.output, catalog.to_json()?)
        .with_context(|| format!("writing {}", arguments.output.display()))?;

    println!(
        "\nWrote {} addons from {} sources to {} ({merged} listed by more than one source).",
        catalog.entries.len(),
        catalog.sources.len(),
        arguments.output.display()
    );

    if !skipped.is_empty() {
        skipped.sort();
        println!("Skipped {}:", skipped.len());
        for line in &skipped {
            println!("  {line}");
        }
    }

    Ok(())
}

/// Lists everything `source` offers, with how the catalog presents it.
///
/// A source that cannot be listed fails the run, rather than quietly
/// publishing a catalog without it.
async fn gather(
    client: &reqwest::Client,
    source: &impl Source,
) -> anyhow::Result<(catalog::Source, Vec<Listing>)> {
    let info = source.info();
    let listed = source
        .list(client)
        .await
        .with_context(|| format!("listing {}", info.name))?;

    println!("{}: {} listed", info.name, listed.len());

    Ok((info, listed))
}

/// Downloads the archive at `url` and looks into it.
async fn contents(client: &reqwest::Client, url: &str) -> anyhow::Result<Contents> {
    let archive = fetch(client, url).await?;

    Ok(tokio::task::spawn_blocking(move || Contents::inspect(&archive)).await??)
}
