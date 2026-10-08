//! Builds the launcher's catalog from every upstream source: lists their
//! addons, downloads every archive to check it and record the folders it
//! installs, merges the listings of the same addon from different sources,
//! and writes the result.

mod fetch;
mod source;

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use old_launcher_core::catalog::Contents;
use old_launcher_core::{Catalog, Toc, catalog, toc};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::fetch::fetch;
use crate::source::{
    Listing, Source, Updated, github, locus313, nom0re, rising_gods, wowinterface,
};

/// Builds the launcher's addon catalog from every upstream source.
#[derive(Debug, Parser)]
struct Arguments {
    /// Where to write the catalog.
    #[arg(long, default_value = "index/wotlk.json")]
    output: PathBuf,
    /// Where to keep what was found in archives that never change, for the
    /// next run to reuse instead of downloading them again.
    #[arg(long, default_value = "index/archives.json")]
    archives: PathBuf,
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
    gathered.push(gather(&client, &wowinterface::Site).await?);
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

    let known = known_archives(&arguments.archives);
    let total = listings.len();
    let downloads = Arc::new(Semaphore::new(arguments.jobs.max(1)));
    let mut inspections = JoinSet::new();

    for (priority, source, listing) in listings {
        let client = client.clone();
        let downloads = Arc::clone(&downloads);
        let earlier = earlier(&listing, &known);

        let _ = inspections.spawn(async move {
            let reused = earlier.is_some();
            let contents = match earlier {
                Some(contents) => Ok(contents),
                None => {
                    async {
                        let _permit = downloads.acquire_owned().await?;

                        contents(&client, &listing.download).await
                    }
                    .await
                }
            };

            (priority, source, listing, contents, reused)
        });
    }

    let mut inspected = Vec::new();
    let mut skipped = Vec::new();
    // What this run found in archives that never change, for the next.
    let mut archives = BTreeMap::new();
    let mut reused_count = 0;

