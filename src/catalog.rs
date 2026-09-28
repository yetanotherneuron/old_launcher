//! Where the app's catalog comes from: a copy built into the binary, so the
//! catalog is there instantly and offline, refreshed from the published one.

use std::fmt;
use std::path::PathBuf;

pub use crate::core::catalog::{Catalog, Entry};

use crate::describe;

/// The catalog as it was when this binary was built.
const EMBEDDED: &str = include_str!("../index/wotlk.json");

/// Where the published catalog lives, set when building
/// (`OLD_LAUNCHER_INDEX_URL=https://… cargo build`). Without it the embedded
/// copy is all there is.
const PUBLISHED: Option<&str> = option_env!("OLD_LAUNCHER_INDEX_URL");

/// Points the app at another catalog: an `http(s)://` URL or a file path.
pub const OVERRIDE: &str = "OLD_LAUNCHER_INDEX";

/// Where a catalog is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// Downloaded over HTTP(S).
    Url(String),
    /// Read from disk.
    File(PathBuf),
}

/// Whether the catalog can be refreshed, and whether it is being.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// There is nowhere to refresh from: this build knows no published
    /// catalog and none was given through [`OVERRIDE`].
    Unavailable,
    /// The catalog is as fresh as it gets for now.
    Idle,
    /// A refresh is in flight.
    Running,
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
    /// `$OLD_LAUNCHER_INDEX` when set, otherwise the published catalog of
    /// this build, if it has one.
    pub fn configured() -> Option<Self> {
        match std::env::var(OVERRIDE) {
            Ok(value) if !value.trim().is_empty() => Some(Self::parse(value.trim())),
            _ => PUBLISHED.map(|url| Self::Url(url.to_owned())),
        }
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
    /// why there is none) when it is on the web.
    pub async fn load(self, client: Result<reqwest::Client, String>) -> Result<Catalog, String> {
        let json = match &self {
            Self::Url(url) => client?
                .get(url)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|error| describe(&error))?
                .bytes()
                .await
                .map_err(|error| describe(&error))?
                .to_vec(),
            Self::File(path) => tokio::fs::read(path)
                .await
                .map_err(|error| format!("{}: {error}", path.display()))?,
        };

        Catalog::parse(&json).map_err(|error| format!("{self}: {}", describe(&error)))
    }
}

/// The catalog built into the binary. A test checks that it parses.
pub fn embedded() -> Result<Catalog, String> {
    Catalog::parse(EMBEDDED.as_bytes()).map_err(|error| describe(&error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_catalog_parses() {
        let catalog = embedded().unwrap();

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
}
