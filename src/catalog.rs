//! Where the app's catalog comes from: the published one, kept on disk once
//! fetched so it shows instantly and offline, and fetched again only when it
//! changed.

use std::fmt;
use std::path::PathBuf;

use reqwest::StatusCode;
use reqwest::header::{ETAG, IF_NONE_MATCH};

pub use crate::core::catalog::{Catalog, Entry};

use crate::describe;

/// The published catalog: the repository's, as committed on its default
/// branch, which a scheduled pipeline keeps up to date.
const PUBLISHED: &str = concat!(
    env!("CARGO_PKG_REPOSITORY"),
    "/-/raw/main/index/wotlk.json"
);

/// Points the app at another catalog: an `http(s)://` URL or a file path.
pub const OVERRIDE: &str = "OLD_LAUNCHER_INDEX";

/// The catalog's file in the cache, and the tag it was served with.
const CACHED: &str = "wotlk.json";
const TAG: &str = "wotlk.etag";

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
pub enum Fetched {
    /// The catalog is still the one shown.
    Unchanged,
    /// A catalog that differs from the one shown, or the first one.
    Changed {
        catalog: Catalog,
        /// The tag the server gave it, to ask next time whether it changed.
        tag: Option<String>,
    },
}

/// The folder keeping the last catalog fetched, with its tag.
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
    /// `$OLD_LAUNCHER_INDEX` when set, otherwise the published catalog.
    pub fn configured() -> Self {
        match std::env::var(OVERRIDE) {
            Ok(value) if !value.trim().is_empty() => Self::parse(value.trim()),
            _ => Self::Url(PUBLISHED.to_owned()),
        }
    }

    /// Whether this is the published catalog, the one worth keeping on disk.
    pub fn is_published(&self) -> bool {
        matches!(self, Self::Url(url) if url == PUBLISHED)
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

    /// Reads the catalog here, downloading it with `client` (or failing with
    /// why there is none) when it is on the web. With `tag`, what the server
    /// gave the catalog shown, the server may answer that it is unchanged
    /// instead. A changed catalog is kept in `cache`, when given.
    pub async fn fetch(
        self,
        client: Result<reqwest::Client, String>,
        tag: Option<String>,
        cache: Option<Cache>,
    ) -> Result<Fetched, String> {
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

        let catalog =
            Catalog::parse(&json).map_err(|error| format!("{self}: {}", describe(&error)))?;

        if let Some(cache) = cache
            && let Err(error) = cache.write(&json, tag.as_deref()).await
        {
            // Shown all the same; only the next start misses it.
            log::warn!(
                "Could not keep the catalog in {}: {error}",
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

    /// The catalog kept, with its tag, if there is one and it parses.
    pub fn read(&self) -> Option<(Catalog, Option<String>)> {
        let path = self.directory.join(CACHED);
        let json = std::fs::read(&path).ok()?;
        let catalog = match Catalog::parse(&json) {
            Ok(catalog) => catalog,
            Err(error) => {
                log::warn!(
                    "The catalog kept in {} is unreadable, fetching it again: {}",
                    path.display(),
                    describe(&error)
                );
                return None;
            }
        };
        let tag = std::fs::read_to_string(self.directory.join(TAG))
            .ok()
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty());

        Some((catalog, tag))
    }

    /// Keeps `json` as the catalog, with its `tag`. The file is written
    /// beside its place and renamed over it, so a crash mid-write never
    /// leaves a truncated catalog.
    async fn write(&self, json: &[u8], tag: Option<&str>) -> Result<(), String> {
        let path = self.directory.join(CACHED);
        let partial = self.directory.join(format!("{CACHED}.partial"));
        let at = |error: std::io::Error| format!("{}: {error}", path.display());

        tokio::fs::create_dir_all(&self.directory)
            .await
            .map_err(at)?;
        tokio::fs::write(&partial, json).await.map_err(at)?;
        tokio::fs::rename(&partial, &path).await.map_err(at)?;

        let tag_path = self.directory.join(TAG);
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

    #[test]
    fn the_published_catalog_is_the_repositorys() {
        assert_eq!(
            PUBLISHED,
            "https://gitlab.com/juxuanu/old_launcher/-/raw/main/index/wotlk.json"
        );
        assert!(Location::Url(PUBLISHED.to_owned()).is_published());
        assert!(!Location::Url("https://example.com/wotlk.json".into()).is_published());
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
        let client = Ok(reqwest::Client::new());

        let fetched = Location::Url(url.clone())
            .fetch(client.clone(), None, Some(cache.clone()))
            .await
            .unwrap();
        let Fetched::Changed { catalog, tag } = fetched else {
            panic!("the first fetch brings the catalog");
        };
        assert_eq!(tag.as_deref(), Some("\"v1\""));
        assert_eq!(
            cache.read(),
            Some((catalog, tag.clone())),
            "kept for the next start"
        );

        assert_eq!(
            Location::Url(url).fetch(client, tag, Some(cache)).await,
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
            .fetch(Ok(reqwest::Client::new()), None, Some(cache.clone()))
            .await
            .unwrap_err();

        assert!(error.contains("wotlk.json"), "{error}");
        assert_eq!(cache.read(), None);
    }
}
