//! CurseForge: <https://www.curseforge.com/wow>, through its API.
//!
//! The biggest addon site, with addons for every version of the game, each
//! file tagged by its author with the game versions it is for. Taken here:
//! every project with a file tagged 3.3.5 whose author lets apps other than
//! CurseForge's own download its files (`allowModDistribution`); for the
//! others the API gives no download link. Of a project's files tagged
//! 3.3.5, the most stable is taken: a release before a beta, a beta before
//! an alpha. The indexer then keeps those whose `.toc` declares 3.3.5a's
//! interface.
//!
//! As the application for the API key promised: the files download from
//! CurseForge's own servers, never copied elsewhere; a project whose author
//! takes the permission back leaves the catalog on the next run; and a run
//! makes a few dozen calls, a search page per 50 projects and a call per 50
//! files.
//!
//! The API needs a key, read from `CURSEFORGE_API_KEY` and sent to the API
//! only, never written anywhere. Without one the run fails, rather than
//! publishing a catalog without CurseForge, unless it is told to skip
//! CurseForge (`--skip-curseforge`).
//!
//! Download links name the file's id, so a new file gets a new link.

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use old_launcher_core::catalog;
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::fetch::send;
use crate::source::{Listing, Source, Updated};

/// The variable holding the API key.
pub const KEY: &str = "CURSEFORGE_API_KEY";

const API: &str = "https://api.curseforge.com/v1";

/// World of Warcraft, as the API numbers games.
const WOW: u32 = 1;

/// The version type 3.3.5 belongs to: the game's own versions, which the
/// API calls "WoW Retail", unlike those of the Classic re-releases.
const RETAIL: u32 = 517;

/// The game version the files are tagged with.
const VERSION: &str = "3.3.5";

/// How many projects a search page holds, the most the API gives.
const PAGE: usize = 50;

/// How far the API's search goes, however it is paged.
const SEARCH_LIMIT: usize = 10_000;

/// How many files to ask the details of at once.
const BATCH: usize = 50;

/// CurseForge's addon site.
pub struct Site;

/// A page of search results.
#[derive(Debug, Deserialize)]
struct Search {
    data: Vec<Project>,
    pagination: Pagination,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pagination {
    /// How many projects the search finds in all.
    total_count: usize,
}

/// A project, as search results describe it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: u64,
    name: String,
    /// What it does, in a sentence or two of plain text.
    #[serde(default)]
    summary: String,
    links: Links,
    #[serde(default)]
    screenshots: Vec<Screenshot>,
    /// The latest file for each game version and release type.
    #[serde(default)]
    latest_files_indexes: Vec<FileIndex>,
    /// Whether apps other than CurseForge's own may download its files.
    #[serde(default)]
    allow_mod_distribution: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Links {
    /// The project's page on the site.
    website_url: String,
}

#[derive(Debug, Deserialize)]
struct Screenshot {
    url: String,
}

/// A project's latest file for a game version and release type.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileIndex {
    game_version: String,
    file_id: u64,
    /// 1 for a release, 2 for a beta, 3 for an alpha.
    release_type: u8,
    #[serde(default)]
    game_version_type_id: Option<u32>,
}

/// The answer to a request for files.
#[derive(Debug, Deserialize)]
struct Files {
    data: Vec<File>,
}

/// A file's details.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    id: u64,
    /// When it was uploaded, as `2010-07-17T11:42:03.26Z`.
    file_date: String,
    /// Where to download it: none when its project keeps its files to
    /// CurseForge's own apps.
    #[serde(default)]
    download_url: Option<String>,
}

impl Source for Site {
    fn info(&self) -> catalog::Source {
        catalog::Source {
            id: "curseforge".into(),
            name: "CurseForge".into(),
            homepage: "https://www.curseforge.com/wow".into(),
        }
    }

    /// Searches every project with a file tagged 3.3.5, then asks for the
    /// details of the file to list from each whose author lets other apps
    /// download it.
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>> {
        let key = key()?;

        let projects = projects(client, &key).await.map_err(refused)?;
        let wanted: Vec<u64> = projects.iter().filter_map(wanted).collect();
        let files = files(client, &key, &wanted).await.map_err(refused)?;

        let listings = listings(projects, &files);
        anyhow::ensure!(
            !listings.is_empty(),
            "CurseForge gave no file tagged {VERSION} to download: the API changed"
        );

        Ok(listings)
    }
}

