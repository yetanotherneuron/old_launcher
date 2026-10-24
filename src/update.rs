//! Whether a newer launcher is published: the version the page's description
//! of what is published declares, against this build's.

use serde::Deserialize;

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The launcher's page, where the downloads are.
pub const PAGE: &str = env!("CARGO_PKG_HOMEPAGE");

/// The description of what is published, beside the page: the version and
/// the binaries, written by `pages/release.sh` once they are built.
const RELEASE: &str = concat!(env!("CARGO_PKG_HOMEPAGE"), "release.json");

/// What the check reads of the description.
#[derive(Debug, Deserialize)]
struct Release {
    version: String,
}

/// The published version, when it is newer than this build's. Nothing when
/// it can't be told: the check is a courtesy, logged but not reported.
pub async fn check(client: Result<reqwest::Client, String>) -> Option<String> {
    match published(client).await {
        Ok(published) if newer(&published, VERSION) => {
            log::info!("Version {published} is published; this is {VERSION}");
            Some(published)
        }
        Ok(published) => {
            log::info!("No newer version: {published} is published, this is {VERSION}");
            None
        }
        Err(why) => {
            log::info!("Could not check for a newer version: {why}");
            None
        }
    }
}

/// The version the page says is published.
async fn published(client: Result<reqwest::Client, String>) -> Result<String, String> {
    log::debug!("Checking for a newer version at {RELEASE}");

    let json = client?
        .get(RELEASE)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| crate::describe(&error))?
        .bytes()
        .await
        .map_err(|error| crate::describe(&error))?;

    let release: Release =
        serde_json::from_slice(&json).map_err(|error| format!("{RELEASE}: {error}"))?;

    Ok(release.version)
}

/// Whether `published` is a later version than `current`, comparing their
/// numbers dot by dot (`0.10.0` is later than `0.9.1`). A version that
/// doesn't read as numbers is never newer.
fn newer(published: &str, current: &str) -> bool {
    let numbers = |version: &str| -> Option<Vec<u64>> {
        version
            .split('-')
            .next()?
            .split('.')
            .map(|number| number.parse().ok())
            .collect()
    };

    match (numbers(published), numbers(current)) {
        (Some(published), Some(current)) => published > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_description_declares_its_version() {
        // As pages/release.sh writes it.
        let json = br#"{
  "version": "0.4.0",
  "binaries": {
    "windows-x86_64": {
      "url": "https://gitlab.com/juxuanu/old_launcher/-/jobs/artifacts/main/raw/old_launcher.exe?job=windows",
      "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
      "size": 16120832
    }
  }
}"#;

        let release: Release = serde_json::from_slice(json).unwrap();

        assert_eq!(release.version, "0.4.0");
        assert!(serde_json::from_slice::<Release>(b"{}").is_err());
    }

    #[test]
    fn the_description_is_beside_the_page() {
        assert_eq!(RELEASE, "https://old-launcher.icarns.xyz/release.json");
    }

    #[test]
    fn later_versions_compare_by_number() {
        assert!(newer("0.3.0", "0.2.0"));
        assert!(newer("0.10.0", "0.9.1"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("0.1.9", "0.2.0"));
        assert!(!newer("latest", "0.2.0"));
        assert!(newer("0.2.1-rc1", "0.2.0"), "a pre-release tag is ignored");
    }

    #[test]
    fn this_build_has_a_version_and_a_page() {
        assert!(newer(VERSION, "0.0.0"));
        assert!(PAGE.starts_with("https://"));
    }
}
