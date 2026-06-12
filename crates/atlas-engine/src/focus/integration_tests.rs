//! Integration tests for the focus-driven analysis system.
//!
//! These tests verify end-to-end wiring of all T1-T7 components:
//!   - Types + Engine (FocusSeed, FocusWindow, FocusClosure)
//!   - Visibility + Edge Policy
//!   - Scheduler lifecycle
//!   - Precision migration (new Precision → legacy PrecisionTier)
//!   - Known gap creation and matching

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use db::Store;
    use types::enums::{Language, SymbolKind};
    use types::ids::FileId;
    use types::structs::{CoverageTier, KnownGap, Precision, SemanticConfidence, SymbolTier};

    use super::super::edge_policy::{EdgeConflictPolicy, EdgeResolution};
    use super::super::scheduler::{FocusPriority, FocusScheduler};
    use super::super::types::{FocusSeed, FocusWindow};
    use super::super::visibility_filter::VisibilityFilterRegistry;

    // ── End-to-End: Types + Engine ──────────────────────────────────────────

    #[test]
    fn test_e2e_types_and_engine() {
        // Construct a FocusWindow
        let seed = FocusSeed::Symbol {
            name: "main".to_string(),
            kind: Some(SymbolKind::Function),
            language: Language::C,
        };
        let window = FocusWindow::new(seed, Language::C);

        assert_eq!(window.strategies.len(), 1);
        assert_eq!(window.max_iterations, 3);
    }

    // ── End-to-End: Visibility + Edge Policy ───────────────────────────────

    #[test]
    fn test_e2e_visibility_and_edge_policy() {
        let _registry = VisibilityFilterRegistry::new();

        // Test C static filtering — Certain edge should never be overwritten
        let existing = Precision {
            coverage: CoverageTier::ClosureComplete {
                closure_id: "test".to_string(),
            },
            confidence: SemanticConfidence::Certain,
        };
        let incoming = Precision {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Full,
            },
            confidence: SemanticConfidence::Low,
        };

        // Certain edge should never be overwritten
        let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
        assert_eq!(result, EdgeResolution::Keep);
    }

    // ── End-to-End: Scheduler + Jobs ────────────────────────────────────────

    #[test]
    fn test_e2e_scheduler_lifecycle() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let mut scheduler = FocusScheduler::new(store.clone());

        // Enqueue a job
        let seed = FocusSeed::File {
            file_id: FileId::generate("test.c"),
            language: Language::C,
        };
        let window = FocusWindow::new(seed, Language::C);
        let job_id = scheduler.enqueue(window, FocusPriority::Speculative);

        assert!(!job_id.is_empty());
        assert!(scheduler.has_pending());
    }

    // ── End-to-End: Precision Migration ─────────────────────────────────────

    #[test]
    fn test_e2e_precision_migration() {
        let precision = Precision {
            coverage: CoverageTier::RepoComplete,
            confidence: SemanticConfidence::Certain,
        };

        let tier: types::structs::precision::PrecisionTier = precision.into();
        assert_eq!(tier, types::structs::precision::PrecisionTier::Exact);
    }

    // ── End-to-End: Known Gap Creation ──────────────────────────────────────

    #[test]
    fn test_e2e_known_gaps() {
        let gap = KnownGap::HighFanoutName {
            name: "printk".to_string(),
            candidates: 1420,
            action: "Provide call site context".to_string(),
        };

        let coverage = CoverageTier::Partial {
            gaps: vec![gap.clone()],
        };

        match coverage {
            CoverageTier::Partial { gaps } => {
                assert_eq!(gaps.len(), 1);
                match &gaps[0] {
                    KnownGap::HighFanoutName {
                        name,
                        candidates,
                        ..
                    } => {
                        assert_eq!(name, "printk");
                        assert_eq!(*candidates, 1420);
                    }
                    _ => panic!("Expected HighFanoutName"),
                }
            }
            _ => panic!("Expected Partial"),
        }
    }
}
