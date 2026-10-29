//! Patch Y, as Lau's "Wrath HD" visual upgrade publishes it in Lau Setup's
//! releases. Players know it as Patch Y, which its mods are named after.
//!
//! Patch Y comes in three editions, each a mod of its own: for clients with
//! an HD model pack (its `patch-f` archives), with the new spell visuals or
//! the original ones, and for clients without one. A client holds one at
//! most, so each names the other two as its alternatives.
//!
//! Each game version is a release tagged `payload-<version>`, whose
//! `manual-downloads.json` names a zip per edition, published once and never
//! rebuilt. A zip holds `patch-y.mpq`, which goes both in `Data` and in the
//! folder of the client's language, and the new spell visuals' pair besides.
//! Its `package-manifest.json` says where each goes, for every language: a
//! zip whose manifest says otherwise than [`PLACES`] is left out, until this
//! module learns it. Lau Setup's other choices, maps and loading screens
//! with a `Wow.exe` of its own, aren't offered.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use anyhow::Context;
use old_launcher_core::mods::{self, LOCALE, Mod, Release, Source};
use serde::Deserialize;

use crate::fetch::fetch;
use crate::mods::{Known, LeftOut, Published, published};

/// The repository its releases are published in.
const REPOSITORY: &str = "CRSD-Lau/Lau-Setup";

/// The file of a game version's release that names its zips.
const DOWNLOADS: &str = "manual-downloads.json";

/// The file of a zip that says where its archives go.
const MANIFEST: &str = "package-manifest.json";

/// The files of a zip that stay out of the client.
const DOCUMENTS: [&str; 2] = ["README.txt", MANIFEST];

/// Where each archive of the zips goes in the client folder.
const PLACES: &[(&str, &[&str])] = &[
    (
        "patch-y.mpq",
        &["Data/patch-y.mpq", "Data/{locale}/patch-{locale}-Y.MPQ"],
    ),
    ("patch-s.mpq", &["Data/patch-s.mpq"]),
    (
        "patch-LOCALE-S.MPQ",
        &["Data/{locale}/patch-{locale}-S.MPQ"],
    ),
];

/// An HD model pack's archives, which the HD editions are made for.
const HD_MODELS: [&str; 2] = ["Data/patch-f.mpq", "Data/{locale}/patch-{locale}-F.MPQ"];

/// What every edition is for, which its releases don't change: what each
/// release brings is its notes' to say. Which client an edition is for goes
/// unsaid, since a client is offered only those it suits.
const DESCRIPTION: &str = "Makes raid bosses' abilities easier to see and avoid.";

/// An edition of Patch Y.
struct Edition {
    id: &'static str,
    /// What tells it from the others an HD client is offered, when one is.
    name: &'static str,
    /// What `manual-downloads.json` and the manifests call it.
    package: &'static str,
    /// Whether it is made for an HD model pack.
    hd: bool,
    /// Whether it brings the new spell visuals.
    spells: bool,
}

/// The editions, a mod each.
const EDITIONS: [Edition; 3] = [
    Edition {
        id: "lau-hd-new-spells",
        name: "Patch Y: new spell visuals",
        package: "HD-New-Spells",
        hd: true,
        spells: true,
    },
    Edition {
        id: "lau-hd-original-spells",
        name: "Patch Y: original spell visuals",
        package: "HD-Original-Spells",
        hd: true,
        spells: false,
    },
    Edition {
        id: "lau-non-hd",
        name: "Patch Y",
        package: "Non-HD",
        hd: false,
        spells: false,
    },
];

/// A game version's `manual-downloads.json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Downloads {
    version: String,
    packages: Vec<Package>,
}

/// An edition's zip, as `manual-downloads.json` names it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Package {
    edition: String,
    url: String,
    sha256: String,
}

/// A zip's `package-manifest.json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Manifest {
    edition: String,
    version: String,
    files: Vec<Packed>,
    #[serde(rename = "RequiresHDModelF")]
    requires_hd_models: bool,
    #[serde(rename = "RequiresSpellSPair")]
    spells: bool,
}

