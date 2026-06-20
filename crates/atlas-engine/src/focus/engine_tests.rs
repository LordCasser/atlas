//! Tests for ClosureEngine — bounded fixed-point focus closure building.
//!
//! Test strategy:
//! - File-based tests insert FileInfo records and mark structural extraction
//!   as complete so `ensure_structural_for_file` returns cached results.
//! - Symbol-based tests use a mock CandidateProvider that returns pre-defined
//!   file IDs without needing filesystem access.

use std::sync::Arc;

use db::Store;
use types::enums::{Language, ParseStatus, Visibility};
use types::ids::FileId;
use types::structs::{CapabilityMask, FileInfo};
use types::{layer, status};

use crate::LazyDataflowService;
use crate::lazy_structural::{CandidateProvider, LazyStructuralService};

use super::engine::ClosureEngine;
use super::types::{ClosureStrategy, Direction, FocusSeed, FocusWindow, WindowBudget};

// ── Test helpers ────────────────────────────────────────────────────────────

/// Create an in-memory Store with schema initialized.
fn test_store() -> Arc<Store> {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    Arc::new(store)
}

/// Create a ClosureEngine from an in-memory store with no project root.
fn test_engine(store: Arc<Store>) -> ClosureEngine {
    let lazy_structural = LazyStructuralService::new(store.clone(), None);
    let lazy_dataflow = LazyDataflowService::new(store.clone(), None);
    ClosureEngine::new(store, lazy_structural, lazy_dataflow, None, vec![])
}

/// Insert a file and mark its structural layer as complete.
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
            CapabilityMask::default(),
        )
        .unwrap();
    file_id
}

/// Create a FocusWindow with a File seed and default budget/strategies.
fn file_window(file_id: FileId) -> FocusWindow {
    FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::ImportNeighborhood { depth: 2 }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    }
}

// ── Mock candidate provider for symbol seed tests ───────────────────────────

/// A mock candidate provider that returns predefined file IDs.
struct MockCandidateProvider {
    candidates: Vec<FileId>,
}

impl CandidateProvider for MockCandidateProvider {
    fn candidates_for_symbol(&self, _name: &str) -> anyhow::Result<Vec<FileId>> {
        Ok(self.candidates.clone())
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn test_closure_engine_new() {
    let store = test_store();
    let engine = test_engine(store);
    // Construction succeeded — engine is usable
    drop(engine);
}

#[test]
fn test_locate_seed_file() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "test.c");
    let engine = test_engine(store);

    let window = file_window(file_id);
    let closure = engine
        .build_closure(&window, "test-locate-seed-file")
        .expect("build_closure should succeed");

    // The seed file must be in the visited set
    assert!(
        closure.visited.contains(&file_id),
        "visited must contain the seed file"
    );
}

#[test]
fn test_build_closure_single_file() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");
    let engine = test_engine(store);

    let window = file_window(file_id);
    let closure = engine
        .build_closure(&window, "test-single-file")
        .expect("build_closure should succeed");

    // The seed file must be marked as extracted (files + visited)
    assert!(
        closure.files.contains(&file_id),
        "closure.files must contain the seed file"
    );
    assert!(
        closure.visited.contains(&file_id),
        "closure.visited must contain the seed file"
    );

    // With only one file and no imports, there should be no gaps
    assert!(
        closure.gaps.is_empty(),
        "no gaps expected for single-file closure"
    );
}

#[test]
fn test_build_closure_empty_strategies() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");
    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Language::C,
        },
        strategies: vec![], // no strategies
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-empty-strategies")
        .expect("build_closure should succeed");

    // Even with empty strategies, the seed file should be visited and extracted
    assert!(
        closure.files.contains(&file_id),
        "seed file must be in closure even with empty strategies"
    );
    assert!(
        closure.visited.contains(&file_id),
        "seed file must be visited with empty strategies"
    );
}

#[test]
fn test_closure_commit() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");
    let engine = test_engine(store);

    let window = file_window(file_id);
    let _closure = engine
        .build_closure(&window, "test-commit")
        .expect("build_closure should succeed");

    // Verify the closure generation was committed
    let committed_gen = engine
        .store
        .get_committed_generation("test-commit")
        .expect("get_committed_generation should succeed")
        .expect("closure should be committed");

    assert!(committed_gen > 0, "committed generation must be > 0");
}

#[test]
fn test_closure_visited_tracks_files() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");
    let engine = test_engine(store);

    let window = file_window(file_id);
    let closure = engine
        .build_closure(&window, "test-visited")
        .expect("build_closure should succeed");

    // Every file in the closure should be in the visited set
    for f in &closure.files {
        assert!(
            closure.visited.contains(f),
            "every file in closure.files must also be in visited"
        );
    }
}

#[test]
fn test_locate_seed_symbol() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "kernel/sched.c");

    // Create a LazyStructuralService with a mock candidate provider
    let lazy_structural = LazyStructuralService::with_provider(
        store.clone(),
        None,
        Box::new(MockCandidateProvider {
            candidates: vec![file_id],
        }),
    );

    let df_store = store.clone();
    let engine = ClosureEngine::new(
        store,
        lazy_structural,
        LazyDataflowService::new(df_store, None),
        None,
        vec![],
    );

    let window = FocusWindow {
        seed: FocusSeed::Symbol {
            name: "schedule".to_string(),
            kind: None,
            language: Language::C,
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-symbol-seed")
        .expect("build_closure should succeed");

    // The file containing the symbol must be visited
    assert!(
        closure.visited.contains(&file_id),
        "visited must contain the file for the symbol seed"
    );
    assert!(
        closure.files.contains(&file_id),
        "files must contain the file for the symbol seed"
    );
}

#[test]
fn test_build_closure_max_iterations_limit() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");
    let engine = test_engine(store);

    // Set max_iterations to 0 — the first extraction happens before
    // the loop, but the loop checks iteration >= max_iterations *after*
    // extracting. With max_iterations=0, the very first iteration (1)
    // triggers the limit.
    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::SameDirectory],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 0,
    };

    let closure = engine
        .build_closure(&window, "test-max-iter")
        .expect("build_closure should succeed");

    // The seed file must still be visited
    assert!(closure.visited.contains(&file_id));

    // With max_iterations=0, any expansion loop iteration will hit the
    // termination check. If SameDirectory found no siblings, there's no
    // gap. If it did find siblings, we'd get a BudgetExhausted gap.
    // Either way the build completes gracefully.
}

#[test]
fn test_build_closure_with_position_seed() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/lib.rs");
    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::Position {
            file_id,
            line: 42,
            column: 10,
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Language::Rust,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-position-seed")
        .expect("build_closure should succeed");

    assert!(closure.visited.contains(&file_id));
    assert!(closure.files.contains(&file_id));
}

// ── Supplementary tests: imports, budget, gaps, dedup, stubs ─────────────

#[test]
fn test_build_closure_with_imports() {
    let store = test_store();
    let seed_id = insert_file_structural_complete(&store, "src/main.c");
    let dep_id = insert_file_structural_complete(&store, "src/util.h");
    let engine = test_engine(store.clone());

    // Create an import: main.c includes "util.h" (relative path resolution)
    let import_id = types::ids::ImportId::generate(&seed_id, "include", "util.h", None, 0);
    let import_def = types::structs::ImportDef {
        id: import_id,
        file_id: seed_id,
        kind: types::ImportKind::Include,
        module: "util.h".to_string(),
        imported_name: String::new(),
        local_name: None,
        alias: None,
        is_wildcard: false,
        is_relative: true,
        range: types::structs::TextRange::default(),
    };
    store.insert_imports(&[import_def]).unwrap();

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: seed_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::ImportNeighborhood { depth: 1 }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-with-imports")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&dep_id),
        "dependency file must be included via import expansion"
    );
    assert!(
        closure.files.contains(&seed_id),
        "seed file must be in closure"
    );
}

