//! Multi-signal scoring for search result ranking.
//!
//! Combines multiple relevance signals:
//!   - BM25-inspired TF-IDF score from FTS5 proximity
//!   - Graph degree signal (callers + callees + references)
//!   - Path relevance (matches in file/module names)
//!   - Kind bonus (class > function > variable for navigation queries)

use atlas_types::SymbolKind;

/// Cumulative relevance score for a search hit.
#[derive(Debug, Clone, Default)]
pub struct SearchScore {
    /// Raw FTS5 match score (normalized 0..1).
    pub fts_score: f64,
    /// Graph centrality score (degree-based, normalized 0..1).
    pub graph_score: f64,
    /// Name similarity bonus (exact → 1.0, fuzzy → 0.0..0.9).
    pub name_score: f64,
    /// Kind bonus for navigation (class > function > variable).
    pub kind_bonus: f64,
    /// Path relevance (query appears in file/module path).
    pub path_bonus: f64,
    /// Weighted total.
    pub total: f64,
}

impl SearchScore {
    /// Create a score from raw signals and apply weights.
    pub fn new(
        fts_score: f64,
        total_degree: usize,
        max_degree: usize,
        name_similarity: f64,
        kind: SymbolKind,
        path_match: bool,
    ) -> Self {
        let graph_score = if max_degree > 0 {
            (total_degree as f64 / max_degree.max(1) as f64).min(1.0)
        } else {
            0.0
        };
        let kind_bonus = kind_weight(kind);
        let path_bonus = if path_match { 0.15 } else { 0.0 };

        // Weighted combination with tunable coefficients
        let total = fts_score * 0.40
            + graph_score * 0.20
            + name_similarity * 0.25
            + kind_bonus * 0.10
            + path_bonus;

        Self {
            fts_score,
            graph_score,
            name_score: name_similarity,
            kind_bonus,
            path_bonus,
            total,
        }
    }
}

/// Return a heuristic kind weight for search relevance.
/// Classes/structs rank higher for navigation queries; functions for API queries.
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
        let score = SearchScore::new(0.5, 10, 50, 0.8, SymbolKind::Class, true);
        assert!(score.total > 0.0);
        assert!(score.total <= 1.0);
        assert!(score.kind_bonus > 0.5);
    }

    #[test]
    fn test_kind_weight_parameter() {
        let score = SearchScore::new(0.5, 10, 50, 0.8, SymbolKind::Parameter, false);
        assert!(score.kind_bonus < 0.2);
    }
}
