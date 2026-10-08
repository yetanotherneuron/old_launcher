//! WoWInterface: <https://www.wowinterface.com>.
//!
//! One of the oldest addon sites, with addons for every version of the
//! game. Its public API, the one addon managers use, lists every addon with
//! the date of its latest file and the game versions it is tagged for. The
//! tags only go back to Wrath Classic (3.4), so the addons taken here are
//! the untagged ones whose latest file came out while 3.3 was the live
//! patch: left as they were since, their last version was made for 3.3. The
//! indexer then keeps those whose `.toc` declares 3.3.5a's interface.
//!
//! Download links carry the upload time of the file, so a new upload gets a
//! new link, and an addon left as it was keeps its own.

use std::collections::HashMap;

use anyhow::Context;
use old_launcher_core::{archive, catalog};
use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::fetch::fetch;
use crate::source::{Listing, Source, Updated, entities};

const API: &str = "https://api.mmoui.com/v3/game/WOW";

/// When patch 3.3.0 came out, 2009-12-08, in milliseconds since the epoch,
/// as the API dates files.
const PATCH_3_3: i64 = 1_260_230_400_000;

/// When patch 4.0.1 replaced it, 2010-10-12.
const PATCH_4_0: i64 = 1_286_841_600_000;

/// Uploads left out, by id: "Phil's Raid/Mage ui" (16339) is someone's
/// whole AddOns folder from 2010, 146 addons, and installing it would
/// overwrite each of them with its 2010 copy.
const EXCLUDED: &[&str] = &["16339"];

/// How many addons to ask the details of at once.
const BATCH: usize = 50;

/// About how long a description may get, in characters.
const SUMMARY_LENGTH: usize = 300;

/// Shorter openings say nothing about the addon yet: an "About" heading, a
/// "Hello there!".
const SHORTEST_SUMMARY: usize = 60;

/// BBCode tags whose text stays when the markup goes.
const FORMATTING: &[&str] = &[
    "b",
    "center",
    "code",
    "color",
    "email",
    "font",
    "highlight",
    "i",
    "indent",
    "left",
    "list",
    "noparse",
    "p",
    "pre",
    "quote",
    "right",
    "s",
    "size",
    "sub",
    "sup",
    "u",
    "url",
];

/// BBCode tags that embed something other than text, and go with their
/// content: images, videos, donation buttons.
const EMBEDS: &[&str] = &[
    "attach", "img", "media", "paypal", "pledgie", "video", "youtube",
];

/// The WoWInterface addon site.
pub struct Site;

/// An addon in the site's file list.
#[derive(Debug, Deserialize)]
struct Listed {
    #[serde(rename = "UID")]
    id: String,
    #[serde(rename = "UIName")]
    name: String,
    /// The version of its latest file.
    #[serde(rename = "UIVersion", default)]
    version: String,
    /// When its latest file was uploaded, in milliseconds since the epoch.
    #[serde(rename = "UIDate")]
    date: i64,
    /// The game versions it is tagged for: none for files from before the
    /// tags.
    #[serde(rename = "UICompatibility", default)]
    compatibility: Option<Vec<IgnoredAny>>,
    #[serde(rename = "UIFileInfoURL")]
    page: String,
    /// Its screenshots.
    #[serde(rename = "UIIMGs", default)]
    images: Option<Vec<String>>,
}

/// An addon's details, which the API gives a batch of addons at a time.
#[derive(Debug, Deserialize)]
struct Details {
    #[serde(rename = "UID")]
    id: String,
    /// Where to download its latest file.
    #[serde(rename = "UIDownload")]
    download: String,
    /// What it does, in BBCode.
    #[serde(rename = "UIDescription", default)]
    description: Option<String>,
}

impl Source for Site {
    fn info(&self) -> catalog::Source {
        catalog::Source {
            id: "wowinterface".into(),
            name: "WoWInterface".into(),
            homepage: "https://www.wowinterface.com/addons.php".into(),
        }
    }

    /// Lists the untagged addons last updated during patch 3.3, asking for
    /// their download links and descriptions a batch at a time.
    async fn list(&self, client: &reqwest::Client) -> anyhow::Result<Vec<Listing>> {
        let all: Vec<Listed> =
            serde_json::from_slice(&fetch(client, &format!("{API}/filelist.json")).await?)
                .context("reading WoWInterface's file list")?;
        let listed = from_patch_3_3(all);

        let mut details = HashMap::new();

        for batch in listed.chunks(BATCH) {
            let ids: Vec<&str> = batch.iter().map(|addon| addon.id.as_str()).collect();
            let url = format!("{API}/filedetails/{}.json", ids.join(","));
            let batch: Vec<Details> = serde_json::from_slice(&fetch(client, &url).await?)
                .context("reading WoWInterface's file details")?;

            details.extend(
                batch
                    .into_iter()
                    .map(|details| (details.id.clone(), details)),
            );
        }

        Ok(listings(listed, &details))
    }
}

