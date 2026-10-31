//! When each addon of a GitHub snapshot last changed.
//!
//! A snapshot of a GitHub repository stamps every file with the date of the
//! commit it was taken at, so the addons of one that is split (see
//! [`split`](crate::split)) would all carry that date, however long ago each
//! last changed. Each folder is dated by the last commit that changed it
//! instead, as GitHub's API says: one call per folder, made once for each
//! commit, since the record of archives keeps the dates found.

use anyhow::Context;
use old_launcher_core::catalog::Contents;
use serde::Deserialize;

use crate::pin;
use crate::source::github;

/// The repository (`owner/name`) and the commit of a GitHub snapshot pinned
/// to it, from its URL: `https://github.com/{repository}/archive/{commit}.zip`.
fn snapshot(url: &str) -> Option<(&str, &str)> {
    let (repository, file) = url
        .strip_prefix("https://github.com/")?
        .split_once("/archive/")?;
    let commit = file.strip_suffix(".zip")?;

    (repository.split('/').count() == 2 && pin::commit(commit)).then_some((repository, commit))
}

/// Dates each folder of `contents`, found in the archive at `url`, by the
/// last commit that changed it, when `url` is a GitHub snapshot of a commit.
/// A folder dated already keeps its date, and one at the repository's root
/// none: everything in the repository is its own, which the snapshot's
/// commit changed. Stops at the first lookup that fails, the folders left
/// keeping no date, until a later run asks again.
pub async fn date(
    client: &reqwest::Client,
    url: &str,
    contents: &mut Contents,
) -> anyhow::Result<()> {
    let Some((repository, commit)) = snapshot(url) else {
        return Ok(());
    };

    for part in &mut contents.parts {
        // The snapshot wraps the repository in a folder of its own.
        let Some((_, path)) = part.path.split_once('/') else {
            continue;
        };

        if part.updated.is_none() {
            part.updated = Some(last_changed(client, repository, commit, path).await?);
        }
    }

    Ok(())
}

/// The day (`YYYY-MM-DD`) of the last commit of the GitHub repository
/// `owner/name`, up to `commit`, that changed what is at `path`.
async fn last_changed(
    client: &reqwest::Client,
    repository: &str,
    commit: &str,
    path: &str,
) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Listed {
        commit: Commit,
    }

    #[derive(Deserialize)]
    struct Commit {
        committer: Committer,
    }

    #[derive(Deserialize)]
    struct Committer {
        date: String,
    }

    let listed: Vec<Listed> = github::api(
        client,
        &format!(
            "repos/{repository}/commits?sha={commit}&path={}&per_page=1",
            pin::query(path)
        ),
    )
    .await?;

    listed
        .into_iter()
        .next()
        .and_then(|listed| listed.commit.committer.date.get(..10).map(str::to_owned))
        .with_context(|| format!("https://github.com/{repository} has no {path} at {commit}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_snapshot_of_a_commit_is_dated_by_its_folders() {
        let commit = "42b037944fc7b75d8670ee9208da270e409e093d";

        assert_eq!(
            snapshot(&format!(
                "https://github.com/UndeadRogue/wow-3.3.5a-addons/archive/{commit}.zip"
            )),
            Some(("UndeadRogue/wow-3.3.5a-addons", commit))
        );

        for url in [
            "https://github.com/o/r/archive/refs/heads/main.zip".to_owned(),
            format!("https://github.com/o/r/raw/{commit}/src/Addons/ACP.zip"),
            format!("https://gitlab.com/o/r/-/archive/{commit}/r-{commit}.zip"),
            "https://addons.rising-gods.de/sites/default/files/addons/LUI.zip".to_owned(),
        ] {
            assert_eq!(snapshot(&url), None, "{url}");
        }
    }

    #[tokio::test]
    async fn folders_dated_already_or_at_the_root_are_not_looked_up() {
        let mut contents = Contents {
            folders: Vec::new(),
            parts: ["r-main", "r-main/Addons/Foo"]
                .into_iter()
                .map(|path| old_launcher_core::catalog::Part {
                    name: "Foo".into(),
                    path: path.into(),
                    dependencies: Vec::new(),
                    notes: None,
                    inside: None,
                    updated: None,
                })
                .collect(),
            notes: None,
            updated: None,
        };
        contents.parts[1].updated = Some("2026-01-01".into());
        let before = contents.clone();
        let commit = "a".repeat(40);

        // Any call would fail: the client can't reach anything.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap();

        date(
            &client,
            &format!("https://github.com/o/r/archive/{commit}.zip"),
            &mut contents,
        )
        .await
        .unwrap();

        assert_eq!(contents, before);
    }

    /// Dates the folders of UndeadRogue's addons at their newest commit, as
    /// GitHub has them now, and prints them.
    #[tokio::test]
    #[ignore = "calls GitHub's API"]
    async fn real_folders_are_dated_by_their_last_commit() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .user_agent("old_launcher-indexer-test")
            .build()
            .unwrap();
        let url = "https://github.com/UndeadRogue/wow-3.3.5a-addons/archive/\
                   42b037944fc7b75d8670ee9208da270e409e093d.zip";
        let archive = crate::fetch::fetch(&client, url).await.unwrap();
        let mut contents = Contents::inspect(&archive).unwrap();

        date(&client, url, &mut contents).await.unwrap();

        for part in &contents.parts {
            println!("{} ({}): {:?}", part.name, part.path, part.updated);
            assert!(part.updated.is_some(), "{}", part.name);
        }
    }
}