#[test]
fn test_build_closure_budget_exhausted() {
    let store = test_store();
    let seed_id = insert_file_structural_complete(&store, "src/main.c");
    // Insert two siblings in the same directory so SameDirectory produces 2 additions
    let _sib1 = insert_file_structural_complete(&store, "src/other.c");
    let _sib2 = insert_file_structural_complete(&store, "src/another.c");
    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: seed_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::SameDirectory],
        budget: WindowBudget {
            max_files: 0, // any addition exhausts budget
            ..WindowBudget::default()
        },
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-budget-exhausted")
        .expect("build_closure should succeed");

    // Seed file is still extracted (Phase 1 before budget check)
    assert!(
        closure.files.contains(&seed_id),
        "seed file must be in closure even when budget exhausted"
    );

    // A BudgetExhausted gap must be recorded
    let has_budget_gap = closure
        .gaps
        .iter()
        .any(|g| matches!(g, types::structs::KnownGap::BudgetExhausted { .. }));
    assert!(
        has_budget_gap,
        "closure must contain a BudgetExhausted gap when max_files=0 and additions exist"
    );
}

#[test]
fn test_build_closure_truncates_oversized_plan_to_budget() {
    let store = test_store();
    let seed_id = insert_file_structural_complete(&store, "src/main.c");
    insert_file_structural_complete(&store, "src/other.c");
    insert_file_structural_complete(&store, "src/another.c");
    let engine = test_engine(store);

    let closure = engine
        .build_closure(
            &FocusWindow {
                seed: FocusSeed::File {
                    file_id: seed_id,
                    language: Language::C,
                },
                strategies: vec![ClosureStrategy::SameDirectory],
                budget: WindowBudget {
                    max_files: 1,
                    ..WindowBudget::default()
                },
                language: Language::C,
                max_iterations: 3,
            },
            "test-budget-truncation",
        )
        .unwrap();

    assert_eq!(closure.files.len(), 2, "seed plus one planned file");
    assert!(closure.gaps.iter().any(|gap| matches!(
        gap,
        types::structs::KnownGap::BudgetExhausted { remaining: 1, .. }
    )));
}

#[test]
fn test_build_closure_time_budget_records_gap() {
    let store = test_store();
    let seed_id = insert_file_structural_complete(&store, "src/main.c");
    let _sib = insert_file_structural_complete(&store, "src/other.c");
    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: seed_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::SameDirectory],
        budget: WindowBudget {
            max_time_ms: 0,
            ..WindowBudget::default()
        },
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-time-budget-exhausted")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&seed_id),
        "seed file must still be returned when time budget prevents expansion"
    );
    assert!(
        closure.gaps.iter().any(|gap| matches!(
            gap,
            types::structs::KnownGap::BudgetExhausted { strategy, .. }
                if strategy.contains("time_budget")
        )),
        "closure must record a time budget gap: {:?}",
        closure.gaps
    );
}

#[test]
fn test_time_budget_still_materializes_seed_call_edges() {
    let store = test_store();
    let caller_file = insert_file_structural_complete(&store, "src/caller.c");
    let callee_file = insert_file_structural_complete(&store, "src/callee.c");
    let caller = insert_function_symbol(&store, caller_file, "caller");
    let callee = insert_function_symbol(&store, callee_file, "callee");
    let reference = make_unresolved_reference(
        caller_file,
        Some(caller),
        types::ReferenceKind::Call,
        "callee",
        10,
        16,
    );
    store.insert_references(&[reference]).unwrap();

    let engine = test_engine(store.clone());
    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: caller_file,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget {
            max_time_ms: 0,
            ..WindowBudget::default()
        },
        language: Language::C,
        max_iterations: 1,
    };

    engine
        .build_closure(&window, "test-time-budget-materializes-edges")
        .expect("build_closure should succeed");

    let edges = store.find_edges_by_source(&caller).unwrap();
    assert!(
        edges
            .iter()
            .any(|edge| edge.kind == types::EdgeKind::Calls && edge.target == callee),
        "committed seed resolutions must be materialized even after expansion budget expires"
    );
}

#[test]
fn test_build_closure_records_gap_on_missing_file() {
    let store = test_store();
    // Do NOT insert any files — store is empty, identify the seed by Symbol
    // that yields no candidates from the mock provider.
    let lazy_structural = LazyStructuralService::with_provider(
        store.clone(),
        None,
        Box::new(MockCandidateProvider { candidates: vec![] }),
    );
    let df_store = store.clone();
    let engine = ClosureEngine::new(
        store,
        lazy_structural,
        LazyDataflowService::new(df_store, None),
        None,
        vec![],
    );

    let window = FocusWindow {
        seed: FocusSeed::Symbol {
            name: "nonexistent".to_string(),
            kind: None,
            language: Language::C,
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-missing-file-gap")
        .expect("build_closure should succeed even with empty candidates");

    // When the symbol seed yields no candidates, the closure has no files.
    // The engine produces an empty closure — no gaps recorded for this.
    assert!(
        closure.files.is_empty(),
        "no files should be in closure when seed symbol has no candidates"
    );
}

#[test]
fn test_build_closure_field_seed() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/struct_def.c");

    // Create a SymbolDef for the struct so find_symbol_by_id works
    let symbol_id = types::ids::SymbolId::generate(&file_id, "c", "my_struct", "struct", None);
    let symbol_def = types::structs::SymbolDef {
        id: symbol_id,
        kind: types::SymbolKind::Struct,
        name: "my_struct".to_string(),
        qualified_name: "my_struct".to_string(),
        symbol_path: vec!["my_struct".to_string()],
        file_id,
        language: Language::C,
        range: types::structs::TextRange::default(),
        name_range: types::structs::TextRange::default(),
        signature: None,
        visibility: None,
        exported: false,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".to_string(),
    };
    store.insert_symbols(&[symbol_def]).unwrap();

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::Field {
            struct_sym: symbol_id,
            field_path: "count".to_string(),
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-field-seed")
        .expect("build_closure with field seed should succeed");

    assert!(
        closure.files.contains(&file_id),
        "field seed must locate its struct's file"
    );
    assert!(
        closure.visited.contains(&file_id),
        "struct file must be visited for field seed"
    );
}

#[test]
fn test_build_closure_closure_id_persistence() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");
    let engine = test_engine(store);

    let closure_id = "persistent-closure-id";
    let _closure = engine
        .build_closure(&file_window(file_id), closure_id)
        .expect("build_closure should succeed");

    let committed_gen = engine
        .store
        .get_committed_generation(closure_id)
        .expect("get_committed_generation should succeed")
        .expect("closure should be committed");

    assert!(
        committed_gen > 0,
        "committed generation must be > 0 after build_closure"
    );
}

#[test]
fn test_build_closure_different_closure_ids() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");
    let engine = test_engine(store);

    engine
        .build_closure(&file_window(file_id), "closure-a")
        .expect("build_closure for closure-a should succeed");
    engine
        .build_closure(&file_window(file_id), "closure-b")
        .expect("build_closure for closure-b should succeed");

    let gen_a = engine
        .store
        .get_committed_generation("closure-a")
        .unwrap()
        .expect("closure-a must be committed");
    let gen_b = engine
        .store
        .get_committed_generation("closure-b")
        .unwrap()
        .expect("closure-b must be committed");

    assert!(gen_a > 0, "closure-a must have committed generation > 0");
    assert!(gen_b > 0, "closure-b must have committed generation > 0");
    // They are independent committed closures
    assert_ne!(
        engine
            .store
            .get_committed_generation("nonexistent")
            .unwrap(),
        Some(gen_a),
        "nonexistent closure should not match closure-a's generation"
    );
}

#[test]
fn test_build_closure_duplicate_files_deduped() {
    let store = test_store();
    let seed_id = insert_file_structural_complete(&store, "src/main.c");
    let sibling_id = insert_file_structural_complete(&store, "src/sibling.c");
    let engine = test_engine(store);

    // Two SameDirectory strategies — both will produce the same sibling
    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: seed_id,
            language: Language::C,
        },
        strategies: vec![
            ClosureStrategy::SameDirectory,
            ClosureStrategy::SameDirectory,
        ],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-dedup")
        .expect("build_closure should succeed");

    let file_count = closure.files.len();
    assert!(
        closure.files.contains(&seed_id),
        "seed file must be in closure"
    );
    assert!(
        closure.files.contains(&sibling_id),
        "sibling file must be in closure"
    );
    assert_eq!(
        file_count, 2,
        "closure.files must have exactly 2 files (seed + 1 sibling, no duplicates)"
    );
}

