//! Tests for FocusRuntime — query preparation and index mode detection.
//!
//! Test strategy:
//! - Index mode detection is tested by manipulating `project_metadata`.
//! - Focus seed location uses in-memory stores with pre-inserted symbols.
//! - Closure building requires structural extraction state to be marked complete.

use std::sync::Arc;

use db::Store;
use types::enums::{Language, ParseStatus};
use types::ids::{FileId, SymbolId};
use types::structs::{CapabilityMask, FileInfo, SymbolDef, TextRange};
use types::{layer, status};

use crate::focus::bootstrap::BootstrapManager;
use crate::focus::query::QueryIntent;
use crate::focus::runtime::{FocusRuntime, IndexMode};
use crate::focus::scheduler::FocusPriority;

// ── Helpers ─────────────────────────────────────────────────────────────────

fn test_store() -> Arc<Store> {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    Arc::new(store)
}

/// Insert a file and mark its structural layer as complete so that
/// `ensure_structural_for_file` returns cached (no actual extraction needed).
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

/// Insert a symbol definition into the store so that `find_symbols_by_name`
/// and `find_symbol_by_id` can find it.
fn insert_symbol(
    store: &Store,
    file_id: FileId,
    name: &str,
    kind: types::enums::SymbolKind,
) -> SymbolId {
    let sym_id = SymbolId::generate(
        &file_id,
        Language::C.as_str(),
        name,
        kind.as_str(),
        None,
    );
    let sym = SymbolDef {
        id: sym_id,
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        symbol_path: vec![name.to_string()],
        file_id,
        language: Language::C,
        range: TextRange::default(),
        name_range: TextRange::default(),
        signature: None,
        visibility: None,
        exported: false,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: layer::STRUCTURAL.to_string(),
    };
    store.insert_symbols(&[sym]).unwrap();
    sym_id
}

// ── Tests: index mode detection ─────────────────────────────────────────────

#[test]
fn test_detect_full_index_when_metadata_present() {
    let store = test_store();
    store.set_metadata("resolution_generation_version", "1").unwrap();
    let rt = FocusRuntime::new(store, None);
    assert_eq!(rt.detect_index_mode(), IndexMode::FullIndex);
}

#[test]
fn test_detect_focus_when_no_metadata() {
    let store = test_store();
    let rt = FocusRuntime::new(store, None);
    assert_eq!(rt.detect_index_mode(), IndexMode::Focus);
}

#[test]
fn test_detect_full_index_with_config_hash() {
    let store = test_store();
    store.set_metadata("resolution_config_hash", "abc123").unwrap();
    let rt = FocusRuntime::new(store, None);
    assert_eq!(rt.detect_index_mode(), IndexMode::FullIndex);
}

// ── Tests: bootstrap lifecycle ──────────────────────────────────────────────

#[test]
fn test_ensure_started_idempotent() {
    let store = test_store();
    let mut rt = FocusRuntime::new(store, None);
    // First call sets started flag and starts bootstrap
    rt.ensure_started();
    assert!(rt.is_ready(), "bootstrap should be ready after ensure_started");

    // Second call should be a no-op (no panic)
    rt.ensure_started();
    assert!(rt.is_ready(), "bootstrap should still be ready after second call");
}

#[test]
fn test_is_ready_false_before_bootstrap() {
    let store = test_store();
    let rt = FocusRuntime::new(store, None);
    assert!(!rt.is_ready(), "should not be ready before bootstrap is started");
}

#[test]
fn test_is_ready_true_after_bootstrap() {
    let store = test_store();
    let mut rt = FocusRuntime::new(store, None);
    rt.ensure_started();
    assert!(rt.is_ready(), "should be ready after bootstrap is started");
}

// ── Tests: prepare — full index ─────────────────────────────────────────────

#[test]
fn test_prepare_full_index_returns_immediately() {
    let store = test_store();
    store.set_metadata("resolution_generation_version", "1").unwrap();
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: None,
        symbol_id: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::FullIndex);
    assert!(result.precision.is_none(), "FullIndex mode should have no precision");
    assert!(result.gaps.is_empty());
    assert!(result.closure_id.is_none());
}

// ── Tests: prepare — focus path with file seed ──────────────────────────────

#[test]
fn test_prepare_focus_with_calls_file_id() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(result.closure_id.is_some(), "focus path should produce a closure_id");
    assert_eq!(result.seed_file_id, Some(file_id),
        "seed file should match provided file_id");
}

// ── Tests: prepare — focus path with symbol_name only ───────────────────────

#[test]
fn test_prepare_focus_with_calls_symbol_name() {
    let store = test_store();
    // Insert a file with structural complete so candidate search + extraction work
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    // Also insert a symbol so FTS5 candidate provider finds it
    insert_symbol(&store, file_id, "main", types::enums::SymbolKind::Function);
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: None,
        symbol_id: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(result.closure_id.is_some(),
        "focus path with symbol_name should produce a closure_id");
}

// ── Tests: prepare — focus path with symbol_id ──────────────────────────────

