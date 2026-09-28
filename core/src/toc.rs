//! Addon `.toc` metadata files.

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
    fn retail_interfaces_target_another_client() {
        assert!(Toc::parse(b"## Interface: 110002\n").targets_other_client());
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
