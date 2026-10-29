//! Dungeon Maps, Trimitor's maps from later versions of the game for 3.3.5a:
//! one patch per language, `patch-<language>-M.MPQ`, which goes in that
//! language's folder of `Data`.
//!
//! Each stable GitHub release attaches the patch of every language it comes
//! in, at addresses that differ only in the language. Its beta releases,
//! the caves and mines, need his WDM addon, and aren't offered.

use std::collections::BTreeMap;

use anyhow::Context;
use old_launcher_core::Locale;
use old_launcher_core::mods::{self, LOCALE, Mod, Release, Source};

use crate::fetch::fetch;
use crate::mods::{Known, LeftOut, Published, published};

/// The repository its releases are published in.
const REPOSITORY: &str = "Trimitor/WDM-patch";

/// Where each language's patch goes in the client folder.
const PATCH: &str = "Data/{locale}/patch-{locale}-M.MPQ";

/// The first release listed: the first GitHub gives checksums for. Earlier
/// ones would take downloading every language's patch, about half a
/// gigabyte each, for releases nobody installed through the launcher.
const FIRST: &[u32] = &[2, 4, 5];

/// Dungeon Maps, with every stable release from [`FIRST`] on, the newest
/// first. Their files go to `found`, and the releases left out to
/// `left_out`, with why.
pub async fn gather(
    client: &reqwest::Client,
    known: &Known,
    found: &mut Known,
    left_out: &mut LeftOut,
) -> anyhow::Result<Mod> {
    let mut releases = Vec::new();

    for published in published(client, REPOSITORY).await? {
        let tag = published.tag_name.clone();
        if number(&tag).as_slice() < FIRST {
            continue;
        }

        match understood(client, known, found, published).await {
            Ok(release) => {
                println!(
                    "Dungeon Maps {}: {}",
                    release.version,
                    release
                        .languages
                        .keys()
                        .map(|language| language.code())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                releases.push(release);
            }
            Err(error) => {
                left_out.note(
                    format!("Dungeon Maps {tag}: left out, {error:#}"),
                    &releases,
                );
            }
        }
    }

    Ok(Mod {
        id: "dungeon-maps".to_owned(),
        name: "Dungeon Maps".to_owned(),
        description: "Maps from later versions of the game, for the dungeons and raids that have \
                      none. Trimitor's WDM and Astrolabe addons are recommended with it."
            .to_owned(),
        source: Source {
            name: "Trimitor on GitHub".to_owned(),
            page: format!("https://github.com/{REPOSITORY}"),
        },
        releases,
        settings: Vec::new(),
        writes: Vec::new(),
        config: BTreeMap::new(),
        platforms: Vec::new(),
        requires: Vec::new(),
        excludes: Vec::new(),
        conflicts: Vec::new(),
        alternatives: Vec::new(),
        version_in: None,
    })
}

/// `published` as the launcher installs it: the patch of each language,
/// whose checksum GitHub gives or `known` has, or else is downloaded (and
/// kept in `found`).
async fn understood(
    client: &reqwest::Client,
    known: &Known,
    found: &mut Known,
    published: Published,
) -> anyhow::Result<Release> {
    let mut sums = BTreeMap::new();

    for asset in &published.assets {
        let Some(language) = language(&asset.name) else {
            continue;
        };
        let url = &asset.browser_download_url;
        let sum = match (known.get(url), &asset.digest) {
            (Some(files), _) => files
                .first()
                .map(|(_, sum)| sum.clone())
                .context("an earlier run found nothing in it")?,
            (None, Some(digest)) if digest.starts_with("sha256:") => {
                digest.trim_start_matches("sha256:").to_owned()
            }
            (None, _) => mods::sha256(&fetch(client, url).await?),
        };
        let _ = found.insert(url.clone(), vec![(asset.name.clone(), sum.clone())]);
        let _ = sums.insert(language, (url.clone(), sum));
    }

    release(published, &sums)
}

/// `published` with each language's patch, by address, with its SHA-256:
/// addresses that differ only in the language, or it is left out.
fn release(
    published: Published,
    sums: &BTreeMap<Locale, (String, String)>,
) -> anyhow::Result<Release> {
    let (first, (url, _)) = sums.first_key_value().context("it has no patch")?;
    let template = url.replace(
        &format!("patch-{first}-M.MPQ"),
        &format!("patch-{LOCALE}-M.MPQ"),
    );
    if let Some((language, _)) = sums
        .iter()
        .find(|(language, (url, _))| *url != template.replace(LOCALE, language.code()))
    {
        anyhow::bail!("its {language} patch is elsewhere than the others");
    }

    Ok(Release {
        version: published
            .tag_name
            .trim_end_matches("-stable")
            .trim_start_matches('v')
            .to_owned(),
        url: template,
        updated: published.day(),
        changes: Some(published.html_url),
        files: Vec::new(),
        from: BTreeMap::new(),
        inside: None,
        setup: Vec::new(),
        languages: sums
            .iter()
            .map(|(language, (_, sum))| (*language, vec![(PATCH.to_owned(), sum.clone())]))
            .collect(),
    })
}

/// The language of the release file named `name`, if it is a patch in a
/// language of the game: `enUS` for `patch-enUS-M.MPQ`.
fn language(name: &str) -> Option<Locale> {
    Locale::parse(name.strip_prefix("patch-")?.strip_suffix("-M.MPQ")?)
}

/// The numbers of a release tag: `2.4.5-stable` is 2, 4, 5. What isn't a
/// number counts as 0.
fn number(tag: &str) -> Vec<u32> {
    tag.trim_end_matches("-stable")
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published(tag: &str) -> Published {
        Published {
            tag_name: tag.to_owned(),
            draft: false,
            prerelease: false,
            published_at: Some("2026-06-22T18:04:39Z".to_owned()),
            html_url: format!("https://github.com/Trimitor/WDM-patch/releases/tag/{tag}"),
            assets: Vec::new(),
        }
    }

    fn at(tag: &str, language: &str) -> String {
        format!(
            "https://github.com/Trimitor/WDM-patch/releases/download/{tag}/patch-{language}-M.MPQ"
        )
    }

    #[test]
    fn a_release_is_the_patch_of_each_language() {
        let sums = BTreeMap::from([
            (Locale::DeDe, (at("2.4.5-stable", "deDE"), "d1".to_owned())),
            (Locale::EnUs, (at("2.4.5-stable", "enUS"), "e2".to_owned())),
        ]);

        let release = release(published("2.4.5-stable"), &sums).unwrap();

        assert_eq!(release.version, "2.4.5");
        assert_eq!(release.url, at("2.4.5-stable", "{locale}"));
        assert_eq!(release.updated.as_deref(), Some("2026-06-22"));
        assert!(release.files.is_empty());
        assert_eq!(
            release.languages[&Locale::EnUs],
            [(PATCH.to_owned(), "e2".to_owned())]
        );
        assert_eq!(release.languages.len(), 2);
    }

    #[test]
    fn a_release_whose_patches_dont_line_up_is_left_out() {
        assert!(release(published("2.4.5-stable"), &BTreeMap::new()).is_err());

        let sums = BTreeMap::from([
            (Locale::DeDe, (at("2.4.5-stable", "deDE"), "d1".to_owned())),
            (Locale::EnUs, (at("2.4.4-stable", "enUS"), "e2".to_owned())),
        ]);
        assert!(release(published("2.4.5-stable"), &sums).is_err());

        assert_eq!(language("patch-enUS-M.MPQ"), Some(Locale::EnUs));
        assert_eq!(language("patch-enUS-N.MPQ"), None, "the caves' beta");
        assert_eq!(language("patch-english-M.MPQ"), None);

        assert!(number("2.4.2-stable").as_slice() < FIRST);
        assert!(number("2.4.5-stable").as_slice() >= FIRST);
        assert!(number("2.4.10-stable") > number("2.4.9-stable"));
    }
}
