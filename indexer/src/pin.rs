//! Pinning downloads that move.
//!
//! Some sources link to downloads that serve something new whenever someone
//! pushes: a snapshot of a branch, a file kept in a branch, a file of a
//! repository's latest release. The catalog describes what the indexer found
//! in each archive, and the launcher downloads it whenever the player asks,
//! so each such download is pinned to the commit or release it points to
//! now: a URL that serves that same archive for good. The next run pins it
//! again, to whatever is newest then, and reuses what was found in an archive
//! it looked into before.
//!
//! A commit comes from git's own HTTP endpoint, the one `git clone` starts
//! with: it lists a repository's branches and tags with their commits, the
//! same way on GitHub and GitLab, and counts against no API limit. Each
//! repository is asked once, however many downloads come from it. A file in
//! a branch is pinned to the last commit that changed it instead, so its URL
//! moves only when the file does: GitHub's API says which, one call per
//! file. A latest release's tag comes from GitHub's API too, one call per
//! repository.
//!
//! Downloads whose URL already says they never change, though their source
//! doesn't (a release's file under its tag, anything of a commit), are
//! marked so as they are.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

use anyhow::{Context, bail};
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::fetch::fetch;
use crate::source::{Listing, github};

/// What a download's URL says about what it serves.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    /// Always the same archive: a GitHub release's file under its tag, or
    /// anything of a commit.
    Fixed,
    /// Whatever its branch, tag or latest release holds now.
    Moving(Moving),
}

/// A download that moves, by the shape of its URL.
#[derive(Debug, PartialEq, Eq)]
enum Moving {
    /// A snapshot of a GitHub repository (`owner/name`) at a branch, or at
    /// its default branch when `None`:
    /// `https://github.com/{repository}/archive/refs/heads/{branch}.zip` or
    /// `…/archive/HEAD.zip`.
    GitHubSnapshot {
        repository: String,
        branch: Option<String>,
    },
    /// A file in a GitHub repository's branch:
    /// `https://github.com/{repository}/raw/{branch}/{path}`.
    GitHubFile {
        repository: String,
        branch: String,
        path: String,
    },
    /// A snapshot of a GitHub repository at a tag or a branch, through the
    /// API: `https://api.github.com/repos/{repository}/zipball/{name}`.
    GitHubZipball { repository: String, name: String },
    /// A snapshot of a GitLab project at a branch:
    /// `{project}/-/archive/{branch}/{name}-{branch}.zip`, where `project` is
    /// the project's URL, under any subgroups, and `name` its last part.
    GitLabSnapshot {
        project: String,
        name: String,
        branch: String,
    },
    /// A file of a GitHub repository's latest release:
    /// `https://github.com/{repository}/releases/latest/download/{file}`.
    LatestRelease { repository: String, file: String },
}

/// What a moving download follows.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// A commit of the git repository at `repository` (its URL): the one the
    /// first of `references` it has points to. `what` names them, for when
    /// it has none ("branch main").
    Commit {
        repository: String,
        references: Vec<String>,
        what: String,
    },
    /// The last commit that changed a file in a GitHub repository's branch.
    LastCommit(File),
    /// The latest release of the GitHub repository `owner/name`.
    LatestRelease(String),
}

/// A file in a GitHub repository's (`owner/name`) branch, at `path` (as its
/// URL spells it).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct File {
    repository: String,
    branch: String,
    path: String,
}

