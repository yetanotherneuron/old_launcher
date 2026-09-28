//! The upstream collections the catalog is built from.
//!
//! Each source is a type implementing [`Source`], in a module of its own.
//! Adding one means writing that module and listing the source in `main`.

pub mod github;
pub mod locus313;
pub mod nom0re;
pub mod rising_gods;

use old_launcher_core::catalog;

/// An upstream collection of 3.3.5a addons.
pub trait Source {
    /// How the catalog presents the source.
    fn info(&self) -> catalog::Source;

    /// Everything the source lists.
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>>;
}

/// An addon as its source lists it, before its archive is inspected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// Display name, unique within the source.
    pub name: String,
    /// What the addon does.
    pub description: String,
    /// Where to download its archive.
    pub download: String,
    /// A screenshot, if the source has one.
    pub image: Option<String>,
    /// The addon's own page at the source, if it has one.
    pub page: Option<String>,
    /// Other pages about the addon.
    pub links: Vec<catalog::Link>,
    /// Where the addon's last-update date comes from.
    pub updated: Updated,
    /// Whether to keep it only if one of its folders declares the 3.3.5a
    /// interface: sources that are not curated for 3.3.5a list other things.
    pub require_wotlk: bool,
}

/// Where a listing's last-update date comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Updated {
    /// The newest file date inside its archive: the last commit, for a
    /// snapshot of a repository.
    FromArchive,
    /// A date the source gives (`YYYY-MM-DD`), or `None` when it has none,
    /// for archives whose file dates mean nothing (a build job stamped them
    /// with the build date).
    Given(Option<String>),
}
