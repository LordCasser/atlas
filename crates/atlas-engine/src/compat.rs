//! Tests for the Precision → PrecisionTier migration adapter.
//!
//! The [`From`] impl lives in `types::structs` (both types defined there
//! to satisfy the orphan rule). This module validates the conversion.

#[cfg(test)]
mod tests {
    use types::structs::precision::PrecisionTier;
    use types::structs::{CoverageTier, Precision, SemanticConfidence, SymbolTier};

    #[test]
    fn precision_exact() {
        let p = Precision {
            coverage: CoverageTier::RepoComplete,
            confidence: SemanticConfidence::Certain,
        };
        assert_eq!(PrecisionTier::from(p), PrecisionTier::Exact);
    }

    #[test]
    fn precision_closure_complete_certain_to_exact() {
        let p = Precision {
            coverage: CoverageTier::ClosureComplete {
                closure_id: "c1".into(),
            },
            confidence: SemanticConfidence::Certain,
        };
        assert_eq!(PrecisionTier::from(p), PrecisionTier::Exact);
    }

    #[test]
    fn precision_closure_complete_high_to_partial_exact() {
        let p = Precision {
            coverage: CoverageTier::ClosureComplete {
                closure_id: "c2".into(),
            },
            confidence: SemanticConfidence::High,
        };
        assert_eq!(PrecisionTier::from(p), PrecisionTier::PartialExact);
    }

    #[test]
    fn precision_boundary_high_to_partial_exact() {
        let p = Precision {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Full,
            },
            confidence: SemanticConfidence::High,
        };
        assert_eq!(PrecisionTier::from(p), PrecisionTier::PartialExact);
    }

    #[test]
    fn precision_boundary_medium_to_degraded_structural() {
        let p = Precision {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Partial,
            },
            confidence: SemanticConfidence::Medium,
        };
        assert_eq!(PrecisionTier::from(p), PrecisionTier::DegradedStructural);
    }

    #[test]
    fn precision_partial_to_local_dataflow_only() {
        let p = Precision {
            coverage: CoverageTier::Partial { gaps: vec![] },
            confidence: SemanticConfidence::High,
        };
        assert_eq!(PrecisionTier::from(p), PrecisionTier::LocalDataflowOnly);
    }

    #[test]
    fn precision_manifest_to_manifest_only() {
        let p = Precision {
            coverage: CoverageTier::Manifest,
            confidence: SemanticConfidence::Low,
        };
        assert_eq!(PrecisionTier::from(p), PrecisionTier::ManifestOnly);
    }

    #[test]
    fn precision_boundary_low_to_degraded_structural() {
        let p = Precision {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Manifest,
            },
            confidence: SemanticConfidence::Low,
        };
        assert_eq!(PrecisionTier::from(p), PrecisionTier::DegradedStructural);
    }
}