impl Moving {
    /// What it follows, to look up.
    fn target(&self) -> Target {
        let target = |repository: String, references: &[&str], what: String| Target::Commit {
            repository,
            references: references
                .iter()
                .map(|&reference| reference.to_owned())
                .collect(),
            what,
        };

        match self {
            Self::GitHubSnapshot {
                repository,
                branch: None,
            } => target(
                format!("https://github.com/{repository}"),
                &["HEAD"],
                "default branch".to_owned(),
            ),
            Self::GitHubSnapshot {
                repository,
                branch: Some(branch),
            } => target(
                format!("https://github.com/{repository}"),
                &[&format!("refs/heads/{branch}")],
                format!("branch {branch}"),
            ),
            Self::GitHubFile {
                repository,
                branch,
                path,
            } => Target::LastCommit(File {
                repository: repository.clone(),
                branch: branch.clone(),
                path: path.clone(),
            }),
            // A tag before a branch of the same name, as git itself reads
            // the name; an annotated tag's commit is on its peeled line.
            Self::GitHubZipball { repository, name } => target(
                format!("https://github.com/{repository}"),
                &[
                    &format!("refs/tags/{name}^{{}}"),
                    &format!("refs/tags/{name}"),
                    &format!("refs/heads/{name}"),
                ],
                format!("tag or branch {name}"),
            ),
            Self::GitLabSnapshot {
                project, branch, ..
            } => target(
                project.clone(),
                &[&format!("refs/heads/{branch}")],
                format!("branch {branch}"),
            ),
            Self::LatestRelease { repository, .. } => Target::LatestRelease(repository.clone()),
        }
    }

    /// Its URL pinned to `id`: the commit, or the latest release's tag.
    fn pinned(&self, id: &str) -> String {
        match self {
            Self::GitHubSnapshot { repository, .. } | Self::GitHubZipball { repository, .. } => {
                format!("https://github.com/{repository}/archive/{id}.zip")
            }
            Self::GitHubFile {
                repository, path, ..
            } => format!("https://github.com/{repository}/raw/{id}/{path}"),
            Self::GitLabSnapshot { project, name, .. } => {
                format!("{project}/-/archive/{id}/{name}-{id}.zip")
            }
            Self::LatestRelease { repository, file } => {
                format!("https://github.com/{repository}/releases/download/{id}/{file}")
            }
        }
    }
}

/// What the download at `url` serves, when the shape of its URL says.
fn shape(url: &str) -> Option<Shape> {
    if let Some(path) = url.strip_prefix("https://github.com/") {
        let mut parts = path.splitn(3, '/');
        let repository = format!("{}/{}", parts.next()?, parts.next()?);
        let rest = parts.next()?;

        if rest == "archive/HEAD.zip" {
            return Some(Shape::Moving(Moving::GitHubSnapshot {
                repository,
                branch: None,
            }));
        }
        if let Some(name) = rest
            .strip_prefix("archive/")
            .and_then(|name| name.strip_suffix(".zip"))
        {
            return match name.strip_prefix("refs/heads/") {
                Some(branch) => Some(Shape::Moving(Moving::GitHubSnapshot {
                    repository,
                    branch: Some(branch.to_owned()),
                })),
                None => commit(name).then_some(Shape::Fixed),
            };
        }
        if let Some(file) = rest.strip_prefix("releases/latest/download/") {
            return Some(Shape::Moving(Moving::LatestRelease {
                repository,
                file: file.to_owned(),
            }));
        }
        if rest.starts_with("releases/download/") {
            return Some(Shape::Fixed);
        }

        let rest = rest.strip_prefix("raw/")?;
        let rest = rest.strip_prefix("refs/heads/").unwrap_or(rest);
        let (branch, path) = rest.split_once('/')?;

        return Some(if commit(branch) {
            Shape::Fixed
        } else {
            Shape::Moving(Moving::GitHubFile {
                repository,
                branch: branch.to_owned(),
                path: path.to_owned(),
            })
        });
    }

    if let Some(path) = url.strip_prefix("https://api.github.com/repos/") {
        let mut parts = path.splitn(4, '/');
        let repository = format!("{}/{}", parts.next()?, parts.next()?);
        let (Some("zipball"), Some(name)) = (parts.next(), parts.next()) else {
            return None;
        };

        return Some(if commit(name) {
            Shape::Fixed
        } else {
            Shape::Moving(Moving::GitHubZipball {
                repository,
                name: name.to_owned(),
            })
        });
    }

    let (project, rest) = url.split_once("/-/archive/")?;
    if !project.starts_with("https://gitlab.com/") {
        return None;
    }
    let (_, name) = project.rsplit_once('/')?;
    let (branch, file) = rest.rsplit_once('/')?;
    if !file.ends_with(".zip") {
        return None;
    }

    Some(if commit(branch) {
        Shape::Fixed
    } else {
        Shape::Moving(Moving::GitLabSnapshot {
            project: project.to_owned(),
            name: name.to_owned(),
            branch: branch.to_owned(),
        })
    })
}

