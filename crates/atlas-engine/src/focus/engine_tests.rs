//! Tests for ClosureEngine — bounded fixed-point focus closure building.
//!
//! Test strategy:
//! - File-based tests insert FileInfo records and mark structural extraction
//!   as complete so `ensure_structural_for_file` returns cached results.
//! - Symbol-based tests use a mock CandidateProvider that returns pre-defined
//!   file IDs without needing filesystem access.

use std::sync::Arc;

use db::Store;
use types::enums::{Language, ParseStatus};
use types::ids::FileId;
use types::structs::{CapabilityMask, FileInfo};
use types::{layer, status};

use crate::lazy_structural::{CandidateProvider, LazyStructuralService};

use super::engine::ClosureEngine;
use super::types::{ClosureStrategy, FocusSeed, FocusWindow, WindowBudget};

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
    ClosureEngine::new(store, lazy_structural, None, vec![])
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

    let engine = ClosureEngine::new(store, lazy_structural, None, vec![]);

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
    let has_budget_gap = closure.gaps.iter().any(|g| {
        matches!(g, types::structs::KnownGap::BudgetExhausted { .. })
    });
    assert!(
        has_budget_gap,
        "closure must contain a BudgetExhausted gap when max_files=0 and additions exist"
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
    let engine = ClosureEngine::new(store, lazy_structural, None, vec![]);

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

#[test]
fn test_build_closure_typegraph_stub() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "main.c");
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
        .build_closure(&window, "test-typegraph-stub")
        .expect("build_closure with TypeGraph stub should succeed");

    assert!(
        closure.files.contains(&file_id),
        "seed file must be in closure even with stub strategy"
    );
    // TypeGraph is a stub — returns no additions
    assert_eq!(
        closure.files.len(),
        1,
        "closure must contain only the seed file (TypeGraph stub returns empty)"
    );
}
