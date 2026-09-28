//! locus313's curated collection:
//! <https://github.com/locus313/WoW-3.3.5a-Addons>.
//!
//! The repository keeps one folder per addon and a CI job publishes each as a
//! zip in its latest release (suites like ElvUI or Titan as one zip). The
//! README's tables name and describe every folder. The job stamps the zips'
//! files with the build date, so an addon's date is the last commit touching
//! its folder instead (one GitHub API call each).

use old_launcher_core::catalog;
use serde::Deserialize;

use crate::fetch::fetch;
use crate::source::{Listing, Source, Updated, github};

const REPOSITORY: &str = "https://github.com/locus313/WoW-3.3.5a-Addons";
const API: &str = "repos/locus313/WoW-3.3.5a-Addons";
const README: &str = "https://raw.githubusercontent.com/locus313/WoW-3.3.5a-Addons/main/README.md";

/// locus313's curated collection.
pub struct Collection;

/// A README table row: `| [Name](Folder/) | … | Description |`.
#[derive(Debug, PartialEq, Eq)]
struct Row {
    name: String,
    folder: String,
    description: String,
}

impl Source for Collection {
    fn info(&self) -> catalog::Source {
        catalog::Source {
            id: "locus313".into(),
            name: "locus313".into(),
            homepage: REPOSITORY.into(),
        }
    }

    /// Lists the zips of the latest release, described by the README.
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>> {
        #[derive(Deserialize)]
        struct Release {
            assets: Vec<Asset>,
        }

        #[derive(Deserialize)]
        struct Asset {
            name: String,
            browser_download_url: String,
        }

        #[derive(Deserialize)]
        struct Commit {
            commit: Details,
        }

        #[derive(Deserialize)]
        struct Details {
            committer: Signature,
        }

        #[derive(Deserialize)]
        struct Signature {
            date: String,
        }

        let release: Release = github::api(client, &format!("{API}/releases/latest")).await?;
        let rows = rows(&String::from_utf8_lossy(&fetch(client, README).await?));
        let mut listings = Vec::new();

        for asset in release.assets {
            let Some(stem) = asset.name.strip_suffix(".zip") else {
                continue;
            };
            let folder = folder(stem);
            let row = rows
                .iter()
                .find(|row| row.folder == folder || row.name == folder);

            // The README links some folders under another spelling
            // (`!Swatter` as `Swatter/`), so both are tried.
            let mut updated = None;

            for path in [Some(folder.as_str()), row.map(|row| row.name.as_str())]
                .into_iter()
                .flatten()
            {
                let commits: Vec<Commit> =
                    github::api(client, &format!("{API}/commits?path={path}&per_page=1")).await?;

                if let Some(commit) = commits.first() {
                    updated = commit.commit.committer.date.get(..10).map(str::to_owned);
                    break;
                }
            }

            listings.push(Listing {
                name: row.map_or_else(|| folder.clone(), |row| row.name.clone()),
                description: row.map(|row| row.description.clone()).unwrap_or_default(),
                download: asset.browser_download_url,
                image: None,
                page: Some(format!("{REPOSITORY}/tree/main/{folder}")),
                links: Vec::new(),
                updated: Updated::Given(updated),
                require_wotlk: false,
            });
        }

        Ok(listings)
    }
}

/// The folder a release zip packs, from its name: `Auc-Advanced-5.9.4960`,
/// `HidingBar-v3.4.20` and `Chronos-git-cc371e0` pack `Auc-Advanced`,
/// `HidingBar` and `Chronos`.
fn folder(stem: &str) -> String {
    let parts: Vec<&str> = stem.split('-').collect();
    let version = parts
        .iter()
        .skip(1)
        .position(|part| {
            let version = part.strip_prefix('v').unwrap_or(part);
            *part == "git" || version.starts_with(|c: char| c.is_ascii_digit())
        })
        .map_or(parts.len(), |position| position + 1);

    parts[..version].join("-")
}

/// Reads the README's addon tables.
fn rows(readme: &str) -> Vec<Row> {
    readme
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("| [")?;
            let (name, rest) = rest.split_once("](")?;
            let (folder, rest) = rest.split_once(')')?;
            let description = rest.trim_end().trim_end_matches('|').rsplit('|').next()?;
            // Drop trailing `([details](…))` pointers.
            let description = description
                .split_once(" ([")
                .map_or(description, |(description, _)| description);

            Some(Row {
                name: name.to_owned(),
                folder: folder.trim_end_matches('/').to_owned(),
                description: description.trim().to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readme_rows_give_names_folders_and_descriptions() {
        let readme = "\
            ### Standalone Addons\n\
            | Addon | Version | Description |\n\
            |-------|---------|-------------|\n\
            | [ACP](ACP/) | 3.2.0.118 | Addon Control Panel ([details](ACP/README.md)) |\n\
            | [!Swatter](Swatter/) | Error-handling and debugging tool |\n";

        assert_eq!(
            rows(readme),
            [
                Row {
                    name: "ACP".into(),
                    folder: "ACP".into(),
                    description: "Addon Control Panel".into(),
                },
                Row {
                    name: "!Swatter".into(),
                    folder: "Swatter".into(),
                    description: "Error-handling and debugging tool".into(),
                },
            ]
        );
    }

    #[test]
    fn release_zip_names_give_their_folder() {
        for (stem, expected) in [
            ("ACP-3.2.0.118", "ACP"),
            ("Auc-Advanced-5.9.4960-WhackyWallaby", "Auc-Advanced"),
            ("HidingBar-v3.4.20", "HidingBar"),
            ("Chronos-git-cc371e0", "Chronos"),
            ("DBM-Party-WotLK-git-cc371e0", "DBM-Party-WotLK"),
            ("Titan-4.3.8.30300-Revision-386", "Titan"),
            ("DejaClassicStats-30400r10", "DejaClassicStats"),
            ("Recount", "Recount"),
        ] {
            assert_eq!(folder(stem), expected);
        }
    }
}
