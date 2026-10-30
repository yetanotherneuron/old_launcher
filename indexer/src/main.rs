//! Builds the launcher's catalog from every upstream source: lists their
//! addons, pins the downloads that move to what they serve now (see
//! [`pin`]), downloads every archive to check it and record the folders it
//! installs, merges the listings of the same addon from different sources,
//! and writes the result. Then the same for the list of mods.
//!
//! What was found in each archive is kept for the next run, which downloads
//! only what may have changed: an archive that never changes isn't
//! downloaded again, but for a release's file uploaded again under the same
//! name (see [`uploads`]), and one that may is asked about, when its server
//! sent what tells its versions apart (see [`fetch::Validators`]).

mod fetch;
mod mods;
mod pin;
mod source;
mod uploads;

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use old_launcher_core::catalog::Contents;
use old_launcher_core::mods::Mods;
use old_launcher_core::{Catalog, Toc, catalog, toc};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::fetch::{Fetched, Validators, fetch_changed};
use crate::pin::Pins;
use crate::source::{
    Listing, Source, Updated, curseforge, github, gitlab, locus313, nom0re, rising_gods,
    wowinterface,
};
use crate::uploads::Uploads;

/// Builds the launcher's addon catalog from every upstream source.
#[derive(Debug, Parser)]
struct Arguments {
    /// Where to write the catalog.
    #[arg(long, default_value = "index/wotlk.json")]
    output: PathBuf,
    /// Where to keep what was found in archives, for the next run to reuse
    /// instead of downloading them again: those that never change, and
    /// those whose server can say whether they did.
    #[arg(long, default_value = "index/archives.json")]
    archives: PathBuf,
    /// How many archives to download, or repositories to look up, at once.
    #[arg(long, default_value_t = 8)]
    jobs: usize,
    /// Where to write the list of mods.
    #[arg(long, default_value = "index/mods.json")]
    mods: PathBuf,
    /// Where to keep what was found in the mods' downloads, for the next run
    /// to reuse instead of downloading them again.
    #[arg(long, default_value = "index/mod-archives.json")]
    mod_archives: PathBuf,
    /// Only build the list of mods, leaving the catalog of addons as it is.
    #[arg(long)]
    mods_only: bool,
    /// Leave CurseForge out, for a run without its API key. The catalog
    /// written lacks its addons, and the record of archives the ones found
    /// there before: neither is for publishing.
    #[arg(long)]
    skip_curseforge: bool,
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

/// What was found in an archive, as kept for the next run, by its URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Archive {
    #[serde(flatten)]
    contents: Contents,
    /// For an archive whose URL may serve another one, what its server sent
    /// to tell the version looked into: the next run asks whether it is
    /// still that one. None for an archive that never changes.
    #[serde(flatten)]
    validators: Validators,
    /// For a release's file, the id of the upload looked into, which
    /// another upload under the same name doesn't share (see [`uploads`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    asset: Option<u64>,
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

    if !arguments.mods_only {
        addons(&client, &arguments).await?;
    }

    if !list_mods(&client, &arguments).await? {
        // Everything is written, and as good as it gets: what the schedule
        // commits. The code tells it to fail afterwards, so someone hears.
        std::process::exit(MISSED);
    }

    Ok(())
}

/// The exit code when the files are written but a mod's newest release
/// isn't in them.
const MISSED: i32 = 3;