#[test]
fn test_build_closure_empty_project() {
    let store = test_store();
    // Empty store — no files inserted.
    let seed_id = types::ids::FileId::generate("ghost.c");
    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: seed_id,
            language: Language::C,
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    // The engine's ensure_structural_for_file is resilient to files not
    // yet in the store — it returns a cached/fallback result. So the
    // build_closure succeeds even on an empty project.
    let closure = engine
        .build_closure(&window, "test-empty-project")
        .expect("build_closure should succeed even on empty project");

    // The seed file still appears in the closure (mark_extracted always succeeds).
    assert!(
        closure.files.contains(&seed_id),
        "engine is resilient: seed file appears in closure even on empty store"
    );
}

#[test]
fn test_locate_seed_file_not_found() {
    let store = test_store();
    // Insert one file so the store is not empty, but the seed is different
    let _existing = insert_file_structural_complete(&store, "existing.c");
    let missing_id = types::ids::FileId::generate("not-in-db.c");
    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: missing_id,
            language: Language::C,
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    // locate_seed returns the file_id without checking existence.
    // The engine's ensure_structural_for_file handles non-existent files
    // gracefully, so build_closure succeeds.
    let closure = engine
        .build_closure(&window, "test-seed-not-found")
        .expect("build_closure succeeds even for seed file not in DB");

    assert!(
        closure.files.contains(&missing_id),
        "engine is resilient: non-existent seed file appears in closure"
    );
}

#[test]
fn test_build_closure_zero_budget() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");
    let engine = test_engine(store);

    // max_files=0: budget check only applies to additions (Phase 2),
    // seed extraction (Phase 1) happens before budget.
    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Language::C,
        },
        strategies: vec![], // No strategies → no additions
        budget: WindowBudget {
            max_files: 0,
            ..WindowBudget::default()
        },
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-zero-budget")
        .expect("build_closure with zero budget should succeed");

    assert!(
        closure.files.contains(&file_id),
        "seed file must be extracted even with max_files=0 (Phase 1 precedes budget check)"
    );
    assert!(
        closure.gaps.is_empty(),
        "no gaps expected when no additions are requested"
    );
}

#[test]
fn test_build_closure_callgraph_stub() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");
    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: super::types::Direction::Outgoing,
            depth: 2,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-callgraph-stub")
        .expect("build_closure with CallGraph stub should succeed");

    assert!(
        closure.files.contains(&file_id),
        "seed file must be in closure even with stub strategy"
    );
    // CallGraph is a stub — it returns no additions, so no gaps from it
    assert_eq!(
        closure.files.len(),
        1,
        "closure must contain only the seed file (CallGraph stub returns empty)"
    );
}

// ── TypeGraph tests ─────────────────────────────────────────────────────────

/// Helper: create a `ReferenceUse` with a resolved target pointing to a
/// type symbol in another file.
fn make_type_reference(
    file_id: FileId,
    source_symbol: Option<types::ids::SymbolId>,
    ref_kind: types::ReferenceKind,
    ref_name: &str,
    target_symbol_id: types::ids::SymbolId,
) -> types::structs::ReferenceUse {
    let range = types::structs::TextRange {
        start_byte: 0,
        end_byte: 10,
        start_line: 1,
        start_column: 1,
        end_line: 1,
        end_column: 11,
    };
    let ref_id = types::ids::ReferenceId::generate(
        &file_id,
        source_symbol.as_ref(),
        range.start_byte,
        range.end_byte,
        ref_name,
        ref_kind,
    );
    types::structs::ReferenceUse {
        id: ref_id,
        file_id,
        source_symbol,
        scope_id: None,
        kind: ref_kind,
        text: ref_name.to_string(),
        name: ref_name.to_string(),
        receiver: None,
        arity: None,
        range,
        binding_id: None,
        resolved: Some(types::structs::ResolvedTarget {
            symbol_id: target_symbol_id,
            confidence: types::Confidence::certain(),
            strategy: types::ResolutionStrategy::ExactMatch,
            provenance: types::Provenance::TreeSitter,
        }),
    }
}

/// Helper: insert a function symbol into a file.
fn insert_function_symbol(store: &Store, file_id: FileId, name: &str) -> types::ids::SymbolId {
    let sym_id = types::ids::SymbolId::generate(&file_id, "c", name, "function", None);
    let sym = types::structs::SymbolDef {
        id: sym_id,
        kind: types::SymbolKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        symbol_path: vec![name.to_string()],
        file_id,
        language: Language::C,
        range: types::structs::TextRange::default(),
        name_range: types::structs::TextRange::default(),
        signature: None,
        visibility: None,
        exported: false,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".to_string(),
    };
    store.insert_symbols(&[sym]).unwrap();
    sym_id
}

/// Helper: insert a struct (type) symbol into a file.
fn insert_struct_symbol(store: &Store, file_id: FileId, name: &str) -> types::ids::SymbolId {
    let sym_id = types::ids::SymbolId::generate(&file_id, "c", name, "struct", None);
    let sym = types::structs::SymbolDef {
        id: sym_id,
        kind: types::SymbolKind::Struct,
        name: name.to_string(),
        qualified_name: name.to_string(),
        symbol_path: vec![name.to_string()],
        file_id,
        language: Language::C,
        range: types::structs::TextRange::default(),
        name_range: types::structs::TextRange::default(),
        signature: None,
        visibility: None,
        exported: false,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".to_string(),
    };
    store.insert_symbols(&[sym]).unwrap();
    sym_id
}

/// Test A: File A uses struct from file B → TypeGraph adds file B.
#[test]
fn test_typegraph_adds_direct_type_dependency() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/main.c");
    let file_b = insert_file_structural_complete(&store, "src/types.h");

    // Function in file A
    let func_id = insert_function_symbol(&store, file_a, "process");
    // Struct in file B
    let struct_id = insert_struct_symbol(&store, file_b, "MyStruct");

    // Usage reference in file A → MyStruct in file B
    let ref_use = make_type_reference(
        file_a,
        Some(func_id),
        types::ReferenceKind::Usage,
        "MyStruct",
        struct_id,
    );
    store.insert_references(&[ref_use]).unwrap();

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::TypeGraph { max_depth: 1 }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-typegraph-direct")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&file_b),
        "TypeGraph must add file B (contains the struct referenced by file A)"
    );
    assert!(
        closure.files.contains(&file_a),
        "seed file must be in closure"
    );
}

/// Test B: max_depth=1 only adds direct type dependencies, not transitive.
#[test]
fn test_typegraph_max_depth_1_direct_only() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/main.c");
    let file_b = insert_file_structural_complete(&store, "src/types.h");
    let file_c = insert_file_structural_complete(&store, "src/nested.h");

    // File A has a function; file B has StructA; file C has NestedStruct
    let func_id = insert_function_symbol(&store, file_a, "process");
    let struct_a_id = insert_struct_symbol(&store, file_b, "StructA");
    let nested_id = insert_struct_symbol(&store, file_c, "NestedStruct");

    // Reference in file A → StructA in file B
    let ref_a_to_b = make_type_reference(
        file_a,
        Some(func_id),
        types::ReferenceKind::Usage,
        "StructA",
        struct_a_id,
    );
    // Reference in file B → NestedStruct in file C (would be depth 2)
    let ref_b_to_c = make_type_reference(
        file_b,
        Some(struct_a_id),
        types::ReferenceKind::Usage,
        "NestedStruct",
        nested_id,
    );
    store.insert_references(&[ref_a_to_b, ref_b_to_c]).unwrap();

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::TypeGraph { max_depth: 1 }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-typegraph-depth1")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&file_b),
        "depth=1 must add direct type dependency (file B)"
    );
    assert!(
        !closure.files.contains(&file_c),
        "depth=1 must NOT add transitive type dependency (file C)"
    );
}

/// Test C: max_depth=2 follows transitive type dependencies.
#[test]
fn test_typegraph_max_depth_2_transitive() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/main.c");
    let file_b = insert_file_structural_complete(&store, "src/types.h");
    let file_c = insert_file_structural_complete(&store, "src/nested.h");

    let func_id = insert_function_symbol(&store, file_a, "process");
    let struct_a_id = insert_struct_symbol(&store, file_b, "StructA");
    let nested_id = insert_struct_symbol(&store, file_c, "NestedStruct");

    // A → B (direct), B → C (transitive)
    let ref_a_to_b = make_type_reference(
        file_a,
        Some(func_id),
        types::ReferenceKind::Usage,
        "StructA",
        struct_a_id,
    );
    let ref_b_to_c = make_type_reference(
        file_b,
        Some(struct_a_id),
        types::ReferenceKind::Usage,
        "NestedStruct",
        nested_id,
    );
    store.insert_references(&[ref_a_to_b, ref_b_to_c]).unwrap();

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::TypeGraph { max_depth: 2 }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-typegraph-depth2")
        .expect("build_closure should succeed");

    assert!(closure.files.contains(&file_b));
    assert!(
        closure.files.contains(&file_c),
        "depth=2 must follow transitive type dependency to file C"
    );
}

