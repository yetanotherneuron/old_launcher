//! The upstream collections the catalog is built from.
//!
//! Each source is a type implementing [`Source`], in a module of its own.
//! Adding one means writing that module and listing the source in `main`.

pub mod github;
pub mod gitlab;
pub mod locus313;
pub mod nom0re;
pub mod rising_gods;
pub mod wowinterface;

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
    /// The history of its changes at the source (a commit log, a list of
    /// releases), if the source keeps one.
    pub changes: Option<String>,
    /// Other pages about the addon.
    pub links: Vec<catalog::Link>,
    /// Where the addon's last-update date comes from.
    pub updated: Updated,
    /// Whether to keep it only if one of its folders declares the 3.3.5a
    /// interface: sources that are not curated for 3.3.5a list other things.
    pub require_wotlk: bool,
    /// Whether the archive at `download` never changes, a new version
    /// getting a new URL: then what an earlier run found in it still holds,
    /// and it is not downloaded again.
    pub immutable: bool,
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

/// `text` with its HTML character references decoded (`&amp;`, `&#039;`,
/// `&#x263A;`). Anything that isn't one, like a lone `&`, stays as it is.
pub fn entities(text: &str) -> String {
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;

    while let Some((before, after)) = rest.split_once('&') {
        decoded.push_str(before);

        let entity = after
            .split_once(';')
            .filter(|(entity, _)| entity.len() <= 8)
            .and_then(|(entity, after)| {
                let c = match entity {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    "nbsp" => ' ',
                    _ => {
                        let code = match entity.strip_prefix("#x") {
                            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                            None => entity.strip_prefix('#')?.parse().ok()?,
                        };
                        char::from_u32(code)?
                    }
                };

                Some((c, after))
            });

        match entity {
            Some((c, after)) => {
                decoded.push(c);
                rest = after;
            }
            None => {
                decoded.push('&');
                rest = after;
            }
        }
    }
    decoded.push_str(rest);

    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_references_are_decoded_and_odd_ampersands_kept() {
        assert_eq!(
            entities("Tom &amp; Jerry & co &#x263A; &#1072; &bogus;"),
            "Tom & Jerry & co ☺ а &bogus;"
        );
    }
}