/// Whether `name` is a commit's id rather than a branch's or a tag's name.
pub fn commit(name: &str) -> bool {
    name.len() == 40 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// How `listing`'s download moves, unless its source says it never changes
/// or its URL says nothing.
fn moving(listing: &Listing) -> Option<Moving> {
    if listing.immutable {
        return None;
    }

    match shape(&listing.download)? {
        Shape::Moving(moving) => Some(moving),
        Shape::Fixed => None,
    }
}

/// What the moving downloads among some listings point to now.
#[derive(Debug, Default)]
pub struct Pins {
    /// Each git repository's references (`HEAD`, `refs/heads/main`) and the
    /// commit each points to, by the repository's URL; or why they couldn't
    /// be read.
    references: HashMap<String, Result<HashMap<String, String>, String>>,
    /// The last commit that changed each file in a branch; or why it
    /// couldn't be found.
    files: HashMap<File, Result<String, String>>,
    /// The tag of each GitHub repository's latest release, by `owner/name`;
    /// or why it couldn't be found.
    releases: HashMap<String, Result<String, String>>,
}

impl Pins {
    /// Looks up what every moving download among `listings` points to,
    /// asking each repository (or file) once: git's endpoint `jobs` at a
    /// time, GitHub's API one call at a time, as GitHub asks. A lookup that
    /// fails fails only the downloads that needed it, when they are pinned.
    pub async fn look_up<'a>(
        client: &reqwest::Client,
        listings: impl IntoIterator<Item = &'a Listing>,
        jobs: usize,
    ) -> anyhow::Result<Self> {
        let Wanted {
            repositories,
            files,
            releases,
        } = wanted(listings);

        let pins = Self {
            references: all(repositories, jobs, |repository| {
                let client = client.clone();
                async move { references_at(&client, &repository).await }
            })
            .await?,
            files: all(files, 1, |file| {
                let client = client.clone();
                async move { last_commit(&client, &file).await }
            })
            .await?,
            releases: all(releases, 1, |repository| {
                let client = client.clone();
                async move { latest_tag(&client, &repository).await }
            })
            .await?,
        };

        println!(
            "Downloads that move: {} repositories, {} files and {} latest releases looked up",
            pins.references.len(),
            pins.files.len(),
            pins.releases.len()
        );

        Ok(pins)
    }

    /// Pins `listing`'s download, unless its source says it never changes:
    /// rewrites it to what it points to now when it moves, and marks it as
    /// never changing then, or when its URL says so already. Fails, leaving
    /// the listing as it was, when what it points to can't be found (the
    /// repository is gone, the branch deleted).
    pub fn pin(&self, listing: &mut Listing) -> anyhow::Result<()> {
        if listing.immutable {
            return Ok(());
        }

        let moving = match shape(&listing.download) {
            None => return Ok(()),
            Some(Shape::Fixed) => {
                listing.immutable = true;
                return Ok(());
            }
            Some(Shape::Moving(moving)) => moving,
        };

        let id = match moving.target() {
            Target::Commit {
                repository,
                references,
                what,
            } => {
                let found = found(&self.references, &repository)?;

                references
                    .iter()
                    .find_map(|reference| found.get(reference))
                    .with_context(|| format!("{repository} has no {what}"))?
            }
            Target::LastCommit(file) => found(&self.files, &file)?,
            Target::LatestRelease(repository) => found(&self.releases, &repository)?,
        };

        listing.download = moving.pinned(id);
        listing.immutable = true;

        Ok(())
    }
}

/// What a lookup found for `key` among `lookups`, or why it couldn't.
fn found<'a, K: Eq + Hash + Debug, T>(
    lookups: &'a HashMap<K, Result<T, String>>,
    key: &K,
) -> anyhow::Result<&'a T> {
    lookups
        .get(key)
        .with_context(|| format!("{key:?} wasn't looked up"))?
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))
}

