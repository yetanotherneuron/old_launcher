//! Telling apart the uploads of a file attached to a GitHub release.
//!
//! A release's file keeps its URL,
//! `https://github.com/{owner}/{name}/releases/download/{tag}/{file}`, when
//! its author deletes it and uploads another under the same name; but GitHub
//! gives every upload an id of its own, which its API lists with the release.
//! What an earlier run found in such a file holds only for the upload it was
//! found in: another is downloaded and looked into again, and so is one
//! whose id can't be read.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Deserialize;

use crate::source::github;

/// The GitHub repository (`owner/name`) of the release the file at `url` is
/// attached to, if it is one.
pub fn repository(url: &str) -> Option<String> {
    let mut parts = url.strip_prefix("https://github.com/")?.splitn(6, '/');
    let (Some(owner), Some(name), Some("releases"), Some("download"), Some(_tag), Some(_file)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };

    Some(format!("{owner}/{name}"))
}

/// Whether what was found in the upload `found` holds for the one at its
/// URL `now`: when both ids are known, and the same.
pub fn same(found: Option<u64>, now: Option<u64>) -> bool {
    found.is_some() && found == now
}

/// The id of the upload each release's file is now, by its URL.
#[derive(Debug, Default)]
pub struct Uploads(HashMap<String, u64>);

impl Uploads {
    /// Reads the releases of every GitHub repository with a file among
    /// `urls`, for the id of each file's upload. A repository whose releases
    /// can't be read gives none, so its files are downloaded again.
    pub async fn look_up<'a>(
        client: &reqwest::Client,
        urls: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut wanted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for url in urls {
            if let Some(repository) = repository(url) {
                let _ = wanted
                    .entry(repository)
                    .or_default()
                    .insert(url.to_lowercase());
            }
        }

        let mut uploads = Self::default();

        for (repository, urls) in &wanted {
            match ids(client, repository, urls).await {
                Ok(ids) => uploads.0.extend(ids),
                Err(error) => {
                    println!("Release files of {repository} are downloaded again: {error:#}");
                }
            }
        }

        println!(
            "Release files: {} uploads read from {} repositories",
            uploads.0.len(),
            wanted.len()
        );

        uploads
    }

    /// The id of the upload at `url` now, if it is a release's file whose id
    /// was read.
    pub fn id(&self, url: &str) -> Option<u64> {
        self.0.get(&url.to_lowercase()).copied()
    }
}

/// A release, as GitHub's API describes it, with what tells its files'
/// uploads apart.
#[derive(Deserialize)]
struct Release {
    assets: Vec<Asset>,
}

/// A file attached to a release: one upload of it.
#[derive(Deserialize)]
struct Asset {
    id: u64,
    browser_download_url: String,
}

/// The ids of the uploads at `urls` (lowercase, files of `repository`'s
/// releases), by URL: its releases are read a hundred at a time, the newest
/// first, until each is found. A link may spell the owner otherwise than
/// GitHub does, so URLs are compared lowercase.
async fn ids(
    client: &reqwest::Client,
    repository: &str,
    urls: &BTreeSet<String>,
) -> anyhow::Result<HashMap<String, u64>> {
    let mut ids = HashMap::new();

    for page in 1.. {
        let releases: Vec<Release> = github::api(
            client,
            &format!("repos/{repository}/releases?per_page=100&page={page}"),
        )
        .await?;
        let last = releases.len() < 100;

        ids.extend(among(releases, urls));

        if last || urls.iter().all(|url| ids.contains_key(url)) {
            break;
        }
    }

    Ok(ids)
}

