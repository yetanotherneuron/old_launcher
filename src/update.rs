//! Whether a newer launcher is published: the version the repository's
//! manifest declares, against this build's.

use crate::REPOSITORY;

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The launcher's page, where the downloads are.
pub const PAGE: &str = env!("CARGO_PKG_HOMEPAGE");

/// The published version, when it is newer than this build's. Nothing when
/// it can't be told: the check is a courtesy, logged but not reported.
pub async fn check(client: Result<reqwest::Client, String>) -> Option<String> {
    match published(client).await {
        Ok(published) if newer(&published, VERSION) => {
            log::info!("Version {published} is published; this is {VERSION}");
            Some(published)
        }
        Ok(published) => {
            log::info!("No newer version: the repository declares {published}, this is {VERSION}");
            None
        }
        Err(why) => {
            log::info!("Could not check for a newer version: {why}");
            None
        }
    }
}

/// The version the repository's manifest declares.
async fn published(client: Result<reqwest::Client, String>) -> Result<String, String> {
    let url = format!("{REPOSITORY}/-/raw/main/Cargo.toml");
    log::debug!("Checking for a newer version at {url}");

    let manifest = client?
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| crate::describe(&error))?
        .text()
        .await
        .map_err(|error| crate::describe(&error))?;

    version_in(&manifest).ok_or_else(|| "the manifest declares no version".to_owned())
}

/// The version a Cargo manifest declares: its first `version = "…"` line,
/// which for the launcher's is the workspace's.
fn version_in(manifest: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        line.trim()
            .strip_prefix("version = \"")?
            .strip_suffix('"')
            .map(str::to_owned)
    })
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
    fn the_manifest_version_is_the_workspaces() {
        let manifest = "[package]\nname = \"old_launcher\"\nversion.workspace = true\n\n\
                        [workspace.package]\nversion = \"0.3.0\"\n\n[workspace.dependencies]\n\
                        iced = { version = \"0.15.0-dev\" }\n";

        assert_eq!(version_in(manifest).as_deref(), Some("0.3.0"));
        assert_eq!(version_in("name = \"x\"\n"), None);
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
