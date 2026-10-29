//! The languages a 3.3.5a client comes in: those `Wow.exe` (build 12340)
//! knows, each with a folder of its own in `Data`. Read whatever their
//! case, as the game finds their folders (the HD client's are lowercase),
//! and written as the game spells them (`enUS`).

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A language of the game, by its code. Sorted as their codes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Locale {
    /// German (`deDE`).
    DeDe,
    /// English, China's (`enCN`).
    EnCn,
    /// English, Europe's (`enGB`).
    EnGb,
    /// English, Taiwan's (`enTW`).
    EnTw,
    /// English, the Americas' (`enUS`).
    EnUs,
    /// Spanish, Europe's (`esES`).
    EsEs,
    /// Spanish, Latin America's (`esMX`).
    EsMx,
    /// French (`frFR`).
    FrFr,
    /// Korean (`koKR`).
    KoKr,
    /// Russian (`ruRU`).
    RuRu,
    /// Chinese, simplified (`zhCN`).
    ZhCn,
    /// Chinese, traditional (`zhTW`).
    ZhTw,
}

impl Locale {
    /// Every one, in the order `Wow.exe` looks through them for one
    /// installed (its own table).
    pub const GAME_ORDER: [Self; 12] = [
        Self::DeDe,
        Self::EnGb,
        Self::EnUs,
        Self::EsEs,
        Self::FrFr,
        Self::KoKr,
        Self::ZhCn,
        Self::ZhTw,
        Self::EnCn,
        Self::EnTw,
        Self::EsMx,
        Self::RuRu,
    ];

    /// Its code, as the game spells it: `enUS`.
    pub const fn code(self) -> &'static str {
        match self {
            Self::DeDe => "deDE",
            Self::EnCn => "enCN",
            Self::EnGb => "enGB",
            Self::EnTw => "enTW",
            Self::EnUs => "enUS",
            Self::EsEs => "esES",
            Self::EsMx => "esMX",
            Self::FrFr => "frFR",
            Self::KoKr => "koKR",
            Self::RuRu => "ruRU",
            Self::ZhCn => "zhCN",
            Self::ZhTw => "zhTW",
        }
    }

    /// The language `code` names, whatever its case: a setting's value, or a
    /// folder's name.
    pub fn parse(code: &str) -> Option<Self> {
        Self::GAME_ORDER
            .into_iter()
            .find(|locale| locale.code().eq_ignore_ascii_case(code))
    }

    /// Its data's folder in the client folder at `root`, as spelled there
    /// (see [`crate::installation::child`]).
    pub fn folder(self, root: &Path) -> std::path::PathBuf {
        use crate::installation::child;

        child(&child(root, "Data"), self.code())
    }

    /// Whether the client at `root` has its data as the game wants it: its
    /// folder, with `locale-<code>.MPQ` in it.
    pub fn installed(self, root: &Path) -> bool {
        crate::installation::child(&self.folder(root), &format!("locale-{self}.MPQ")).is_file()
    }

    /// The codes of `locales`, between commas (`deDE, enUS`), for people:
    /// `none` when there are none.
    pub fn codes(locales: &[Self]) -> String {
        if locales.is_empty() {
            return "none".to_owned();
        }

        locales
            .iter()
            .map(|locale| locale.code())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl Serialize for Locale {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for Locale {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;

        Self::parse(&code)
            .ok_or_else(|| serde::de::Error::custom(format!("{code} is no language of the game")))
    }
}

/// Reading what files hold of languages without refusing them whole for
/// one the game doesn't know, which is left out.
pub mod lenient {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer};

    use super::Locale;

    /// Languages, but those the game doesn't know.
    pub fn list<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Locale>, D::Error> {
        let codes = Vec::<String>::deserialize(deserializer)?;

        Ok(codes
            .iter()
            .filter_map(|code| Locale::parse(code))
            .collect())
    }

    /// A language, or none when the game doesn't know it.
    pub fn option<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Locale>, D::Error> {
        let code = Option::<String>::deserialize(deserializer)?;

        Ok(code.as_deref().and_then(Locale::parse))
    }

    /// What is kept by language, but for languages the game doesn't know.
    pub fn map<'de, D: Deserializer<'de>, V: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<Locale, V>, D::Error> {
        let by_code = BTreeMap::<String, V>::deserialize(deserializer)?;

        Ok(by_code
            .into_iter()
            .filter_map(|(code, value)| Some((Locale::parse(&code)?, value)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_language_is_read_whatever_its_case_and_written_as_the_game_spells_it() {
        assert_eq!(Locale::parse("enUS"), Some(Locale::EnUs));
        assert_eq!(Locale::parse("enus"), Some(Locale::EnUs), "the HD client's");
        assert_eq!(Locale::parse("ENUS"), Some(Locale::EnUs));
        assert_eq!(Locale::parse("itIT"), None, "no 3.3.5a client's");
        assert_eq!(Locale::parse("Data"), None);
        assert_eq!(Locale::EsMx.to_string(), "esMX");

        for locale in Locale::GAME_ORDER {
            assert_eq!(Locale::parse(locale.code()), Some(locale));
        }
        let mut sorted = Locale::GAME_ORDER;
        sorted.sort();
        assert!(
            sorted
                .windows(2)
                .all(|pair| pair[0].code() < pair[1].code()),
            "sorted by code"
        );

        assert_eq!(serde_json::to_string(&Locale::ZhTw).unwrap(), "\"zhTW\"");
        assert_eq!(
            serde_json::from_str::<Locale>("\"frfr\"").unwrap(),
            Locale::FrFr
        );
        assert!(serde_json::from_str::<Locale>("\"plPL\"").is_err());
    }
}