/// The addons whose latest file came out while 3.3 was the live patch and
/// is tagged for no game version, but those [`EXCLUDED`].
fn from_patch_3_3(all: Vec<Listed>) -> Vec<Listed> {
    all.into_iter()
        .filter(|addon| {
            (PATCH_3_3..PATCH_4_0).contains(&addon.date)
                && addon.compatibility.as_ref().is_none_or(Vec::is_empty)
                && !EXCLUDED.contains(&addon.id.as_str())
        })
        .collect()
}

/// `listed` as listings, with the `details` the API gave for them. Addons
/// it gave none for are left out.
fn listings(listed: Vec<Listed>, details: &HashMap<String, Details>) -> Vec<Listing> {
    let names = unique_names(&listed);

    listed
        .into_iter()
        .zip(names)
        .filter_map(|(addon, name)| {
            let details = details.get(&addon.id)?;

            Some(Listing {
                name,
                description: summary(details.description.as_deref().unwrap_or_default()),
                download: details.download.clone(),
                image: addon.images.and_then(|images| images.into_iter().next()),
                page: Some(addon.page),
                // The change log is a tab of the addon's page, which the card
                // links already.
                changes: None,
                links: Vec::new(),
                updated: Updated::Given(Some(archive::date(addon.date.div_euclid(1_000)))),
                // Its date only suggests it was made for 3.3.
                require_wotlk: true,
                // The link carries the upload time.
                immutable: true,
            })
        })
        .collect()
}

/// The names to list `listed` under, in order. Listings are told apart by
/// name, so a name several addons share gets the addon's version added,
/// and its id when that is shared too: one addon is sometimes uploaded
/// twice, a stable version and a beta.
fn unique_names(listed: &[Listed]) -> Vec<String> {
    fn shared(names: &[String]) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for name in names {
            *counts.entry(name.to_lowercase()).or_default() += 1;
        }
        counts
    }

    let plain: Vec<String> = listed
        .iter()
        .map(|addon| entities(addon.name.trim()))
        .collect();
    let counts = shared(&plain);
    let versioned: Vec<String> = listed
        .iter()
        .zip(plain)
        .map(|(addon, name)| {
            if counts
                .get(&name.to_lowercase())
                .is_some_and(|&count| count > 1)
            {
                format!("{name} {}", addon.version.trim())
            } else {
                name
            }
        })
        .collect();
    let counts = shared(&versioned);

    listed
        .iter()
        .zip(versioned)
        .map(|(addon, name)| {
            if counts
                .get(&name.to_lowercase())
                .is_some_and(|&count| count > 1)
            {
                format!("{name} ({})", addon.id)
            } else {
                name
            }
        })
        .collect()
}