/// The API key, from [`KEY`], marked so it never shows when debugged.
fn key() -> anyhow::Result<HeaderValue> {
    let key = std::env::var(KEY).ok().filter(|key| !key.is_empty());
    let key = key.with_context(|| {
        format!(
            "{KEY} is not set: CurseForge's API needs a key. Export it in single quotes, or \
             the shell takes its $ signs for variables; or leave CurseForge out with \
             --skip-curseforge"
        )
    })?;

    let mut value =
        HeaderValue::from_str(&key).with_context(|| format!("{KEY} isn't a valid header value"))?;
    value.set_sensitive(true);

    Ok(value)
}

/// `error`, saying what likely went wrong when the API refused the key.
fn refused(error: anyhow::Error) -> anyhow::Error {
    let forbidden = error
        .downcast_ref::<reqwest::Error>()
        .and_then(reqwest::Error::status)
        .is_some_and(|status| status == reqwest::StatusCode::FORBIDDEN);

    if forbidden {
        error.context(format!(
            "CurseForge refused the key in {KEY}: a key is 60 characters, starting with \
             $2a$10$. Exported in double quotes or none, the shell takes its $ signs for \
             variables and cuts it"
        ))
    } else {
        error
    }
}

/// Every project with a file tagged [`VERSION`], each once. The search goes
/// by name, an order that holds while it is paged.
async fn projects(client: &reqwest::Client, key: &HeaderValue) -> anyhow::Result<Vec<Project>> {
    let mut projects = Vec::new();
    let mut seen = HashSet::new();

    for index in (0..SEARCH_LIMIT).step_by(PAGE) {
        let url = format!(
            "{API}/mods/search?gameId={WOW}&gameVersionTypeId={RETAIL}&gameVersion={VERSION}\
             &sortField=4&sortOrder=asc&pageSize={PAGE}&index={index}"
        );
        let page: Search = call(|| client.get(&url).header("x-api-key", key), &url).await?;

        anyhow::ensure!(
            page.pagination.total_count <= SEARCH_LIMIT,
            "CurseForge has {} projects tagged {VERSION}, more than its search gives",
            page.pagination.total_count
        );

        let last = page.data.len() < PAGE || index + PAGE >= page.pagination.total_count;
        projects.extend(
            page.data
                .into_iter()
                .filter(|project| seen.insert(project.id)),
        );

        if last {
            break;
        }
    }

    Ok(projects)
}

/// The details of the files `ids`, by id, asked for a batch at a time.
async fn files(
    client: &reqwest::Client,
    key: &HeaderValue,
    ids: &[u64],
) -> anyhow::Result<HashMap<u64, File>> {
    let url = format!("{API}/mods/files");
    let mut files = HashMap::new();

    for batch in ids.chunks(BATCH) {
        let body = serde_json::to_vec(&serde_json::json!({ "fileIds": batch }))?;
        let answer: Files = call(
            || {
                client
                    .post(&url)
                    .header("x-api-key", key)
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.clone())
            },
            &url,
        )
        .await?;

        files.extend(answer.data.into_iter().map(|file| (file.id, file)));
    }

    Ok(files)
}

/// Calls the API with the request `build` makes, reading its answer.
async fn call<T: DeserializeOwned>(
    build: impl Fn() -> reqwest::RequestBuilder,
    url: &str,
) -> anyhow::Result<T> {
    let answer = send(|| build().header("Accept", "application/json"), url).await?;

    serde_json::from_slice(&answer).with_context(|| format!("reading {url}"))
}

/// The file of `project`'s to list, if its author lets other apps download
/// it: of those tagged [`VERSION`], the most stable, and of those the
/// latest.
fn wanted(project: &Project) -> Option<u64> {
    if project.allow_mod_distribution != Some(true) {
        return None;
    }

    project
        .latest_files_indexes
        .iter()
        .filter(|index| {
            index.game_version == VERSION
                && index.game_version_type_id.is_none_or(|kind| kind == RETAIL)
        })
        .min_by_key(|index| (index.release_type, std::cmp::Reverse(index.file_id)))
        .map(|index| index.file_id)
}

