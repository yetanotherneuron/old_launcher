//! Addon `.toc` metadata files.

use std::cmp::Ordering;

/// The metadata header (`## Key: Value` lines) of an addon's `.toc` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Toc {
    /// The client interface version the addon targets (30300 for 3.3.5a).
    pub interface: Option<u32>,
    /// Display name, with UI escape sequences removed.
    pub title: Option<String>,
    /// Short description, with UI escape sequences removed.
    pub notes: Option<String>,
    /// Author field, as written.
    pub author: Option<String>,
    /// Version field, as written.
    pub version: Option<String>,
    /// Addons that must be loaded first (`Dependencies`, `RequiredDeps`, `Dep*`).
    pub dependencies: Vec<String>,
    /// Addons loaded first when present (`OptionalDeps`).
    pub optional_dependencies: Vec<String>,
    /// Account-wide saved variable globals.
    pub saved_variables: Vec<String>,
    /// Per-character saved variable globals.
    pub saved_variables_per_character: Vec<String>,
    /// Whether the client only loads the addon when another addon asks for it.
    pub load_on_demand: bool,
    /// The addon's own web pages (`X-Website`, `URL`, `X-Repository`, …), as
    /// http(s) addresses, in the order written and without repeats.
    pub websites: Vec<String>,
}

impl Toc {
    /// The interface version of the 3.3.5a client.
    pub const WOTLK_INTERFACE: u32 = 30300;

    /// Parses the raw bytes of a `.toc` file.
    ///
    /// Old addons are often Latin-1 rather than UTF-8, so invalid UTF-8 is
    /// decoded as Latin-1 instead of being rejected.
    pub fn parse(bytes: &[u8]) -> Self {
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        let source = match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) => bytes.iter().map(|&byte| char::from(byte)).collect(),
        };

        let mut toc = Self::default();

        for line in source.lines() {
            let Some(field) = line.strip_prefix("##") else {
                continue;
            };
            let Some((key, value)) = field.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();

            if value.is_empty() {
                continue;
            }

            match key.as_str() {
                "interface" => toc.interface = value.parse().ok(),
                "title" => toc.title = Some(strip_escapes(value)),
                "notes" => toc.notes = Some(strip_escapes(value)),
                "author" => toc.author = Some(strip_escapes(value)),
                "version" => toc.version = Some(strip_escapes(value)),
                "optionaldeps" => toc.optional_dependencies.extend(list(value)),
                "savedvariables" => toc.saved_variables.extend(list(value)),
                "savedvariablespercharacter" => {
                    toc.saved_variables_per_character.extend(list(value));
                }
                "loadondemand" => toc.load_on_demand = value == "1",
                // The names addons use for their page, from the 2010
                // packagers' `X-Website` to the backports' repositories.
                "x-website"
                | "x-url"
                | "url"
                | "website"
                | "x-repository"
                | "x-github"
                | "x-github-project-homepage" => {
                    let address = strip_escapes(value)
                        .split_whitespace()
                        .find_map(web_address);

                    if let Some(address) = address
                        && !toc.websites.contains(&address)
                    {
                        toc.websites.push(address);
                    }
                }
                "dependencies" | "requireddeps" => toc.dependencies.extend(list(value)),
                key if key.starts_with("dep") => toc.dependencies.extend(list(value)),
                _ => {}
            }
        }

        toc
    }

    /// Whether the addon declares an interface other than 3.3.5a's.
    ///
    /// This is a hint, not a verdict: backports often keep the interface
    /// number of the client they were ported from.
    pub fn targets_other_client(&self) -> bool {
        self.interface
            .is_some_and(|interface| interface != Self::WOTLK_INTERFACE)
    }
}

