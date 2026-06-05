//! Multi-signal scoring for search result ranking.
//!
//! Combines multiple relevance signals:
//!   - BM25-inspired TF-IDF score from FTS5 proximity
//!   - Graph degree signal (callers + callees + references)
//!   - Name similarity (exact/fuzzy match confidence)
//!   - Qualified name bonus (query matches qualified path)
//!   - Kind bonus (class > function > variable for navigation queries)
//!   - Path relevance (matches in file/module names, test files downranked)
//!
//! Weights are configurable via [`ScoreWeights`] for long-term tuning.

use types::SymbolKind;

/// Configurable signal weights for hybrid ranking.
///
/// Default weights prioritize FTS5 (full-text) and name similarity,
/// with moderate graph centrality and kind bonuses.
#[derive(Debug, Clone)]
pub struct ScoreWeights {
    pub fts: f64,
    pub name: f64,
    pub graph: f64,
    pub qualified: f64,
    pub kind: f64,
    pub path: f64,
    pub language: f64,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            fts: 0.35,
            name: 0.25,
            graph: 0.10,
            qualified: 0.15,
            kind: 0.10,
            path: 0.05,
            language: 0.12,
        }
    }
}

/// Cumulative relevance score for a search hit.
#[derive(Debug, Clone, Default)]
pub struct SearchScore {
    /// Raw FTS5 match score (normalized 0..1).
    pub fts_score: f64,
    /// Graph centrality score (degree-based, normalized 0..1).
    pub graph_score: f64,
    /// Name similarity bonus (exact → 1.0, fuzzy → 0.0..0.9).
    pub name_score: f64,
    /// Qualified name bonus (query appears in qualified path).
    pub qualified_bonus: f64,
    /// Kind bonus for navigation (class > function > variable).
    pub kind_bonus: f64,
    /// Path relevance (query appears in file path, test files downranked).
    pub path_bonus: f64,
    /// Language preference bonus (project/scope primary language).
    pub language_bonus: f64,
    /// Weighted total.
    pub total: f64,
}

impl SearchScore {
    /// Create a score from raw signals and apply configurable weights.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fts_score: f64,
        total_degree: usize,
        max_degree: usize,
        name_similarity: f64,
        qualified_match: bool,
        kind: SymbolKind,
        file_path: Option<&str>,
        weights: &ScoreWeights,
    ) -> Self {
        let graph_score = if max_degree > 0 {
            (total_degree as f64 / max_degree.max(1) as f64).min(1.0)
        } else {
            0.0
        };
        let qualified_bonus = if qualified_match { 1.0 } else { 0.0 };
        let kind_bonus = kind_weight(kind);
        let path_bonus = file_path.map_or(0.5, |p| if is_test_file(p) { 0.2 } else { 0.5 });

        let total = fts_score * weights.fts
            + graph_score * weights.graph
            + name_similarity * weights.name
            + qualified_bonus * weights.qualified
            + kind_bonus * weights.kind
            + path_bonus * weights.path;

        Self {
            fts_score,
            graph_score,
            name_score: name_similarity,
            qualified_bonus,
            kind_bonus,
            path_bonus,
            language_bonus: 0.0,
            total,
        }
    }

    /// Apply a project/scope language preference as a soft ranking boost.
    pub fn with_language_preference(mut self, preferred: bool, weights: &ScoreWeights) -> Self {
        self.language_bonus = language_preference_bonus(preferred);
        self.total += self.language_bonus * weights.language;
        self
    }
}

/// Soft language affinity score used when no explicit language filter is set.
pub fn language_preference_bonus(preferred: bool) -> f64 {
    if preferred { 1.0 } else { 0.0 }
}