/// An archive of a zip, and where it goes in each language.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Packed {
    archive_name: String,
    sha256: String,
    destinations: Vec<Destination>,
}

/// Where an archive goes in a client of `locale`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Destination {
    locale: String,
    path: String,
}

impl Edition {
    /// The edition as a mod, of `releases`.
    fn game_mod(&self, releases: Vec<Release>) -> Mod {
        let models: Vec<String> = HD_MODELS.iter().copied().map(str::to_owned).collect();
        let (requires, excludes) = if self.hd {
            (models, Vec::new())
        } else {
            (Vec::new(), models)
        };

        Mod {
            id: self.id.to_owned(),
            name: self.name.to_owned(),
            description: DESCRIPTION.to_owned(),
            source: Source {
                name: "Lau Setup on GitHub".to_owned(),
                page: format!("https://github.com/{REPOSITORY}"),
            },
            releases,
            settings: Vec::new(),
            writes: Vec::new(),
            config: BTreeMap::new(),
            platforms: Vec::new(),
            requires,
            excludes,
            alternatives: EDITIONS
                .iter()
                .filter(|other| other.id != self.id)
                .map(|other| other.id.to_owned())
                .collect(),
            version_in: None,
        }
    }
}

/// The editions, each with every game version understood, the files of
/// their zips kept in `found` and the versions left out noted in
/// `left_out`.
pub async fn gather(
    client: &reqwest::Client,
    known: &Known,
    found: &mut Known,
    left_out: &mut LeftOut,
) -> anyhow::Result<Vec<Mod>> {
    let mut releases: [Vec<Release>; 3] = Default::default();

    for published in published(client, REPOSITORY).await? {
        // The installer's own releases.
        let Some(version) = published.tag_name.strip_prefix("payload-") else {
            continue;
        };

        let downloads = match downloads(client, &published).await {
            Ok(downloads) => downloads,
            Err(error) => {
                for (edition, listed) in EDITIONS.iter().zip(&releases) {
                    left_out.note(
                        format!("{} {version}: left out, {error:#}", edition.name),
                        listed,
                    );
                }
                continue;
            }
        };

        for (edition, listed) in EDITIONS.iter().zip(&mut releases) {
            match release(client, known, found, &published, &downloads, edition).await {
                Ok(release) => {
                    println!(
                        "{} {}: {} files",
                        edition.name,
                        release.version,
                        release.files.len()
                    );
                    listed.push(release);
                }
                Err(error) => left_out.note(
                    format!("{} {version}: left out, {error:#}", edition.name),
                    listed,
                ),
            }
        }
    }

    Ok(EDITIONS
        .iter()
        .zip(releases)
        .map(|(edition, releases)| edition.game_mod(releases))
        .collect())
}

/// The `manual-downloads.json` of the game version `published`.
async fn downloads(client: &reqwest::Client, published: &Published) -> anyhow::Result<Downloads> {
    let url = published
        .asset(|name| name == DOWNLOADS)
        .with_context(|| format!("it has no {DOWNLOADS}"))?;

    serde_json::from_slice(&fetch(client, &url).await?).with_context(|| format!("reading {url}"))
}

/// `edition`'s release of the game version `published`, whose zips
/// `downloads` names: the files of its zip as `known` says, or else
/// downloaded, checked against its manifest (see [`understood`]) and kept
/// in `found`.
async fn release(
    client: &reqwest::Client,
    known: &Known,
    found: &mut Known,
    published: &Published,
    downloads: &Downloads,
    edition: &Edition,
) -> anyhow::Result<Release> {
    let package = downloads
        .packages
        .iter()
        .find(|package| package.edition == edition.package)
        .with_context(|| format!("{DOWNLOADS} names no {} zip", edition.package))?;

    let files = match known.get(&package.url) {
        Some(files) => files.clone(),
        None => {
            let zip = fetch(client, &package.url).await?;
            anyhow::ensure!(
                mods::sha256(&zip) == package.sha256,
                "its zip isn't the one {DOWNLOADS} names"
            );
            let manifest: Manifest = serde_json::from_slice(&member(&zip, MANIFEST)?)
                .with_context(|| format!("reading its {MANIFEST}"))?;
            let files = tokio::task::spawn_blocking(move || mods::inspect(&zip)).await??;
            understood(&manifest, edition, &downloads.version, &files)?;

            files
        }
    };
    let _ = found.insert(package.url.clone(), files.clone());
    let (placed, from) = placed(&files)?;

    Ok(Release {
        version: downloads.version.clone(),
        url: package.url.clone(),
        updated: published.day(),
        changes: Some(published.html_url.clone()),
        files: placed,
        from,
        inside: None,
        setup: Vec::new(),
    })
}

