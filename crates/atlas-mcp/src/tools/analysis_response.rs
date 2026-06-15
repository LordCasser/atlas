//! Analysis response helpers for MCP tool responses.
//!
//! # Architecture invariant
//! - Internal focus/lazy concepts (closure_id, Precision enum variants,
//!   lazy_diagnostics internals, pending_closures, focus scheduler priorities)
//!   MUST NOT appear in MCP responses.
//! - Public coverage labels: `repo_complete`, `local_complete`, `boundary`,
//!   `partial`, `basic` — NOT `ClosureComplete` or other internal names.

use serde::Serialize;

// ---------------------------------------------------------------------------
// Precision view
// ---------------------------------------------------------------------------

/// Precision contract visible to users/agents.
#[derive(Debug, Clone, Serialize)]
pub struct PrecisionView {
    /// Public coverage label: repo_complete | local_complete | boundary | partial | basic
    pub coverage: String,
    /// Confidence level: certain | high | medium | low
    pub confidence: String,
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

use atlas_engine::structs::{CoverageTier, Precision, SemanticConfidence};

/// Convert a [`Precision`] struct to [`PrecisionView`],
/// mapping internal [`CoverageTier`] to public label.
pub fn precision_to_view(p: &Precision) -> PrecisionView {
    PrecisionView {
        coverage: coverage_tier_to_label(&p.coverage),
        confidence: confidence_to_string(p.confidence).into(),
    }
}

/// Map internal [`CoverageTier`] to public coverage label.
pub fn coverage_tier_to_label(tier: &CoverageTier) -> String {
    match tier {
        CoverageTier::RepoComplete => "repo_complete".into(),
        CoverageTier::ClosureComplete { .. } => "local_complete".into(),
        CoverageTier::Boundary { .. } => "boundary".into(),
        CoverageTier::Partial { .. } => "partial".into(),
        CoverageTier::Manifest => "basic".into(),
    }
}

/// Map internal [`SemanticConfidence`] to public string.
pub fn confidence_to_string(c: SemanticConfidence) -> &'static str {
    match c {
        SemanticConfidence::Certain => "certain",
        SemanticConfidence::High => "high",
        SemanticConfidence::Medium => "medium",
        SemanticConfidence::Low => "low",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::structs::SymbolTier;

    // ── test: CoverageTier → public label ─────────────────────────────

    #[test]
    fn test_precision_view_from_coverage_tier() {
        // ClosureComplete → local_complete
        let ct = CoverageTier::ClosureComplete {
            closure_id: "cl_42".into(),
        };
        assert_eq!(coverage_tier_to_label(&ct), "local_complete");

        // RepoComplete → repo_complete
        assert_eq!(
            coverage_tier_to_label(&CoverageTier::RepoComplete),
            "repo_complete"
        );

        // Boundary → boundary
        assert_eq!(
            coverage_tier_to_label(&CoverageTier::Boundary {
                target_tier: SymbolTier::Full
            }),
            "boundary"
        );

        // Partial → partial
        assert_eq!(
            coverage_tier_to_label(&CoverageTier::Partial { gaps: vec![] }),
            "partial"
        );

        // Manifest → basic
        assert_eq!(coverage_tier_to_label(&CoverageTier::Manifest), "basic");
    }

    // ── test: confidence_to_string ─────────────────────────────────────

    #[test]
    fn test_confidence_to_string() {
        assert_eq!(confidence_to_string(SemanticConfidence::Certain), "certain");
        assert_eq!(confidence_to_string(SemanticConfidence::High), "high");
        assert_eq!(confidence_to_string(SemanticConfidence::Medium), "medium");
        assert_eq!(confidence_to_string(SemanticConfidence::Low), "low");
    }
}
