use types::structs::{CoverageTier, Precision, SemanticConfidence, SymbolTier};

/// Compute the precision for structural lazy extraction.
pub fn structural_precision(built: usize, cached: usize, budget_exceeded: bool) -> Precision {
    if built == 0 && cached == 0 {
        if budget_exceeded {
            // budget exceeded before any file was built
            Precision::worst()
        } else {
            Precision::worst()
        }
    } else if budget_exceeded {
        if built == 0 {
            // budget exceeded before any file was built (but some were cached)
            Precision::worst()
        } else {
            // some files built, but budget exceeded mid-build
            Precision {
                coverage: CoverageTier::Boundary {
                    target_tier: SymbolTier::Full,
                },
                confidence: SemanticConfidence::Medium,
            }
        }
    } else {
        Precision::best()
    }
}

/// Suggested next action based on structural precision.
pub fn next_action_structural(precision: &Precision) -> Option<&'static str> {
    if precision.is_exact() {
        None
    } else if precision.coverage == CoverageTier::Manifest {
        if precision.confidence == SemanticConfidence::Low {
            Some("no structural data — run 'atlas index --analysis structural'")
        } else {
            Some("manifest-only — run 'atlas index' for full structural data")
        }
    } else if matches!(precision.coverage, CoverageTier::Partial { .. }) {
        Some(
            "partial — cross-file references not resolved; run 'atlas index' for full structural data",
        )
    } else if matches!(precision.coverage, CoverageTier::Boundary { .. }) {
        if precision.confidence == SemanticConfidence::High {
            Some("structural complete, dataflow truncated — increase budget or narrow scope")
        } else {
            Some("budget exceeded — increase LAZY_STRUCTURAL_BUDGET_MS or reduce scope")
        }
    } else {
        // ClosureComplete or other repo-complete-like state
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::structs::KnownGap;

    #[test]
    fn next_action_covers_all_states() {
        // Exact → None (no action needed)
        assert_eq!(next_action_structural(&Precision::best()), None);
        // Worst (Manifest + Low) → Some action
        assert!(next_action_structural(&Precision::worst()).is_some());
        // Manifest + Medium → Some action
        assert!(next_action_structural(&Precision::manifest(SemanticConfidence::Medium)).is_some());
        // Partial coverage → Some action
        let partial = Precision::partial(vec![], SemanticConfidence::Medium);
        assert!(next_action_structural(&partial).is_some());
        // Boundary coverage → Some action
        let boundary = Precision {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Full,
            },
            confidence: SemanticConfidence::Medium,
        };
        assert!(next_action_structural(&boundary).is_some());
    }

    #[test]
    fn next_action_unavailable() {
        let hint = next_action_structural(&Precision::worst()).unwrap();
        assert!(hint.contains("no structural data"));
    }

    #[test]
    fn next_action_manifest_medium() {
        let hint =
            next_action_structural(&Precision::manifest(SemanticConfidence::Medium)).unwrap();
        assert!(hint.contains("manifest-only"));
    }

    #[test]
    fn next_action_partial() {
        let partial = Precision::partial(
            vec![KnownGap::UnresolvedImport {
                from: "a".into(),
                import_path: "b".into(),
            }],
            SemanticConfidence::Medium,
        );
        let hint = next_action_structural(&partial).unwrap();
        assert!(hint.contains("cross-file references not resolved"));
    }

    #[test]
    fn next_action_boundary_degraded() {
        let boundary = Precision {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Full,
            },
            confidence: SemanticConfidence::Medium,
        };
        let hint = next_action_structural(&boundary).unwrap();
        assert!(hint.contains("budget exceeded"));
    }

    #[test]
    fn next_action_boundary_partial_exact() {
        let boundary = Precision {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Full,
            },
            confidence: SemanticConfidence::High,
        };
        let hint = next_action_structural(&boundary).unwrap();
        assert!(hint.contains("dataflow truncated"));
    }

    // ── Precision convenience methods ───────────────────────────────────

    #[test]
    fn precision_best_is_repo_complete_certain() {
        let p = Precision::best();
        assert!(matches!(p.coverage, CoverageTier::RepoComplete));
        assert_eq!(p.confidence, SemanticConfidence::Certain);
        assert!(p.is_exact());
    }

    #[test]
    fn precision_worst_is_manifest_low() {
        let p = Precision::worst();
        assert!(matches!(p.coverage, CoverageTier::Manifest));
        assert_eq!(p.confidence, SemanticConfidence::Low);
        assert!(p.is_unavailable());
    }

    #[test]
    fn precision_is_unavailable_returns_false_for_best() {
        assert!(!Precision::best().is_unavailable());
    }

    #[test]
    fn precision_is_exact_returns_false_for_worst() {
        assert!(!Precision::worst().is_exact());
    }

    #[test]
    fn structural_precision_full_built_is_best() {
        let p = structural_precision(5, 0, false);
        assert!(p.is_exact());
    }

    #[test]
    fn dataflow_precision_partial_budget_exceeded() {
        let p = types::structs::dataflow_precision(3, 5, true);
        assert!(matches!(p.coverage, CoverageTier::Boundary { .. }));
        assert_eq!(p.confidence, SemanticConfidence::High);
    }
}
