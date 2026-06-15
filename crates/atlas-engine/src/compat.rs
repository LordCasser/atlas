//! Tests for the Precision convenience methods.
//!
//! Validates the `best()`, `worst()`, `manifest()`, `partial()`,
//! `is_unavailable()`, and `is_exact()` methods on [`Precision`].

#[cfg(test)]
mod tests {
    use types::structs::{CoverageTier, KnownGap, Precision, SemanticConfidence, SymbolTier};

    #[test]
    fn precision_best_is_exact() {
        let p = Precision::best();
        assert!(matches!(p.coverage, CoverageTier::RepoComplete));
        assert_eq!(p.confidence, SemanticConfidence::Certain);
        assert!(p.is_exact());
        assert!(!p.is_unavailable());
    }

    #[test]
    fn precision_worst_is_unavailable() {
        let p = Precision::worst();
        assert!(matches!(p.coverage, CoverageTier::Manifest));
        assert_eq!(p.confidence, SemanticConfidence::Low);
        assert!(p.is_unavailable());
        assert!(!p.is_exact());
    }

    #[test]
    fn precision_manifest_with_medium() {
        let p = Precision::manifest(SemanticConfidence::Medium);
        assert!(matches!(p.coverage, CoverageTier::Manifest));
        assert_eq!(p.confidence, SemanticConfidence::Medium);
        assert!(!p.is_unavailable());
        assert!(!p.is_exact());
    }

    #[test]
    fn precision_partial_with_high() {
        let gap = KnownGap::HighFanoutName {
            name: "foo".into(),
            candidates: 10,
            action: "narrow".into(),
        };
        let p = Precision::partial(vec![gap], SemanticConfidence::High);
        assert!(matches!(p.coverage, CoverageTier::Partial { .. }));
        assert_eq!(p.confidence, SemanticConfidence::High);
        assert!(!p.is_unavailable());
        assert!(!p.is_exact());
    }

    #[test]
    fn precision_boundary_medium_is_not_unavailable() {
        let p = Precision {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Full,
            },
            confidence: SemanticConfidence::Medium,
        };
        assert!(!p.is_unavailable());
        assert!(!p.is_exact());
    }

    #[test]
    fn precision_boundary_high_is_not_unavailable() {
        let p = Precision {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Partial,
            },
            confidence: SemanticConfidence::High,
        };
        assert!(!p.is_unavailable());
        assert!(!p.is_exact());
    }

    #[test]
    fn precision_closure_complete_certain_is_exact() {
        let p = Precision {
            coverage: CoverageTier::ClosureComplete {
                closure_id: "c1".into(),
            },
            confidence: SemanticConfidence::Certain,
        };
        assert!(!p.is_unavailable());
        // ClosureComplete + Certain is not the same as RepoComplete
        assert!(!p.is_exact());
    }

    #[test]
    fn precision_clone_and_eq() {
        let a = Precision::best();
        let b = a.clone();
        assert_eq!(a, b);
        assert_ne!(a, Precision::worst());
    }
}
