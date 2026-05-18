//! Name-based reference matching with exact and proximity scoring.

use crate::types::*;

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
    pub fn name_similarity(&self, a: &str, b: &str) -> Confidence {
        if a == b {
            return Confidence::certain();
        }
        // Case-insensitive match
        if a.eq_ignore_ascii_case(b) {
            return Confidence::new(0.9);
        }
        // Edit-distance based: Levenshtein ratio
        let dist = crate::types::levenshtein(a, b);
        let max_len = a.len().max(b.len()).max(1) as f64;
        let similarity = 1.0 - (dist as f64 / max_len);
        Confidence::new((similarity * 0.7) as f32) // 0.7 × ratio
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
        assert_eq!(crate::types::levenshtein("kitten", "sitting"), 3);
        assert_eq!(crate::types::levenshtein("foo", "foo"), 0);
        assert_eq!(crate::types::levenshtein("foo", "bar"), 3);
        assert_eq!(crate::types::levenshtein("", "abc"), 3);
    }
}
