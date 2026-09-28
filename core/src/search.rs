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

    /// Whether every term appears in at least one of `fields`.
    pub fn matches<'a>(&self, fields: impl IntoIterator<Item = &'a str>) -> bool {
        if self.terms.is_empty() {
            return true;
        }

        let fields: Vec<String> = fields.into_iter().map(str::to_lowercase).collect();

        self.terms
            .iter()
            .all(|term| fields.iter().any(|field| field.contains(term)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(Query::new("   ").matches(["anything"]));
    }

    #[test]
    fn terms_match_across_fields_ignoring_case() {
        let query = Query::new("boss MODS");

        assert!(query.matches(["Deadly Boss Mods"]));
        assert!(query.matches(["DBM", "Raid boss mods"]));
        assert!(!query.matches(["Deadly Boss"]));
    }
}