/// Builds the catalog of addons and writes it, with what was found in
/// archives for the next run.
async fn addons(client: &reqwest::Client, arguments: &Arguments) -> anyhow::Result<()> {
    // Every source, in order of preference: when several list the same addon,
    // the first describes it (so English descriptions come before German
    // ones).
    let mut gathered = vec![
        gather(client, &nom0re::Collection).await?,
        gather(client, &locus313::Collection).await?,
    ];
    for owner in github::MAINTAINERS {
        gathered.push(gather(client, &github::Maintainer(owner)).await?);
    }
    for repository in github::COLLECTIONS {
        gathered.push(gather(client, &github::Collection(repository)).await?);
    }
    for author in gitlab::AUTHORS {
        gathered.push(gather(client, author).await?);
    }
    if arguments.skip_curseforge {
        println!("CurseForge: skipped");
    } else {
        gathered.push(gather(client, &curseforge::Site).await?);
    }
    gathered.push(gather(client, &wowinterface::Site).await?);
    gathered.push(gather(client, &rising_gods::Collection).await?);

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

    let pins = Pins::look_up(
        client,
        listings.iter().map(|(_, _, listing)| listing),
        arguments.jobs,
    )
    .await?;
    // A download that moves is pinned to what it serves now, which the
    // catalog then describes; one that can't be is skipped.
    let listings: Vec<_> = listings
        .into_iter()
        .map(|(priority, source, mut listing)| {
            let pinned = pins.pin(&mut listing);

            (priority, source, listing, pinned)
        })
        .collect();
    let uploads = Uploads::look_up(
        client,
        listings
            .iter()
            .map(|(_, _, listing, _)| listing.download.as_str()),
    )
    .await;
    let known = known_archives(&arguments.archives);
    let total = listings.len();
    let downloads = Arc::new(Semaphore::new(arguments.jobs.max(1)));
    let mut inspections = JoinSet::new();

    for (priority, source, listing, pinned) in listings {
        let client = client.clone();
        let downloads = Arc::clone(&downloads);
        let asset = uploads.id(&listing.download);
        let earlier = earlier(&listing, asset, &known);

        let _ = inspections.spawn(async move {
            let inspection = async {
                pinned?;

                match earlier {
                    // It never changes, and was looked into before.
                    Some(archive) if listing.immutable => Ok((archive, true)),
                    earlier => {
                        let _permit = downloads.acquire_owned().await?;
                        let (archive, reused) =
                            inspect(&client, &listing.download, earlier).await?;

                        // Looked into as uploaded now, for a release's file.
                        anyhow::Ok((Archive { asset, ..archive }, reused))
                    }
                }
            }
            .await;

            (priority, source, listing, inspection)
        });
    }

    let mut inspected = Vec::new();
    let mut skipped = Vec::new();
    // What this run found in archives, for the next.
    let mut archives = BTreeMap::new();
    let mut reused_count = 0;

    while let Some(inspection) = inspections.join_next().await {
        let (priority, source, listing, inspection) = inspection?;
        let done = inspected.len() + skipped.len() + 1;
        let note = if let Ok((_, true)) = &inspection {
            reused_count += 1;
            " (inspected before)"
        } else {
            ""
        };

        if let Ok((archive, _)) = &inspection
            && let Some(archive) = kept(&listing, archive)
        {
            let _ = archives.insert(listing.download.clone(), archive);
        }

        let reason = match inspection {
            Ok((Archive { contents, .. }, _)) => match left_out(&listing, &contents) {
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
            let description = description(&listings);
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

/// Builds the list of mods and writes it, with what was found in their
/// releases' downloads: whether every mod's newest release is in it. One
/// left out newer than those listed means the launcher offers an older
/// one until the indexer is taught the new one, which is said here.
async fn list_mods(client: &reqwest::Client, arguments: &Arguments) -> anyhow::Result<bool> {
    let known = mods::known(&arguments.mod_archives);
    let mods::Gathered {
        mods: listed,
        found,
        missed,
        skipped,
    } = mods::gather(client, &known).await?;

    let list = Mods {
        version: Mods::VERSION,
        mods: listed,
    };

    for (path, json) in [
        (&arguments.mods, list.to_json()?),
        (
            &arguments.mod_archives,
            serde_json::to_string_pretty(&found)? + "\n",
        ),
    ] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))?;
    }

    println!(
        "\nWrote {} mods with {} releases to {}.",
        list.mods.len(),
        list.mods
            .iter()
            .map(|listed| listed.releases.len())
            .sum::<usize>(),
        arguments.mods.display()
    );
    if !skipped.is_empty() {
        println!("Left out {}, older than those listed:", skipped.len());
        for line in &skipped {
            println!("  {line}");
        }
    }
    if !missed.is_empty() {
        println!(
            "Left out {}, NEWER than those listed: the launcher offers older ones until the \
             indexer understands these:",
            missed.len()
        );
        for line in &missed {
            println!("  {line}");
        }
    }

    Ok(missed.is_empty())
}

/// What earlier runs found in archives, by download URL, as kept at `path`.
/// Without the file, or when it doesn't read, every archive is downloaded
/// again, which costs time and nothing else.
fn known_archives(path: &Path) -> BTreeMap<String, Archive> {
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

/// What an earlier run found in `listing`'s archive, if it was inspected
/// before and the finding is of use: as it is, when the archive never
/// changes; to ask its server whether it is still the same, when it may
/// have changed. A release's file is the same only while it is the same
/// upload: `asset`, the id of the one there now, when it could be read.
fn earlier(
    listing: &Listing,
    asset: Option<u64>,
    known: &BTreeMap<String, Archive>,
) -> Option<Archive> {
    known
        .get(&listing.download)
        .filter(|archive| {
            if uploads::repository(&listing.download).is_some() {
                uploads::same(archive.asset, asset)
            } else {
                listing.immutable || !archive.validators.is_empty()
            }
        })
        .cloned()
}

/// What to keep for the next run of `archive`, found at `listing`'s
/// download: what is in it (and the upload it is, for a release's file),
/// when it never changes; that and what tells its version, when its server
/// can say whether it changed; nothing otherwise, so it is downloaded again.
fn kept(listing: &Listing, archive: &Archive) -> Option<Archive> {
    if listing.immutable {
        return Some(Archive {
            validators: Validators::default(),
            ..archive.clone()
        });
    }

    (!archive.validators.is_empty()).then(|| archive.clone())
}

/// What an addon does, as the first of its `listings` that says so tells
/// it (they are in order of preference), or else as its own `.toc` notes
/// do: the game shows those in its addon list too.
fn description(listings: &[Inspected]) -> String {
    listings
        .iter()
        .map(|item| &item.listing.description)
        .find(|description| !description.is_empty())
        .or_else(|| {
            listings
                .iter()
                .find_map(|item| item.contents.notes.as_ref())
        })
        .cloned()
        .unwrap_or_default()
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

/// Downloads the archive at `url` and looks into it, unless its server says
/// it is still the one `earlier` was found in (by the validators it sent
/// then): then that finding stands. Says which, true for the earlier one.
async fn inspect(
    client: &reqwest::Client,
    url: &str,
    earlier: Option<Archive>,
) -> anyhow::Result<(Archive, bool)> {
    let validators = earlier
        .as_ref()
        .map(|archive| archive.validators.clone())
        .unwrap_or_default();

    match fetch_changed(client, url, &validators).await? {
        Fetched::Unchanged => earlier
            .map(|archive| (archive, true))
            .with_context(|| format!("{url} answered that nothing changed, but it wasn't asked")),
        Fetched::Changed(archive, validators) => {
            let contents =
                tokio::task::spawn_blocking(move || Contents::inspect(&archive)).await??;

            Ok((
                Archive {
                    contents,
                    validators,
                    asset: None,
                },
                false,
            ))
        }
    }
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
                notes: None,
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
            notes: None,
            updated: None,
        }
    }

    #[test]
    fn an_addon_no_source_describes_says_what_it_does_with_its_toc_notes() {
        let mut preferred = inspected(0, "1.0", None);
        let mut other = inspected(1, "1.0", None);
        preferred.contents.notes = Some("What its .toc says.".into());
        other.contents.notes = Some("What the other .toc says.".into());

        assert_eq!(
            description(&[preferred, other]),
            "What its .toc says.",
            "the preferred listing's notes"
        );

        let mut preferred = inspected(0, "1.0", None);
        let mut other = inspected(1, "1.0", None);
        preferred.contents.notes = Some("What its .toc says.".into());
        other.listing.description = "What the source says.".into();

        assert_eq!(
            description(&[preferred, other]),
            "What the source says.",
            "any source's description first"
        );
        assert_eq!(
            description(&[inspected(0, "1.0", None)]),
            "",
            "nothing to say"
        );
    }

    /// What was found in an archive holding one folder per interface given,
    /// with the `ETag` its server sent, if any.
    fn archive(interfaces: &[Option<u32>], etag: Option<&str>) -> Archive {
        Archive {
            contents: folders(interfaces),
            validators: Validators {
                etag: etag.map(str::to_owned),
                last_modified: None,
            },
            asset: None,
        }
    }

    #[test]
    fn archives_are_reused_when_they_never_change_or_their_server_can_say() {
        let mut listing = inspected(0, "1.0", None).listing;
        let mut known = BTreeMap::from([(listing.download.clone(), archive(&[Some(30300)], None))]);

        assert_eq!(
            earlier(&listing, None, &known),
            None,
            "it may have changed, and its server can't say"
        );

        listing.immutable = true;
        assert_eq!(
            earlier(&listing, None, &known),
            Some(archive(&[Some(30300)], None))
        );

        listing.immutable = false;
        let _ = known.insert(
            listing.download.clone(),
            archive(&[Some(30300)], Some("\"v1\"")),
        );
        assert_eq!(
            earlier(&listing, None, &known),
            Some(archive(&[Some(30300)], Some("\"v1\""))),
            "to ask its server"
        );

        listing.download = "https://example.com/new.zip".into();
        assert_eq!(earlier(&listing, None, &known), None, "never inspected");
    }

    #[test]
    fn a_releases_file_is_reused_only_while_it_is_the_same_upload() {
        let mut listing = inspected(0, "1.0", None).listing;
        listing.download =
            "https://github.com/Trimitor/WDM-addons/releases/download/1.0.9-stable/WDM.zip".into();
        listing.immutable = true;
        let found = Archive {
            asset: Some(7),
            ..archive(&[Some(30300)], None)
        };
        let mut known = BTreeMap::from([(listing.download.clone(), found.clone())]);

        assert_eq!(earlier(&listing, Some(7), &known), Some(found));
        assert_eq!(earlier(&listing, Some(8), &known), None, "uploaded again");
        assert_eq!(
            earlier(&listing, None, &known),
            None,
            "the upload there now is unknown"
        );

        let _ = known.insert(listing.download.clone(), archive(&[Some(30300)], None));
        assert_eq!(
            earlier(&listing, Some(7), &known),
            None,
            "found before uploads were told apart"
        );

        let new = Archive {
            asset: Some(8),
            ..archive(&[Some(30300)], Some("\"v1\""))
        };
        assert_eq!(
            kept(&listing, &new),
            Some(Archive {
                asset: Some(8),
                ..archive(&[Some(30300)], None)
            }),
            "kept with the upload it is"
        );
    }

    #[test]
    fn what_is_kept_is_what_the_next_run_can_reuse() {
        let mut listing = inspected(0, "1.0", None).listing;

        assert_eq!(
            kept(&listing, &archive(&[Some(30300)], None)),
            None,
            "it may change, and its server can't say"
        );
        assert_eq!(
            kept(&listing, &archive(&[Some(30300)], Some("\"v1\""))),
            Some(archive(&[Some(30300)], Some("\"v1\""))),
            "with what tells its version"
        );

        listing.immutable = true;
        assert_eq!(
            kept(&listing, &archive(&[Some(30300)], Some("\"v1\""))),
            Some(archive(&[Some(30300)], None)),
            "it never changes: nothing to ask"
        );
    }

    #[test]
    fn archives_found_before_are_read_back_and_a_bad_record_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archives.json");
        assert!(known_archives(&path).is_empty(), "no record yet");

        let known = BTreeMap::from([
            (
                "https://example.com/a.zip".to_owned(),
                archive(&[Some(30300), None], None),
            ),
            (
                "https://example.com/b.zip".to_owned(),
                archive(&[Some(30300)], Some("\"v1\"")),
            ),
            (
                "https://github.com/o/r/releases/download/v1/r.zip".to_owned(),
                Archive {
                    asset: Some(7),
                    ..archive(&[Some(30300)], None)
                },
            ),
        ]);
        std::fs::write(&path, serde_json::to_string_pretty(&known).unwrap()).unwrap();
        assert_eq!(known_archives(&path), known);

        // As earlier versions of the indexer wrote it.
        std::fs::write(
            &path,
            r#"{ "https://example.com/a.zip": { "folders": [], "updated": "2026-01-01" } }"#,
        )
        .unwrap();
        let earlier = &known_archives(&path)["https://example.com/a.zip"];
        assert_eq!(earlier.validators, Validators::default());
        assert_eq!(earlier.asset, None);

        std::fs::write(&path, "{ not json").unwrap();
        assert!(known_archives(&path).is_empty(), "a broken record");
    }

    /// A local server answering each request with what `respond` makes of
    /// it, and recording in `seen` each request's headers, lowercase.
    /// Returns the URL of its one file.
    fn server(
        respond: impl Fn(&str) -> Vec<u8> + Send + 'static,
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        let _ = std::thread::spawn(move || {
            use std::io::{Read, Write};

            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut request = [0; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]).to_lowercase();
                let answer = respond(&request);

                seen.lock().unwrap().push(request);
                stream.write_all(&answer).unwrap();
            }
        });

        format!("http://{address}/Addon.zip")
    }

    /// An answer of `200 OK` with `headers` and a zip of one addon.
    fn addon_zip(headers: &str) -> Vec<u8> {
        use std::io::Write;

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("Addon/Addon.toc", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"## Interface: 30300\n").unwrap();
        let zip = writer.finish().unwrap().into_inner();

        let mut answer = format!(
            "HTTP/1.1 200 OK\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n",
            zip.len()
        )
        .into_bytes();
        answer.extend(zip);

        answer
    }

    #[tokio::test]
    async fn an_archive_its_server_says_is_unchanged_is_not_downloaded_again() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        // As Rising Gods' server does: an ETag and a date with the file, and
        // a 304 when asked with the ETag.
        let url = server(
            |request| {
                if request.contains("if-none-match: \"v1\"") {
                    b"HTTP/1.1 304 Not Modified\r\netag: \"v1\"\r\nconnection: close\r\n\r\n"
                        .to_vec()
                } else {
                    addon_zip("etag: \"v1\"\r\nlast-modified: Thu, 09 Feb 2017 14:01:14 GMT\r\n")
                }
            },
            Arc::clone(&seen),
        );
        let client = reqwest::Client::new();

        let (first, reused) = inspect(&client, &url, None).await.unwrap();
        assert!(!reused);
        assert_eq!(first.contents.folders[0].name, "Addon");
        assert_eq!(
            first.validators,
            Validators {
                etag: Some("\"v1\"".into()),
                last_modified: Some("Thu, 09 Feb 2017 14:01:14 GMT".into()),
            }
        );

        let (again, reused) = inspect(&client, &url, Some(first.clone())).await.unwrap();
        assert!(reused, "nothing downloaded");
        assert_eq!(again, first);

        let seen = seen.lock().unwrap();
        assert!(
            !seen[0].contains("if-none-match"),
            "nothing to ask the first time"
        );
        assert!(
            seen[1].contains("if-none-match: \"v1\"")
                && seen[1].contains("if-modified-since: thu, 09 feb 2017 14:01:14 gmt"),
            "{}",
            seen[1]
        );
    }

    #[tokio::test]
    async fn an_archive_whose_server_tells_no_version_is_downloaded_every_time() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let url = server(|_| addon_zip(""), Arc::clone(&seen));
        let client = reqwest::Client::new();
        let mut listing = inspected(0, "1.0", None).listing;
        listing.download = url.clone();

        let (found, reused) = inspect(&client, &url, None).await.unwrap();

        assert!(!reused);
        assert_eq!(found.validators, Validators::default());
        assert_eq!(kept(&listing, &found), None, "nothing to ask next time");
    }

    /// Rising Gods' site as it is now: a file comes with what tells its
    /// version, and asking with that downloads nothing.
    #[tokio::test]
    #[ignore = "downloads from Rising Gods"]
    async fn rising_gods_says_when_a_file_is_unchanged() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .user_agent("old_launcher-indexer-test")
            .build()
            .unwrap();
        let url = "https://addons.rising-gods.de/sites/default/files/addons/Ace3-Release-r960.zip";

        let (first, reused) = inspect(&client, url, None).await.unwrap();
        println!("{:?}", first.validators);
        assert!(!reused && !first.validators.is_empty());

        let (_, reused) = inspect(&client, url, Some(first)).await.unwrap();
        assert!(reused, "answered 304 Not Modified");
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