/// `projects` as listings, with the `files` the API gave for them. Projects
/// without a file it gives a download link for are left out.
fn listings(projects: Vec<Project>, files: &HashMap<u64, File>) -> Vec<Listing> {
    let names = unique_names(&projects);

    projects
        .into_iter()
        .zip(names)
        .filter_map(|(project, name)| {
            let file = files.get(&wanted(&project)?)?;
            let download = file.download_url.clone()?;
            let page = project.links.website_url;

            Some(Listing {
                name,
                description: project.summary.trim().to_owned(),
                download,
                image: project
                    .screenshots
                    .into_iter()
                    .next()
                    .map(|screenshot| screenshot.url),
                changes: Some(format!("{page}/files")),
                page: Some(page),
                links: Vec::new(),
                updated: Updated::Given(file.file_date.get(..10).map(str::to_owned)),
                // Authors tag their files themselves, and a file is often
                // tagged for many versions at once.
                require_wotlk: true,
                // The link names the file's id.
                immutable: true,
            })
        })
        .collect()
}

/// The names to list `projects` under, in order. Listings are told apart by
/// name, so a name several projects share gets the project's id added.
fn unique_names(projects: &[Project]) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for project in projects {
        *counts
            .entry(project.name.trim().to_lowercase())
            .or_default() += 1;
    }

    projects
        .iter()
        .map(|project| {
            let name = project.name.trim();

            if counts
                .get(&name.to_lowercase())
                .is_some_and(|&count| count > 1)
            {
                format!("{name} ({})", project.id)
            } else {
                name.to_owned()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A search page as the API gives it, shortened: TomTom, with a release
    /// tagged 3.3.5; LittleWigs, with an alpha and a beta; DBM, whose
    /// author keeps its files to CurseForge's own apps; and one with
    /// nothing for 3.3.5 but a file tagged 3.3.5 for another version type.
    const SEARCH: &str = r#"{
        "data": [
            {"id": 18808, "gameId": 1, "name": "TomTom", "slug": "tomtom",
             "links": {"websiteUrl": "https://www.curseforge.com/wow/addons/tomtom",
                       "wikiUrl": "", "issuesUrl": null, "sourceUrl": null},
             "summary": " Simple waypoints and arrow. ", "status": 4, "classId": 1,
             "screenshots": [{"id": 1, "modId": 18808, "title": "Arrow",
                              "url": "https://media.forgecdn.net/attachments/1/1/arrow.png"}],
             "latestFilesIndexes": [
                 {"gameVersion": "11.0.2", "fileId": 5600000, "filename": "TomTom-v4.0.zip",
                  "releaseType": 1, "gameVersionTypeId": 517},
                 {"gameVersion": "3.3.5", "fileId": 439781, "filename": "TomTom-r240-release.zip",
                  "releaseType": 1, "gameVersionTypeId": 517}
             ],
             "allowModDistribution": true, "isAvailable": true},
            {"id": 4383, "name": "LittleWigs",
             "links": {"websiteUrl": "https://www.curseforge.com/wow/addons/littlewigs"},
             "summary": "Boss mods for dungeons.",
             "latestFilesIndexes": [
                 {"gameVersion": "3.3.5", "fileId": 450002, "releaseType": 3,
                  "gameVersionTypeId": 517},
                 {"gameVersion": "3.3.5", "fileId": 450001, "releaseType": 2,
                  "gameVersionTypeId": 517}
             ],
             "allowModDistribution": true},
            {"id": 3358, "name": "DBM - Deadly Boss Mods (DBM-Core)",
             "links": {"websiteUrl": "https://www.curseforge.com/wow/addons/deadly-boss-mods"},
             "latestFilesIndexes": [
                 {"gameVersion": "3.3.5", "fileId": 447605, "releaseType": 1,
                  "gameVersionTypeId": 517}
             ],
             "allowModDistribution": false},
            {"id": 9, "name": "Elsewhere",
             "links": {"websiteUrl": "https://www.curseforge.com/wow/addons/elsewhere"},
             "latestFilesIndexes": [
                 {"gameVersion": "3.3.5", "fileId": 9001, "releaseType": 1,
                  "gameVersionTypeId": 67408}
             ],
             "allowModDistribution": true}
        ],
        "pagination": {"index": 0, "pageSize": 50, "resultCount": 4, "totalCount": 4}
    }"#;

    /// The files asked for, as the API gives them, shortened. LittleWigs'
    /// beta has no download link.
    const FILES: &str = r#"{
        "data": [
            {"id": 439781, "gameId": 1, "modId": 18808, "isAvailable": true,
             "displayName": "r240-release", "fileName": "TomTom-r240-release.zip",
             "releaseType": 1, "fileStatus": 4,
             "hashes": [{"value": "897be533211854e211417deb630532e2", "algo": 2}],
             "fileDate": "2010-07-17T11:42:03.26Z", "fileLength": 372699,
             "downloadUrl": "https://edge.forgecdn.net/files/439/781/TomTom-r240-release.zip",
             "gameVersions": ["3.3.5"]},
            {"id": 450001, "modId": 4383, "fileDate": "2010-09-01T00:00:00Z",
             "downloadUrl": null}
        ]
    }"#;

    fn projects() -> Vec<Project> {
        serde_json::from_str::<Search>(SEARCH).unwrap().data
    }

    fn files() -> HashMap<u64, File> {
        serde_json::from_str::<Files>(FILES)
            .unwrap()
            .data
            .into_iter()
            .map(|file| (file.id, file))
            .collect()
    }

    #[test]
    fn the_most_stable_file_tagged_3_3_5_is_wanted_from_projects_that_allow_it() {
        let wanted: Vec<Option<u64>> = projects().iter().map(wanted).collect();

        assert_eq!(
            wanted,
            [
                Some(439_781),
                Some(450_001),
                None, // DBM keeps its files to CurseForge's own apps.
                None, // Tagged 3.3.5, but not the game's own 3.3.5.
            ]
        );
    }

    #[test]
    fn listings_download_from_curseforge_and_are_dated_by_their_file() {
        let listings = listings(projects(), &files());

        // LittleWigs' file came without a download link.
        assert_eq!(listings.len(), 1);
        let tomtom = &listings[0];

        assert_eq!(tomtom.name, "TomTom");
        assert_eq!(tomtom.description, "Simple waypoints and arrow.");
        assert_eq!(
            tomtom.download,
            "https://edge.forgecdn.net/files/439/781/TomTom-r240-release.zip"
        );
        assert_eq!(
            tomtom.page.as_deref(),
            Some("https://www.curseforge.com/wow/addons/tomtom")
        );
        assert_eq!(
            tomtom.changes.as_deref(),
            Some("https://www.curseforge.com/wow/addons/tomtom/files")
        );
        assert_eq!(
            tomtom.image.as_deref(),
            Some("https://media.forgecdn.net/attachments/1/1/arrow.png")
        );
        assert_eq!(tomtom.updated, Updated::Given(Some("2010-07-17".into())));
        assert!(tomtom.require_wotlk && tomtom.immutable);
    }

    #[test]
    fn projects_sharing_a_name_are_told_apart_by_id() {
        let mut projects = projects();
        projects[1].name = "tomtom ".into();

        assert_eq!(
            &unique_names(&projects)[..2],
            ["TomTom (18808)", "tomtom (4383)"]
        );
    }

    #[tokio::test]
    async fn a_refused_key_says_what_likely_cut_it() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        // A server that refuses every request, as the API refuses a bad key.
        let _ = std::thread::spawn(move || {
            use std::io::{Read, Write};

            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).unwrap();
                stream
                    .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                    .unwrap();
            }
        });

        let url = format!("http://{address}/v1/games/1");
        let client = reqwest::Client::new();
        let error = call::<serde_json::Value>(|| client.get(&url), &url)
            .await
            .unwrap_err();
        let message = format!("{:#}", refused(error));

        assert!(message.contains("refused the key"), "{message}");
        assert!(message.contains("403"), "{message}");
    }

    #[test]
    fn other_errors_are_left_as_they_are() {
        let error = anyhow::anyhow!("no network");

        assert_eq!(format!("{:#}", refused(error)), "no network");
    }
}