#[test]
fn test_prepare_focus_with_calls_symbol_id() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let sym_id = insert_symbol(&store, file_id, "main", types::enums::SymbolKind::Function);
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: None,
        symbol_id: Some(sym_id),
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert_eq!(result.seed_symbol_id, Some(sym_id),
        "seed symbol should match provided symbol_id");
    assert_eq!(result.seed_file_id, Some(file_id),
        "seed file should be resolved from symbol");
}

// ── Tests: precision, gaps, closure_id ─────────────────────────────────────

#[test]
fn test_prepare_focus_returns_precision_and_closure_id() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(result.precision.is_some(), "focus path should return precision");
    let precision = result.precision.unwrap();
    assert_eq!(precision.confidence,
        types::structs::SemanticConfidence::Medium);
    assert!(result.closure_id.is_some(), "should have closure_id");
}

// ── Tests: background enqueue ───────────────────────────────────────────────

#[test]
fn test_prepare_focus_enqueues_background() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
    };
    let _result = rt.prepare(&intent).unwrap();
    // After prepare, the scheduler should have at least one pending job
    // (the background expansion enqueued at UserFocus priority)
    assert!(rt.has_pending_jobs(),
        "scheduler should have pending jobs after prepare() on focus path");
}

// ── Tests: TracePoint intent ────────────────────────────────────────────────

#[test]
fn test_prepare_focus_with_trace_point() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::TracePoint {
        file_id,
        line: 10,
        column: 5,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert_eq!(result.seed_file_id, Some(file_id),
        "seed file should match trace point file");
    assert!(result.closure_id.is_some(), "should produce a closure_id");
}

// ── Tests: BootstrapManager standalone ──────────────────────────────────────

#[test]
fn test_bootstrap_manager_initial_state() {
    let store = test_store();
    let bm = BootstrapManager::new(store, None);
    assert!(!bm.is_minimum_ready(), "bootstrap should not be ready initially");
}

#[test]
fn test_bootstrap_manager_start_makes_ready() {
    let store = test_store();
    let mut bm = BootstrapManager::new(store, None);
    bm.start();
    assert!(bm.is_minimum_ready(), "bootstrap should be ready after start");
}

#[test]
fn test_bootstrap_manager_start_idempotent() {
    let store = test_store();
    let mut bm = BootstrapManager::new(store, None);
    bm.start();
    bm.start(); // should not panic
    assert!(bm.is_minimum_ready());
}

#[test]
fn test_bootstrap_manager_ensure_minimum_ready() {
    let store = test_store();
    let mut bm = BootstrapManager::new(store, None);
    // Start bootstrap first, then ensure_minimum_ready returns immediately
    bm.start();
    bm.ensure_minimum_ready();
    assert!(bm.is_minimum_ready());
}

// ── Tests: coverage_counts ──────────────────────────────────────────────────

#[test]
fn test_prepare_full_index_returns_no_coverage_counts() {
    let store = test_store();
    store.set_metadata("resolution_generation_version", "1").unwrap();
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: None,
        symbol_id: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::FullIndex);
    assert!(
        result.coverage_counts.is_none(),
        "FullIndex mode should have no coverage_counts"
    );
}

#[test]
fn test_prepare_focus_populates_coverage_counts() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(
        result.coverage_counts.is_some(),
        "focus path should populate coverage_counts"
    );
    let counts = result.coverage_counts.unwrap();
    // Seed file + extracted file(s) should appear. The default strategies
    // produce at least one entry (seed_file source).
    assert!(!counts.is_empty(), "coverage_counts should not be empty");
    // The seed_file source string maps as-is (no special mapping).
    // We expect at least a key like "seed_file" with count > 0.
    let total: usize = counts.values().sum();
    assert!(total > 0, "total coverage count should be positive");
}

#[test]
fn test_prepare_focus_coverage_counts_with_symbol_id() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let sym_id = insert_symbol(&store, file_id, "main", types::enums::SymbolKind::Function);
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: None,
        symbol_id: Some(sym_id),
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(
        result.coverage_counts.is_some(),
        "coverage_counts should be populated for symbol_id seed"
    );
}

// ── Tests: prewarm_investigation via prepare() ──────────────────────────

#[test]
fn test_prewarm_called_after_prepare() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
    };
    let _result = rt.prepare(&intent).unwrap();

    // After prepare(), the scheduler should have:
    // - UserFocus job (background expansion)
    // - Recent job(s) (from prewarm_investigation)
    assert!(rt.has_pending_jobs(),
        "scheduler should have pending jobs after prepare()");

    let depths = rt.queue_depths();
    // Recent queue (index 2) must have at least 1 job from prewarm
    assert!(
        depths[2].1 > 0,
        "Recent queue should have prewarm jobs, got depth: {}",
        depths[2].1
    );
}

#[test]
fn test_on_file_read_queues_recent_job() {
    let store = test_store();
    let file_id = FileId::generate("src/read_file.rs");
    let rt = FocusRuntime::new(store, None);

    rt.on_file_read(file_id);

    assert!(rt.has_pending_jobs(),
        "scheduler should have pending jobs after on_file_read");

    let depths = rt.queue_depths();
    assert_eq!(depths[2], (FocusPriority::Recent, 1),
        "on_file_read should create one Recent-priority job");
}
