//! Where the app's catalogs come from, the addons' and the mods': the
//! published ones, each kept on disk once fetched so it shows instantly and
//! offline, and fetched again only when it changed.

use std::fmt;
use std::path::PathBuf;

use reqwest::StatusCode;
use reqwest::header::{ETAG, IF_NONE_MATCH};

pub use crate::core::catalog::{Catalog, Entry};
use crate::core::mods::Mods;

use crate::describe;

/// A catalog the indexer generates and the launcher reads.
pub trait Document: Sized {
    /// Its file's name, in the repository's `index` folder and in the cache,
    /// without `.json`.
    const NAME: &str;
    /// The variable that points the app at another one: an `http(s)://`
    /// URL or a file path.
    const OVERRIDE: &str;

    /// Reads it.
    fn parse(json: &[u8]) -> Result<Self, crate::core::Error>;
}

impl Document for Catalog {
    const NAME: &str = "wotlk";
    const OVERRIDE: &str = "OLD_LAUNCHER_INDEX";

    fn parse(json: &[u8]) -> Result<Self, crate::core::Error> {
        Catalog::parse(json)
    }
}

impl Document for Mods {
    const NAME: &str = "mods";
    const OVERRIDE: &str = "OLD_LAUNCHER_MODS";

    fn parse(json: &[u8]) -> Result<Self, crate::core::Error> {
        Mods::parse(json)
    }
}

/// The published `T`: the repository's, as committed on its default branch,
/// which a scheduled pipeline keeps up to date.
fn published<T: Document>() -> String {
    format!(
        concat!(env!("CARGO_PKG_REPOSITORY"), "/-/raw/main/index/{}.json"),
        T::NAME
    )
}

/// Where a catalog is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// Downloaded over HTTP(S).
    Url(String),
    /// Read from disk.
    File(PathBuf),
}

/// Whether a refresh is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// The catalog is as fresh as it gets for now.
    Idle,
    /// A refresh is in flight.
    Running,
}

/// What a fetch found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched<T> {
    /// The catalog is still the one shown.
    Unchanged,
    /// A catalog that differs from the one shown, or the first one.
    Changed {
        catalog: T,
        /// The tag the server gave it, to ask next time whether it changed.
        tag: Option<String>,
    },
}

/// The folder keeping the last catalogs fetched, with their tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cache {
    directory: PathBuf,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(url) => f.write_str(url),
            Self::File(path) => write!(f, "{}", path.display()),
        }
    }
}

impl Location {
    /// `T`'s override when set (`$OLD_LAUNCHER_INDEX` for the addons'),
    /// otherwise the published `T`.
    pub fn configured<T: Document>() -> Self {
        match std::env::var(T::OVERRIDE) {
            Ok(value) if !value.trim().is_empty() => Self::parse(value.trim()),
            _ => Self::Url(published::<T>()),
        }
    }

    /// Whether this is the published `T`, the one worth keeping on disk.
    pub fn is_published<T: Document>(&self) -> bool {
        matches!(self, Self::Url(url) if *url == published::<T>())
    }

    fn parse(value: &str) -> Self {
        if value.starts_with("http://") || value.starts_with("https://") {
            Self::Url(value.to_owned())
        } else {
            Self::File(PathBuf::from(
                value.strip_prefix("file://").unwrap_or(value),
            ))
        }
    }

    /// Reads the `T` here, downloading it with `client` (or failing with
    /// why there is none) when it is on the web. With `tag`, what the server
    /// gave the one shown, the server may answer that it is unchanged
    /// instead. A changed one is kept in `cache`, when given.
    pub async fn fetch<T: Document>(
        self,
        client: Result<reqwest::Client, String>,
        tag: Option<String>,
        cache: Option<Cache>,
    ) -> Result<Fetched<T>, String> {
        let (json, tag) = match &self {
            Self::Url(url) => {
                let mut request = client?.get(url);
                if let Some(tag) = &tag {
                    request = request.header(IF_NONE_MATCH, tag);
                }

                let response = request.send().await.map_err(|error| describe(&error))?;
                if response.status() == StatusCode::NOT_MODIFIED {
                    return Ok(Fetched::Unchanged);
                }

                let response = response
                    .error_for_status()
                    .map_err(|error| describe(&error))?;
                let tag = response
                    .headers()
                    .get(ETAG)
                    .and_then(|tag| tag.to_str().ok())
                    .map(str::to_owned);
                let json = response
                    .bytes()
                    .await
                    .map_err(|error| describe(&error))?
                    .to_vec();

                (json, tag)
            }
            Self::File(path) => (
                tokio::fs::read(path)
                    .await
                    .map_err(|error| format!("{}: {error}", path.display()))?,
                None,
            ),
        };

        let catalog = T::parse(&json).map_err(|error| format!("{self}: {}", describe(&error)))?;

        if let Some(cache) = cache
            && let Err(error) = cache.write::<T>(&json, tag.as_deref()).await
        {
            // Shown all the same; only the next start misses it.
            log::warn!(
                "Could not keep {}.json in {}: {error}",
                T::NAME,
                cache.directory.display()
            );
        }

        Ok(Fetched::Changed { catalog, tag })
    }
}

