//! Name-based reference matching with exact and proximity scoring.

use types::*;

/// Strategy for matching reference names to symbol candidates.
#[derive(Debug, Clone)]
pub struct NameMatcher;

/// Potential match result with confidence.
#[derive(Debug, Clone)]
pub struct NameMatch {
    pub symbol_id: SymbolId,
    pub confidence: Confidence,
    pub strategy: ResolutionStrategy,
    pub provenance: Provenance,
}

impl Default for NameMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl NameMatcher {
    pub fn new() -> Self {
        Self
    }

    /// Match a reference name against a list of candidate symbols, returning
    /// the best match (if any) with confidence above a threshold.
    pub fn best_match(
        &self,
        candidates: &[SymbolDef],
        ref_name: &str,
        min_confidence: Confidence,
    ) -> Option<NameMatch> {
        let mut best: Option<NameMatch> = None;

        for sym in candidates {
            let confidence = self.name_similarity(ref_name, &sym.name);
            if confidence < min_confidence {
                continue;
            }
            match &best {
                Some(current) if current.confidence >= confidence => {}
                _ => {
                    best = Some(NameMatch {
                        symbol_id: sym.id,
                        confidence,
                        strategy: ResolutionStrategy::NameOnly,
                        provenance: Provenance::Heuristic,
                    });
                }
            }
        }

        best
    }

    /// Calculate name similarity [0.0, 1.0].
    ///
    /// Delegates to `search::compute_name_similarity` for a richer matching
    /// strategy: exact → case-insensitive → prefix → camelCase/snake_case
    /// normalization → word overlap → Levenshtein fallback.
    pub fn name_similarity(&self, a: &str, b: &str) -> Confidence {
        let query_norm = search::normalize_name_for_search(a);
        let sim = search::compute_name_similarity(a, b, &query_norm);
        Confidence::new(sim as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let matcher = NameMatcher::new();
        assert_eq!(matcher.name_similarity("foo", "foo"), Confidence::certain());
    }

    #[test]
    fn test_case_insensitive() {
        let matcher = NameMatcher::new();
        assert_eq!(matcher.name_similarity("Foo", "foo"), Confidence::new(0.9));
    }

    #[test]
    fn test_similar_names() {
        let matcher = NameMatcher::new();
        let sim = matcher.name_similarity("getUser", "get_user");
        assert!(sim.as_f32() > 0.4);
        assert!(sim.as_f32() < 0.9);
    }

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(types::levenshtein("kitten", "sitting"), 3);
        assert_eq!(types::levenshtein("foo", "foo"), 0);
        assert_eq!(types::levenshtein("foo", "bar"), 3);
        assert_eq!(types::levenshtein("", "abc"), 3);
    }
}
