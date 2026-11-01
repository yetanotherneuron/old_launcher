//! Builds the launcher's catalog from every upstream source: lists their
//! addons, pins the downloads that move to what they serve now (see
//! [`pin`]), downloads every archive to check it and record the folders it
//! installs, splits the archives known to be compilations of unrelated
//! addons into a listing for each (see [`split`]), merges the listings of
//! the same addon from different sources, and writes the result. Then the
//! same for the list of mods.
//!
//! What was found in each archive is kept for the next run, which downloads
//! only what may have changed: an archive that never changes isn't
//! downloaded again, but for a release's file uploaded again under the same
//! name (see [`uploads`]), and one that may is asked about, when its server
//! sent what tells its versions apart (see [`fetch::Validators`]).

mod commits;
mod fetch;
mod mods;
mod pin;
mod source;
mod split;
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
    /// The addon's name as the catalog shows it: the listing's, or, for one
    /// of the several addons of an archive, its anchor folder's `.toc` title
    /// (see [`split`]), else that folder's name (see [`shown_names`]).
    name: String,
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
    for repository in github::REPOSITORIES {
        gathered.push(gather(client, &github::Repository(repository)).await?);
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
    // The archives to split that no listing is: one renamed upstream would
    // otherwise stay whole without anyone noticing.
    let unlisted: Vec<String> = split::ARCHIVES
        .iter()
        .filter(|&&(archive_source, archive_listing)| {
            !listings.iter().any(|(_, source, listing, _)| {
                source == archive_source && listing.name == archive_listing
            })
        })
        .map(|(source, name)| format!("{source} / {name}"))
        .collect();
    let downloads = Arc::new(Semaphore::new(arguments.jobs.max(1)));
    let mut inspections = JoinSet::new();

    for (priority, source, listing, pinned) in listings {
        let client = client.clone();
        let downloads = Arc::clone(&downloads);
        let asset = uploads.id(&listing.download);
        let to_split = split::wanted(&source, &listing.name);
        let earlier = earlier(&listing, asset, &known, to_split);

        let _ = inspections.spawn(async move {
            let inspection = async {
                pinned?;

                let (mut archive, reused) = match earlier {
                    // It never changes, and was looked into before.
                    Some(archive) if listing.immutable => (archive, true),
                    earlier => {
                        let _permit = downloads.acquire_owned().await?;
                        let (archive, reused) =
                            inspect(&client, &listing.download, earlier).await?;

                        // Looked into as uploaded now, for a release's file.
                        (Archive { asset, ..archive }, reused)
                    }
                };

                // Folders left undated by a commit give their addons their
                // files' date, until a later run dates them.
                if to_split
                    && let Err(error) =
                        commits::date(&client, &listing.download, &mut archive.contents).await
                {
                    println!("{}: folders not dated, {error:#}", listing.name);
                }

                anyhow::Ok((archive, reused))
            }
            .await;

            (priority, source, listing, to_split, inspection)
        });
    }

    let mut inspected = Vec::new();
    let mut skipped = Vec::new();
    // What this run found in archives, for the next.
    let mut archives = BTreeMap::new();
    let mut reused_count = 0;
    // The archives holding several addons, and the listings made of them.
    let mut split_count = 0;
    let mut piece_count = 0;
    let mut done = 0;

    while let Some(inspection) = inspections.join_next().await {
        let (priority, source, listing, to_split, inspection) = inspection?;
        done += 1;
        let note = if let Ok((_, true)) = &inspection {
            reused_count += 1;
            " (inspected before)"
        } else {
            ""
        };

        if let Ok((archive, _)) = &inspection
            && let Some(archive) = kept(&listing, archive, to_split)
        {
            let _ = archives.insert(listing.download.clone(), archive);
        }

        let contents = match inspection {
            Ok((Archive { contents, .. }, _)) => contents,
            Err(error) => {
                let reason = format!("{error:#}");

                println!("[{done}/{total}] {}: skipped{note}, {reason}", listing.name);
                skipped.push(format!("{source} / {}: {reason}", listing.name));
                continue;
            }
        };

        let sorted = outcome(priority, &source, listing, contents, to_split);

        if let Some(line) = &sorted.split {
            split_count += 1;
            piece_count += sorted.kept.len() + sorted.left_out.len();
            println!("[{done}/{total}] {line}");
        }

        for (name, reason) in sorted.left_out {
            println!("[{done}/{total}] {name}: skipped{note}, {reason}");
            skipped.push(format!("{source} / {name}: {reason}"));
        }

        for item in sorted.kept {
            if sorted.split.is_none() {
                println!(
                    "[{done}/{total}] {}: {} folders{note}",
                    item.listing.name,
                    item.contents.folders.len()
                );
            }
            inspected.push(item);
        }
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

            let name = listings[0].name.clone();
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
    println!("Split {split_count} archives holding several addons into {piece_count}.");
    for archive in &unlisted {
        println!("Not split, as it isn't listed: {archive}");
    }

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
///
/// When `to_split`, the listing is split into its addons (see [`split`]),
/// and an archive of several folders found without their parts, or without
/// where they sit, as the indexer once did, isn't of use: it can't be split
/// or dated (see [`commits`]) without them, so it is downloaded and looked
/// into again. Any other listing reuses what was found, parts or not.
fn earlier(
    listing: &Listing,
    asset: Option<u64>,
    known: &BTreeMap<String, Archive>,
    to_split: bool,
) -> Option<Archive> {
    known
        .get(&listing.download)
        .filter(|archive| {
            // Of several folders, at most one is the archive's root, with no
            // path.
            !to_split
                || archive.contents.folders.len() < 2
                || (archive.contents.parts.len() == archive.contents.folders.len()
                    && archive
                        .contents
                        .parts
                        .iter()
                        .any(|part| !part.path.is_empty()))
        })
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
///
/// The parts of its folders (see [`Contents::parts`]) are kept only when
/// `to_split`, the listing being split into its addons (see [`split`]),
/// which needs them again: for any other they would only make the record
/// bigger.
fn kept(listing: &Listing, archive: &Archive, to_split: bool) -> Option<Archive> {
    let mut found = archive.clone();

    if !to_split {
        found.contents.parts = Vec::new();
    }

    if listing.immutable {
        found.validators = Validators::default();

        return Some(found);
    }

    (!found.validators.is_empty()).then_some(found)
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

/// When the addon of `listing`, whose archive holds `contents`, was last
/// updated, wherever the listing says to look.
fn updated_of(listing: &Listing, contents: &Contents) -> Option<String> {
    match &listing.updated {
        Updated::FromArchive => contents.updated.clone(),
        Updated::Given(date) => date.clone(),
    }
}

/// The names the catalog shows for the addons of `groups`, in their order,
/// those of the archive that holds `contents`: the anchor folder's `.toc`
/// title, or else the folder's name.
///
/// The names of the downloads are made of these (see [`piece`]) and must
/// tell the addons of an archive apart, so addons that would share a name are
/// shown by their folders' names instead, which are all different. A folder's
/// name may be another addon's title, which then gives way too.
fn shown_names(contents: &Contents, groups: &[split::Group]) -> Vec<String> {
    let anchors: Vec<&catalog::Folder> = groups
        .iter()
        .map(|group| &contents.folders[group.anchor])
        .collect();
    let mut names: Vec<String> = anchors
        .iter()
        .map(|anchor| {
            anchor
                .title
                .clone()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| anchor.name.clone())
        })
        .collect();

    loop {
        let shared: Vec<usize> = names
            .iter()
            .enumerate()
            .filter(|&(index, name)| {
                *name != anchors[index].name
                    && names.iter().filter(|other| *other == name).count() > 1
            })
            .map(|(index, _)| index)
            .collect();

        if shared.is_empty() {
            return names;
        }

        for index in shared {
            names[index].clone_from(&anchors[index].name);
        }
    }
}

/// The addon that `group` makes up in the archive of `listing`, which holds
/// `contents`, as a listing of its own, shown as `shown` (see
/// [`shown_names`]). It takes the folders of the group, dated by the newest
/// of them, each by the last commit that changed it (see [`commits`]) or else
/// by its newest file, or as the archive is when none has a date, and the
/// page and changes of the listing; what the listing says of the archive as a
/// whole (its description, screenshot and links) isn't its own to say.
///
/// Its download is named for the addon and the listing it comes from
/// (`Bartender4 (LUI v3)`), as players read download names: on the source's
/// pill, in what needs it, in the title of what the launcher can't match.
/// Markers identify downloads by source and name, so the name stays unique
/// within the source as long as `shown` is unique among the archive's addons.
fn piece(
    priority: usize,
    source: &str,
    listing: &Listing,
    contents: &Contents,
    group: &split::Group,
    shown: &str,
) -> Inspected {
    let notes = |folder: usize| contents.parts[folder].notes.clone();
    let contents = Contents {
        folders: group
            .folders
            .iter()
            .map(|&folder| contents.folders[folder].clone())
            .collect(),
        parts: group
            .folders
            .iter()
            .map(|&folder| contents.parts[folder].clone())
            .collect(),
        notes: notes(group.anchor)
            .or_else(|| group.folders.iter().find_map(|&folder| notes(folder))),
        updated: group
            .folders
            .iter()
            .filter_map(|&folder| {
                let part = &contents.parts[folder];
                part.committed.as_ref().or(part.updated.as_ref())
            })
            .max()
            .or(contents.updated.as_ref())
            .cloned(),
    };
    let listing = Listing {
        name: format!("{shown} ({})", listing.name),
        description: String::new(),
        image: None,
        links: Vec::new(),
        ..listing.clone()
    };

    Inspected {
        priority,
        source: source.to_owned(),
        name: shown.to_owned(),
        updated: updated_of(&listing, &contents),
        listing,
        contents,
    }
}

/// What came of a listing whose archive was looked into.
struct Outcome {
    /// The line telling the archive was split, when it was: how many folders
    /// it holds and which addons they make up. None when it stays one
    /// listing.
    split: Option<String>,
    /// The listings the catalog keeps: the listing itself, or each addon of
    /// the archive that stays when it is split.
    kept: Vec<Inspected>,
    /// The listings left out (see [`left_out`]), by download name, with why.
    left_out: Vec<(String, String)>,
}

/// Turns the `listing` of `source`, whose archive was found to hold
/// `contents`, into what the catalog is made of.
///
/// When `to_split` and the archive holds several addons (see [`split`]), each
/// is a listing of its own (see [`piece`]), kept or left out by itself;
/// otherwise the listing is one, as it is.
fn outcome(
    priority: usize,
    source: &str,
    listing: Listing,
    contents: Contents,
    to_split: bool,
) -> Outcome {
    let groups = if to_split {
        split::groups(&contents)
    } else {
        Vec::new()
    };

    let (summary, items) = if groups.len() > 1 {
        let shown = shown_names(&contents, &groups);
        let line = format!(
            "{}: {} folders, {} addons: {}",
            listing.name,
            contents.folders.len(),
            groups.len(),
            groups
                .iter()
                .map(|group| contents.folders[group.anchor].name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let pieces = groups
            .iter()
            .zip(&shown)
            .map(|(group, shown)| piece(priority, source, &listing, &contents, group, shown))
            .collect();

        (Some(line), pieces)
    } else {
        let updated = updated_of(&listing, &contents);

        (
            None,
            vec![Inspected {
                priority,
                source: source.to_owned(),
                name: listing.name.clone(),
                listing,
                contents,
                updated,
            }],
        )
    };

    let mut sorted = Outcome {
        split: summary,
        kept: Vec::new(),
        left_out: Vec::new(),
    };

    for item in items {
        match left_out(&item.listing, &item.contents) {
            Some(reason) => sorted.left_out.push((item.listing.name, reason)),
            None => sorted.kept.push(item),
        }
    }

    sorted
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
            name: version.to_owned(),
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
                parts: Vec::new(),
                notes: None,
                updated: None,
            },
            updated: updated.map(str::to_owned),
        }
    }

    /// An archive of one folder per interface given, none requiring any.
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
            parts: (0..interfaces.len())
                .map(|index| catalog::Part {
                    name: format!("Folder{index}"),
                    path: format!("Folder{index}"),
                    dependencies: Vec::new(),
                    notes: None,
                    inside: None,
                    updated: None,
                    committed: None,
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
            earlier(&listing, None, &known, false),
            None,
            "it may have changed, and its server can't say"
        );

        listing.immutable = true;
        assert_eq!(
            earlier(&listing, None, &known, false),
            Some(archive(&[Some(30300)], None))
        );

        listing.immutable = false;
        let _ = known.insert(
            listing.download.clone(),
            archive(&[Some(30300)], Some("\"v1\"")),
        );
        assert_eq!(
            earlier(&listing, None, &known, false),
            Some(archive(&[Some(30300)], Some("\"v1\""))),
            "to ask its server"
        );

        listing.download = "https://example.com/new.zip".into();
        assert_eq!(
            earlier(&listing, None, &known, false),
            None,
            "never inspected"
        );
    }

    #[test]
    fn an_archive_to_split_found_before_parts_were_recorded_is_looked_into_again() {
        let mut listing = inspected(0, "1.0", None).listing;
        listing.immutable = true;
        let mut found = archive(&[Some(30300), Some(30300)], None);
        let known = |found: &Archive| BTreeMap::from([(listing.download.clone(), found.clone())]);

        assert_eq!(
            earlier(&listing, None, &known(&found), true),
            Some(found.clone())
        );

        let mut unplaced = found.clone();
        for part in &mut unplaced.contents.parts {
            part.path.clear();
        }
        assert_eq!(
            earlier(&listing, None, &known(&unplaced), true),
            None,
            "nor dated without where its folders sit"
        );

        found.contents.parts.clear();
        assert_eq!(
            earlier(&listing, None, &known(&found), true),
            None,
            "it can't be split without them"
        );
        assert_eq!(
            earlier(&listing, None, &known(&found), false),
            Some(found.clone()),
            "any other listing doesn't need them"
        );

        found.contents.folders.pop();
        assert_eq!(
            earlier(&listing, None, &known(&found), true),
            Some(found.clone()),
            "one folder has nothing to be told apart from"
        );
    }

    #[test]
    fn each_addon_of_an_archive_is_a_listing_of_its_own() {
        let part = |name: &str, dependencies: &[&str], notes: Option<&str>| catalog::Part {
            name: name.to_owned(),
            path: name.to_owned(),
            dependencies: dependencies.iter().map(|name| (*name).to_owned()).collect(),
            notes: notes.map(str::to_owned),
            inside: None,
            updated: None,
            committed: None,
        };
        let folder = |name: &str, title: Option<&str>| catalog::Folder {
            name: name.to_owned(),
            title: title.map(str::to_owned),
            version: None,
            interface: Some(Toc::WOTLK_INTERFACE),
        };
        let contents = Contents {
            folders: vec![
                folder("Core", Some("Pack Core")),
                folder("Core_Extra", None),
                folder("Other", None),
            ],
            parts: vec![
                part("Core", &[], None),
                part("Core_Extra", &["Core"], Some("Adds to the core.")),
                part("Other", &[], None),
            ],
            notes: Some("Adds to the core.".into()),
            updated: Some("2026-02-03".into()),
        };
        let mut listing = inspected(2, "pack", None).listing;
        listing.description = "A pack of addons.".into();
        listing.image = Some("https://example.com/pack.png".into());
        listing.page = Some("https://example.com/pack".into());
        listing.links = vec![catalog::Link {
            label: "Home".into(),
            url: "https://example.com".into(),
        }];

        let groups = split::groups(&contents);
        let shown = shown_names(&contents, &groups);
        let pieces: Vec<Inspected> = groups
            .iter()
            .zip(&shown)
            .map(|(group, shown)| piece(2, "source", &listing, &contents, group, shown))
            .collect();

        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].name, "Pack Core", "the anchor's title");
        assert_eq!(
            pieces[0].listing.name, "Pack Core (pack)",
            "as players read it"
        );
        assert_eq!(pieces[0].priority, 2);
        assert_eq!(pieces[0].source, "source");
        assert_eq!(
            pieces[0]
                .contents
                .folders
                .iter()
                .map(|folder| folder.name.as_str())
                .collect::<Vec<_>>(),
            ["Core", "Core_Extra"]
        );
        assert_eq!(pieces[0].contents.parts.len(), 2);
        assert_eq!(
            pieces[0].contents.notes.as_deref(),
            Some("Adds to the core."),
            "the first notes of the group, as the anchor has none"
        );
        assert_eq!(pieces[0].updated.as_deref(), Some("2026-02-03"));
        assert_eq!(pieces[1].name, "Other", "no title: the folder's name");
        assert_eq!(pieces[1].listing.name, "Other (pack)");
        assert_eq!(pieces[1].contents.notes, None);

        let listed = &pieces[1].listing;
        assert!(listed.description.is_empty() && listed.image.is_none());
        assert!(listed.links.is_empty());
        assert_eq!(listed.page.as_deref(), Some("https://example.com/pack"));
        assert_eq!(listed.download, listing.download);
    }

    #[test]
    fn each_addon_of_an_archive_is_dated_by_its_folders() {
        let mut contents = pack(&[
            ("Core", None, None, &[]),
            ("Core_Extra", None, None, &["Core"]),
            ("Other", None, None, &[]),
        ]);
        contents.updated = Some("2026-10-04".into());
        contents.parts[0].updated = Some("2026-03-01".into());
        contents.parts[1].updated = Some("2026-05-02".into());
        let listing = inspected(0, "pack", None).listing;
        let dates = |contents: &Contents| -> Vec<Option<String>> {
            split::groups(contents)
                .iter()
                .map(|group| piece(0, "source", &listing, contents, group, "Shown").updated)
                .collect()
        };

        assert_eq!(
            dates(&contents),
            [Some("2026-05-02".into()), Some("2026-10-04".into())],
            "the newest file of its folders; without one, the archive's"
        );

        // A snapshot of a repository: every file of the commit's day.
        for part in &mut contents.parts {
            part.updated.clone_from(&contents.updated);
        }
        contents.parts[0].committed = Some("2026-03-01".into());
        contents.parts[1].committed = Some("2026-05-02".into());
        assert_eq!(
            dates(&contents),
            [Some("2026-05-02".into()), Some("2026-10-04".into())],
            "the last commit that changed its folders; without one, its files'"
        );

        contents.parts[1].committed = None;
        assert_eq!(
            dates(&contents),
            [Some("2026-10-04".into()), Some("2026-10-04".into())],
            "one of its folders not dated by a commit"
        );
    }

    /// A folder of an archive for [`pack`]: its name, the title and interface
    /// its `.toc` declares, and the folders it requires.
    type Spec<'a> = (&'a str, Option<&'a str>, Option<u32>, &'a [&'a str]);

    /// An archive of the folders given, none inside another.
    fn pack(folders: &[Spec]) -> Contents {
        Contents {
            folders: folders
                .iter()
                .map(|&(name, title, interface, _)| catalog::Folder {
                    name: name.to_owned(),
                    title: title.map(str::to_owned),
                    version: None,
                    interface,
                })
                .collect(),
            parts: folders
                .iter()
                .map(|&(name, _, _, dependencies)| catalog::Part {
                    name: name.to_owned(),
                    path: name.to_owned(),
                    dependencies: dependencies.iter().map(|name| (*name).to_owned()).collect(),
                    notes: None,
                    inside: None,
                    updated: None,
                    committed: None,
                })
                .collect(),
            notes: None,
            updated: None,
        }
    }

    #[test]
    fn addons_of_an_archive_with_the_same_title_are_shown_by_their_folders() {
        let contents = pack(&[
            ("ElvUI_A", Some("Plugin"), None, &[]),
            ("ElvUI_B", Some("Plugin"), None, &[]),
            ("Solo", Some("Solo Addon"), None, &[]),
            ("Plain", Some(" "), None, &[]),
        ]);

        assert_eq!(
            shown_names(&contents, &split::groups(&contents)),
            ["ElvUI_A", "ElvUI_B", "Solo Addon", "Plain"],
            "the shared title only; a blank one is no title"
        );

        // Falling back to a folder's name may make it a name that another
        // addon is shown by.
        let contents = pack(&[
            ("X", Some("Y"), None, &[]),
            ("W", Some("Y"), None, &[]),
            ("Z", Some("X"), None, &[]),
        ]);

        assert_eq!(
            shown_names(&contents, &split::groups(&contents)),
            ["X", "W", "Z"]
        );
    }

    #[test]
    fn the_downloads_of_an_archive_split_into_addons_have_names_that_tell_them_apart() {
        let contents = pack(&[
            ("ElvUI_A", Some("Plugin"), Some(Toc::WOTLK_INTERFACE), &[]),
            ("ElvUI_B", Some("Plugin"), Some(Toc::WOTLK_INTERFACE), &[]),
            ("Solo", Some("Solo Addon"), Some(Toc::WOTLK_INTERFACE), &[]),
        ]);
        let listing = inspected(0, "pack", None).listing;

        let sorted = outcome(0, "source", listing, contents, true);

        assert_eq!(
            sorted
                .kept
                .iter()
                .map(|item| (item.name.as_str(), item.listing.name.as_str()))
                .collect::<Vec<_>>(),
            [
                ("ElvUI_A", "ElvUI_A (pack)"),
                ("ElvUI_B", "ElvUI_B (pack)"),
                ("Solo Addon", "Solo Addon (pack)")
            ]
        );
    }

    #[test]
    fn an_addon_of_an_archive_to_split_is_left_out_by_itself() {
        let wotlk = Some(Toc::WOTLK_INTERFACE);
        let contents = pack(&[
            ("Alpha", None, wotlk, &[]),
            ("Alpha_Extra", None, Some(30200), &["Alpha"]),
            ("Beta", Some("Beta Addon"), wotlk, &[]),
            ("Gamma", None, Some(30200), &[]),
        ]);
        let mut listing = inspected(1, "pack", None).listing;
        listing.require_wotlk = true;

        let sorted = outcome(1, "source", listing, contents, true);

        assert_eq!(
            sorted.split.as_deref(),
            Some("pack: 4 folders, 3 addons: Alpha, Beta, Gamma")
        );
        assert_eq!(
            sorted
                .kept
                .iter()
                .map(|item| item.listing.name.as_str())
                .collect::<Vec<_>>(),
            ["Alpha (pack)", "Beta Addon (pack)"],
            "Alpha's other folder declares it"
        );
        assert_eq!(sorted.kept[0].contents.folders.len(), 2);
        assert_eq!(
            sorted.left_out,
            [(
                "Gamma (pack)".to_owned(),
                "no folder declares the 3.3.5a interface".to_owned()
            )]
        );
    }

    #[test]
    fn folders_that_hang_together_stay_the_listing_they_are() {
        let wotlk = Some(Toc::WOTLK_INTERFACE);
        let contents = pack(&[
            ("Core", Some("Core Title"), wotlk, &[]),
            ("Core_Options", None, wotlk, &["Core"]),
            ("Core_Data", None, wotlk, &["Core_Options", "Core"]),
        ]);
        let mut listing = inspected(1, "pack", None).listing;
        listing.description = "A pack of addons.".into();
        listing.image = Some("https://example.com/pack.png".into());
        listing.links = vec![catalog::Link {
            label: "Home".into(),
            url: "https://example.com".into(),
        }];

        let sorted = outcome(1, "source", listing.clone(), contents, true);

        assert_eq!(sorted.split, None);
        assert!(sorted.left_out.is_empty());
        let [item] = sorted.kept.as_slice() else {
            panic!("expected one listing, got {}", sorted.kept.len());
        };
        assert_eq!(item.name, "pack", "not the anchor's title");
        assert_eq!(item.listing, listing, "its description, image and links");
        assert_eq!(item.contents.folders.len(), 3);
    }

    #[test]
    fn a_listing_not_to_split_stays_one_whatever_its_folders() {
        let wotlk = Some(Toc::WOTLK_INTERFACE);
        let contents = pack(&[
            ("One", Some("One"), wotlk, &[]),
            ("Two", None, wotlk, &[]),
            ("Three", None, wotlk, &["One"]),
        ]);
        let listing = inspected(1, "pack", None).listing;

        let sorted = outcome(1, "source", listing.clone(), contents, false);

        assert_eq!(sorted.split, None);
        assert!(sorted.left_out.is_empty());
        let [item] = sorted.kept.as_slice() else {
            panic!("expected one listing, got {}", sorted.kept.len());
        };
        assert_eq!(item.name, "pack");
        assert_eq!(item.listing, listing);
        assert_eq!(item.priority, 1);
        assert_eq!(item.source, "source");
        assert_eq!(item.contents.folders.len(), 3);
    }

    #[test]
    fn an_archive_of_no_folder_or_of_one_is_not_split() {
        for to_split in [false, true] {
            let listing = inspected(0, "pack", None).listing;

            let none = outcome(0, "source", listing.clone(), pack(&[]), to_split);

            assert_eq!(none.split, None, "to split: {to_split}");
            assert!(none.kept.is_empty());
            assert_eq!(
                none.left_out,
                [("pack".to_owned(), "the archive holds no addon".to_owned())]
            );

            let one = outcome(
                0,
                "source",
                listing.clone(),
                pack(&[("Solo", Some("Solo Addon"), None, &[])]),
                to_split,
            );

            assert_eq!(one.split, None, "to split: {to_split}");
            assert!(one.left_out.is_empty());
            let [item] = one.kept.as_slice() else {
                panic!("expected one listing, got {}", one.kept.len());
            };
            assert_eq!(item.name, "pack", "to split: {to_split}");
            assert_eq!(item.listing, listing);
        }
    }

    #[test]
    fn a_repository_snapshot_is_told_into_its_addons() {
        // A GitHub snapshot: a wrapper folder named for the commit, a folder
        // for the addons, and in it an addon in a folder not named for it
        // (`Alpha-main`), so that the module inside it that requires it is
        // placed beside it; another addon requiring it; and an unrelated one.
        let archive = zip_of(&[
            ("repo-0123abc/README.md", "Addons."),
            (
                "repo-0123abc/Addons/Alpha-main/Alpha.toc",
                "## Title: Alpha\n## Interface: 30300\n",
            ),
            (
                "repo-0123abc/Addons/Alpha-main/Alpha_Options/Alpha_Options.toc",
                "## Dependencies: Alpha\n## Interface: 30300\n",
            ),
            (
                "repo-0123abc/Addons/Beta/Beta.toc",
                "## Dependencies: Alpha\n## Interface: 30300\n",
            ),
            (
                "repo-0123abc/Addons/Gamma/Gamma.toc",
                "## Interface: 30300\n",
            ),
        ]);

        let contents = Contents::inspect(&archive).unwrap();

        assert_eq!(
            contents
                .folders
                .iter()
                .map(|folder| folder.name.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "Beta", "Gamma", "Alpha_Options"]
        );
        assert_eq!(contents.parts[3].inside.as_deref(), Some("Alpha"));
        assert_eq!(
            split::groups(&contents),
            [
                split::Group {
                    anchor: 0,
                    folders: vec![0, 1, 3]
                },
                split::Group {
                    anchor: 2,
                    folders: vec![2]
                }
            ],
            "Alpha with its module and its plugin; Gamma on its own"
        );
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

        assert_eq!(earlier(&listing, Some(7), &known, false), Some(found));
        assert_eq!(
            earlier(&listing, Some(8), &known, false),
            None,
            "uploaded again"
        );
        assert_eq!(
            earlier(&listing, None, &known, false),
            None,
            "the upload there now is unknown"
        );

        let _ = known.insert(listing.download.clone(), archive(&[Some(30300)], None));
        assert_eq!(
            earlier(&listing, Some(7), &known, false),
            None,
            "found before uploads were told apart"
        );

        let new = Archive {
            asset: Some(8),
            ..archive(&[Some(30300)], Some("\"v1\""))
        };
        assert_eq!(
            kept(&listing, &new, true),
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
            kept(&listing, &archive(&[Some(30300)], None), true),
            None,
            "it may change, and its server can't say"
        );
        assert_eq!(
            kept(&listing, &archive(&[Some(30300)], Some("\"v1\"")), true),
            Some(archive(&[Some(30300)], Some("\"v1\""))),
            "with what tells its version"
        );

        listing.immutable = true;
        assert_eq!(
            kept(&listing, &archive(&[Some(30300)], Some("\"v1\"")), true),
            Some(archive(&[Some(30300)], None)),
            "it never changes: nothing to ask"
        );
    }

    #[test]
    fn the_parts_of_an_archive_are_kept_only_to_split_it() {
        let mut listing = inspected(0, "1.0", None).listing;
        let found = archive(&[Some(30300), Some(30300)], Some("\"v1\""));
        let mut bare = found.clone();
        bare.contents.parts = Vec::new();

        assert_eq!(
            kept(&listing, &found, true),
            Some(found.clone()),
            "the next run splits it by them"
        );
        assert_eq!(
            kept(&listing, &found, false),
            Some(bare.clone()),
            "nothing needs them"
        );

        listing.immutable = true;
        let mut never_changing = found.clone();
        never_changing.validators = Validators::default();
        bare.validators = Validators::default();
        assert_eq!(kept(&listing, &found, true), Some(never_changing));
        assert_eq!(kept(&listing, &found, false), Some(bare));

        listing.immutable = false;
        let unaskable = archive(&[Some(30300), Some(30300)], None);
        assert_eq!(kept(&listing, &unaskable, true), None, "not kept at all");
        assert_eq!(kept(&listing, &unaskable, false), None);
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

    /// A zip archive holding `files` (path, contents).
    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));

        for (path, contents) in files {
            writer
                .start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }

        writer.finish().unwrap().into_inner()
    }

    /// An answer of `200 OK` with `headers` and a zip of one addon.
    fn addon_zip(headers: &str) -> Vec<u8> {
        let zip = zip_of(&[("Addon/Addon.toc", "## Interface: 30300\n")]);

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
        assert_eq!(
            kept(&listing, &found, false),
            None,
            "nothing to ask next time"
        );
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
