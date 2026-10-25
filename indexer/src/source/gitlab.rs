//! Addon authors on GitLab, and the projects of theirs that hold a 3.3.5a
//! addon.
//!
//! Unlike GitHub's maintainers, only the projects named here are listed:
//! GitLab doesn't tell anonymous callers how big a project is, so listing an
//! author's every project would download all of them to keep one. Reading a
//! public project through GitLab's API needs no token.

use anyhow::Context;
use old_launcher_core::catalog;
use serde::Deserialize;

use crate::fetch::fetch;
use crate::source::{Listing, Source, Updated};

/// The authors indexed, in order of preference, with their addon projects.
pub const AUTHORS: &[Author] = &[Author {
    owner: "juxuanu",
    projects: &["kekator"],
}];

/// An author on GitLab, by user or group path, and the projects of theirs
/// that hold an addon, by path.
pub struct Author {
    pub owner: &'static str,
    pub projects: &'static [&'static str],
}

impl Source for Author {
    fn info(&self) -> catalog::Source {
        catalog::Source {
            id: format!("gitlab-{}", self.owner.to_lowercase()),
            name: format!("{} on GitLab", self.owner),
            homepage: format!("https://gitlab.com/{}", self.owner),
        }
    }

    /// Reads each project's name, description and default branch.
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>> {
        let mut listings = Vec::new();

        for project in self.projects {
            let url = api_url(self.owner, project);
            let body = fetch(client, &url).await?;

            listings.push(parse(&body).with_context(|| format!("reading {url}"))?);
        }

        Ok(listings)
    }
}

/// Where GitLab's API describes `owner`'s `project`: its full path, with
/// every slash escaped, so an owner that is a subgroup works too.
fn api_url(owner: &str, project: &str) -> String {
    format!(
        "https://gitlab.com/api/v4/projects/{}",
        format!("{owner}/{project}").replace('/', "%2F")
    )
}

/// The listing of a project as GitLab's API describes it: a snapshot of its
/// default branch.
fn parse(project: &[u8]) -> anyhow::Result<Listing> {
    #[derive(Deserialize)]
    struct Project {
        name: String,
        path: String,
        description: Option<String>,
        default_branch: String,
        web_url: String,
    }

    let Project {
        name,
        path,
        description,
        default_branch: branch,
        web_url,
    } = serde_json::from_slice(project)?;

    Ok(Listing {
        download: format!("{web_url}/-/archive/{branch}/{path}-{branch}.zip"),
        changes: Some(format!("{web_url}/-/commits/{branch}")),
        page: Some(web_url),
        name,
        description: description.unwrap_or_default().trim().to_owned(),
        image: None,
        links: Vec::new(),
        // A branch snapshot's files carry the last commit's date.
        updated: Updated::FromArchive,
        require_wotlk: true,
        // The branch moves on under the same URL.
        immutable: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_is_listed_as_a_snapshot_of_its_default_branch() {
        let project = br#"{
            "id": 1,
            "name": "Kekator",
            "path": "kekator",
            "path_with_namespace": "juxuanu/kekator",
            "description": " Guesses what the other faction said. ",
            "default_branch": "master",
            "web_url": "https://gitlab.com/juxuanu/kekator",
            "visibility": "public"
        }"#;

        let listing = parse(project).unwrap();

        assert_eq!(listing.name, "Kekator");
        assert_eq!(listing.description, "Guesses what the other faction said.");
        assert_eq!(
            listing.download,
            "https://gitlab.com/juxuanu/kekator/-/archive/master/kekator-master.zip"
        );
        assert_eq!(
            listing.page.as_deref(),
            Some("https://gitlab.com/juxuanu/kekator")
        );
        assert_eq!(
            listing.changes.as_deref(),
            Some("https://gitlab.com/juxuanu/kekator/-/commits/master")
        );
        assert_eq!(listing.updated, Updated::FromArchive);
        assert!(listing.require_wotlk);
        assert!(!listing.immutable, "the branch moves on");
    }

    #[test]
    fn a_project_without_a_description_has_an_empty_one() {
        let project = br#"{
            "name": "kekator",
            "path": "kekator",
            "description": null,
            "default_branch": "master",
            "web_url": "https://gitlab.com/juxuanu/kekator"
        }"#;

        assert_eq!(parse(project).unwrap().description, "");
    }

    #[test]
    fn projects_are_read_by_their_escaped_path() {
        assert_eq!(
            api_url("juxuanu", "kekator"),
            "https://gitlab.com/api/v4/projects/juxuanu%2Fkekator"
        );
        assert_eq!(
            api_url("group/subgroup", "addon"),
            "https://gitlab.com/api/v4/projects/group%2Fsubgroup%2Faddon"
        );
    }

    #[test]
    fn an_author_is_a_source_named_after_them() {
        let author = Author {
            owner: "juxuanu",
            projects: &["kekator"],
        };

        assert_eq!(
            author.info(),
            catalog::Source {
                id: "gitlab-juxuanu".into(),
                name: "juxuanu on GitLab".into(),
                homepage: "https://gitlab.com/juxuanu".into(),
            }
        );
    }
}