    while let Some(inspection) = inspections.join_next().await {
        let (priority, source, listing, contents, reused) = inspection?;
        let done = inspected.len() + skipped.len() + 1;
        let note = if reused {
            reused_count += 1;
            " (inspected before)"
        } else {
            ""
        };

        if listing.immutable
            && let Ok(contents) = &contents
        {
            let _ = archives.insert(listing.download.clone(), contents.clone());
        }

        let reason = match contents {
            Ok(contents) => match left_out(&listing, &contents) {
                Some(reason) => reason,
                None => {
                    println!(
                        "[{done}/{total}] {}: {} folders{note}",
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
            },
            Err(error) => format!("{error:#}"),
        };

        println!("[{done}/{total}] {}: skipped{note}, {reason}", listing.name);
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

            let mut urls = HashSet::new();
            let downloads = order(listings)
                .into_iter()
                .filter(|item| urls.insert(item.listing.download.clone()))
                .map(|item| catalog::Download {
                    source: item.source,
                    name: item.listing.name,
                    url: item.listing.download,
                    updated: item.updated,
                    page: item.listing.page,
                    changes: item.listing.changes,
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
                preferred: None,
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

    // Only the archives listed this run: the rest are gone or changed.
    if let Some(parent) = arguments.archives.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &arguments.archives,
        serde_json::to_string_pretty(&archives)? + "\n",
    )
    .with_context(|| format!("writing {}", arguments.archives.display()))?;

    println!(
        "\nWrote {} addons from {} sources to {} ({merged} listed by more than one source).",
        catalog.entries.len(),
        catalog.sources.len(),
        arguments.output.display()
    );
    println!(
        "Reused {reused_count} archives inspected before, and kept {} in {} for the next run.",
        archives.len(),
        arguments.archives.display()
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

/// What earlier runs found in archives that never change, by download URL,
/// as kept at `path`. Without the file, or when it doesn't read, every
/// archive is downloaded again, which costs time and nothing else.
fn known_archives(path: &Path) -> BTreeMap<String, Contents> {
    let json = match std::fs::read(path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(error) => {
            println!("Not reusing {}: {error}", path.display());
            return BTreeMap::new();
        }
    };

    serde_json::from_slice(&json).unwrap_or_else(|error| {
        println!("Not reusing {}: {error}", path.display());
        BTreeMap::new()
    })
}

/// What an earlier run found in `listing`'s archive, if it never changes
/// and was inspected before.
fn earlier(listing: &Listing, known: &BTreeMap<String, Contents>) -> Option<Contents> {
    if !listing.immutable {
        return None;
    }

    known.get(&listing.download).cloned()
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

/// Why `listing`, whose archive holds `contents`, stays out of the catalog,
/// if it does: the archive holds no addon; a folder declares a later
/// expansion's interface (40000 on), which a 3.3.5a client can't load;
/// or the source isn't curated for 3.3.5a and no folder declares its
/// interface. Other WotLK numbers (30200, or a typo like 33000) pass: the
/// card says what they were made for.
fn left_out(listing: &Listing, contents: &Contents) -> Option<String> {
    if contents.folders.is_empty() {
        return Some("the archive holds no addon".to_owned());
    }

    let later = contents.folders.iter().find_map(|folder| {
        folder
            .interface
            .filter(|interface| interface / 10_000 > Toc::WOTLK_INTERFACE / 10_000)
    });
    if let Some(interface) = later {
        return Some(format!(
            "a folder declares a later expansion's interface, {interface}"
        ));
    }

    let wotlk = contents
        .folders
        .iter()
        .any(|folder| folder.interface == Some(Toc::WOTLK_INTERFACE));
    if listing.require_wotlk && !wotlk {
        return Some("no folder declares the 3.3.5a interface".to_owned());
    }

    None
}

/// Downloads the archive at `url` and looks into it.
async fn contents(client: &reqwest::Client, url: &str) -> anyhow::Result<Contents> {
    let archive = fetch(client, url).await?;

    Ok(tokio::task::spawn_blocking(move || Contents::inspect(&archive)).await??)
}

/// Orders an addon's listings as the launcher tries them: those made for
/// 3.3.5a, as a folder's `.toc` declares, before the rest (a source that
/// isn't curated for it may list another game's version), and each group
/// newest first (see [`newest_first`]).
fn order(mut listings: Vec<Inspected>) -> Vec<Inspected> {
    let (wotlk, others): (Vec<_>, Vec<_>) = listings.drain(..).partition(|item| {
        item.contents
            .folders
            .iter()
            .any(|folder| folder.interface == Some(Toc::WOTLK_INTERFACE))
    });

    let mut ordered = newest_first(wotlk);
    ordered.extend(newest_first(others));

    ordered
}

/// Orders listings newest first: the most recently updated one, unless
/// another's version compares with its own and is newer; then the same
/// again among the rest. Ties go by the sources' order of preference, and
/// undated listings come last.
fn newest_first(mut listings: Vec<Inspected>) -> Vec<Inspected> {
    listings.sort_by_key(|item| {
        (
            item.updated.is_none(),
            Reverse(item.updated.clone()),
            item.priority,
        )
    });

    let mut ordered = Vec::with_capacity(listings.len());

    while !listings.is_empty() {
        let mut best = 0;

        for candidate in 1..listings.len() {
            if compare(&listings[candidate], &listings[best]) == Some(Ordering::Greater) {
                best = candidate;
            }
        }

        ordered.push(listings.remove(best));
    }

    ordered
}

/// How `a`'s version compares with `b`'s, going by the first folder (by
/// name) both install with a version; nothing when the versions don't
/// compare (see [`toc::compare_versions`]).
fn compare(a: &Inspected, b: &Inspected) -> Option<Ordering> {
    let mut folders: Vec<&catalog::Folder> = a.contents.folders.iter().collect();
    folders.sort_by_cached_key(|folder| folder.name.to_lowercase());

    folders.into_iter().find_map(|folder| {
        let version = folder.version.as_deref()?;
        let other = b
            .contents
            .folders
            .iter()
            .find(|other| other.name.eq_ignore_ascii_case(&folder.name))?
            .version
            .as_deref()?;

        Some(toc::compare_versions(version, other))
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listing of one folder at `version`, from the source of that
    /// `priority`, updated on `updated`.
    fn inspected(priority: usize, version: &str, updated: Option<&str>) -> Inspected {
        Inspected {
            priority,
            source: format!("source-{priority}"),
            listing: Listing {
                name: version.to_owned(),
                description: String::new(),
                download: format!("https://example.com/{priority}.zip"),
                image: None,
                page: None,
                changes: None,
                links: Vec::new(),
                updated: Updated::FromArchive,
                require_wotlk: false,
                immutable: false,
            },
            contents: Contents {
                folders: vec![catalog::Folder {
                    name: "Addon".into(),
                    title: None,
                    version: Some(version.to_owned()),
                    interface: Some(Toc::WOTLK_INTERFACE),
                }],
                updated: None,
            },
            updated: updated.map(str::to_owned),
        }
    }

    /// An archive of one folder per interface given.
    fn folders(interfaces: &[Option<u32>]) -> Contents {
        Contents {
            folders: interfaces
                .iter()
                .enumerate()
                .map(|(index, interface)| catalog::Folder {
                    name: format!("Folder{index}"),
                    title: None,
                    version: None,
                    interface: *interface,
                })
                .collect(),
            updated: None,
        }
    }

    #[test]
    fn only_archives_that_never_change_are_reused() {
        let mut listing = inspected(0, "1.0", None).listing;
        let known = BTreeMap::from([(listing.download.clone(), folders(&[Some(30300)]))]);

        assert_eq!(earlier(&listing, &known), None, "it may have changed");

        listing.immutable = true;
        assert_eq!(earlier(&listing, &known), Some(folders(&[Some(30300)])));

        listing.download = "https://example.com/new.zip".into();
        assert_eq!(earlier(&listing, &known), None, "never inspected");
    }

    #[test]
    fn archives_found_before_are_read_back_and_a_bad_record_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archives.json");
        assert!(known_archives(&path).is_empty(), "no record yet");

        let known = BTreeMap::from([(
            "https://example.com/a.zip".to_owned(),
            folders(&[Some(30300), None]),
        )]);
        std::fs::write(&path, serde_json::to_string_pretty(&known).unwrap()).unwrap();
        assert_eq!(known_archives(&path), known);

        std::fs::write(&path, "{ not json").unwrap();
        assert!(known_archives(&path).is_empty(), "a broken record");
    }

    #[test]
    fn listings_for_later_expansions_stay_out() {
        let curated = inspected(0, "1.0", None).listing;
        let mut uncurated = curated.clone();
        uncurated.require_wotlk = true;

        assert_eq!(left_out(&curated, &folders(&[Some(30300)])), None);
        assert_eq!(
            left_out(&curated, &folders(&[Some(30200)])),
            None,
            "an older patch"
        );
        assert_eq!(left_out(&curated, &folders(&[Some(33000)])), None, "a typo");
        assert_eq!(left_out(&curated, &folders(&[None])), None, "unknown");
        assert_eq!(
            left_out(&curated, &folders(&[Some(30300), Some(40000)])).as_deref(),
            Some("a folder declares a later expansion's interface, 40000")
        );
        assert_eq!(
            left_out(&curated, &folders(&[])).as_deref(),
            Some("the archive holds no addon")
        );
        assert_eq!(
            left_out(&uncurated, &folders(&[Some(30200)])).as_deref(),
            Some("no folder declares the 3.3.5a interface")
        );
        assert_eq!(left_out(&uncurated, &folders(&[Some(30300), None])), None);
    }

    #[test]
    fn versions_made_for_another_game_come_last_whatever_their_number() {
        // BindPad as Rising Gods lists it: 2.3.3, but for Cataclysm.
        let mut cataclysm = inspected(0, "2.3.3", Some("2026-01-01"));
        cataclysm.contents.folders[0].interface = Some(40000);
        let mut unknown = inspected(1, "2.4.0", Some("2026-01-01"));
        unknown.contents.folders[0].interface = None;

        let ordered = order(vec![
            cataclysm,
            unknown,
            inspected(2, "2.2.4", Some("2023-08-26")),
        ]);
        let versions: Vec<&str> = ordered
            .iter()
            .map(|item| item.listing.name.as_str())
            .collect();

        assert_eq!(versions, ["2.2.4", "2.4.0", "2.3.3"]);
    }

    #[test]
    fn the_newest_version_comes_first_and_the_date_places_the_rest() {
        let ordered = order(vec![
            inspected(0, "1.0", Some("2026-01-01")),
            inspected(1, "2.0", Some("2025-01-01")),
            inspected(2, "r5", Some("2025-06-01")),
            inspected(3, "1.5", None),
        ]);
        let versions: Vec<&str> = ordered
            .iter()
            .map(|item| item.listing.name.as_str())
            .collect();

        // 2.0 beats the freshest, 1.0, by version, and so does the undated
        // 1.5; r5 compares with none of them, so its date places it.
        assert_eq!(versions, ["2.0", "1.5", "1.0", "r5"]);
    }

    #[test]
    fn same_versions_go_by_date_then_preference() {
        let ordered = order(vec![
            inspected(1, "1.0", Some("2025-01-01")),
            inspected(0, "1.0", Some("2025-01-01")),
            inspected(2, "1.0", Some("2026-01-01")),
        ]);
        let sources: Vec<&str> = ordered.iter().map(|item| item.source.as_str()).collect();

        assert_eq!(sources, ["source-2", "source-0", "source-1"]);
    }
}