/// Return a heuristic kind weight for search relevance.
fn kind_weight(kind: SymbolKind) -> f64 {
    match kind {
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Trait => 0.8,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => 0.6,
        SymbolKind::Struct | SymbolKind::Enum | SymbolKind::TypeAlias => 0.5,
        SymbolKind::Module | SymbolKind::Namespace | SymbolKind::Package => 0.4,
        SymbolKind::Variable | SymbolKind::Field | SymbolKind::Property => 0.3,
        SymbolKind::Constant | SymbolKind::EnumMember => 0.25,
        SymbolKind::Parameter | SymbolKind::Macro | SymbolKind::Decorator => 0.15,
        _ => 0.1,
    }
}

/// Check if a file path looks like a test file.
fn is_test_file(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains("_test.")
        || p.contains(".test.")
        || p.contains("__test__")
        || p.contains("/test/")
        || p.contains("\\test\\")
}

/// BM25-inspired inverse document frequency.
/// N = total symbol count, n = number of symbols matching the term.
pub fn idf_weight(total_symbols: usize, matching_symbols: usize) -> f64 {
    if matching_symbols == 0 || total_symbols == 0 {
        return 0.0;
    }
    let n = matching_symbols as f64;
    let n_total = total_symbols as f64;
    // BM25 saturation: terms that appear everywhere get low IDF
    ((n_total - n + 0.5) / (n + 0.5) + 1.0).ln()
}

/// Normalize a raw FTS5 match_info score into 0..1 range.
/// FTS5 match_info returns bytes-like scores; higher = better match.
pub fn normalize_fts_score(raw_score: f64, max_score: f64) -> f64 {
    if max_score <= 0.0 {
        return 0.0;
    }
    (raw_score / max_score).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idf_common_term() {
        // A term that appears in all documents → low IDF
        let idf = idf_weight(1000, 1000);
        assert!(idf < 0.5, "expected low IDF for ubiquitous term, got {idf}");
    }

    #[test]
    fn test_idf_rare_term() {
        // A term that appears in few documents → high IDF
        let idf = idf_weight(1000, 1);
        assert!(idf > 5.0, "expected high IDF for rare term, got {idf}");
    }

    #[test]
    fn test_normalize_fts_score() {
        assert_eq!(normalize_fts_score(50.0, 100.0), 0.5);
        assert_eq!(normalize_fts_score(0.0, 100.0), 0.0);
        assert_eq!(normalize_fts_score(200.0, 100.0), 1.0); // clamped
    }

    #[test]
    fn test_kind_weight_class() {
        let score = SearchScore::new(
            0.5,
            10,
            50,
            0.8,
            true,
            SymbolKind::Class,
            None,
            &ScoreWeights::default(),
        );
        assert!(score.total > 0.0);
        assert!(score.kind_bonus > 0.5);
    }

    #[test]
    fn test_kind_weight_parameter() {
        let score = SearchScore::new(
            0.5,
            10,
            50,
            0.8,
            false,
            SymbolKind::Parameter,
            None,
            &ScoreWeights::default(),
        );
        assert!(score.kind_bonus < 0.2);
    }

    #[test]
    fn test_qualified_bonus_boosts_match() {
        let a = SearchScore::new(
            0.5,
            10,
            50,
            0.8,
            true,
            SymbolKind::Function,
            None,
            &ScoreWeights::default(),
        );
        let b = SearchScore::new(
            0.5,
            10,
            50,
            0.8,
            false,
            SymbolKind::Function,
            None,
            &ScoreWeights::default(),
        );
        assert!(a.total > b.total, "qualified match should increase score");
    }

    #[test]
    fn test_test_file_downranked() {
        let prod = SearchScore::new(
            0.5,
            10,
            50,
            0.8,
            false,
            SymbolKind::Function,
            Some("src/main.rs"),
            &ScoreWeights::default(),
        );
        let test_file = SearchScore::new(
            0.5,
            10,
            50,
            0.8,
            false,
            SymbolKind::Function,
            Some("src/main_test.rs"),
            &ScoreWeights::default(),
        );
        assert!(
            prod.path_bonus > test_file.path_bonus,
            "test files should have lower path bonus"
        );
    }
}
