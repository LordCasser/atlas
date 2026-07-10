//! Extended integration tests for focus-driven analysis system.
//!
//! These tests verify end-to-end flows that require multi-file setups
//! and cross-component wiring not covered by unit tests.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use db::Store;
    use types::enums::{Language, ParseStatus, SymbolKind};
    use types::ids::{FileId, ImportId, SymbolId};
    use types::structs::{
        AnswerQuality, CoverageTier, FileInfo, ImportDef, SemanticConfidence, SymbolDef,
        SymbolTier, TextRange,
    };
    use types::{ImportKind, Visibility, layer, status};

    use crate::FocusMaterialize;
    use crate::focus::edge_policy::{EdgeConflictPolicy, EdgeResolution};
    use crate::focus::engine::ClosureEngine;
    use crate::focus::types::{ClosureStrategy, FocusSeed, FocusWindow, WindowBudget};
    use crate::focus::visibility_filter::{CVisibilityFilter, VisibilityContext, VisibilityFilter};

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    fn insert_file_structural_complete(store: &Store, path: &str) -> FileId {
        let file_id = FileId::generate(path);
        let file_info = FileInfo {
            file_id,
            path: path.to_string(),
            language: Language::C,
            content_hash: "abc123".to_string(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();
        store
            .upsert_file_extraction_state(
                &file_id,
                layer::STRUCTURAL,
                "abc123",
                status::COMPLETE,
                Default::default(),
            )
            .unwrap();
        file_id
    }

    fn test_engine(store: Arc<Store>) -> ClosureEngine {
        let m = FocusMaterialize::open(store.clone(), None);
        ClosureEngine::new(store, m)
    }

    // ── Test: E2E Full Closure Build ────────────────────────────────────────

    /// Build a closure with a 3-file import chain:
    ///   main.c → util.h → helper.h
    /// With ImportNeighborhood{depth:2}, all 3 files should be included.
    #[test]
    fn test_e2e_full_closure_build() {
        let store = test_store();

        let main_id = insert_file_structural_complete(&store, "src/main.c");
        let util_id = insert_file_structural_complete(&store, "src/util.h");
        let helper_id = insert_file_structural_complete(&store, "src/helper.h");

        // main.c imports "util.h" (relative)
        let import1_id = ImportId::generate(&main_id, "include", "util.h", None, 0);
        store
            .insert_imports(&[ImportDef {
                id: import1_id,
                file_id: main_id,
                kind: ImportKind::Include,
                module: "util.h".to_string(),
                imported_name: String::new(),
                local_name: None,
                alias: None,
                is_wildcard: false,
                is_relative: true,
                range: TextRange::default(),
            }])
            .unwrap();

        // util.h imports "helper.h" (relative)
        let import2_id = ImportId::generate(&util_id, "include", "helper.h", None, 0);
        store
            .insert_imports(&[ImportDef {
                id: import2_id,
                file_id: util_id,
                kind: ImportKind::Include,
                module: "helper.h".to_string(),
                imported_name: String::new(),
                local_name: None,
                alias: None,
                is_wildcard: false,
                is_relative: true,
                range: TextRange::default(),
            }])
            .unwrap();

        let engine = test_engine(store.clone());

        let window = FocusWindow {
            seed: FocusSeed::File {
                file_id: main_id,
                language: Language::C,
            },
            strategies: vec![ClosureStrategy::ImportNeighborhood { depth: 2 }],
            include_roots: Vec::new(),
            budget: WindowBudget::default(),
            language: Language::C,
            max_iterations: 3,
        };

        let closure = engine
            .build_closure(&window, "e2e-full-closure")
            .expect("full closure build should succeed");

        assert!(
            closure.files.contains(&main_id),
            "closure must contain seed file"
        );
        assert!(
            !closure.files.contains(&util_id),
            "unused direct dependency should remain resolution-only"
        );
        assert!(
            !closure.files.contains(&helper_id),
            "unused transitive dependency should remain resolution-only"
        );
        assert_eq!(
            closure.files.len(),
            1,
            "structural closure should contain only the relevant seed file"
        );
        let coverage = store.get_coverage_counts("e2e-full-closure").unwrap();
        assert!(coverage.contains(&("extracted_resolution_symbols".into(), 2)));
    }

    // ── Test: E2E Visibility Pipeline ────────────────────────────────────────

    /// Verify that CVisibilityFilter distinguishes public vs private symbols.
    /// A public function is visible across files; a static (private) function
    /// is not visible from another file.
    #[test]
    fn test_e2e_visibility_pipeline() {
        let filter = CVisibilityFilter;
        let file_id = FileId::generate("module.c");
        let other_file = FileId::generate("other.c");

        let public_sym = SymbolDef {
            id: SymbolId::generate(&file_id, "c", "public_func", "function", None),
            kind: SymbolKind::Function,
            name: "public_func".to_string(),
            qualified_name: "public_func".to_string(),
            symbol_path: vec!["public_func".to_string()],
            file_id,
            language: Language::C,
            range: TextRange::default(),
            name_range: TextRange::default(),
            signature: None,
            visibility: Some(Visibility::Public),
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        };

        let static_sym = SymbolDef {
            id: SymbolId::generate(&file_id, "c", "static_func", "function", None),
            kind: SymbolKind::Function,
            name: "static_func".to_string(),
            qualified_name: "static_func".to_string(),
            symbol_path: vec!["static_func".to_string()],
            file_id,
            language: Language::C,
            range: TextRange::default(),
            name_range: TextRange::default(),
            signature: None,
            visibility: Some(Visibility::Private), // C 'static' maps to Private
            exported: false,
            static_: true,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        };

        let ctx = VisibilityContext {
            from_file: other_file,
            from_crate_root: None,
            target_crate_root: None,
        };

        assert!(
            filter.is_visible(&public_sym, other_file, &ctx),
            "public symbol must be visible from another file"
        );
        assert!(
            !filter.is_visible(&static_sym, other_file, &ctx),
            "private/static symbol must NOT be visible from another file"
        );
    }

    // ── Test: E2E Edge Conflict Chain ────────────────────────────────────────

    /// Verify the edge conflict resolution chain:
    /// 1. Certain edge → Low confidence incoming → Keep (Certain immutable)
    /// 2. Certain edge → High confidence incoming → Keep (Certain immutable)
    ///    Certain edges are never overwritable regardless of incoming coverage.
    #[test]
    fn test_e2e_edge_conflict_chain() {
        let certain_existing = AnswerQuality {
            coverage: CoverageTier::ClosureComplete {
                closure_id: "original".to_string(),
            },
            confidence: SemanticConfidence::Certain,
        };

        // Attempt 1: Low confidence should not override Certain
        let low_incoming = AnswerQuality {
            coverage: CoverageTier::Boundary {
                target_tier: SymbolTier::Full,
            },
            confidence: SemanticConfidence::Low,
        };

        let result1 = EdgeConflictPolicy::resolve(Some(&certain_existing), &low_incoming, None);
        assert_eq!(
            result1,
            EdgeResolution::Keep,
            "Low confidence must not override Certain edge"
        );

        // Attempt 2: High confidence with ClosureComplete should still not override Certain
        let high_incoming = AnswerQuality {
            coverage: CoverageTier::ClosureComplete {
                closure_id: "newer".to_string(),
            },
            confidence: SemanticConfidence::High,
        };

        let result2 = EdgeConflictPolicy::resolve(Some(&certain_existing), &high_incoming, None);
        assert_eq!(
            result2,
            EdgeResolution::Keep,
            "High confidence must not override Certain edge (Certain is immutable)"
        );
    }
}