impl Cache {
    /// The cache in the user's cache directory.
    pub fn default_location() -> Self {
        Self::at(dirs::cache_dir().unwrap_or_default().join("old_launcher"))
    }

    /// The cache in `directory`.
    pub fn at(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// The `T` kept, with its tag, if there is one and it parses.
    pub fn read<T: Document>(&self) -> Option<(T, Option<String>)> {
        let path = self.directory.join(format!("{}.json", T::NAME));
        let json = std::fs::read(&path).ok()?;
        let catalog = match T::parse(&json) {
            Ok(catalog) => catalog,
            Err(error) => {
                log::warn!(
                    "{} is unreadable, fetching it again: {}",
                    path.display(),
                    describe(&error)
                );
                return None;
            }
        };
        let tag = std::fs::read_to_string(self.directory.join(format!("{}.etag", T::NAME)))
            .ok()
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty());

        Some((catalog, tag))
    }

    /// Keeps `json` as the `T`, with its `tag`. The file is written beside
    /// its place and renamed over it, so a crash mid-write never leaves a
    /// truncated one.
    async fn write<T: Document>(&self, json: &[u8], tag: Option<&str>) -> Result<(), String> {
        let path = self.directory.join(format!("{}.json", T::NAME));
        let partial = self.directory.join(format!("{}.json.partial", T::NAME));
        let at = |error: std::io::Error| format!("{}: {error}", path.display());

        tokio::fs::create_dir_all(&self.directory)
            .await
            .map_err(at)?;
        tokio::fs::write(&partial, json).await.map_err(at)?;
        tokio::fs::rename(&partial, &path).await.map_err(at)?;

        let tag_path = self.directory.join(format!("{}.etag", T::NAME));
        match tag {
            Some(tag) => tokio::fs::write(&tag_path, tag).await.map_err(at),
            None => match tokio::fs::remove_file(&tag_path).await {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(at(error)),
                _ => Ok(()),
            },
        }
    }
}

