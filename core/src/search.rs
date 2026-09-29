//! Case-insensitive, multi-word text search.

/// A parsed search box: every word must appear in at least one field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    terms: Vec<String>,
}

impl Query {
    /// Splits `text` into lowercase search terms.
    pub fn new(text: &str) -> Self {
        Self {
            terms: text.split_whitespace().map(str::to_lowercase).collect(),
        }
    }

    /// Whether the query matches everything.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Whether every term appears in at least one of `fields`, whatever the
    /// case. This runs for every addon on every redraw, so it allocates
    /// nothing.
    pub fn matches(&self, fields: &[&str]) -> bool {
        self.terms
            .iter()
            .all(|term| fields.iter().any(|field| contains_ignore_case(field, term)))
    }
}

/// Whether `text` contains `term`, which is already lowercase, ignoring
/// the case of `text`.
fn contains_ignore_case(text: &str, term: &str) -> bool {
    text.char_indices().any(|(start, _)| {
        let mut lowered = text[start..].chars().flat_map(char::to_lowercase);

        term.chars()
            .all(|expected| lowered.next() == Some(expected))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(Query::new("   ").matches(&["anything"]));
        assert!(Query::new("").matches(&[]));
    }

    #[test]
    fn matching_ignores_case_beyond_ascii() {
        let query = Query::new("ähnlich İ");

        assert!(query.matches(&["ÄHNLICH wie GearScore, i̇"]));
        assert!(!query.matches(&["ÄHNLICH"]));
        assert!(!Query::new("nothing").matches(&[]));
    }

    #[test]
    fn terms_match_across_fields_ignoring_case() {
        let query = Query::new("boss MODS");

        assert!(query.matches(&["Deadly Boss Mods"]));
        assert!(query.matches(&["DBM", "Raid boss mods"]));
        assert!(!query.matches(&["Deadly Boss"]));
    }
}