/// The file `name` of the zip `archive`.
fn member(archive: &[u8], name: &str) -> anyhow::Result<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))?;
    let mut file = zip
        .by_name(name)
        .with_context(|| format!("it has no {name}"))?;
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes)?;

    Ok(bytes)
}

/// Where the archive `name` of a zip goes, as [`PLACES`] says.
fn places(name: &str) -> Option<&'static [&'static str]> {
    PLACES
        .iter()
        .find(|(archive, _)| *archive == name)
        .map(|(_, places)| *places)
}

/// Checks that `manifest`, of `edition`'s zip of `version` holding `files`,
/// says of it what this module knows: the edition, what it is made for,
/// and every archive in it going where [`PLACES`] says, in every language.
fn understood(
    manifest: &Manifest,
    edition: &Edition,
    version: &str,
    files: &[(String, String)],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        manifest.edition == edition.package && manifest.version == version,
        "its manifest is of {} {}",
        manifest.edition,
        manifest.version
    );
    anyhow::ensure!(
        manifest.requires_hd_models == edition.hd && manifest.spells == edition.spells,
        "its manifest makes it for other models or spells than the edition"
    );

    for packed in &manifest.files {
        let name = &packed.archive_name;
        let places = places(name)
            .with_context(|| format!("its manifest places {name}, which the indexer doesn't"))?;
        anyhow::ensure!(
            files
                .iter()
                .any(|(path, sum)| path == name && *sum == packed.sha256),
            "{name} isn't in it as its manifest says"
        );

        let mut by_locale: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for destination in &packed.destinations {
            let _ = by_locale
                .entry(&destination.locale)
                .or_default()
                .insert(destination.path.replace(&destination.locale, LOCALE));
        }
        let known: BTreeSet<String> = places.iter().copied().map(str::to_owned).collect();
        anyhow::ensure!(
            !by_locale.is_empty() && by_locale.values().all(|placed| *placed == known),
            "its manifest puts {name} elsewhere than the indexer does"
        );
    }

    for (path, _) in files {
        anyhow::ensure!(
            DOCUMENTS.contains(&path.as_str())
                || manifest
                    .files
                    .iter()
                    .any(|packed| packed.archive_name == *path),
            "it holds {path}, which its manifest doesn't place"
        );
    }

    Ok(())
}

/// What a zip puts in the client folder: the files of a release, and the
/// archive of the zip each is (see [`Release::from`]).
type Placed = (Vec<(String, String)>, BTreeMap<String, String>);