/// The game version an addon declaring `interface` was made for, as players
/// call it ("Cataclysm", "WotLK 3.2", "Wrath Classic"), or `None` for 3.3.5a
/// itself.
///
/// Interface numbers are the patch as digits: 30300 is 3.3.0, 110002 is
/// 11.0.2. The Classic re-releases reuse their expansion's major version
/// with a later minor one (1.13 on, 2.5, 3.4, 4.4, 5.5).
pub fn made_for(interface: u32) -> Option<String> {
    if interface == Toc::WOTLK_INTERFACE {
        return None;
    }

    let major = interface / 10_000;
    let minor = interface % 10_000 / 100;

    Some(match (major, minor) {
        (1, 13..=15) => "WoW Classic".to_owned(),
        (1, _) => "Vanilla".to_owned(),
        (2, 5) => "Burning Crusade Classic".to_owned(),
        (2, _) => "The Burning Crusade".to_owned(),
        (3, 0..=2) => format!("WotLK 3.{minor}"),
        (3, 4) => "Wrath Classic".to_owned(),
        (3, _) => "another WotLK patch".to_owned(),
        (4, 4) => "Cataclysm Classic".to_owned(),
        (4, _) => "Cataclysm".to_owned(),
        (5, 5) => "Mists of Pandaria Classic".to_owned(),
        (5, _) => "Mists of Pandaria".to_owned(),
        (6, _) => "Warlords of Draenor".to_owned(),
        (7, _) => "Legion".to_owned(),
        (8, _) => "Battle for Azeroth".to_owned(),
        (9, _) => "Shadowlands".to_owned(),
        (10, _) => "Dragonflight".to_owned(),
        (11, _) => "The War Within".to_owned(),
        (12, _) => "Midnight".to_owned(),
        (0, _) => "an unknown game version".to_owned(),
        _ => "a newer game version".to_owned(),
    })
}

/// Compares two `.toc` versions, when they have the same shape: the same
/// text around their numbers, a leading `v` aside. `r736` and `r720`
/// compare, as do `9.9.1-335` and `9.8.0-335`, `1.19.5 (r240)` and
/// `1.19.5 (r238)`, `3.3e` and `3.3f`, and dotted numbers of different
/// lengths (`2.0.1` is newer than `2.0`). Versions of different shapes
/// don't compare: `r736` and `1.2`, a lone number and dotted ones, or
/// `1.0-beta` and `1.0`. Neither do versions without a number.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    let a = a.trim();
    let b = b.trim();

    if a.eq_ignore_ascii_case(b) {
        return Some(Ordering::Equal);
    }

    compare_parts(strip_v(a), strip_v(b))
}

