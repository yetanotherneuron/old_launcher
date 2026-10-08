//! Rising Gods' addon collection: <https://addons.rising-gods.de/addons>.
//!
//! A Drupal site listing addons for the Rising Gods 3.3.5a server. Its
//! sitemap links every addon page, and each page has the addon's name, a
//! (German) description, screenshots and a table of downloads.

use std::sync::Arc;

use anyhow::Context;
use old_launcher_core::catalog;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::fetch::fetch;
use crate::source::{Listing, Source, Updated, entities};

const SITEMAP: &str = "https://addons.rising-gods.de/sitemap.xml";
const PAGES: &str = "https://addons.rising-gods.de/addons/";

/// The Rising Gods server's addon collection.
pub struct Collection;

impl Source for Collection {
    fn info(&self) -> catalog::Source {
        catalog::Source {
            id: "rising-gods".into(),
            name: "Rising Gods".into(),
            homepage: "https://addons.rising-gods.de/addons".into(),
        }
    }

    /// Reads every addon page the sitemap lists. Pages without a download
    /// are left out.
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>> {
        let sitemap = String::from_utf8_lossy(&fetch(client, SITEMAP).await?).into_owned();

        let pages: Vec<String> = sitemap
            .split("<loc>")
            .skip(1)
            .filter_map(|rest| rest.split_once("</loc>").map(|(url, _)| url.trim()))
            .filter(|url| url.starts_with(PAGES))
            .map(str::to_owned)
            .collect();

        // A few at a time: this is a small community site.
        let requests = Arc::new(Semaphore::new(4));
        let mut reads = JoinSet::new();

        for page in pages {
            let client = client.clone();
            let requests = Arc::clone(&requests);

            let _ = reads.spawn(async move {
                let _permit = requests.acquire_owned().await?;
                let html = fetch(&client, &page).await?;

                anyhow::Ok(parse(&String::from_utf8_lossy(&html), &page))
            });
        }

        let mut listings = Vec::new();

        while let Some(read) = reads.join_next().await {
            listings.extend(read.context("reading an addon page")??);
        }

        Ok(listings)
    }
}

/// Reads an addon page at `url`, if it offers a download.
fn parse(html: &str, url: &str) -> Option<Listing> {
    let name = text(between(
        html,
        "<h1 class=\"page-header\"><span>",
        "</span>",
    )?);
    let download = links(html)
        .find(|link| link.contains("/sites/default/files/addons/"))?
        .to_owned();

    // The first paragraph of the body is its summary.
    let description = between(html, "field--name-body", "</div>")
        .and_then(|body| body.split_once('>'))
        .map(|(_, body)| text(body.split("</p>").next().unwrap_or(body)))
        .unwrap_or_default();

    let image = between(html, "field--name-field-screenshots", "</div>")
        .and_then(|screenshots| links(screenshots).next())
        .map(str::to_owned);

    Some(Listing {
        name,
        description,
        download,
        image,
        page: Some(url.to_owned()),
        // The site keeps no history per addon.
        changes: None,
        links: Vec::new(),
        updated: Updated::FromArchive,
        // The collection is curated for the Rising Gods 3.3.5a server.
        require_wotlk: false,
        // A new upload may keep the old file name.
        immutable: false,
    })
}

/// The text between the first `start` and the next `end` after it.
fn between<'a>(html: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let (_, rest) = html.split_once(start)?;
    let (inside, _) = rest.split_once(end)?;

    Some(inside)
}

/// Every `href` target in `html`, in order.
fn links(html: &str) -> impl Iterator<Item = &str> {
    html.split("href=\"")
        .skip(1)
        .filter_map(|rest| rest.split_once('"').map(|(link, _)| link))
}

/// Visible text: tags removed, entities decoded, whitespace collapsed.
fn text(html: &str) -> String {
    let mut plain = String::with_capacity(html.len());
    let mut in_tag = false;

    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                // `<br />` and friends separate words.
                plain.push(' ');
            }
            '>' => in_tag = false,
            c if !in_tag => plain.push(c),
            _ => {}
        }
    }

    entities(&plain)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
        <h1 class="page-header"><span>Quartz &amp; Friends</span></h1>
        <div class="field field--name-body field--type-text-with-summary"><p>Quartz ist eine
        modulare Implementierung von Zauberleisten.<br />Mit &#039;Modulen&#039;.</p>
        <p>Weitere Details.</p></div>
        <div class="field field--name-field-screenshots"><div class="field__items">
        <span><a href="https://addons.rising-gods.de/sites/default/files/2016-11/quartz.jpg"></a></span>
        </div></div>
        <table><tr><td><a href="https://addons.rising-gods.de/sites/default/files/addons/Quartz.zip"
        type="application/zip">Quartz.zip</a></td></tr></table>
    "#;

    #[test]
    fn addon_pages_are_read_into_listings() {
        let listing = parse(PAGE, "https://addons.rising-gods.de/addons/quartz").unwrap();

        assert_eq!(listing.name, "Quartz & Friends");
        assert_eq!(
            listing.description,
            "Quartz ist eine modulare Implementierung von Zauberleisten. Mit 'Modulen'."
        );
        assert_eq!(
            listing.download,
            "https://addons.rising-gods.de/sites/default/files/addons/Quartz.zip"
        );
        assert_eq!(
            listing.image.as_deref(),
            Some("https://addons.rising-gods.de/sites/default/files/2016-11/quartz.jpg")
        );
        assert_eq!(
            listing.page.as_deref(),
            Some("https://addons.rising-gods.de/addons/quartz")
        );
    }

    #[test]
    fn pages_without_a_download_are_skipped() {
        let page = r#"<h1 class="page-header"><span>Gone</span></h1>"#;

        assert_eq!(
            parse(page, "https://addons.rising-gods.de/addons/gone"),
            None
        );
    }

    #[test]
    fn text_survives_odd_ampersands() {
        assert_eq!(
            text("Tom &amp; Jerry & co &#x263A; &bogus;"),
            "Tom & Jerry & co ☺ &bogus;"
        );
    }
}