/// The files a zip holding `files` puts in the client folder, with the
/// archive of the zip each is.
fn placed(files: &[(String, String)]) -> anyhow::Result<Placed> {
    let mut placed = Vec::new();
    let mut from = BTreeMap::new();

    for (name, sum) in files
        .iter()
        .filter(|(path, _)| !DOCUMENTS.contains(&path.as_str()))
    {
        let places = places(name)
            .with_context(|| format!("it holds {name}, which the indexer doesn't place"))?;
        for place in places {
            placed.push(((*place).to_owned(), sum.clone()));
            let _ = from.insert((*place).to_owned(), name.clone());
        }
    }

    Ok((placed, from))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Non-HD manifest of 3.1.0, shortened to two languages.
    const MANIFEST_3_1_0: &str = r#"{
        "Author": "Neil Mitchell",
        "ClientBuild": "3.3.5a.12340",
        "ClientLocales": ["deDE", "enGB"],
        "Edition": "Non-HD",
        "Files": [{
            "ArchiveName": "patch-y.mpq",
            "AssetId": "Y-Non-HD-Consecration-On",
            "Bytes": 46747861,
            "Destinations": [
                {"Locale": "deDE", "Path": "Data/patch-y.mpq"},
                {"Locale": "deDE", "Path": "Data/deDE/patch-deDE-Y.MPQ"},
                {"Locale": "enGB", "Path": "Data/patch-y.mpq"},
                {"Locale": "enGB", "Path": "Data/enGB/patch-enGB-Y.MPQ"}
            ],
            "Sha256": "6efe03"
        }],
        "RequiresHDModelF": false,
        "RequiresSpellSPair": false,
        "Version": "3.1.0"
    }"#;

    fn files() -> Vec<(String, String)> {
        vec![
            ("README.txt".to_owned(), "a1".to_owned()),
            (MANIFEST.to_owned(), "b2".to_owned()),
            ("patch-y.mpq".to_owned(), "6efe03".to_owned()),
        ]
    }

    #[test]
    fn a_zip_is_understood_by_its_manifest() {
        let manifest: Manifest = serde_json::from_str(MANIFEST_3_1_0).unwrap();
        let non_hd = &EDITIONS[2];

        understood(&manifest, non_hd, "3.1.0", &files()).unwrap();

        let error = understood(&manifest, &EDITIONS[0], "3.1.0", &files()).unwrap_err();
        assert_eq!(error.to_string(), "its manifest is of Non-HD 3.1.0");

        let mut elsewhere: Manifest = serde_json::from_str(MANIFEST_3_1_0).unwrap();
        elsewhere.files[0].destinations[1].path = "Data/deDE/patch-deDE-Z.MPQ".to_owned();
        assert_eq!(
            understood(&elsewhere, non_hd, "3.1.0", &files())
                .unwrap_err()
                .to_string(),
            "its manifest puts patch-y.mpq elsewhere than the indexer does"
        );

        let mut more = files();
        more.push(("patch-q.mpq".to_owned(), "c3".to_owned()));
        assert_eq!(
            understood(&manifest, non_hd, "3.1.0", &more)
                .unwrap_err()
                .to_string(),
            "it holds patch-q.mpq, which its manifest doesn't place"
        );
    }

    #[test]
    fn its_archives_go_in_data_and_in_the_folder_of_the_language() {
        let mut files = files();
        files.push(("patch-LOCALE-S.MPQ".to_owned(), "d4".to_owned()));
        let (placed, from) = placed(&files).unwrap();

        assert_eq!(
            placed,
            [
                ("Data/patch-y.mpq".to_owned(), "6efe03".to_owned()),
                (
                    "Data/{locale}/patch-{locale}-Y.MPQ".to_owned(),
                    "6efe03".to_owned()
                ),
                (
                    "Data/{locale}/patch-{locale}-S.MPQ".to_owned(),
                    "d4".to_owned()
                ),
            ]
        );
        assert_eq!(from["Data/{locale}/patch-{locale}-Y.MPQ"], "patch-y.mpq");
        assert_eq!(
            from["Data/{locale}/patch-{locale}-S.MPQ"],
            "patch-LOCALE-S.MPQ"
        );
    }

    #[test]
    fn the_editions_are_alternatives_for_clients_with_or_without_hd_models() {
        let mods: Vec<Mod> = EDITIONS
            .iter()
            .map(|edition| edition.game_mod(Vec::new()))
            .collect();

        assert_eq!(
            mods[0].alternatives,
            ["lau-hd-original-spells", "lau-non-hd"]
        );
        assert_eq!(
            mods[2].alternatives,
            ["lau-hd-new-spells", "lau-hd-original-spells"]
        );
        assert_eq!(mods[1].requires, HD_MODELS);
        assert!(mods[1].excludes.is_empty());
        assert_eq!(mods[2].excludes, HD_MODELS);
        assert!(mods[2].requires.is_empty());
    }
}