/// `version` without a leading `v` or `v.`, when a number follows it.
fn strip_v(version: &str) -> &str {
    ["v.", "V.", "v", "V"]
        .into_iter()
        .find_map(|prefix| {
            version
                .strip_prefix(prefix)
                .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .unwrap_or(version)
}

/// Compares the text before the first numbers, then the numbers, then
/// whatever follows them, the same way.
fn compare_parts(a: &str, b: &str) -> Option<Ordering> {
    let (prefix_a, numbers_a, rest_a) = parts(a)?;
    let (prefix_b, numbers_b, rest_b) = parts(b)?;

    if !prefix_a.eq_ignore_ascii_case(prefix_b) {
        return None;
    }

    // A lone number counts builds or revisions: not the shape of dotted
    // numbers, however many.
    if (numbers_a.len() == 1) != (numbers_b.len() == 1) {
        return None;
    }

    let length = numbers_a.len().max(numbers_b.len());
    let padded = |numbers: &[u64]| -> Vec<u64> {
        numbers
            .iter()
            .copied()
            .chain(std::iter::repeat(0))
            .take(length)
            .collect()
    };

    match padded(&numbers_a).cmp(&padded(&numbers_b)) {
        Ordering::Equal if rest_a.eq_ignore_ascii_case(rest_b) => Some(Ordering::Equal),
        Ordering::Equal if rest_a.is_empty() || rest_b.is_empty() => None,
        // A patch letter: `3.3e`, then `3.3f`.
        Ordering::Equal if patch_letter(rest_a) && patch_letter(rest_b) => Some(rest_a.cmp(rest_b)),
        Ordering::Equal => compare_parts(rest_a, rest_b),
        ordering => Some(ordering),
    }
}

/// Whether `rest`, what follows a version's numbers, is one lowercase
/// letter.
fn patch_letter(rest: &str) -> bool {
    rest.len() == 1 && rest.bytes().all(|byte| byte.is_ascii_lowercase())
}

/// `version` split at its first run of numbers: the text before, the
/// numbers (`1.2.3`, `1-2`: each after a single separator), and the text
/// after. Nothing when there is no number, or one too big.
fn parts(version: &str) -> Option<(&str, Vec<u64>, &str)> {
    let start = version.find(|c: char| c.is_ascii_digit())?;
    let (prefix, mut rest) = version.split_at(start);
    let mut numbers = Vec::new();

    loop {
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        numbers.push(rest[..end].parse().ok()?);
        rest = &rest[end..];

        match rest.strip_prefix(['.', '-', '_']) {
            Some(next) if next.starts_with(|c: char| c.is_ascii_digit()) => rest = next,
            _ => break,
        }
    }

    Some((prefix, numbers, rest))
}

/// Removes the client's UI escape sequences (colors, textures, hyperlinks)
/// from a string, leaving the plain text.
pub fn strip_escapes(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '|' {
            plain.push(c);
            continue;
        }

        match chars.next() {
            // `|cAARRGGBB` starts a color.
            Some('c' | 'C') => {
                for _ in 0..8 {
                    let _ = chars.next_if(char::is_ascii_hexdigit);
                }
            }
            // `|r` ends a color, `|H…|h` wraps a hyperlink's visible text.
            Some('r' | 'R' | 'h') => {}
            Some('H') => {
                while chars.next_if(|&c| c != '|').is_some() {}
                // Consume the `|h` that ends the link payload.
                let _ = chars.next_if_eq(&'|');
                let _ = chars.next_if_eq(&'h');
            }
            // `|T…|t` embeds a texture; drop it entirely.
            Some('T') => {
                while let Some(c) = chars.next() {
                    if c == '|' && chars.next_if_eq(&'t').is_some() {
                        break;
                    }
                }
            }
            Some('n') => plain.push(' '),
            Some('|') => plain.push('|'),
            Some(other) => {
                plain.push('|');
                plain.push(other);
            }
            None => plain.push('|'),
        }
    }

    plain.trim().to_owned()
}

/// `word` as a web address, when it is one: an http(s) address, or one
/// starting with `www.`, taken as http. Brackets, quotes and punctuation
/// around it are left out. Anything else (an email address, another
/// scheme, a host without a dot) is not one, so nothing but a web page is
/// ever opened from a `.toc`.
fn web_address(word: &str) -> Option<String> {
    let word = word.trim_matches(['(', ')', '<', '>', '[', ']', '"', '\'', ',', ';', '.']);
    let (scheme, rest) = match word.split_once("://") {
        Some((scheme, rest))
            if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") =>
        {
            (scheme.to_ascii_lowercase(), rest)
        }
        Some(_) => return None,
        None if word
            .get(..4)
            .is_some_and(|start| start.eq_ignore_ascii_case("www.")) =>
        {
            ("http".to_owned(), word)
        }
        None => return None,
    };

    // A host name with a dot, as in example.com (and maybe a port), and
    // nothing an address can't hold.
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let labels: Vec<&str> = host.split('.').collect();
    let valid = labels.len() > 1
        && labels.iter().all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ':'))
        })
        && rest.chars().all(|c| c.is_ascii_graphic());

    valid.then(|| format!("{scheme}://{rest}"))
}