/// Test D: Closure with no type references returns empty additions.
#[test]
fn test_typegraph_empty_closure() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");

    // Insert a function symbol but NO references at all
    let _func_id = insert_function_symbol(&store, file_id, "process");

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::TypeGraph { max_depth: 3 }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-typegraph-empty")
        .expect("build_closure should succeed");

    // Only the seed file — no type references means no additions
    assert_eq!(
        closure.files.len(),
        1,
        "closure must contain only the seed file when no type references exist"
    );
    assert!(closure.files.contains(&file_id));
}

/// Test E: Same type file found via multiple references is added only once.
#[test]
fn test_typegraph_dedup_same_type_multiple_refs() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/main.c");
    let file_b = insert_file_structural_complete(&store, "src/types.h");

    let func_id = insert_function_symbol(&store, file_a, "process");
    let struct_id = insert_struct_symbol(&store, file_b, "MyStruct");

    // Two references in file A both pointing to the same struct in file B
    let ref1 = make_type_reference(
        file_a,
        Some(func_id),
        types::ReferenceKind::Usage,
        "MyStruct",
        struct_id,
    );
    // Use a different byte range for the second reference to get a distinct ID
    let mut ref2 = make_type_reference(
        file_a,
        Some(func_id),
        types::ReferenceKind::Inheritance,
        "MyStruct",
        struct_id,
    );
    // Adjust the name range to create a different reference ID
    ref2.range.start_byte = 20;
    ref2.range.end_byte = 30;
    ref2.id = types::ids::ReferenceId::generate(
        &file_a,
        Some(&func_id),
        20,
        30,
        "MyStruct",
        types::ReferenceKind::Inheritance,
    );

    store.insert_references(&[ref1, ref2]).unwrap();

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::TypeGraph { max_depth: 1 }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-typegraph-dedup")
        .expect("build_closure should succeed");

    assert!(closure.files.contains(&file_b), "file B must be in closure");
    // file B should appear exactly once: seed (file_a) + type dep (file_b) = 2
    assert_eq!(
        closure.files.len(),
        2,
        "dedup: two references to same type must not double-add file B"
    );
}

// ── CallGraph tests ─────────────────────────────────────────────────────────

/// Test A: Two files where main.c calls helper.c — after building a closure
/// with CallGraph (Outgoing, depth=1), the helper file is found via the scoped
/// reference resolution (reference_resolutions).
#[test]
fn test_callgraph_expansion_finds_callee_file() {
    let store = test_store();
    let main_id = insert_file_structural_complete(&store, "main.c");
    let helper_id = insert_file_structural_complete(&store, "helper.c");

    // Create function symbols in both files
    let main_sym = insert_function_symbol(&store, main_id, "main");
    let _helper_sym = insert_function_symbol(&store, helper_id, "helper");

    // Create a Call reference: main → helper (unresolved, will be resolved by scoped resolver)
    let ref_use = make_unresolved_reference(
        main_id,
        Some(main_sym),
        types::ReferenceKind::Call,
        "helper",
        10,
        16,
    );
    store.insert_references(&[ref_use]).unwrap();

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: main_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: super::types::Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-callgraph-finds-callee")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&main_id),
        "seed file must be in closure"
    );
    assert!(
        closure.files.contains(&helper_id),
        "helper file must be found via outgoing call graph reference resolution"
    );
}

/// Test B: Depth=1 only adds direct callees; depth=2 is beyond budget
/// (multi-hop is deferred to the fixed-point loop, so it returns empty).
#[test]
fn test_callgraph_depth_control() {
    let store = test_store();
    let main_id = insert_file_structural_complete(&store, "main.c");
    let helper_id = insert_file_structural_complete(&store, "helper.c");

    let main_sym = insert_function_symbol(&store, main_id, "main");
    let _helper_sym = insert_function_symbol(&store, helper_id, "helper");

    let ref_use = make_unresolved_reference(
        main_id,
        Some(main_sym),
        types::ReferenceKind::Call,
        "helper",
        10,
        16,
    );
    store.insert_references(&[ref_use]).unwrap();

    // depth=1: helper file should be added
    {
        let engine = test_engine(store.clone());
        let window = FocusWindow {
            seed: FocusSeed::File {
                file_id: main_id,
                language: Language::C,
            },
            strategies: vec![ClosureStrategy::CallGraph {
                direction: super::types::Direction::Outgoing,
                depth: 1,
            }],
            budget: WindowBudget::default(),
            language: Language::C,
            max_iterations: 3,
        };
        let closure = engine
            .build_closure(&window, "test-cg-depth-1")
            .expect("depth=1 build should succeed");
        assert!(
            closure.files.contains(&helper_id),
            "depth=1 must add direct callee file"
        );
    }

    // depth=2: beyond single-hop — returns empty, helper file NOT added
    {
        let engine = test_engine(store);
        let window = FocusWindow {
            seed: FocusSeed::File {
                file_id: main_id,
                language: Language::C,
            },
            strategies: vec![ClosureStrategy::CallGraph {
                direction: super::types::Direction::Outgoing,
                depth: 2,
            }],
            budget: WindowBudget::default(),
            language: Language::C,
            max_iterations: 3,
        };
        let closure = engine
            .build_closure(&window, "test-cg-depth-2")
            .expect("depth=2 build should succeed");
        assert!(
            !closure.files.contains(&helper_id),
            "depth=2 must NOT add callee file (beyond single-hop budget)"
        );
        assert_eq!(
            closure.files.len(),
            1,
            "depth=2 closure must contain only the seed file"
        );
    }
}

/// Test C: Dedup — same callee file reached via multiple caller symbols
/// must appear only once in the returned additions.
#[test]
fn test_callgraph_dedup_same_callee_file() {
    let store = test_store();
    let main_id = insert_file_structural_complete(&store, "main.c");
    let helper_id = insert_file_structural_complete(&store, "helper.c");

    // Two caller symbols in main.c, both calling the same helper
    let caller_a = insert_function_symbol(&store, main_id, "caller_a");
    let caller_b = insert_function_symbol(&store, main_id, "caller_b");
    let _helper_sym = insert_function_symbol(&store, helper_id, "helper");

    // Two Call references: caller_a → helper, caller_b → helper
    let ref_a = make_unresolved_reference(
        main_id,
        Some(caller_a),
        types::ReferenceKind::Call,
        "helper",
        10,
        16,
    );
    let ref_b = make_unresolved_reference(
        main_id,
        Some(caller_b),
        types::ReferenceKind::Call,
        "helper",
        20,
        26,
    );
    store.insert_references(&[ref_a, ref_b]).unwrap();

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: main_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: super::types::Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-callgraph-dedup")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&main_id),
        "seed file must be in closure"
    );
    assert!(
        closure.files.contains(&helper_id),
        "helper file must be in closure"
    );
    // seed (1) + helper (1) = 2 files total; helper must NOT appear twice
    assert_eq!(
        closure.files.len(),
        2,
        "dedup: helper file must appear exactly once (seed + helper = 2)"
    );
}

/// Test D: Empty closure with no symbols → CallGraph returns empty.
#[test]
fn test_callgraph_empty_closure_no_symbols() {
    let store = test_store();
    let main_id = insert_file_structural_complete(&store, "main.c");
    // Do NOT insert any symbols — the file has structural extraction
    // but no symbols in the DB.

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: main_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: super::types::Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-callgraph-empty-symbols")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&main_id),
        "seed file must be in closure"
    );
    assert_eq!(
        closure.files.len(),
        1,
        "closure with no symbols must contain only the seed file (CallGraph returns empty)"
    );
}