/// The catalog in the repository, for tests.
#[cfg(test)]
pub fn repository() -> Catalog {
    let json = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/index/wotlk.json")).unwrap();
    Catalog::parse(&json).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_catalog_parses() {
        let catalog = repository();

        assert!(!catalog.entries.is_empty());
        assert!(
            catalog
                .entries
                .iter()
                .flat_map(|entry| &entry.downloads)
                .all(|download| catalog.source(&download.source).is_some()),
            "every download's source is described"
        );
    }

    /// `version` with its first or last run of digits one higher, or
    /// nothing when it has none.
    fn bumped(version: &str, last: bool) -> Option<String> {
        let digit = |c: char| c.is_ascii_digit();
        let (start, end) = if last {
            let end = version.rfind(digit)? + 1;
            let start = version[..end]
                .rfind(|c: char| !c.is_ascii_digit())
                .map_or(0, |index| index + 1);
            (start, end)
        } else {
            let start = version.find(digit)?;
            let end = version[start..]
                .find(|c: char| !c.is_ascii_digit())
                .map_or(version.len(), |index| start + index);
            (start, end)
        };
        let number: u64 = version[start..end].parse().ok()?;

        Some(format!(
            "{}{}{}",
            &version[..start],
            number + 1,
            &version[end..]
        ))
    }

    /// Every shape of version the published catalog has compares with
    /// itself, and reads as older than itself with any of its numbers
    /// raised; a version without a number compares with nothing.
    #[test]
    fn every_version_shape_in_the_repository_catalog_compares() {
        use std::cmp::Ordering;
        use std::collections::BTreeSet;

        use crate::core::toc::compare_versions;

        let catalog = repository();
        let versions: BTreeSet<&str> = catalog
            .entries
            .iter()
            .flat_map(|entry| &entry.downloads)
            .flat_map(|download| &download.folders)
            .filter_map(|folder| folder.version.as_deref())
            .collect();
        assert!(versions.len() > 100, "{} versions", versions.len());

        for version in versions {
            assert_eq!(
                compare_versions(version, version),
                Some(Ordering::Equal),
                "{version:?}"
            );

            for last in [false, true] {
                match bumped(version, last) {
                    Some(newer) => assert_eq!(
                        compare_versions(&newer, version),
                        Some(Ordering::Greater),
                        "{newer:?} vs {version:?}"
                    ),
                    None => assert_eq!(
                        compare_versions(version, "1.0"),
                        None,
                        "{version:?} has no number"
                    ),
                }
            }
        }
    }

    #[test]
    fn the_published_catalogs_are_the_repositorys() {
        assert_eq!(
            published::<Catalog>(),
            "https://gitlab.com/juxuanu/old_launcher/-/raw/main/index/wotlk.json"
        );
        assert_eq!(
            published::<Mods>(),
            "https://gitlab.com/juxuanu/old_launcher/-/raw/main/index/mods.json"
        );
        assert!(Location::Url(published::<Catalog>()).is_published::<Catalog>());
        assert!(!Location::Url(published::<Catalog>()).is_published::<Mods>());
        assert!(!Location::Url("https://example.com/wotlk.json".into()).is_published::<Catalog>());
    }

    #[test]
    fn the_repository_list_of_mods_parses() {
        let json = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/index/mods.json")).unwrap();
        let mods = Mods::parse(&json).unwrap();

        for id in ["lumenveil", "dungeon-maps"] {
            assert!(
                mods.mods
                    .iter()
                    .any(|listed| listed.id == id && !listed.releases.is_empty()),
                "{id}"
            );
        }
    }

    #[test]
    fn overrides_are_urls_or_paths() {
        assert_eq!(
            Location::parse("https://example.com/wotlk.json"),
            Location::Url("https://example.com/wotlk.json".into())
        );
        assert_eq!(
            Location::parse("file:///srv/wotlk.json"),
            Location::File("/srv/wotlk.json".into())
        );
        assert_eq!(
            Location::parse("index/wotlk.json"),
            Location::File("index/wotlk.json".into())
        );
    }

    /// An HTTP client, with the TLS provider the app installs at start.
    fn client() -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::new()
    }

    /// Serves `json` as the catalog, tagged `tag`, answering that it is
    /// unchanged to a request that already has that tag.
    async fn serve(json: Vec<u8>, tag: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        drop(tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 4096];
                let read = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]).to_lowercase();

                let response = if request.contains(&format!("if-none-match: {tag}")) {
                    b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".to_vec()
                } else {
                    let mut response = format!(
                        "HTTP/1.1 200 OK\r\nETag: {tag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        json.len()
                    )
                    .into_bytes();
                    response.extend(&json);
                    response
                };

                stream.write_all(&response).await.unwrap();
            }
        }));

        format!("http://{address}/wotlk.json")
    }

    #[tokio::test]
    async fn a_catalog_is_kept_and_fetched_again_only_when_it_changed() {
        let json = br#"{ "version": 2, "sources": [], "entries": [] }"#.to_vec();
        let url = serve(json, "\"v1\"").await;
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::at(directory.path().to_path_buf());
        let client = Ok(client());

        let fetched = Location::Url(url.clone())
            .fetch::<Catalog>(client.clone(), None, Some(cache.clone()))
            .await
            .unwrap();
        let Fetched::Changed { catalog, tag } = fetched else {
            panic!("the first fetch brings the catalog");
        };
        assert_eq!(tag.as_deref(), Some("\"v1\""));
        assert_eq!(
            cache.read::<Catalog>(),
            Some((catalog, tag.clone())),
            "kept for the next start"
        );
        assert_eq!(cache.read::<Mods>(), None, "kept apart");

        assert_eq!(
            Location::Url(url)
                .fetch::<Catalog>(client, tag, Some(cache))
                .await,
            Ok(Fetched::Unchanged),
            "the server is asked whether it changed"
        );
    }

    #[tokio::test]
    async fn a_catalog_that_doesnt_parse_is_not_kept() {
        let url = serve(b"<html>".to_vec(), "\"v1\"").await;
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::at(directory.path().to_path_buf());

        let error = Location::Url(url)
            .fetch::<Catalog>(Ok(client()), None, Some(cache.clone()))
            .await
            .unwrap_err();

        assert!(error.contains("wotlk.json"), "{error}");
        assert_eq!(cache.read::<Catalog>(), None);
    }
}