fn list(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split([',', ' '])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_wotlk_toc() {
        let toc = Toc::parse(
            b"## Interface: 30300\r\n\
              ## Title: |cff33ff99Deadly Boss Mods|r - Core\r\n\
              ## Notes: Boss mods\r\n\
              ## Version: 4.24\r\n\
              ## Dependencies: Ace3, LibStub\r\n\
              ## OptionalDeps: LibSharedMedia-3.0\r\n\
              ## SavedVariables: DBM_SavedOptions, DBM_Minimap\r\n\
              ## SavedVariablesPerCharacter: DBM_Char\r\n\
              ## LoadOnDemand: 0\r\n\
              DBM-Core.lua\r\n",
        );

        assert_eq!(toc.interface, Some(30300));
        assert_eq!(toc.title.as_deref(), Some("Deadly Boss Mods - Core"));
        assert_eq!(toc.version.as_deref(), Some("4.24"));
        assert_eq!(toc.dependencies, ["Ace3", "LibStub"]);
        assert_eq!(toc.optional_dependencies, ["LibSharedMedia-3.0"]);
        assert_eq!(toc.saved_variables, ["DBM_SavedOptions", "DBM_Minimap"]);
        assert_eq!(toc.saved_variables_per_character, ["DBM_Char"]);
        assert!(!toc.load_on_demand);
        assert!(!toc.targets_other_client());
    }

    #[test]
    fn required_deps_aliases_are_all_dependencies() {
        let toc = Toc::parse(b"## RequiredDeps: A\n## Dep1: B\n## Deps: C\n");

        assert_eq!(toc.dependencies, ["A", "B", "C"]);
    }

    #[test]
    fn latin1_tocs_are_decoded_instead_of_rejected() {
        let toc = Toc::parse(b"## Title: Caf\xe9\n");

        assert_eq!(toc.title.as_deref(), Some("Café"));
    }

    #[test]
    fn a_bom_does_not_hide_the_first_field() {
        let toc = Toc::parse(b"\xEF\xBB\xBF## Interface: 30300\n");

        assert_eq!(toc.interface, Some(30300));
    }

    #[test]
    fn web_pages_come_from_the_fields_addons_name_them_in() {
        let toc = Toc::parse(
            b"## X-Website: http://www.wowace.com/projects/bartender4/\r\n\
              ## URL: www.example.org\r\n\
              ## X-Repository: https://github.com/example/Addon\r\n\
              ## X-Github-Project-Homepage: |cff00ff00https://github.com/example/Addon|r\r\n\
              ## X-Website: HTTP://Example.NET/  [does not work anymore]\r\n\
              ## X-Feedback: http://forums.example.com\r\n\
              ## X-Email: someone@example.com\r\n",
        );

        assert_eq!(
            toc.websites,
            [
                "http://www.wowace.com/projects/bartender4/",
                "http://www.example.org",
                "https://github.com/example/Addon",
                "http://Example.NET/",
            ]
        );
    }

    #[test]
    fn only_web_addresses_are_web_pages() {
        for value in [
            "someone@example.com",
            "mailto:someone@example.com",
            "javascript:alert(1)",
            "file:///C:/Windows/notepad.exe",
            "ftp://example.org/addon.zip",
            "http://localhost/",
            "https://",
            "http://example..org",
            "http://user@example.org",
            "See the forums",
        ] {
            let toc = Toc::parse(format!("## X-Website: {value}\n").as_bytes());

            assert!(toc.websites.is_empty(), "{value}: {:?}", toc.websites);
        }

        let toc = Toc::parse(b"## URL: Visit (https://example.org/addon).\n");
        assert_eq!(toc.websites, ["https://example.org/addon"]);
    }

    #[test]
    fn retail_interfaces_target_another_client() {
        assert!(Toc::parse(b"## Interface: 110002\n").targets_other_client());
    }

    #[test]
    fn interfaces_name_the_game_version_they_were_made_for() {
        let named = |interface| made_for(interface).unwrap_or_default();

        assert_eq!(made_for(30300), None, "3.3.5a itself");
        assert_eq!(named(11200), "Vanilla");
        assert_eq!(named(11507), "WoW Classic");
        assert_eq!(named(20100), "The Burning Crusade");
        assert_eq!(named(20504), "Burning Crusade Classic");
        assert_eq!(named(30200), "WotLK 3.2");
        assert_eq!(named(30000), "WotLK 3.0");
        assert_eq!(named(30403), "Wrath Classic");
        assert_eq!(named(33000), "another WotLK patch", "a typo in the .toc");
        assert_eq!(named(40100), "Cataclysm");
        assert_eq!(named(41000), "Cataclysm", "a typo, not the Classic 4.4");
        assert_eq!(named(40400), "Cataclysm Classic");
        assert_eq!(named(50500), "Mists of Pandaria Classic");
        assert_eq!(named(100207), "Dragonflight");
        assert_eq!(named(110002), "The War Within");
        assert_eq!(named(120001), "Midnight");
        assert_eq!(named(130000), "a newer game version");
        assert_eq!(named(300), "an unknown game version");
    }

    /// One pair per shape of version the catalog has, the newer first.
    #[test]
    fn versions_of_the_same_shape_compare_by_their_numbers() {
        use Ordering::{Equal, Greater, Less};

        for (a, b, expected) in [
            // Dotted numbers, however many, with or without a `v`.
            ("0.0.21", "0.0.20", Greater),
            ("1.3.2", "1.2", Greater),
            ("2.0", "2.0.1", Less),
            ("0.1", "0.1.0", Equal),
            ("0.2.0.5", "0.2.0.4", Greater),
            ("1.0", "1.0", Equal),
            (" 1.0 ", "1.0", Equal),
            ("v0.6.2", "v0.6.1", Greater),
            ("v3.2.21", "3.2.20", Greater),
            ("V1.2", "v1.2", Equal),
            ("v0.8", "v0.7", Greater),
            ("v3.2.0.799", "v3.2.0.798", Greater),
            ("v.1.26", "v.1.25", Greater),
            ("v.1.26", "1.25", Greater),
            // Lone numbers: builds and revisions.
            ("263", "262", Greater),
            ("r113", "r112", Greater),
            ("r736", "r720", Greater),
            ("r26-release", "r25-release", Greater),
            ("r26", "r25-release", Greater),
            ("r280-beta", "r279-beta", Greater),
            ("r629 (Revision: 629)", "r628 (Revision: 628)", Greater),
            ("b1.9056.2", "b1.9056.1", Greater),
            ("beta4.11", "beta4.10", Greater),
            // Numbers after a dash or underscore.
            ("9.9.1-335", "9.8.0-335", Greater),
            ("2.2.6-152", "2.2.6-151", Greater),
            ("3.0-57", "3.0-56", Greater),
            ("30300-2", "30300-1", Greater),
            ("3.3.5-1.0.8", "3.3.5-1.0.7", Greater),
            ("1.0.1-3.3.5a", "1.0.0-3.3.5a", Greater),
            ("1.0.2010-09-18", "1.0.2010-09-17", Greater),
            ("3.3.5_16r-PA", "3.3.5_15r-PA", Greater),
            ("2024-01-05", "2023-12-31", Greater),
            // A patch letter, or a number after a letter.
            ("3.3f", "3.3e", Greater),
            ("11.2.5c", "11.2.5b", Greater),
            ("11.2.6", "11.2.5b", Greater),
            ("0.5b", "0.4b", Greater),
            ("v1.10.20a", "v1.10.19a", Greater),
            ("30400r11", "30400r10", Greater),
            ("2.3r163", "2.3r162", Greater),
            ("3.34c10", "3.34c9", Greater),
            ("5.0b9", "5.0b8", Greater),
            ("0.9.8p2", "0.9.8p1", Greater),
            ("11.b3", "11.b2", Greater),
            ("3x05", "3x04", Greater),
            ("3.0.1W", "3.0.0W", Greater),
            // A revision, stage or name after the numbers.
            ("1.18.2 (r232)", "1.18.2 (r231)", Greater),
            ("1.19.5 (r238)", "1.19.5 (r238)", Equal),
            ("3.0 (r226)", "3.0 (r225)", Greater),
            ("1.9 r250", "1.9 r249", Greater),
            ("v3.0.2 (r119)", "v3.0.1 (r119)", Greater),
            ("1.5.4beta (r177)", "1.5.4beta (r176)", Greater),
            ("5.13.3 (R40)", "5.13.3 (R39)", Greater),
            (
                "4.3.8.30300 - Revision 387",
                "4.3.8.30300 - Revision 386",
                Greater,
            ),
            ("3.3.5 (3.0.132)", "3.3.5 (3.0.131)", Greater),
            ("3.3.5 release 2", "3.3.5 release 1", Greater),
            ("3.1.0 Beta 2", "3.1.0 Beta 1", Greater),
            ("1.1.1 Alpha 3", "1.1.1 Alpha 2", Greater),
            ("2.16.0-beta2", "2.16.0-beta1", Greater),
            ("v1.0-beta-13", "v1.0-beta-12", Greater),
            ("1.1-Release", "1.0-Release", Greater),
            ("3.1.16 - Release", "3.1.15 - Release", Greater),
            ("1.7.9_release", "1.7.8_release", Greater),
            ("10.1.14_alpha", "10.1.13_alpha", Greater),
            ("2.9.7-beta", "2.9.6-beta", Greater),
            ("2.16-Ascension", "2.15-Ascension", Greater),
            ("v4.14.67-wrath", "v4.14.66-wrath", Greater),
            ("v2.9.3-PA", "v2.9.2-PA", Greater),
            (
                "5.9.4961 (WhackyWallaby)",
                "5.9.4960 (WhackyWallaby)",
                Greater,
            ),
            ("5.6.4425 (KangaII)", "5.6.4424 (KangaII)", Greater),
            ("5.07.22 (Lite)", "5.07.21 (Lite)", Greater),
            ("3.1.17 (<%codename%>)", "3.1.16 (<%codename%>)", Greater),
            ("v1.7.35 (MLMods.net)", "v1.7.34 (MLMods.net)", Greater),
            ("Release 1.01", "Release 1.00", Greater),
            (
                "asc-1.0.9 (v1.0.3 base)",
                "asc-1.0.8 (v1.0.3 base)",
                Greater,
            ),
            (
                "TriviaBot 2.5.5 for WoW 3.3 // WoWQuiz 2.0 (WoWQuiz500)",
                "TriviaBot 2.5.4 for WoW 3.3 // WoWQuiz 2.0 (WoWQuiz500)",
                Greater,
            ),
            // `git describe`: the commit count decides.
            ("4.4.2-15-gabcdef0", "4.4.2-12-g94f3b58", Greater),
            ("v2.1.6-11-g50f5ae7", "v2.1.6-10-g50f5ae7", Greater),
            ("v2.01-15-gfc58e9f", "v2.01-14-gfc58e9f", Greater),
            (
                "v4.0.0-beta11-32-gb39172e",
                "v4.0.0-beta11-31-gb39172e",
                Greater,
            ),
        ] {
            assert_eq!(compare_versions(a, b), Some(expected), "{a} vs {b}");
        }
    }

    #[test]
    fn versions_of_different_shapes_do_not_compare() {
        for (a, b) in [
            // Different scaffolding around the numbers.
            ("r736", "1.2"),
            ("r112", "112"),
            ("b1.9056.1", "1.9056.1"),
            ("beta4.10", "4.10"),
            ("Release 1.00", "1.00"),
            ("asc-1.0.8 (v1.0.3 base)", "1.0.8"),
            // A lone number against dotted ones.
            ("262", "1.2"),
            ("30300-1", "30300"),
            // Stages and names that can't be ordered.
            ("1.0-Release", "1.0-Beta"),
            ("2.16.0-beta1", "2.16.0"),
            ("2.9.6-beta", "2.9.6"),
            ("11.2.5b", "11.2.5"),
            ("3.3.002b", "3.3.002"),
            ("3.0.0W", "3.0.0S"),
            ("5.9.4960 (WhackyWallaby)", "5.9.4960 (KangaII)"),
            ("5.07.21 (Lite)", "5.07.21"),
            ("v3.0.1 (r119)", "3.0.1"),
            // Commits, and hashes on their own.
            ("4.4.2-12-gabcdef0", "4.4.2-12-g94f3b58"),
            ("49d3d7e", "a1b2c3d"),
            // No number at all.
            ("GIT", "1.0"),
            ("wowi:revision", "1.0"),
            ("$project-version$", "1.0"),
            ("beta", "alpha"),
            ("", "1.0"),
            ("99999999999999999999", "1"),
        ] {
            assert_eq!(compare_versions(a, b), None, "{a} vs {b}");
        }
    }

    #[test]
    fn strip_escapes_removes_colors_textures_and_links() {
        assert_eq!(strip_escapes("|cFFFF0000Red|r text"), "Red text");
        assert_eq!(strip_escapes("|TInterface\\Icons\\Spell:16|t Icon"), "Icon");
        assert_eq!(
            strip_escapes("|Hitem:1234|h[Sword]|h and ||pipes"),
            "[Sword] and |pipes"
        );
    }
}