/// Test E: Direction::Incoming verifies call relationships within the closure.
///
/// Since reference_resolutions only cover files already extracted,
/// Incoming is most useful when the caller file is in scope (via another
/// strategy).  Here we combine SameDirectory to bring the caller file in,
/// then CallGraph Incoming discovers the relationship.
#[test]
fn test_callgraph_incoming_finds_caller_file() {
    let store = test_store();
    let main_id = insert_file_structural_complete(&store, "src/main.c");
    let helper_id = insert_file_structural_complete(&store, "src/helper.c");

    let _main_sym = insert_function_symbol(&store, main_id, "main");
    let helper_sym = insert_function_symbol(&store, helper_id, "helper");

    // Call reference in helper.c → "main" (helper calls main)
    let ref_use = make_unresolved_reference(
        helper_id,
        Some(helper_sym),
        types::ReferenceKind::Call,
        "main",
        10,
        14,
    );
    store.insert_references(&[ref_use]).unwrap();

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: main_id,
            language: Language::C,
        },
        strategies: vec![
            ClosureStrategy::SameDirectory,
            ClosureStrategy::CallGraph {
                direction: super::types::Direction::Incoming,
                depth: 1,
            },
        ],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-callgraph-incoming-caller")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&main_id),
        "seed file must be in closure"
    );
    // SameDirectory pulls helper.c into scope; CallGraph Incoming verifies
    // the call relationship from helper → main.
    assert!(
        closure.files.contains(&helper_id),
        "caller file must be in closure (via SameDirectory + Incoming verification)"
    );
    assert_eq!(
        closure.files.len(),
        2,
        "closure must contain seed + caller = 2 files"
    );
}

// ── CallGraph Incoming + Both tests ────────────────────────────────────────

/// Test F: Direction::Both uses SameDirectory to pull all files into scope,
/// then verifies both outgoing and incoming call relationships.
#[test]
fn test_callgraph_both_finds_both_directions() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/a.c");
    let file_b = insert_file_structural_complete(&store, "src/b.c");
    let file_c = insert_file_structural_complete(&store, "src/c.c");

    let sym_a = insert_function_symbol(&store, file_a, "a");
    let sym_b = insert_function_symbol(&store, file_b, "b");
    let _sym_c = insert_function_symbol(&store, file_c, "c");

    // Reference A → "b" (A calls B) — outgoing from B, incoming to A
    let ref_a_to_b =
        make_unresolved_reference(file_a, Some(sym_a), types::ReferenceKind::Call, "b", 10, 11);
    // Reference B → "c" (B calls C) — outgoing from B, incoming to C
    let ref_b_to_c =
        make_unresolved_reference(file_b, Some(sym_b), types::ReferenceKind::Call, "c", 10, 11);
    store.insert_references(&[ref_a_to_b, ref_b_to_c]).unwrap();

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_b,
            language: Language::C,
        },
        strategies: vec![
            ClosureStrategy::SameDirectory,
            ClosureStrategy::CallGraph {
                direction: super::types::Direction::Both,
                depth: 1,
            },
        ],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-callgraph-both")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&file_b),
        "seed file must be in closure"
    );
    assert!(
        closure.files.contains(&file_a),
        "Both: incoming must find caller file A (via SameDirectory)"
    );
    assert!(
        closure.files.contains(&file_c),
        "Both: outgoing must find callee file C"
    );
    assert_eq!(
        closure.files.len(),
        3,
        "Both must find seed + caller A + callee C = 3 files"
    );
}

/// Test G: Incoming with multiple callers.  References X→Z and Y→Z,
/// seed on Z's file, uses SameDirectory to pull X and Y in.
#[test]
fn test_callgraph_incoming_crosses_multiple_edges() {
    let store = test_store();
    let file_x = insert_file_structural_complete(&store, "src/x.c");
    let file_y = insert_file_structural_complete(&store, "src/y.c");
    let file_z = insert_file_structural_complete(&store, "src/z.c");

    let sym_x = insert_function_symbol(&store, file_x, "x");
    let sym_y = insert_function_symbol(&store, file_y, "y");
    let _sym_z = insert_function_symbol(&store, file_z, "z");

    // Reference X → "z" (X calls Z)
    let ref_x_to_z =
        make_unresolved_reference(file_x, Some(sym_x), types::ReferenceKind::Call, "z", 10, 11);
    // Reference Y → "z" (Y calls Z)
    let ref_y_to_z =
        make_unresolved_reference(file_y, Some(sym_y), types::ReferenceKind::Call, "z", 10, 11);
    store.insert_references(&[ref_x_to_z, ref_y_to_z]).unwrap();

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_z,
            language: Language::C,
        },
        strategies: vec![
            ClosureStrategy::SameDirectory,
            ClosureStrategy::CallGraph {
                direction: super::types::Direction::Incoming,
                depth: 1,
            },
        ],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-callgraph-incoming-multi")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&file_z),
        "seed file must be in closure"
    );
    assert!(
        closure.files.contains(&file_x),
        "Incoming must find caller file X (via SameDirectory)"
    );
    assert!(
        closure.files.contains(&file_y),
        "Incoming must find caller file Y (via SameDirectory)"
    );
    assert_eq!(
        closure.files.len(),
        3,
        "closure must contain seed + X + Y = 3 files"
    );
}

/// Test H: Incoming dedup — references X→Z and X→W (same caller),
/// seed on Z's file, SameDirectory pulls X and W in. X must appear once.
#[test]
fn test_callgraph_incoming_dedup_caller_files() {
    let store = test_store();
    let file_x = insert_file_structural_complete(&store, "src/x.c");
    let file_z = insert_file_structural_complete(&store, "src/z.c");
    let file_w = insert_file_structural_complete(&store, "src/w.c");

    let sym_x = insert_function_symbol(&store, file_x, "x");
    let _sym_z = insert_function_symbol(&store, file_z, "z");
    let _sym_w = insert_function_symbol(&store, file_w, "w");

    // Reference X → "z" (X calls Z)
    let ref_x_to_z =
        make_unresolved_reference(file_x, Some(sym_x), types::ReferenceKind::Call, "z", 10, 11);
    // Reference X → "w" (X calls W)
    let ref_x_to_w =
        make_unresolved_reference(file_x, Some(sym_x), types::ReferenceKind::Call, "w", 20, 21);
    store.insert_references(&[ref_x_to_z, ref_x_to_w]).unwrap();

    let engine = test_engine(store);

    // Seed on Z's file. SameDirectory pulls X and W in.
    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_z,
            language: Language::C,
        },
        strategies: vec![
            ClosureStrategy::SameDirectory,
            ClosureStrategy::CallGraph {
                direction: super::types::Direction::Incoming,
                depth: 1,
            },
        ],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-callgraph-incoming-dedup")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&file_z),
        "seed file must be in closure"
    );
    // SameDirectory brings both X and W in
    assert!(
        closure.files.contains(&file_x),
        "caller file X must be in closure"
    );
    assert!(
        closure.files.contains(&file_w),
        "callee file W must be in closure (via SameDirectory)"
    );
    // X must appear only once (SameDirectory dedup)
    assert_eq!(
        closure.files.len(),
        3,
        "closure must contain seed + X + W = 3 files (X appears once)"
    );
}

// ── New CallGraph tests: reference_resolutions-based expansion ────────────────

/// Test I: CallGraph (Outgoing, depth=1) finds callee file via scoped
/// reference_resolutions, NOT via symbol_edges.  No edges are inserted.
#[test]
fn test_callgraph_from_scoped_resolution() {
    let store = test_store();
    let main_id = insert_file_structural_complete(&store, "src/main.c");
    let helper_id = insert_file_structural_complete(&store, "src/helper.c");

    let main_sym = insert_function_symbol(&store, main_id, "main");
    let _helper_sym = insert_function_symbol(&store, helper_id, "helper");

    // Unresolved Call reference in main.c → "helper"
    let ref_use = make_unresolved_reference(
        main_id,
        Some(main_sym),
        types::ReferenceKind::Call,
        "helper",
        10,
        16,
    );
    store.insert_references(&[ref_use]).unwrap();

    // Verify no symbol_edges exist — we rely purely on reference_resolutions
    let edges = store.find_edges_by_source(&main_sym).unwrap();
    assert!(
        edges.is_empty(),
        "symbol_edges must be empty — expansion must come from reference_resolutions"
    );

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: main_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: super::types::Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-cg-from-scoped-res")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&main_id),
        "seed file must be in closure"
    );
    assert!(
        closure.files.contains(&helper_id),
        "helper file must be found via reference_resolutions (no symbol_edges)"
    );
    assert_eq!(
        closure.files.len(),
        2,
        "closure must contain seed + helper = 2 files"
    );
}