/// What an addon does, from the start of its BBCode `description`: the
/// markup taken out, character references decoded, and paragraphs joined
/// until there is a sentence's worth of text, then cut to whole sentences
/// near [`SUMMARY_LENGTH`] characters.
fn summary(description: &str) -> String {
    let text = entities(&plain(description));
    let mut summary = String::new();
    let mut paragraph = String::new();

    // A blank line ends a paragraph, and so does a line that is only a
    // rule; one more is added after the last.
    for line in text.lines().chain([""]) {
        let words: Vec<&str> = line.split_whitespace().filter(|word| !rule(word)).collect();

        if !words.is_empty() {
            paragraph.push(' ');
            paragraph.push_str(&words.join(" "));
            continue;
        }
        if paragraph.is_empty() {
            continue;
        }

        summary.push(' ');
        summary.push_str(&paragraph);
        paragraph.clear();

        if summary.chars().count() >= SHORTEST_SUMMARY {
            break;
        }
    }

    shorten(&summary.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Whether `word` is a rule drawn with symbols, like `*****` or `=====`:
/// decoration, not text.
fn rule(word: &str) -> bool {
    let mut chars = word.chars();

    chars.next().is_some_and(|first| {
        matches!(first, '-' | '=' | '_' | '*' | '#' | '~' | '+')
            && word.len() >= 3
            && chars.all(|c| c == first)
    })
}

/// `bbcode` without its markup: the [`FORMATTING`] tags leave their text
/// behind, the [`EMBEDS`] go with their content, and list items start on a
/// line of their own, after a bullet. Brackets that aren't BBCode, like
/// "[Friend]", stay.
fn plain(bbcode: &str) -> String {
    let mut plain = String::with_capacity(bbcode.len());
    let mut rest = bbcode;

    while let Some(start) = rest.find('[') {
        plain.push_str(&rest[..start]);
        let after = &rest[start + 1..];

        let Some(end) = after.find(']') else {
            plain.push('[');
            rest = after;
            continue;
        };
        let inside = &after[..end];
        let closing = inside.starts_with('/');
        let name = inside
            .trim_start_matches('/')
            .split('=')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let beyond = &after[end + 1..];

        if name == "*" {
            plain.push_str("\n• ");
            rest = beyond;
        } else if EMBEDS.contains(&name.as_str()) && !closing {
            // Lowercasing ASCII keeps every byte where it was.
            let close = format!("[/{name}]");
            rest = beyond
                .to_ascii_lowercase()
                .find(&close)
                .map_or(beyond, |at| &beyond[at + close.len()..]);
        } else if FORMATTING.contains(&name.as_str()) || EMBEDS.contains(&name.as_str()) {
            rest = beyond;
        } else {
            plain.push('[');
            rest = after;
        }
    }
    plain.push_str(rest);

    plain
}

/// `text` cut to the whole sentences that fit in [`SUMMARY_LENGTH`]
/// characters, or to whole words and an ellipsis when not even the first
/// sentence does.
fn shorten(text: &str) -> String {
    let Some((limit, _)) = text.char_indices().nth(SUMMARY_LENGTH) else {
        return text.to_owned();
    };
    let head = &text[..limit];

    // Sentence marks are one byte, so the next character starts right after.
    let sentence = head.char_indices().rfind(|&(at, c)| {
        matches!(c, '.' | '!' | '?') && text[at + 1..].starts_with(char::is_whitespace)
    });

    match sentence {
        Some((at, _)) => text[..=at].to_owned(),
        None => {
            let words = head
                .rfind(char::is_whitespace)
                .map_or(head, |at| &head[..at]);
            format!("{}…", words.trim_end())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FlightMap as the file list has it; a tagged addon; untagged ones from
    /// before and after patch 3.3; an excluded one; and a beta of FlightMap
    /// under its name.
    const FILE_LIST: &str = r#"[
        {"UID": "3897", "UICATID": "24", "UIVersion": "3.3", "UIDate": 1264031816000,
         "UIName": "FlightMap", "UIAuthorName": "dhask",
         "UIFileInfoURL": "https://www.wowinterface.com/downloads/info3897-FlightMap.html",
         "UIDownloadTotal": "157610", "UIDownloadMonthly": "31", "UIFavoriteTotal": "788",
         "UICompatibility": null, "UIDir": ["FlightMap"],
         "UIIMG_Thumbs": ["https://cdn-wow.mmoui.com/preview/tiny/pvw188.jpg"],
         "UIIMGs": ["https://cdn-wow.mmoui.com/preview/pvw188.jpg"], "UISiblings": null},
        {"UID": "3826", "UIVersion": "11.0.2.3", "UIDate": 1724089877000, "UIName": "CTMod",
         "UIFileInfoURL": "https://www.wowinterface.com/downloads/info3826-CTMod.html",
         "UICompatibility": [{"version": "11.0.2", "name": "The War Within"}],
         "UIIMGs": null},
        {"UID": "1", "UIVersion": "1.0", "UIDate": 1250000000000, "UIName": "Before",
         "UIFileInfoURL": "https://www.wowinterface.com/downloads/info1.html",
         "UICompatibility": null, "UIIMGs": null},
        {"UID": "2", "UIVersion": "1.0", "UIDate": 1300000000000, "UIName": "After",
         "UIFileInfoURL": "https://www.wowinterface.com/downloads/info2.html",
         "UICompatibility": null, "UIIMGs": null},
        {"UID": "16339", "UIVersion": "1.0", "UIDate": 1268000000000,
         "UIName": "Phil's Raid/Mage ui",
         "UIFileInfoURL": "https://www.wowinterface.com/downloads/info16339.html",
         "UICompatibility": null, "UIIMGs": null},
        {"UID": "4", "UIVersion": "3.4 beta", "UIDate": 1270000000000, "UIName": "FlightMap ",
         "UIFileInfoURL": "https://www.wowinterface.com/downloads/info4.html",
         "UICompatibility": null, "UIIMGs": null}
    ]"#;

    const DETAILS: &str = r#"[
        {"UID": "3897", "UICATID": "24", "UIVersion": "3.3", "UIDate": 1264031816000,
         "UIMD5": "0d1b", "UIFileName": "FlightMap-v3.3.zip",
         "UIDownload": "https://cdn.wowinterface.com/downloads/getfile.php?id=3897&d=1264031816&minion",
         "UIPending": "0", "UIName": "FlightMap", "UIAuthorName": "dhask",
         "UIDescription": "FlightMap is all about the flights you take in the World of Warcraft.\r\n\r\nUse /fmap.",
         "UIChangeLog": "", "UIHitCount": "157610", "UIHitCountMonthly": "31",
         "UIFavoriteTotal": "788"},
        {"UID": "4", "UIDownload": "https://cdn.wowinterface.com/downloads/getfile.php?id=4&d=1270000000&minion",
         "UIDescription": null}
    ]"#;

    fn listed() -> Vec<Listed> {
        from_patch_3_3(serde_json::from_str(FILE_LIST).unwrap())
    }

    fn details() -> HashMap<String, Details> {
        let details: Vec<Details> = serde_json::from_str(DETAILS).unwrap();

        details
            .into_iter()
            .map(|details| (details.id.clone(), details))
            .collect()
    }

    #[test]
    fn untagged_addons_last_updated_during_patch_3_3_are_taken_but_the_excluded() {
        let ids: Vec<String> = listed().into_iter().map(|addon| addon.id).collect();

        assert_eq!(ids, ["3897", "4"], "not 16339, Phil's whole AddOns folder");
    }

    #[test]
    fn listings_download_from_the_site_and_are_dated_by_their_upload() {
        let listings = listings(listed(), &details());
        let flight_map = &listings[0];

        assert_eq!(
            flight_map.download,
            "https://cdn.wowinterface.com/downloads/getfile.php?id=3897&d=1264031816&minion"
        );
        assert_eq!(
            flight_map.page.as_deref(),
            Some("https://www.wowinterface.com/downloads/info3897-FlightMap.html")
        );
        assert_eq!(
            flight_map.image.as_deref(),
            Some("https://cdn-wow.mmoui.com/preview/pvw188.jpg")
        );
        assert_eq!(
            flight_map.updated,
            Updated::Given(Some("2010-01-20".into()))
        );
        // The first paragraph says enough on its own.
        assert_eq!(
            flight_map.description,
            "FlightMap is all about the flights you take in the World of Warcraft."
        );
        assert!(flight_map.require_wotlk && flight_map.immutable);
        assert_eq!(listings[1].description, "");
    }

    #[test]
    fn addons_sharing_a_name_are_told_apart_by_version_then_id() {
        let names: Vec<String> = listings(listed(), &details())
            .into_iter()
            .map(|listing| listing.name)
            .collect();
        assert_eq!(names, ["FlightMap 3.3", "FlightMap 3.4 beta"]);

        let mut twins = listed();
        twins[1].version = "3.3".into();
        assert_eq!(
            unique_names(&twins),
            ["FlightMap 3.3 (3897)", "FlightMap 3.3 (4)"]
        );
    }

    #[test]
    fn addons_the_api_gave_no_details_for_are_left_out() {
        let mut details = details();
        let _ = details.remove("4");

        assert_eq!(listings(listed(), &details).len(), 1);
    }

    #[test]
    fn descriptions_lose_their_markup_and_open_with_something_said() {
        let description = "Hello there!\r\n\r\n[B]Foo[/B] does [COLOR=\"Orange\"]things[/COLOR] \
            for your [Friend] list[paypal]4714451[/paypal] &#1072;nd \
            [url=https://example.com]more[/url].\r\n[img]https://example.com/a.png[/IMG]\
            Second line.\r\n\r\nThird paragraph.";

        assert_eq!(
            summary(description),
            "Hello there! Foo does things for your [Friend] list \u{430}nd more. Second line."
        );
    }

    #[test]
    fn rules_drawn_with_symbols_are_left_out() {
        // As EnchantingSeller's description opens.
        let description = "*****************\r\nv30300.01-release\r\n- updated toc\r\n\
            *****************\r\n\r\nSells the enchanting materials in your bags... \
            ==== for you ====";

        assert_eq!(
            summary(description),
            "v30300.01-release - updated toc Sells the enchanting materials in your bags... \
             for you"
        );
    }

    #[test]
    fn list_items_follow_bullets() {
        assert_eq!(
            summary("Features:\r\n[list][*]Movable frames[*]Hidden Blizzard frames[/list]"),
            "Features: • Movable frames • Hidden Blizzard frames"
        );
    }

    #[test]
    fn long_descriptions_end_at_a_whole_sentence() {
        let sentence = "This addon does one thing and does it rather well, mostly. ";
        let long = sentence.repeat(8);
        let summary = summary(&long);

        assert!(summary.chars().count() <= SUMMARY_LENGTH, "{summary}");
        assert!(summary.ends_with("mostly."), "{summary}");
        assert_eq!(summary, sentence.repeat(5).trim_end());

        let rambling = "word ".repeat(100);
        let cut = shorten(rambling.trim_end());
        assert!(cut.ends_with("word…"), "{cut}");
        assert!(cut.chars().count() <= SUMMARY_LENGTH + 1, "{cut}");
    }
}