/// What the moving downloads among some listings follow, each once.
#[derive(Debug, Default, PartialEq, Eq)]
struct Wanted {
    /// Git repositories, by URL.
    repositories: BTreeSet<String>,
    /// Files in GitHub repositories' branches.
    files: BTreeSet<File>,
    /// GitHub repositories whose latest release they take, by `owner/name`.
    releases: BTreeSet<String>,
}

/// What the moving downloads among `listings` follow, each once.
fn wanted<'a>(listings: impl IntoIterator<Item = &'a Listing>) -> Wanted {
    let mut wanted = Wanted::default();

    for moving in listings.into_iter().filter_map(moving) {
        match moving.target() {
            Target::Commit { repository, .. } => {
                let _ = wanted.repositories.insert(repository);
            }
            Target::LastCommit(file) => {
                let _ = wanted.files.insert(file);
            }
            Target::LatestRelease(repository) => {
                let _ = wanted.releases.insert(repository);
            }
        }
    }

    wanted
}

/// What `look_up` finds for each of `keys`, `jobs` at a time, by key; or
/// why it couldn't.
async fn all<K, T, F>(
    keys: BTreeSet<K>,
    jobs: usize,
    look_up: impl Fn(K) -> F,
) -> anyhow::Result<HashMap<K, Result<T, String>>>
where
    K: Clone + Eq + Hash + Send + 'static,
    T: Send + 'static,
    F: Future<Output = anyhow::Result<T>> + Send + 'static,
{
    let permits = Arc::new(Semaphore::new(jobs.max(1)));
    let mut lookups = JoinSet::new();

    for key in keys {
        let permits = Arc::clone(&permits);
        let lookup = look_up(key.clone());

        let _ = lookups.spawn(async move {
            let found = async {
                let _permit = permits.acquire_owned().await?;

                lookup.await
            }
            .await;

            (key, found.map_err(|error| format!("{error:#}")))
        });
    }

    let mut found = HashMap::new();

    while let Some(lookup) = lookups.join_next().await {
        let (key, result) = lookup?;
        let _ = found.insert(key, result);
    }

    Ok(found)
}

/// The references of the git repository at `repository` (its URL), with
/// their commits, as its server lists them for `git clone`.
async fn references_at(
    client: &reqwest::Client,
    repository: &str,
) -> anyhow::Result<HashMap<String, String>> {
    let url = format!("{repository}.git/info/refs?service=git-upload-pack");
    let answer = fetch(client, &url)
        .await
        .with_context(|| format!("looking up {repository}"))?;

    references(&answer).with_context(|| format!("reading {url}"))
}

/// The references a git server lists and the commit each points to, from its
/// answer to `info/refs?service=git-upload-pack`. Each line of it starts
/// with its length in four hex digits, and `0000` ends a group of lines. A
/// `# service=git-upload-pack` line comes first, in a group of its own; then
/// one line per reference, `<commit> <name>`, the first followed by the
/// server's capabilities after a NUL. `HEAD`, the default branch, comes
/// first, and an annotated tag's commit is on a line of its own, the tag's
/// name followed by `^{}`.
fn references(answer: &[u8]) -> anyhow::Result<HashMap<String, String>> {
    let mut lines = Vec::new();
    let mut rest = answer;

    while !rest.is_empty() {
        let length = rest
            .get(..4)
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| usize::from_str_radix(digits, 16).ok())
            .context("not a git server's answer")?;

        if length == 0 {
            rest = &rest[4..];
            continue;
        }

        let line = rest
            .get(4..length)
            .context("a line of the answer is cut short")?;
        lines.push(String::from_utf8_lossy(line));
        rest = &rest[length..];
    }

    let Some((service, listed)) = lines.split_first() else {
        bail!("not a git server's answer");
    };
    if service.trim_end() != "# service=git-upload-pack" {
        bail!("not a git server's answer");
    }

    listed
        .iter()
        .map(|line| {
            let line = line.trim_end_matches('\n');
            let line = line.split('\0').next().unwrap_or_default();
            let (commit, name) = line
                .split_once(' ')
                .with_context(|| format!("an odd line in the answer: {line:?}"))?;

            Ok((name.to_owned(), commit.to_owned()))
        })
        .collect()
}

