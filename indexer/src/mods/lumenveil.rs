//! Lumenveil, a graphics mod: a `version.dll` that `Wow.exe` loads from its
//! folder, which loads the renderer, `CoAVolFog.dll`, beside it. Its setup
//! then finds where the world's lamps and water are, in the client's own
//! archives.
//!
//! Each GitHub release is a zip of its files, with a `SHA256SUMS.txt` they
//! are checked against. What the launcher can't read from them is written
//! here, as each release's own scripts do it: `Lumenveil-Setup.cmd` runs the
//! setup, and `Lumenveil-Uninstall.cmd` says what the mod writes and which
//! files are the player's. A release whose scripts this module doesn't know
//! is left out, so the launcher goes on offering the last one it does, until
//! the new scripts are read and written down here.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use old_launcher_core::mods::{Mod, Release, Source, Step};

use crate::fetch::fetch;
use crate::mods::{Known, LeftOut, Published, files, published};

/// The repository its releases are published in.
const REPOSITORY: &str = "lumenveil/Lumenveil";

/// A `Lumenveil-Setup.cmd` script, by checksum, with the steps it runs.
type Setup = (&'static str, fn() -> Vec<Step>);

/// The `Lumenveil-Setup.cmd` scripts known.
const SETUPS: &[Setup] = &[(
    // 0.1.0 and 0.1.1
    "5d10a76c23acefb325c27aeec4cb2ba309f39e3c66d42d5cce0ee042f12a9ac3",
    two_runs,
)];

/// The `Lumenveil-Uninstall.cmd` scripts known, by checksum: each removes
/// what [`SETTINGS`] and [`WRITES`] name, with the release's own files.
const UNINSTALLS: &[&str] = &[
    // 0.1.0 and 0.1.1
    "7d09a1cc933f2b3fab75d6225450dfe29c2a91ad513ea1f466c8c4cf6055dea2",
];

/// The player's files: its settings, the look of each zone (tuned in the
/// game too), and the spots they save in the game.
const SETTINGS: &[&str] = &[
    "CoAVolFog.ini",
    "CoAVolFog-regions.ini",
    "CoAVolFog-spots.txt",
    "CoAVolFog-spots",
];

/// What it writes as it runs, and what its setup script writes when run by
/// hand.
const WRITES: &[&str] = &[
    "CoAVolFog.log",
    "Lumenveil/setup-lamps.log",
    "Lumenveil/setup-water.log",
];

/// The setup of 0.1.0 and 0.1.1: the lamps, then the lakes and the sea.
fn two_runs() -> Vec<Step> {
    let run = |arguments: &[&str], creates: &str| Step {
        run: "Lumenveil/lamp_extract.exe".to_owned(),
        arguments: arguments
            .iter()
            .copied()
            .chain(["--locale", "{locale}", "--background"])
            .map(str::to_owned)
            .collect(),
        creates: creates.to_owned(),
    };

    vec![
        run(&["--data", "Data", "--out", "lampdata.bin"], "lampdata.bin"),
        run(
            &[
                "--data",
                "Data",
                "--liquids",
                "liquiddata.bin",
                "--maps",
                "0,1,530,571",
            ],
            "liquiddata.bin",
        ),
    ]
}

/// Lumenveil, with every release understood, the newest first. Their files
/// go to `found`, and the releases left out to `left_out`, with why.
pub async fn gather(
    client: &reqwest::Client,
    known: &Known,
    found: &mut Known,
    left_out: &mut LeftOut,
) -> anyhow::Result<Mod> {
    let mut releases = Vec::new();

    for published in published(client, REPOSITORY).await? {
        let tag = published.tag_name.clone();
        match understood(client, known, found, published).await {
            Ok(release) => {
                println!(
                    "Lumenveil {}: {} files",
                    release.version,
                    release.files.len()
                );
                releases.push(release);
            }
            Err(error) => {
                left_out.note(format!("Lumenveil {tag}: left out, {error:#}"), &releases);
            }
        }
    }

    Ok(Mod {
        id: "lumenveil".to_owned(),
        name: "Lumenveil".to_owned(),
        description: "Volumetric fog, night lights, light shafts and water. In the game, \
                      Ctrl+F12 switches it off and on, and Ctrl+F7 opens its settings."
            .to_owned(),
        source: Source {
            name: "Lumenveil on GitHub".to_owned(),
            page: format!("https://github.com/{REPOSITORY}"),
        },
        releases,
        settings: SETTINGS.iter().copied().map(str::to_owned).collect(),
        writes: WRITES.iter().copied().map(str::to_owned).collect(),
        config: BTreeMap::from([("gxApi".to_owned(), "d3d9".to_owned())]),
        platforms: Vec::new(),
        requires: Vec::new(),
        excludes: Vec::new(),
        alternatives: Vec::new(),
        // Its DLLs say their version in their version resource.
        version_in: None,
    })
}

/// `published` as the launcher installs it: its zip's files, checked
/// against its `SHA256SUMS.txt` (and kept in `found`), and the steps of its
/// setup.
async fn understood(
    client: &reqwest::Client,
    known: &Known,
    found: &mut Known,
    published: Published,
) -> anyhow::Result<Release> {
    let url = published
        .asset(|name| name.ends_with(".zip"))
        .context("it has no zip")?;
    let sums = published
        .asset(|name| name == "SHA256SUMS.txt")
        .context("it has no SHA256SUMS.txt")?;

    let files = files(client, known, &url).await?;
    if !known.contains_key(&url) {
        let sums = String::from_utf8_lossy(&fetch(client, &sums).await?).into_owned();
        checked(&files, &sums)?;
    }
    let _ = found.insert(url.clone(), files.clone());

    release(published, url, files)
}

/// Whether `files` are those `sums`, a `SHA256SUMS.txt`, lists, the zip's
/// own line aside: why not, if they aren't.
fn checked(files: &[(String, String)], sums: &str) -> anyhow::Result<()> {
    let listed: BTreeSet<(String, String)> = sums
        .lines()
        .filter_map(|line| {
            let (sum, path) = line.trim_end().split_once(' ')?;
            let path = path.strip_prefix([' ', '*']).unwrap_or(path);

            (!path.ends_with(".zip")).then(|| (path.replace('\\', "/"), sum.to_ascii_lowercase()))
        })
        .collect();
    let zipped: BTreeSet<(String, String)> = files.iter().cloned().collect();

    if let Some((path, _)) = zipped.symmetric_difference(&listed).next() {
        anyhow::bail!("{path} in its zip isn't as its SHA256SUMS.txt says");
    }

    Ok(())
}

/// `published` with its zip at `url`, which holds `files`, if this module
/// knows its scripts.
fn release(
    published: Published,
    url: String,
    files: Vec<(String, String)>,
) -> anyhow::Result<Release> {
    let sum = |path: &str| {
        files
            .iter()
            .find(|(file, _)| file == path)
            .map(|(_, sum)| sum.clone())
            .with_context(|| format!("it has no {path}"))
    };

    let setup = sum("Lumenveil-Setup.cmd")?;
    let steps = SETUPS
        .iter()
        .find(|(known, _)| *known == setup)
        .map(|(_, steps)| steps())
        .with_context(|| {
            format!(
                "its Lumenveil-Setup.cmd (SHA-256 {setup}) isn't one the indexer knows: write its \
                 steps in SETUPS"
            )
        })?;
    let uninstall = sum("Lumenveil-Uninstall.cmd")?;
    if !UNINSTALLS.contains(&uninstall.as_str()) {
        anyhow::bail!(
            "its Lumenveil-Uninstall.cmd (SHA-256 {uninstall}) isn't one the indexer knows: check \
             what it removes against SETTINGS and WRITES, then add it to UNINSTALLS"
        );
    }

    Ok(Release {
        version: published.tag_name.trim_start_matches('v').to_owned(),
        url,
        updated: published.day(),
        changes: Some(published.html_url),
        files,
        from: BTreeMap::new(),
        inside: None,
        setup: steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The files of 0.1.1 the scripts are known by, and another.
    fn files(setup: &str) -> Vec<(String, String)> {
        vec![
            ("CoAVolFog.dll".to_owned(), "19ebfe".to_owned()),
            ("Lumenveil-Setup.cmd".to_owned(), setup.to_owned()),
            (
                "Lumenveil-Uninstall.cmd".to_owned(),
                UNINSTALLS[0].to_owned(),
            ),
        ]
    }

    fn published() -> Published {
        Published {
            tag_name: "v0.1.1".to_owned(),
            draft: false,
            prerelease: false,
            published_at: Some("2026-10-20T18:02:11Z".to_owned()),
            html_url: "https://github.com/lumenveil/Lumenveil/releases/tag/v0.1.1".to_owned(),
            assets: Vec::new(),
        }
    }

    #[test]
    fn a_release_whose_scripts_are_known_comes_with_its_setup() {
        let release = release(
            published(),
            "https://example.com/Lumenveil-0.1.1.zip".to_owned(),
            files(SETUPS[0].0),
        )
        .unwrap();

        assert_eq!(release.version, "0.1.1");
        assert_eq!(release.updated.as_deref(), Some("2026-10-20"));
        assert_eq!(
            release
                .setup
                .iter()
                .map(|step| format!("{} {}", step.run, step.arguments.join(" ")))
                .collect::<Vec<_>>(),
            [
                "Lumenveil/lamp_extract.exe --data Data --out lampdata.bin --locale {locale} \
                 --background",
                "Lumenveil/lamp_extract.exe --data Data --liquids liquiddata.bin --maps \
                 0,1,530,571 --locale {locale} --background",
            ],
            "as Lumenveil-Setup.cmd runs it"
        );
    }

    #[test]
    fn a_release_whose_scripts_changed_is_left_out() {
        let error = release(
            published(),
            "https://example.com/Lumenveil-0.2.0.zip".to_owned(),
            files("5e7"),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Lumenveil-Setup.cmd (SHA-256 5e7)"),
            "{error}"
        );
    }

    #[test]
    fn its_files_are_checked_against_its_sums() {
        let files = files(SETUPS[0].0);
        let sums: String = files
            .iter()
            .map(|(path, sum)| format!("{sum}  {path}\n"))
            .chain(["abc  Lumenveil-0.1.1.zip\n".to_owned()])
            .collect();

        assert!(checked(&files, &sums).is_ok());
        assert!(checked(&files[1..], &sums).is_err(), "one is missing");
    }
}