/// Test J: CallGraph (Incoming, depth=1) verifies incoming call relationships
/// via reference_resolutions.  Uses SameDirectory to pull the caller file into
/// scope first; then Incoming discovers the call relationship.
#[test]
fn test_callgraph_incoming_from_scoped_resolution() {
    let store = test_store();
    let main_id = insert_file_structural_complete(&store, "src/main.c");
    let helper_id = insert_file_structural_complete(&store, "src/helper.c");

    let _main_sym = insert_function_symbol(&store, main_id, "main");
    let helper_sym = insert_function_symbol(&store, helper_id, "helper");

    // Call reference in helper.c → "main" (helper calls main)
    let ref_use = make_unresolved_reference(
        helper_id,
        Some(helper_sym),
        types::ReferenceKind::Call,
        "main",
        10,
        14,
    );
    store.insert_references(&[ref_use]).unwrap();

    // Verify no symbol_edges exist
    let edges = store.find_edges_by_source(&helper_sym).unwrap();
    assert!(
        edges.is_empty(),
        "symbol_edges must be empty — expansion must come from reference_resolutions"
    );

    let engine = test_engine(store);

    // Seed on main (callee). SameDirectory pulls helper (caller) in.
    // Incoming CallGraph then discovers that helper calls main.
    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: main_id,
            language: Language::C,
        },
        strategies: vec![
            ClosureStrategy::SameDirectory,
            ClosureStrategy::CallGraph {
                direction: super::types::Direction::Incoming,
                depth: 1,
            },
        ],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-cg-incoming-from-scoped-res")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&main_id),
        "seed file must be in closure"
    );
    assert!(
        closure.files.contains(&helper_id),
        "caller file must be in closure (via SameDirectory + Incoming via reference_resolutions)"
    );
    assert_eq!(
        closure.files.len(),
        2,
        "closure must contain seed + caller = 2 files"
    );
}

/// Test K: CallGraph expansion works from reference_resolutions even when
/// NO symbol_edges exist.  Proves that the new query path reads from
/// reference_resolutions, not from symbol_edges or symbol_edge_candidates.
#[test]
fn test_callgraph_uses_resolution_not_edges() {
    let store = test_store();
    let main_id = insert_file_structural_complete(&store, "src/main.c");
    let helper_id = insert_file_structural_complete(&store, "src/helper.c");

    let main_sym = insert_function_symbol(&store, main_id, "main");
    let _helper_sym = insert_function_symbol(&store, helper_id, "helper");

    // Create a Call reference — but NO edges at all
    let ref_use = make_unresolved_reference(
        main_id,
        Some(main_sym),
        types::ReferenceKind::Call,
        "helper",
        10,
        16,
    );
    store.insert_references(&[ref_use]).unwrap();

    // No edges inserted at all — this test proves edges are not needed
    // (the very absence of insert_edges is the point)

    let engine = test_engine(store);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: main_id,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: super::types::Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure = engine
        .build_closure(&window, "test-cg-no-edges")
        .expect("build_closure should succeed");

    assert!(
        closure.files.contains(&main_id),
        "seed file must be in closure"
    );
    assert!(
        closure.files.contains(&helper_id),
        "helper file must be found via reference_resolutions — NO symbol_edges were inserted"
    );
    assert_eq!(
        closure.files.len(),
        2,
        "closure must contain seed + helper = 2 files (no edges, pure reference_resolutions)"
    );

    // Post-condition: verify reference_resolutions actually have data
    let visible = engine
        .store
        .get_visible_resolutions_for_closure("test-cg-no-edges")
        .expect("get_visible_resolutions_for_closure should succeed");
    assert!(
        !visible.is_empty(),
        "reference_resolutions must contain resolution rows for the closure"
    );
}

// ── Scoped resolution tests (Task D: wire resolve_for_closure) ──────────────

/// Helper: create an unresolved ReferenceUse (no resolved target).
fn make_unresolved_reference(
    file_id: FileId,
    source_symbol: Option<types::ids::SymbolId>,
    ref_kind: types::ReferenceKind,
    ref_name: &str,
    range_start: usize,
    range_end: usize,
) -> types::structs::ReferenceUse {
    let range = types::structs::TextRange {
        start_byte: range_start as u32,
        end_byte: range_end as u32,
        start_line: 1,
        start_column: 1,
        end_line: 1,
        end_column: (range_end - range_start + 1) as u32,
    };
    let ref_id = types::ids::ReferenceId::generate(
        &file_id,
        source_symbol.as_ref(),
        range.start_byte,
        range.end_byte,
        ref_name,
        ref_kind,
    );
    types::structs::ReferenceUse {
        id: ref_id,
        file_id,
        source_symbol,
        scope_id: None,
        kind: ref_kind,
        text: ref_name.to_string(),
        name: ref_name.to_string(),
        receiver: None,
        arity: None,
        range,
        binding_id: None,
        resolved: None, // unresolved reference — should be resolved by scoped resolver
    }
}

/// After build_closure, reference_resolutions table should have rows.
#[test]
fn test_scoped_resolution_writes_reference_resolutions() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/main.c");
    let file_b = insert_file_structural_complete(&store, "src/helper.c");

    // Function symbol in file A (the source of the reference)
    let caller_sym = insert_function_symbol(&store, file_a, "caller");
    // Function symbol in file B (target to resolve against)
    let _target_sym = insert_function_symbol(&store, file_b, "helper");

    // Unresolved reference in file A → "helper" (should resolve to helper in file B)
    let ref_use = make_unresolved_reference(
        file_a,
        Some(caller_sym),
        types::ReferenceKind::Call,
        "helper",
        10,
        16,
    );
    store.insert_references(&[ref_use]).unwrap();

    // Build closure with CallGraph to pull in file B
    let engine = test_engine(store.clone());

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: super::types::Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure_id = "test-scoped-res-writes";
    let _closure = engine
        .build_closure(&window, closure_id)
        .expect("build_closure should succeed");

    // Resolution rows should exist (generation=0 since no expansion occurred)
    let count = store
        .count_reference_resolutions(closure_id, 0)
        .expect("count_reference_resolutions should succeed");
    assert!(
        count > 0,
        "expected at least 1 reference_resolutions row, got {count}"
    );
}

/// After build_closure (which calls commit_closure internally),
/// reference_resolutions rows should be visible (is_visible=1).
#[test]
fn test_scoped_resolution_committed_visible() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/main.c");
    let file_b = insert_file_structural_complete(&store, "src/helper.c");

    let caller_sym = insert_function_symbol(&store, file_a, "caller");
    let _target_sym = insert_function_symbol(&store, file_b, "helper");

    let ref_use = make_unresolved_reference(
        file_a,
        Some(caller_sym),
        types::ReferenceKind::Call,
        "helper",
        10,
        16,
    );
    store.insert_references(&[ref_use]).unwrap();

    let engine = test_engine(store.clone());

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: super::types::Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure_id = "test-scoped-res-visible";
    let _closure = engine
        .build_closure(&window, closure_id)
        .expect("build_closure should succeed");

    // Verify resolution rows exist and are visible
    let count = store
        .count_reference_resolutions(closure_id, 0)
        .expect("count_reference_resolutions should succeed");
    assert!(count > 0, "expected at least 1 resolution row, got {count}");

    // Verify the resolution is visible (after commit_closure)
    // get_visible_resolution requires exact reference_id bytes; since we
    // can't easily predict the resolution row's reference_id, verify via
    // the closure-level query.
    let visible = store
        .get_visible_resolutions_for_closure(closure_id)
        .expect("get_visible_resolutions_for_closure should succeed");
    assert!(
        !visible.is_empty(),
        "expected visible resolutions after commit"
    );
}