/// The tag of the latest release of the GitHub repository `owner/name`.
async fn latest_tag(client: &reqwest::Client, repository: &str) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
    }

    let release: Release =
        github::api(client, &format!("repos/{repository}/releases/latest")).await?;

    Ok(release.tag_name)
}

/// The last commit of `file`'s branch that changed it.
async fn last_commit(client: &reqwest::Client, file: &File) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Commit {
        sha: String,
    }

    let File {
        repository,
        branch,
        path,
    } = file;
    let commits: Vec<Commit> = github::api(
        client,
        &format!(
            "repos/{repository}/commits?sha={}&path={}&per_page=1",
            query(branch),
            query(path)
        ),
    )
    .await?;

    commits
        .into_iter()
        .next()
        .map(|commit| commit.sha)
        .with_context(|| {
            format!("https://github.com/{repository} has no {path} on branch {branch}")
        })
}

/// `text` as a value in a URL's query: everything but letters, digits,
/// `-._~` and `/` percent-encoded, but for the escapes it has already (a
/// path taken from a URL may have some, which stand for what they encode).
pub fn query(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut encoded = String::with_capacity(text.len());

    for (index, &byte) in bytes.iter().enumerate() {
        let escape = byte == b'%'
            && bytes
                .get(index + 1..index + 3)
                .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit));

        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) || escape {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Updated;

    /// A listing of the download at `url`, which its source says may change.
    fn listing(url: &str) -> Listing {
        Listing {
            name: "Addon".into(),
            description: String::new(),
            download: url.into(),
            image: None,
            page: None,
            changes: Some("https://github.com/o/r/commits/main".into()),
            links: Vec::new(),
            updated: Updated::FromArchive,
            require_wotlk: true,
            immutable: false,
        }
    }

    /// A git server's answer listing `references` (name, commit), the first
    /// with capabilities, as GitHub and GitLab send it.
    fn answer(references: &[(&str, &str)]) -> Vec<u8> {
        let mut answer = b"001e# service=git-upload-pack\n0000".to_vec();

        for (index, (name, commit)) in references.iter().enumerate() {
            let capabilities = if index == 0 {
                "\0multi_ack side-band-64k symref=HEAD:refs/heads/main agent=git/2.56"
            } else {
                ""
            };
            let line = format!("{commit} {name}{capabilities}\n");
            answer.extend(format!("{:04x}{line}", line.len() + 4).bytes());
        }
        answer.extend(b"0000");

        answer
    }

    /// A commit id made of `digit`.
    fn id(digit: char) -> String {
        digit.to_string().repeat(40)
    }

    /// A file in NoM0Re's collection, at `path`, on its `branch`.
    fn file(branch: &str, path: &str) -> File {
        File {
            repository: "NoM0Re/WoW-3.3.5a-Addons".into(),
            branch: branch.into(),
            path: path.into(),
        }
    }

    /// What a few repositories hold: GitHub's and GitLab's, a repository
    /// that is gone, a file that is gone and a repository with no release.
    fn pins() -> Pins {
        let branches =
            |references: &[(&str, &str)]| Ok(super::references(&answer(references)).unwrap());

        Pins {
            references: HashMap::from([
                (
                    "https://github.com/KhalGH/_VirtualPlates-WotLK".into(),
                    branches(&[("HEAD", &id('a')), ("refs/heads/main", &id('a'))]),
                ),
                (
                    "https://github.com/Zendevve/CartoMapper".into(),
                    branches(&[("HEAD", &id('c')), ("refs/heads/dev", &id('d'))]),
                ),
                (
                    "https://github.com/Drakensangs/RecipeColor-WotLK".into(),
                    branches(&[
                        ("HEAD", &id('e')),
                        ("refs/heads/main", &id('e')),
                        ("refs/heads/3.3", &id('1')),
                        ("refs/tags/3.3", &id('2')),
                    ]),
                ),
                (
                    "https://github.com/shadovvs/WotLK-Storyline".into(),
                    branches(&[
                        ("HEAD", &id('f')),
                        ("refs/tags/v1.0.1", &id('3')),
                        ("refs/tags/v1.0.1^{}", &id('4')),
                    ]),
                ),
                (
                    "https://gitlab.com/tsoukie-3.3.5/clique".into(),
                    branches(&[("HEAD", &id('5')), ("refs/heads/main", &id('5'))]),
                ),
                (
                    "https://gitlab.com/group/subgroup/addon".into(),
                    branches(&[("HEAD", &id('6')), ("refs/heads/master", &id('6'))]),
                ),
                (
                    "https://github.com/o/gone".into(),
                    Err(
                        "looking up https://github.com/o/gone: downloading …: 401 Unauthorized"
                            .into(),
                    ),
                ),
            ]),
            files: HashMap::from([
                (file("main", "src/Addons/ACP.zip"), Ok(id('7'))),
                (
                    file("main", "src/Addons/Gone.zip"),
                    Err("https://github.com/NoM0Re/WoW-3.3.5a-Addons has no \
                         src/Addons/Gone.zip on branch main"
                        .into()),
                ),
            ]),
            releases: HashMap::from([
                ("NoM0Re/DalaranAH".into(), Ok("v1.2".into())),
                (
                    "o/unreleased".into(),
                    Err("calling …: 404 Not Found".into()),
                ),
            ]),
        }
    }

    #[test]
    fn references_are_read_from_a_git_servers_answer() {
        let references = references(&answer(&[
            ("HEAD", &id('a')),
            ("refs/heads/main", &id('a')),
            ("refs/heads/dev", &id('b')),
            ("refs/tags/v1", &id('c')),
            ("refs/tags/v1^{}", &id('d')),
        ]))
        .unwrap();

        assert_eq!(references.get("HEAD"), Some(&id('a')), "the default branch");
        assert_eq!(references.get("refs/heads/dev"), Some(&id('b')));
        assert_eq!(
            references.get("refs/tags/v1^{}"),
            Some(&id('d')),
            "an annotated tag's commit"
        );
        assert_eq!(references.get("refs/heads/gone"), None, "a missing branch");
    }

    #[test]
    fn answers_that_arent_a_git_servers_are_refused() {
        let mut cut = answer(&[("HEAD", &id('a'))]);
        cut.truncate(cut.len() - 10);

        for (answer, expected) in [
            (
                &b"<!DOCTYPE html><html>Sign in</html>"[..],
                "not a git server's answer",
            ),
            (&b"0000"[..], "not a git server's answer"),
            (&b"000bnothing0000"[..], "not a git server's answer"),
            (&cut[..], "a line of the answer is cut short"),
        ] {
            let error = references(answer).unwrap_err();

            assert_eq!(
                error.to_string(),
                expected,
                "{}",
                String::from_utf8_lossy(answer)
            );
        }
    }

    #[test]
    fn each_moving_download_is_pinned_to_what_it_points_to_now() {
        let pins = pins();
        let (a, c, d) = (id('a'), id('c'), id('d'));
        let (two, four, five, six, seven) = (id('2'), id('4'), id('5'), id('6'), id('7'));

        for (url, pinned) in [
            (
                "https://github.com/KhalGH/_VirtualPlates-WotLK/archive/refs/heads/main.zip".to_owned(),
                format!("https://github.com/KhalGH/_VirtualPlates-WotLK/archive/{a}.zip"),
            ),
            (
                "https://github.com/Zendevve/CartoMapper/archive/HEAD.zip".to_owned(),
                format!("https://github.com/Zendevve/CartoMapper/archive/{c}.zip"),
            ),
            (
                "https://github.com/Zendevve/CartoMapper/archive/refs/heads/dev.zip".to_owned(),
                format!("https://github.com/Zendevve/CartoMapper/archive/{d}.zip"),
            ),
            // To the last commit that changed the file, not the branch's.
            (
                "https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/main/src/Addons/ACP.zip".to_owned(),
                format!(
                    "https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/{seven}/src/Addons/ACP.zip"
                ),
            ),
            (
                "https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/refs/heads/main/src/Addons/ACP.zip"
                    .to_owned(),
                format!(
                    "https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/{seven}/src/Addons/ACP.zip"
                ),
            ),
            (
                "https://api.github.com/repos/Drakensangs/RecipeColor-WotLK/zipball/3.3".to_owned(),
                format!("https://github.com/Drakensangs/RecipeColor-WotLK/archive/{two}.zip"),
            ),
            (
                "https://api.github.com/repos/shadovvs/WotLK-Storyline/zipball/v1.0.1".to_owned(),
                format!("https://github.com/shadovvs/WotLK-Storyline/archive/{four}.zip"),
            ),
            (
                "https://gitlab.com/tsoukie-3.3.5/clique/-/archive/main/clique-main.zip".to_owned(),
                format!("https://gitlab.com/tsoukie-3.3.5/clique/-/archive/{five}/clique-{five}.zip"),
            ),
            (
                "https://gitlab.com/group/subgroup/addon/-/archive/master/addon-master.zip"
                    .to_owned(),
                format!(
                    "https://gitlab.com/group/subgroup/addon/-/archive/{six}/addon-{six}.zip"
                ),
            ),
            (
                "https://github.com/NoM0Re/DalaranAH/releases/latest/download/DalaranAH.zip"
                    .to_owned(),
                "https://github.com/NoM0Re/DalaranAH/releases/download/v1.2/DalaranAH.zip"
                    .to_owned(),
            ),
        ] {
            let mut listing = listing(&url);

            pins.pin(&mut listing).unwrap();

            assert_eq!(listing.download, pinned);
            assert!(listing.immutable, "{url} is pinned");
            assert_eq!(
                listing.changes.as_deref(),
                Some("https://github.com/o/r/commits/main"),
                "the history stays the branch's"
            );
        }
    }

    #[test]
    fn downloads_that_never_change_are_marked_so_as_they_are() {
        let commit = id('a');

        for url in [
            "https://github.com/KhalGH/BattleGroundHealers-WotLK/releases/download/v1.6.3/BattleGroundHealers-v1.6.3.zip".to_owned(),
            format!("https://github.com/o/r/archive/{commit}.zip"),
            format!("https://github.com/o/r/raw/{commit}/src/Addons/ACP.zip"),
            format!("https://api.github.com/repos/o/r/zipball/{commit}"),
            format!("https://gitlab.com/o/r/-/archive/{commit}/r-{commit}.zip"),
        ] {
            let mut listing = listing(&url);

            Pins::default().pin(&mut listing).unwrap();

            assert_eq!(listing.download, url, "left as it is");
            assert!(listing.immutable, "{url} never changes");
        }
    }

    #[test]
    fn other_downloads_are_left_alone() {
        for url in [
            "https://edge.forgecdn.net/files/440/245/1337_Achievement_Helper.zip",
            "https://cdn.wowinterface.com/downloads/getfile.php?id=15951&d=1265833477&minion",
            "https://addons.rising-gods.de/sites/default/files/addons/Ace3-Release-r960.zip",
            "https://github.com/o/r/archive/refs/tags/v1.zip",
            "https://github.com/o/r",
            "https://example.com/o/r/-/archive/main/r-main.zip",
        ] {
            let mut listing = listing(url);

            Pins::default().pin(&mut listing).unwrap();

            assert_eq!(listing, self::listing(url), "{url}");
        }

        let mut immutable = listing("https://github.com/o/r/archive/refs/heads/main.zip");
        immutable.immutable = true;
        let before = immutable.clone();

        Pins::default().pin(&mut immutable).unwrap();

        assert_eq!(immutable, before, "its source says it never changes");
    }

    #[test]
    fn a_download_that_cant_be_pinned_says_why_and_stays_as_it_was() {
        let pins = pins();

        for (url, reason) in [
            (
                "https://github.com/KhalGH/_VirtualPlates-WotLK/archive/refs/heads/dev.zip",
                "https://github.com/KhalGH/_VirtualPlates-WotLK has no branch dev",
            ),
            (
                "https://api.github.com/repos/Drakensangs/RecipeColor-WotLK/zipball/9.9",
                "https://github.com/Drakensangs/RecipeColor-WotLK has no tag or branch 9.9",
            ),
            (
                "https://github.com/o/gone/archive/HEAD.zip",
                "looking up https://github.com/o/gone: downloading …: 401 Unauthorized",
            ),
            (
                "https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/main/src/Addons/Gone.zip",
                "https://github.com/NoM0Re/WoW-3.3.5a-Addons has no src/Addons/Gone.zip on \
                 branch main",
            ),
            (
                "https://github.com/o/unreleased/releases/latest/download/r.zip",
                "calling …: 404 Not Found",
            ),
        ] {
            let mut listing = listing(url);

            let error = pins.pin(&mut listing).unwrap_err();

            assert_eq!(format!("{error:#}"), reason);
            assert_eq!(listing, self::listing(url), "{url} stays as it was");
        }
    }

    #[test]
    fn each_repository_and_file_is_looked_up_once() {
        let mut immutable = listing("https://github.com/o/fixed/archive/refs/heads/main.zip");
        immutable.immutable = true;
        let listings = [
            listing("https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/main/src/Addons/ACP.zip"),
            listing(
                "https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/refs/heads/main/src/Addons/ACP.zip",
            ),
            listing("https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/main/src/Addons/Bagnon.zip"),
            listing("https://github.com/NoM0Re/Cell-WotLK/archive/HEAD.zip"),
            listing("https://github.com/NoM0Re/Cell-WotLK/archive/refs/heads/main.zip"),
            listing("https://github.com/NoM0Re/Cell-WotLK/releases/latest/download/Cell.zip"),
            listing("https://gitlab.com/juxuanu/kekator/-/archive/master/kekator-master.zip"),
            listing("https://github.com/o/r/releases/download/v1/r.zip"),
            listing("https://addons.rising-gods.de/sites/default/files/addons/Quartz.zip"),
            immutable,
        ];

        assert_eq!(
            wanted(&listings),
            Wanted {
                repositories: BTreeSet::from([
                    "https://github.com/NoM0Re/Cell-WotLK".to_owned(),
                    "https://gitlab.com/juxuanu/kekator".to_owned(),
                ]),
                files: BTreeSet::from([
                    file("main", "src/Addons/ACP.zip"),
                    file("main", "src/Addons/Bagnon.zip"),
                ]),
                releases: BTreeSet::from(["NoM0Re/Cell-WotLK".to_owned()]),
            }
        );
    }

    #[test]
    fn paths_and_branches_are_encoded_for_a_query_once() {
        assert_eq!(query("src/Addons/ACP.zip"), "src/Addons/ACP.zip");
        assert_eq!(
            query("src/My Addon+Extras&co#1.zip"),
            "src/My%20Addon%2BExtras%26co%231.zip"
        );
        assert_eq!(
            query("src/My%20Addon.zip"),
            "src/My%20Addon.zip",
            "an escape the link had already"
        );
        assert_eq!(query("100%.zip"), "100%25.zip");
        assert_eq!(query("Ä.zip"), "%C3%84.zip");
    }

    /// Pins one download of each kind the catalog has, against GitHub and
    /// GitLab as they are now, prints what they became and downloads them.
    #[tokio::test]
    #[ignore = "downloads from GitHub and GitLab"]
    async fn real_downloads_are_pinned_to_zips() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .user_agent("old_launcher-indexer-test")
            .build()
            .unwrap();
        let mut listings = [
            "https://gitlab.com/juxuanu/kekator/-/archive/master/kekator-master.zip",
            "https://github.com/KhalGH/_VirtualPlates-WotLK/archive/refs/heads/main.zip",
            "https://github.com/NoM0Re/WoW-3.3.5a-Addons/raw/main/src/Addons/ACP.zip",
            "https://github.com/NoM0Re/DalaranAH/releases/latest/download/DalaranAH.zip",
            "https://api.github.com/repos/Drakensangs/RecipeColor-WotLK/zipball/3.3",
            "https://api.github.com/repos/shadovvs/WotLK-Storyline/zipball/v1.0.1",
        ]
        .map(listing);

        let pins = Pins::look_up(&client, &listings, 4).await.unwrap();

        for listing in &mut listings {
            let before = listing.download.clone();
            pins.pin(listing).unwrap();
            println!("{before}\n  -> {}", listing.download);

            let archive = fetch(&client, &listing.download).await.unwrap();
            assert!(listing.immutable);
            assert!(archive.starts_with(b"PK"), "{}", listing.download);
        }
    }
}
