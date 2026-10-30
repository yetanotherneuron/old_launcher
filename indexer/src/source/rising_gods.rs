//! Rising Gods' addon collection: <https://addons.rising-gods.de/addons>.
//!
//! A site listing addons for the Rising Gods 3.3.5a server. Its sitemap
//! links every addon page, and each page has the addon's name, a (German)
//! description, then sections with screenshots and downloads, and comments.
//! Links on the pages are relative to the site.

use std::sync::Arc;

use anyhow::Context;
use old_launcher_core::catalog;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::fetch::fetch;
use crate::source::{Listing, Source, Updated, entities};

/// The site, which the links on its pages are relative to.
const SITE: &str = "https://addons.rising-gods.de";
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
    /// are left out, but not one read at all means the site changed, which
    /// fails the run rather than dropping the whole collection.
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>> {
        let sitemap = String::from_utf8_lossy(&fetch(client, SITEMAP).await?).into_owned();

        let pages: Vec<String> = sitemap
            .split("<loc>")
            .skip(1)
            .filter_map(|rest| rest.split_once("</loc>").map(|(url, _)| url.trim()))
            .filter(|url| url.starts_with(PAGES))
            .map(str::to_owned)
            .collect();

        let listed = pages.len();
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

        anyhow::ensure!(
            !listings.is_empty(),
            "none of the {listed} addon pages in {SITEMAP} could be read: the site changed"
        );

        Ok(listings)
    }
}

/// Reads an addon page at `url`, if it offers a download.
fn parse(html: &str, url: &str) -> Option<Listing> {
    let (_, name) = between(html, "<h1", "</h1>")?.split_once('>')?;
    let name = text(name);
    let download =
        absolute(links(html).find(|link| link.contains("/sites/default/files/addons/"))?);

    // The first paragraph of the body is its summary. The body comes before
    // the sections, so a comment's text is never taken for it.
    let description = between(html, "</h1>", "<section")
        .and_then(|header| between(header, "class=\"content", "</div>"))
        .and_then(|body| body.split_once('>'))
        .map(|(_, body)| text(body.split("</p>").next().unwrap_or(body)))
        .unwrap_or_default();

    let image = between(html, ">Screenshots</h2>", "</section>")
        .and_then(|screenshots| links(screenshots).next())
        .map(absolute);

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
        // A new upload may keep the old file name: the indexer asks the
        // site whether the file changed instead.
        immutable: false,
    })
}

/// The text between the first `start` and the next `end` after it.
fn between<'a>(html: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let (_, rest) = html.split_once(start)?;
    let (inside, _) = rest.split_once(end)?;

    Some(inside)
}

/// `link` as an absolute URL, when it is relative to the site.
fn absolute(link: &str) -> String {
    if link.starts_with('/') {
        format!("{SITE}{link}")
    } else {
        link.to_owned()
    }
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

    /// An addon page as the site writes them since October 2026.
    const PAGE: &str = r#"
        <main class="panel"><article><p class="muted text-sm"><a class="link" href="/addons">AddOns</a></p>
        <div class="mb-4 flex"><h1 class="text-3xl font-bold">Quartz &amp; Friends</h1></div>
        <div class="mb-4 flex"><span class="muted">34.289<!-- --> Aufrufe</span>
        <a class="toggle toggle-off" href="/tags/castbars">Castbars</a></div>
        <div class="content mb-6"><p>Quartz ist eine
        modulare Implementierung von Zauberleisten.<br>Mit &#39;Modulen&#39;.</p>
        <p>Weitere Details.</p></div>
        <section class="mb-6"><h2 class="mb-2 text-xl font-bold">Screenshots</h2><ul class="flex">
        <li><a href="/sites/default/files/2016-11/quartz.jpg" class="block"><img src="/sites/default/files/2016-11/quartz.jpg"/></a></li>
        </ul></section>
        <section class="mb-6"><h2 class="mb-2 text-xl font-bold">Downloads</h2><ul class="space-y-2">
        <li class="flex"><a href="/sites/default/files/addons/Quartz.zip" class="button" download="Quartz.zip">Quartz.zip</a></li>
        </ul></section>
        <section id="kommentare"><h2 class="mb-2 text-xl font-bold">Kommentare</h2><ol class="mb-4">
        <li id="kommentar-397"><div class="content"><p>autoshot goes prrrrrrrr</p></div></li>
        </ol></section></article></main>
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
    fn a_comment_is_never_taken_for_the_description() {
        let page = r#"<h1 class="text-3xl font-bold">Quiet</h1>
            <section><h2>Downloads</h2><a href="/sites/default/files/addons/Quiet.zip">Quiet.zip</a></section>
            <section id="kommentare"><div class="content"><p>Great addon!</p></div></section>"#;

        let listing = parse(page, "https://addons.rising-gods.de/addons/quiet").unwrap();

        assert_eq!(listing.description, "");
        assert_eq!(listing.image, None);
    }

    #[test]
    fn pages_without_a_download_are_skipped() {
        let page = r#"<h1 class="text-3xl font-bold">Gone</h1>"#;

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