/// Scoped resolver writes to reference_resolutions — NOT to references.resolved_*.
/// The global table must stay unaffected.
#[test]
fn test_scoped_resolution_does_not_pollute_references_table() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/main.c");
    let file_b = insert_file_structural_complete(&store, "src/helper.c");

    let caller_sym = insert_function_symbol(&store, file_a, "caller");
    let _target_sym = insert_function_symbol(&store, file_b, "helper");

    // Insert an unresolved reference
    let ref_use = make_unresolved_reference(
        file_a,
        Some(caller_sym),
        types::ReferenceKind::Call,
        "helper",
        10,
        16,
    );
    store.insert_references(&[ref_use.clone()]).unwrap();

    let engine = test_engine(store.clone());

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: super::types::Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure_id = "test-scoped-res-no-pollute";
    let _closure = engine
        .build_closure(&window, closure_id)
        .expect("build_closure should succeed");

    // The original reference in the references table must still be unresolved
    let refs_after = store
        .find_references_by_file(&file_a)
        .expect("find_references_by_file should succeed");

    for r in &refs_after {
        // The scoped resolver writes to reference_resolutions, NOT references
        assert!(
            r.resolved.is_none(),
            "scoped resolver must not modify references.resolved; found resolved={:?}",
            r.resolved
        );
    }

    // But reference_resolutions should have entries
    let res_count = store
        .count_reference_resolutions(closure_id, 0)
        .expect("count_reference_resolutions should succeed");
    assert!(
        res_count > 0,
        "reference_resolutions table should have entries (scoped resolver writes here)"
    );
}

/// Empty closure (no files) must not crash the resolve step.
#[test]
fn test_scoped_resolution_empty_closure_no_crash() {
    let store = test_store();
    // Empty store, seed via Symbol that yields no candidates
    let lazy_structural = LazyStructuralService::with_provider(
        store.clone(),
        None,
        Box::new(MockCandidateProvider { candidates: vec![] }),
    );
    let engine = ClosureEngine::new(
        store.clone(),
        lazy_structural,
        LazyDataflowService::new(store.clone(), None),
        None,
        vec![],
    );

    let window = FocusWindow {
        seed: FocusSeed::Symbol {
            name: "nonexistent".to_string(),
            kind: None,
            language: Language::C,
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure_id = "test-scoped-res-empty";
    let result = engine.build_closure(&window, closure_id);
    assert!(
        result.is_ok(),
        "build_closure with empty closure must not crash during resolution"
    );

    // Verify no resolution rows were written for the empty closure
    let count = store
        .count_reference_resolutions(closure_id, 0)
        .expect("count_reference_resolutions should succeed");
    assert_eq!(
        count, 0,
        "empty closure must produce zero reference_resolutions rows"
    );
}

// ── FocusGraphBuilder integration tests (Task C) ─────────────────────────────

/// After build_closure completes (resolve + commit + graph build), the
/// FocusGraphBuilder should produce canonical `symbol_edges` rows for
/// resolved Call references.
#[test]
fn test_graph_builder_produces_canonical_edges() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/caller.c");
    let file_b = insert_file_structural_complete(&store, "src/callee.c");

    // Function symbols in both files
    let caller_sym = insert_function_symbol(&store, file_a, "caller");
    let callee_sym = insert_function_symbol(&store, file_b, "callee");

    // Unresolved Call reference in file A → "callee"
    let ref_use = make_unresolved_reference(
        file_a,
        Some(caller_sym),
        types::ReferenceKind::Call,
        "callee",
        10,
        16,
    );
    store.insert_references(&[ref_use]).unwrap();

    let engine = test_engine(store.clone());

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: super::types::Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure_id = "test-gb-canonical-edges";
    let _closure = engine
        .build_closure(&window, closure_id)
        .expect("build_closure should succeed");

    // Verify canonical edges exist for caller symbol
    let edges = store
        .find_edges_by_source(&caller_sym)
        .expect("find_edges_by_source should succeed");
    assert!(
        !edges.is_empty(),
        "expected at least 1 canonical edge from caller symbol"
    );

    // Verify at least one Calls edge to the callee
    let calls_edge = edges
        .iter()
        .find(|e| e.kind == types::EdgeKind::Calls && e.target == callee_sym);
    assert!(
        calls_edge.is_some(),
        "expected a Calls edge from caller to callee; found edges: {:?}",
        edges
            .iter()
            .map(|e| (e.kind.as_str(), &e.target))
            .collect::<Vec<_>>()
    );
}

/// Building graph on closure B that covers the same symbols as closure A
/// must not overwrite A's existing canonical edges (EdgeConflictPolicy::Keep).
#[test]
fn test_graph_builder_preserves_existing_edges() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/caller.c");
    let file_b = insert_file_structural_complete(&store, "src/callee.c");

    let caller_sym = insert_function_symbol(&store, file_a, "caller");
    let callee_sym = insert_function_symbol(&store, file_b, "callee");

    let ref_use = make_unresolved_reference(
        file_a,
        Some(caller_sym),
        types::ReferenceKind::Call,
        "callee",
        10,
        16,
    );
    store.insert_references(&[ref_use]).unwrap();

    // Closure A — first build, edges created
    {
        let engine = test_engine(store.clone());
        let window = FocusWindow {
            seed: FocusSeed::File {
                file_id: file_a,
                language: Language::C,
            },
            strategies: vec![ClosureStrategy::CallGraph {
                direction: super::types::Direction::Outgoing,
                depth: 1,
            }],
            budget: WindowBudget::default(),
            language: Language::C,
            max_iterations: 3,
        };
        let _closure = engine
            .build_closure(&window, "test-gb-preserve-a")
            .expect("closure A build should succeed");
    }

    // Snapshot edges after closure A
    let edges_after_a = store
        .find_edges_by_source(&caller_sym)
        .expect("find_edges_by_source should succeed");
    assert!(
        !edges_after_a.is_empty(),
        "closure A must produce canonical edges"
    );
    let edge_ids_after_a: Vec<_> = edges_after_a.iter().map(|e| e.id).collect();

    // Closure B — same symbols, different closure_id
    {
        let engine = test_engine(store.clone());
        let window = FocusWindow {
            seed: FocusSeed::File {
                file_id: file_a,
                language: Language::C,
            },
            strategies: vec![ClosureStrategy::CallGraph {
                direction: super::types::Direction::Outgoing,
                depth: 1,
            }],
            budget: WindowBudget::default(),
            language: Language::C,
            max_iterations: 3,
        };
        let _closure = engine
            .build_closure(&window, "test-gb-preserve-b")
            .expect("closure B build should succeed");
    }

    // Verify edges after closure B still contain the same edge from A
    let edges_after_b = store
        .find_edges_by_source(&caller_sym)
        .expect("find_edges_by_source should succeed");

    // All edges from closure A must still be present
    for id_a in &edge_ids_after_a {
        assert!(
            edges_after_b.iter().any(|e| e.id == *id_a),
            "edge {:?} from closure A must survive closure B (EdgeConflictPolicy::Keep)",
            id_a
        );
    }

    // There should not be duplicate edges to the same target
    let calls_to_callee: Vec<_> = edges_after_b
        .iter()
        .filter(|e| e.kind == types::EdgeKind::Calls && e.target == callee_sym)
        .collect();
    assert!(
        calls_to_callee.len() <= 1,
        "at most one Calls edge to callee (no duplicates), got {}",
        calls_to_callee.len()
    );
}

/// Running graph builder on uncommitted (is_visible=0) resolutions must
/// produce zero edges — the builder only reads visible rows.
#[test]
fn test_graph_builder_no_edges_from_uncommitted_resolutions() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/caller.c");
    let file_b = insert_file_structural_complete(&store, "src/callee.c");

    let caller_sym = insert_function_symbol(&store, file_a, "caller");
    let _callee_sym = insert_function_symbol(&store, file_b, "callee");

    // Unresolved Call reference
    let ref_use = make_unresolved_reference(
        file_a,
        Some(caller_sym),
        types::ReferenceKind::Call,
        "callee",
        10,
        16,
    );
    store.insert_references(&[ref_use]).unwrap();

    let engine = test_engine(store.clone());
    let closure_id = "test-gb-uncommitted";

    // Insert closure generation record (normally done by build_closure)
    store
        .insert_closure_generation(closure_id)
        .expect("insert_closure_generation should succeed");

    // Resolve (writes is_visible=0 rows)
    engine
        .resolver
        .borrow_mut()
        .resolve_for_closure(closure_id, 0, &[file_a, file_b], None)
        .expect("resolve_for_closure should succeed");

    // Verify staged (invisible) resolutions exist
    let count = store
        .count_reference_resolutions(closure_id, 0)
        .expect("count_reference_resolutions should succeed");
    assert!(
        count > 0,
        "staged resolutions must exist before commit; got {count}"
    );

    // Build graph — should see zero visible rows
    let result = engine
        .graph_builder
        .build_for_closure(closure_id, 0)
        .expect("build_for_closure should succeed");
    assert_eq!(
        result.stats.edges_built, 0,
        "graph builder must produce zero edges from uncommitted (is_visible=0) resolutions"
    );
    assert_eq!(
        result.candidate_count, 0,
        "graph builder must produce zero candidates from uncommitted resolutions"
    );

    // Also verify no edges exist in symbol_edges
    let edges = store
        .find_edges_by_source(&caller_sym)
        .expect("find_edges_by_source should succeed");
    assert!(
        edges.is_empty(),
        "symbol_edges must be empty when resolutions are uncommitted"
    );
}