/// The ids of the uploads at `urls` (lowercase) attached to `releases`.
fn among(releases: Vec<Release>, urls: &BTreeSet<String>) -> impl Iterator<Item = (String, u64)> {
    releases
        .into_iter()
        .flat_map(|release| release.assets)
        .map(|asset| (asset.browser_download_url.to_lowercase(), asset.id))
        .filter(|(url, _)| urls.contains(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_files_are_known_by_their_repository() {
        assert_eq!(
            repository(
                "https://github.com/Trimitor/WDM-addons/releases/download/1.0.9-stable/WDM.zip"
            )
            .as_deref(),
            Some("Trimitor/WDM-addons")
        );

        for url in [
            "https://github.com/NoM0Re/DalaranAH/releases/latest/download/DalaranAH.zip",
            "https://github.com/o/r/releases/download/v1",
            "https://github.com/o/r/archive/refs/heads/main.zip",
            "https://addons.rising-gods.de/sites/default/files/addons/Quartz.zip",
        ] {
            assert_eq!(repository(url), None, "{url}");
        }
    }

    #[test]
    fn a_finding_holds_only_for_the_upload_it_was_found_in() {
        assert!(same(Some(7), Some(7)));
        assert!(!same(Some(7), Some(8)), "uploaded again");
        assert!(!same(Some(7), None), "the upload there now is unknown");
        assert!(!same(None, Some(7)), "found before uploads were told apart");
        assert!(!same(None, None));
    }

    #[test]
    fn uploads_are_read_from_the_releases_and_found_whatever_the_case() {
        let releases: Vec<Release> = serde_json::from_str(
            r#"[
                {
                    "tag_name": "1.0.9-stable",
                    "assets": [
                        {
                            "id": 263111,
                            "name": "WDM.zip",
                            "updated_at": "2026-06-11T09:30:00Z",
                            "browser_download_url": "https://github.com/Trimitor/WDM-addons/releases/download/1.0.9-stable/WDM.zip"
                        },
                        {
                            "id": 263112,
                            "name": "Astrolabe.zip",
                            "browser_download_url": "https://github.com/Trimitor/WDM-addons/releases/download/1.0.9-stable/Astrolabe.zip"
                        }
                    ]
                },
                { "tag_name": "1.0.8-stable", "assets": [] }
            ]"#,
        )
        .unwrap();
        let wanted = BTreeSet::from([
            "https://github.com/trimitor/wdm-addons/releases/download/1.0.9-stable/wdm.zip"
                .to_owned(),
        ]);

        let ids: HashMap<String, u64> = among(releases, &wanted).collect();
        let uploads = Uploads(ids);

        assert_eq!(
            uploads.id(
                "https://github.com/trimitor/WDM-addons/releases/download/1.0.9-stable/WDM.zip"
            ),
            Some(263_111)
        );
        assert_eq!(
            uploads.id(
                "https://github.com/Trimitor/WDM-addons/releases/download/1.0.9-stable/Astrolabe.zip"
            ),
            None,
            "not wanted"
        );
    }

    /// Reads the uploads of Trimitor's collection, as its source lists it,
    /// and of DXVK's oldest and newest releases listed, as GitHub has them
    /// now.
    #[tokio::test]
    #[ignore = "asks GitHub's API"]
    async fn real_uploads_are_read() {
        use crate::source::Source;

        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .user_agent("old_launcher-indexer-test")
            .build()
            .unwrap();
        let mut urls: Vec<String> = github::Collection("Trimitor/WDM-addons")
            .list(&client)
            .await
            .unwrap()
            .into_iter()
            .map(|listing| listing.download)
            .collect();
        urls.extend([
            "https://github.com/doitsujin/dxvk/releases/download/v1.5/dxvk-1.5.tar.gz".to_owned(),
            "https://github.com/doitsujin/dxvk/releases/download/v3.1.1/dxvk-3.1.1.tar.gz"
                .to_owned(),
        ]);

        let uploads = Uploads::look_up(&client, urls.iter().map(String::as_str)).await;

        for url in &urls {
            println!("{url}: {:?}", uploads.id(url));
            assert!(uploads.id(url).is_some(), "{url}");
        }
    }
}