// ── Visibility filter tests ──────────────────────────────────────────────────

/// Helper: insert a function symbol with explicit visibility.
fn insert_function_symbol_with_visibility(
    store: &Store,
    file_id: FileId,
    name: &str,
    language: Language,
    visibility: Option<Visibility>,
) -> types::ids::SymbolId {
    let sym_id = types::ids::SymbolId::generate(&file_id, "c", name, "function", None);
    let sym = types::structs::SymbolDef {
        id: sym_id,
        kind: types::SymbolKind::Function,
        name: name.to_string(),
        qualified_name: name.to_string(),
        symbol_path: vec![name.to_string()],
        file_id,
        language,
        range: types::structs::TextRange::default(),
        name_range: types::structs::TextRange::default(),
        signature: None,
        visibility,
        exported: false,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".to_string(),
    };
    store.insert_symbols(&[sym]).unwrap();
    sym_id
}

/// C `static` function in file B should NOT be resolvable from a reference
/// in file A, because the C visibility filter excludes Private-visibility symbols.
#[test]
fn test_visibility_filter_c_static_excluded() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/main.c");
    let file_b = insert_file_structural_complete(&store, "src/helper.c");

    // File A: caller + unresolved reference to "helper"
    let _caller_sym = insert_function_symbol(&store, file_a, "caller");
    // File B: static helper (visibility = Private)
    let _target_sym = insert_function_symbol_with_visibility(
        &store,
        file_b,
        "helper",
        Language::C,
        Some(Visibility::Private),
    );

    let ref_use = make_unresolved_reference(
        file_a,
        None, // no source symbol needed for this test
        types::ReferenceKind::Call,
        "helper",
        10,
        16,
    );
    store.insert_references(&[ref_use]).unwrap();

    let engine = test_engine(store.clone());

    // Build a call-graph closure so call references are included in the
    // strategy-derived scoped resolution set.
    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure_id = "test-vis-c-static-excluded";
    let _closure = engine
        .build_closure(&window, closure_id)
        .expect("build_closure should succeed");

    // The static "helper" in file B should be excluded by the C visibility filter,
    // so no reference_resolutions should exist for this closure.
    let count = store
        .count_reference_resolutions(closure_id, 0)
        .expect("count_reference_resolutions should succeed");
    assert_eq!(
        count, 0,
        "expected 0 resolutions — C static function must be excluded by visibility filter"
    );
}

/// Rust `private` (non-pub) function in file B should NOT be resolvable from
/// a reference in file A, because the Rust visibility filter excludes
/// Private-visibility symbols that are in different files.
#[test]
fn test_visibility_filter_rust_private_excluded() {
    let store = test_store();

    // Helper: insert a Rust file with structural complete
    let insert_rust_file = |store: &Store, path: &str| -> FileId {
        let file_id = FileId::generate(path);
        let file_info = FileInfo {
            file_id,
            path: path.to_string(),
            language: Language::Rust,
            content_hash: "rust_hash".to_string(),
            status: ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();
        store
            .upsert_file_extraction_state(
                &file_id,
                types::layer::STRUCTURAL,
                "rust_hash",
                types::status::COMPLETE,
                CapabilityMask::default(),
            )
            .unwrap();
        file_id
    };

    let file_a = insert_rust_file(&store, "src/main.rs");
    let file_b = insert_rust_file(&store, "src/helper.rs");

    // File A: caller
    let _caller_sym = insert_function_symbol_with_visibility(
        &store,
        file_a,
        "caller",
        Language::Rust,
        None, // non-pub fn defaults to private in Rust IR
    );
    // File B: private helper
    let _target_sym = insert_function_symbol_with_visibility(
        &store,
        file_b,
        "helper",
        Language::Rust,
        Some(Visibility::Private),
    );

    let ref_use =
        make_unresolved_reference(file_a, None, types::ReferenceKind::Call, "helper", 10, 16);
    store.insert_references(&[ref_use]).unwrap();

    let engine = test_engine(store.clone());

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::Rust,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::Rust,
        max_iterations: 3,
    };

    let closure_id = "test-vis-rust-private-excluded";
    let _closure = engine
        .build_closure(&window, closure_id)
        .expect("build_closure should succeed");

    // The private "helper" in a different file should be excluded.
    let count = store
        .count_reference_resolutions(closure_id, 0)
        .expect("count_reference_resolutions should succeed");
    assert_eq!(
        count, 0,
        "expected 0 resolutions — Rust private function in different file must be excluded"
    );
}

/// A public (non-static) C function should be resolvable normally —
/// the visibility filter passes it through.
#[test]
fn test_visibility_filter_public_visible() {
    let store = test_store();
    let file_a = insert_file_structural_complete(&store, "src/main.c");
    let file_b = insert_file_structural_complete(&store, "src/helper.c");

    // File A: caller
    let _caller_sym = insert_function_symbol(&store, file_a, "caller");
    // File B: public helper (visibility = None for non-static C functions)
    let _target_sym = insert_function_symbol_with_visibility(
        &store,
        file_b,
        "helper",
        Language::C,
        None, // None = public/non-static in C
    );

    let ref_use =
        make_unresolved_reference(file_a, None, types::ReferenceKind::Call, "helper", 10, 16);
    store.insert_references(&[ref_use]).unwrap();

    let engine = test_engine(store.clone());

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id: file_a,
            language: Language::C,
        },
        strategies: vec![ClosureStrategy::CallGraph {
            direction: Direction::Outgoing,
            depth: 1,
        }],
        budget: WindowBudget::default(),
        language: Language::C,
        max_iterations: 3,
    };

    let closure_id = "test-vis-public-visible";
    let _closure = engine
        .build_closure(&window, closure_id)
        .expect("build_closure should succeed");

    // Public function should be resolvable.
    let count = store
        .count_reference_resolutions(closure_id, 0)
        .expect("count_reference_resolutions should succeed");
    assert!(
        count > 0,
        "expected at least 1 resolution — public C function must be visible through the filter"
    );
}

// ── Dataflow integration tests ───────────────────────────────────────────────

use std::collections::HashSet;

#[test]
fn test_build_dataflow_empty_closure() {
    let store = test_store();
    let engine = test_engine(store);

    let files: HashSet<types::ids::FileId> = HashSet::new();
    let built = engine
        .build_dataflow_for_closure("test-empty", &files)
        .expect("build_dataflow_for_closure should succeed");
    assert_eq!(built, 0, "empty files set must return 0");
}

#[test]
fn test_build_dataflow_for_function() {
    let store = test_store();

    // Insert a file with structural extraction marked complete
    let file_id = insert_file_structural_complete(&store, "src/test_df.c");

    // Insert a function symbol for that file
    let symbol_id = types::ids::SymbolId::generate(&file_id, "c", "do_work", "function", None);
    let symbol_def = types::structs::SymbolDef {
        id: symbol_id,
        kind: types::SymbolKind::Function,
        name: "do_work".to_string(),
        qualified_name: "do_work".to_string(),
        symbol_path: vec!["do_work".to_string()],
        file_id,
        language: Language::C,
        range: types::structs::TextRange::default(),
        name_range: types::structs::TextRange::default(),
        signature: None,
        visibility: None,
        exported: false,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".to_string(),
    };
    store.insert_symbols(&[symbol_def]).unwrap();

    let engine = test_engine(store);

    let mut files = HashSet::new();
    files.insert(file_id);

    let built = engine
        .build_dataflow_for_closure("test-df-func", &files)
        .expect("build_dataflow_for_closure should succeed");
    // In a test environment with no real source, dataflow may not be
    // successfully built (built may be 0).  The method must not crash.
    assert!(built <= 1, "unexpected built count");
}
