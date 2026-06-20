//! Tests for FocusRuntime — query preparation and index mode detection.
//!
//! Test strategy:
//! - Index mode detection is tested by manipulating `project_metadata`.
//! - Focus seed location uses in-memory stores with pre-inserted symbols.
//! - Closure building requires structural extraction state to be marked complete.

use std::sync::Arc;

use db::Store;
use tempfile::TempDir;
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

fn persistent_test_store() -> (Arc<Store>, TempDir) {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("atlas.db");
    let store = Store::open_db(&db_path).unwrap();
    store.init_schema().unwrap();
    (Arc::new(store), temp_dir)
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
    let sym_id = SymbolId::generate(&file_id, Language::C.as_str(), name, kind.as_str(), None);
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

/// Create a FocusRuntime with index mode overridden to Focus.
///
/// Used by tests that pre-populate structural extraction state (so closure
/// building can use cached results) but need `detect_index_mode()` to return
/// `IndexMode::Focus` for exercising the Focus code path.
fn test_runtime_focus_mode(store: Arc<Store>) -> FocusRuntime {
    let mut rt = FocusRuntime::new(store, None);
    rt.detect_index_mode_override = Some(IndexMode::Focus);
    rt
}

// ── Tests: index mode detection ─────────────────────────────────────────────

#[test]
fn test_detect_focus_when_metadata_present_but_no_extraction() {
    // Metadata keys that the old buggy code used to check — they must
    // NOT fool the new detection into returning FullIndex.
    let store = test_store();
    store
        .set_metadata("resolution_generation_version", "1")
        .unwrap();
    store
        .set_metadata("resolution_config_hash", "abc123")
        .unwrap();
    let rt = FocusRuntime::new(store, None);
    assert_eq!(
        rt.detect_index_mode(),
        IndexMode::Focus,
        "metadata keys should not fool detection; must check fresh extraction state"
    );
}

#[test]
fn test_detect_focus_when_no_metadata() {
    let store = test_store();
    let rt = FocusRuntime::new(store, None);
    assert_eq!(rt.detect_index_mode(), IndexMode::Focus);
}

#[test]
fn test_detect_full_index_with_finalized_structural_extraction() {
    // A file with fresh complete structural extraction plus CLI finalization
    // metadata should make read_index_mode() return a reusable rich mode,
    // triggering FullIndex.
    let store = test_store();
    insert_file_structural_complete(&store, "src/main.c");
    store.set_metadata("last_index_time", "1").unwrap();
    let rt = FocusRuntime::new(store, None);
    assert_eq!(
        rt.detect_index_mode(),
        IndexMode::FullIndex,
        "finalized structural extraction should be detected as FullIndex"
    );
}

#[test]
fn test_detect_focus_with_unfinalized_structural_extraction() {
    // Focus can materialize a small rich closure. Without index-finalization
    // metadata, those rows must remain Focus mode rather than masquerading as
    // project-wide full coverage.
    let store = test_store();
    insert_file_structural_complete(&store, "src/main.c");
    let rt = FocusRuntime::new(store, None);
    assert_eq!(
        rt.detect_index_mode(),
        IndexMode::Focus,
        "unfinalized structural extraction is a focus cache, not a full index"
    );
}

#[test]
fn test_detect_focus_with_only_manifest_extraction() {
    // Manifest extraction alone is not rich — should return Focus.
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/a.c");
    // Remove the structural layer to leave only manifest-like state
    store.delete_file_extraction_state(&file_id).unwrap();
    // Insert only manifest
    store
        .upsert_file_extraction_state(
            &file_id,
            &types::layer::MANIFEST,
            "abc123",
            &types::status::COMPLETE,
            CapabilityMask::default(),
        )
        .unwrap();
    let rt = FocusRuntime::new(store, None);
    assert_eq!(
        rt.detect_index_mode(),
        IndexMode::Focus,
        "manifest-only extraction should not be detected as FullIndex"
    );
}

#[test]
fn test_detect_index_mode_respects_stale_metadata() {
    // Simulate a degraded DB: metadata keys suggest full index,
    // but extraction state is stale (no structural/dataflow rows).
    // detect_index_mode() MUST return Focus because read_index_mode()
    // only counts fresh extraction state rows.
    let store = test_store();
    store
        .set_metadata("resolution_config_hash", "abc123")
        .unwrap();
    store
        .set_metadata("resolution_generation_version", "5")
        .unwrap();
    // No extraction state rows — simulates a DB where the index was
    // downgraded or files were changed, making old metadata irrelevant.
    let rt = FocusRuntime::new(store, None);
    let mode = rt.detect_index_mode();
    assert_eq!(
        mode,
        IndexMode::Focus,
        "stale metadata keys should not fool detection; must check fresh extraction state"
    );
}

// ── Tests: bootstrap lifecycle ──────────────────────────────────────────────

#[test]
fn test_ensure_started_idempotent() {
    let store = test_store();
    let mut rt = FocusRuntime::new(store, None);
    // First call sets started flag and starts bootstrap
    rt.ensure_started();
    assert!(
        rt.is_ready(),
        "bootstrap should be ready after ensure_started"
    );

    // Second call should be a no-op (no panic)
    rt.ensure_started();
    assert!(
        rt.is_ready(),
        "bootstrap should still be ready after second call"
    );
}

#[test]
fn test_is_ready_false_before_bootstrap() {
    let store = test_store();
    let rt = FocusRuntime::new(store, None);
    assert!(
        !rt.is_ready(),
        "should not be ready before bootstrap is started"
    );
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
    // FullIndex requires both rich extraction state (structural layer) AND
    // index-finalization metadata (last_index_time).  Focus-written rich layers
    // alone must NOT trigger FullIndex (detect_index_mode hardening).
    insert_file_structural_complete(&store, "src/main.c");
    store.set_metadata("last_index_time", "1").unwrap();
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: None,
        symbol_id: None,
        direction: None,
        depth: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::FullIndex);
    assert!(
        result.precision.is_none(),
        "FullIndex mode should have no precision"
    );
    assert!(result.gaps.is_empty());
    assert!(result.closure_id.is_none());
}

// ── Tests: prepare — focus path with file seed ──────────────────────────────

#[test]
fn test_prepare_focus_with_calls_file_id() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
        direction: None,
        depth: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(
        result.closure_id.is_some(),
        "focus path should produce a closure_id"
    );
    assert_eq!(
        result.seed_file_id,
        Some(file_id),
        "seed file should match provided file_id"
    );
}

// ── Tests: prepare — focus path with symbol_name only ───────────────────────

#[test]
fn test_prepare_focus_with_calls_symbol_name() {
    let store = test_store();
    // Insert a file with structural complete so candidate search + extraction work
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    // Also insert a symbol so FTS5 candidate provider finds it
    insert_symbol(&store, file_id, "main", types::enums::SymbolKind::Function);
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: None,
        symbol_id: None,
        direction: None,
        depth: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(
        result.closure_id.is_some(),
        "focus path with symbol_name should produce a closure_id"
    );
}

// ── Tests: prepare — focus path with symbol_id ──────────────────────────────

#[test]
fn test_prepare_focus_with_calls_symbol_id() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let sym_id = insert_symbol(&store, file_id, "main", types::enums::SymbolKind::Function);
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: None,
        symbol_id: Some(sym_id),
        direction: None,
        depth: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert_eq!(
        result.seed_symbol_id,
        Some(sym_id),
        "seed symbol should match provided symbol_id"
    );
    assert_eq!(
        result.seed_file_id,
        Some(file_id),
        "seed file should be resolved from symbol"
    );
}

// ── Tests: precision, gaps, closure_id ─────────────────────────────────────

#[test]
fn test_prepare_focus_returns_precision_and_closure_id() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
        direction: None,
        depth: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(
        result.precision.is_some(),
        "focus path should return precision"
    );
    let precision = result.precision.unwrap();
    // Confidence is High because the test store has coverage entries
    // for the closure that was built synchronously.
    assert_eq!(
        precision.confidence,
        types::structs::SemanticConfidence::High
    );
    assert!(result.closure_id.is_some(), "should have closure_id");
}

// ── Tests: background enqueue ───────────────────────────────────────────────

#[test]
fn test_prepare_focus_enqueues_background() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
        direction: None,
        depth: None,
    };
    let _result = rt.prepare(&intent).unwrap();
    // After prepare, the scheduler should have at least one pending job
    // (the background expansion enqueued at UserFocus priority)
    assert!(
        rt.has_pending_jobs(),
        "scheduler should have pending jobs after prepare() on focus path"
    );
}

#[test]
fn test_prepare_boundary_hit_expands_existing_hot_region() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
        direction: None,
        depth: None,
    };

    let first = rt.prepare(&intent).unwrap();
    assert_eq!(
        first.pending_closure_ids.len(),
        1,
        "first query should return only background expansion (foreground is marked done immediately)"
    );
    assert_eq!(
        rt.hot_regions.regions.len(),
        1,
        "first query should establish one hot region"
    );
    let first_depth = rt.hot_regions.regions[0].depth;

    let second = rt.prepare(&intent).unwrap();
    assert_eq!(
        second.pending_closure_ids.len(),
        2,
        "boundary hit should add background expansion plus region-extension closure (no foreground)"
    );
    assert_eq!(
        rt.hot_regions.regions.len(),
        1,
        "boundary hit should expand the existing region instead of creating a new one"
    );
    assert!(
        rt.hot_regions.regions[0].depth > first_depth,
        "existing hot region depth should grow after boundary expansion"
    );
}

#[test]
fn test_memory_hot_region_lru_keeps_recent_region() {
    let store = test_store();
    let files: Vec<FileId> = (0..=10)
        .map(|i| insert_file_structural_complete(&store, &format!("src/file_{i}.c")))
        .collect();
    let mut rt = test_runtime_focus_mode(store);

    for file_id in files.iter().take(10) {
        let intent = QueryIntent::Calls {
            symbol_name: "main".to_string(),
            file_id: Some(*file_id),
            symbol_id: None,
            direction: None,
            depth: None,
        };
        rt.prepare(&intent).unwrap();
    }
    assert_eq!(rt.hot_regions.regions.len(), 10);

    let recent_intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(files[0]),
        symbol_id: None,
        direction: None,
        depth: None,
    };
    rt.prepare(&recent_intent).unwrap();

    let cold_intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(files[10]),
        symbol_id: None,
        direction: None,
        depth: None,
    };
    rt.prepare(&cold_intent).unwrap();

    assert_eq!(
        rt.hot_regions.regions.len(),
        10,
        "in-memory hot regions must stay bounded"
    );
    assert!(
        rt.hot_regions
            .regions
            .iter()
            .any(|region| region.files.contains(&files[0])),
        "recently touched region must not be evicted"
    );
    assert!(
        !rt.hot_regions
            .regions
            .iter()
            .any(|region| region.files.contains(&files[1])),
        "oldest shallow untouched region should be evicted first"
    );
}

#[test]
fn test_prepare_after_memory_hot_region_eviction_still_commits_closure() {
    let store = test_store();
    let files: Vec<FileId> = (0..=10)
        .map(|i| insert_file_structural_complete(&store, &format!("src/evict_{i}.c")))
        .collect();
    let mut rt = test_runtime_focus_mode(store.clone());

    for file_id in &files {
        let intent = QueryIntent::Calls {
            symbol_name: "main".to_string(),
            file_id: Some(*file_id),
            symbol_id: None,
            direction: None,
            depth: None,
        };
        rt.prepare(&intent).unwrap();
    }
    assert_eq!(rt.hot_regions.regions.len(), 10);
    assert!(
        !rt.hot_regions
            .regions
            .iter()
            .any(|region| region.files.contains(&files[0])),
        "first region should have been evicted from in-memory hot-region state"
    );

    let replay_intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(files[0]),
        symbol_id: None,
        direction: None,
        depth: None,
    };
    let replay = rt.prepare(&replay_intent).unwrap();
    let closure_id = replay
        .closure_id
        .expect("prepare should return a foreground closure id");
    let generation = store
        .get_committed_generation(&closure_id)
        .unwrap()
        .expect("replayed evicted region should still commit to the DB");

    assert!(
        generation > 0,
        "closure generation must be committed after memory LRU eviction"
    );
}

#[test]
fn test_persistent_hot_regions_are_not_lru_evicted() {
    let (store, _temp_dir) = persistent_test_store();
    let files: Vec<FileId> = (0..=10)
        .map(|i| insert_file_structural_complete(&store, &format!("src/persist_{i}.c")))
        .collect();
    let mut rt = test_runtime_focus_mode(store);

    for file_id in files {
        let intent = QueryIntent::Calls {
            symbol_name: "main".to_string(),
            file_id: Some(file_id),
            symbol_id: None,
            direction: None,
            depth: None,
        };
        rt.prepare(&intent).unwrap();
    }

    assert_eq!(
        rt.hot_regions.regions.len(),
        11,
        "persistent stores should retain all hot regions instead of applying the in-memory LRU cap"
    );
}

// ── Tests: TracePoint intent ────────────────────────────────────────────────

#[test]
fn test_prepare_focus_with_trace_point() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::TracePoint {
        file_id,
        line: 10,
        column: 5,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert_eq!(
        result.seed_file_id,
        Some(file_id),
        "seed file should match trace point file"
    );
    assert!(result.closure_id.is_some(), "should produce a closure_id");
}

// ── Tests: BootstrapManager standalone ──────────────────────────────────────

#[test]
fn test_bootstrap_manager_initial_state() {
    let store = test_store();
    let bm = BootstrapManager::new(store, None);
    assert!(
        !bm.is_minimum_ready(),
        "bootstrap should not be ready initially"
    );
}

#[test]
fn test_bootstrap_manager_start_makes_ready() {
    let store = test_store();
    let mut bm = BootstrapManager::new(store, None);
    bm.start();
    assert!(
        bm.is_minimum_ready(),
        "bootstrap should be ready after start"
    );
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
    // FullIndex requires both rich extraction state (structural layer) AND
    // index-finalization metadata (last_index_time).  Focus-written rich layers
    // alone must NOT trigger FullIndex (detect_index_mode hardening).
    insert_file_structural_complete(&store, "src/main.c");
    store.set_metadata("last_index_time", "1").unwrap();
    let mut rt = FocusRuntime::new(store, None);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: None,
        symbol_id: None,
        direction: None,
        depth: None,
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
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
        direction: None,
        depth: None,
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
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: None,
        symbol_id: Some(sym_id),
        direction: None,
        depth: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(
        result.coverage_counts.is_some(),
        "coverage_counts should be populated for symbol_id seed"
    );
}

// ── Tests: tracked background expansion via prepare() ───────────────────

#[test]
fn test_prepare_does_not_enqueue_redundant_recent_closures() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
        direction: None,
        depth: None,
    };
    let result = rt.prepare(&intent).unwrap();

    // The single tracked UserFocus expansion is sufficient. Re-enqueuing one
    // Recent closure per foreground file creates hidden N+1 work after tasks
    // reports the query complete.
    assert!(
        rt.has_pending_jobs(),
        "scheduler should have pending jobs after prepare()"
    );
    assert_eq!(result.pending_closure_ids.len(), 1);

    let depths = rt.queue_depths();
    assert_eq!(depths[2], (FocusPriority::Recent, 0));
}

#[test]
fn test_on_file_read_queues_recent_job() {
    let store = test_store();
    let file_id = FileId::generate("src/read_file.rs");
    let rt = FocusRuntime::new(store, None);

    rt.on_file_read(file_id);

    assert!(
        rt.has_pending_jobs(),
        "scheduler should have pending jobs after on_file_read"
    );

    let depths = rt.queue_depths();
    assert_eq!(
        depths[2],
        (FocusPriority::Recent, 1),
        "on_file_read should create one Recent-priority job"
    );
}

// ── Tests: Explore intent ────────────────────────────────────────────────────

#[test]
fn test_prepare_explore_intent() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Explore {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(
        result.closure_id.is_some(),
        "focus path should produce a closure_id"
    );
    assert_eq!(
        result.seed_file_id,
        Some(file_id),
        "seed file should match provided file_id"
    );
}

// ── Tests: Context intent ────────────────────────────────────────────────────

#[test]
fn test_prepare_context_intent() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Context {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert!(
        result.closure_id.is_some(),
        "focus path should produce a closure_id"
    );
}

// ── Tests: TraceVariable intent ──────────────────────────────────────────────

#[test]
fn test_prepare_trace_variable_intent() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::TraceVariable {
        file_id,
        line: 10,
        column: 5,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(result.mode, IndexMode::Focus);
    assert_eq!(
        result.seed_file_id,
        Some(file_id),
        "seed file should match trace variable file"
    );
}

// ── Tests: Search intent ─────────────────────────────────────────────────────

#[test]
fn test_prepare_search_intent() {
    let store = test_store();
    let _file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Search {
        query: "foo".to_string(),
        scope: None,
    };
    let result = rt.prepare(&intent).unwrap();
    assert_eq!(
        result.mode,
        IndexMode::Focus,
        "search should enter focus path"
    );
    assert!(
        result.closure_id.is_some(),
        "search should produce a closure_id"
    );
}

// ── Tests: FullIndex for all intents ─────────────────────────────────────────

#[test]
fn test_prepare_full_index_all_intents() {
    let store = test_store();
    // FullIndex requires both rich extraction state (structural layer) AND
    // index-finalization metadata (last_index_time).  Focus-written rich layers
    // alone must NOT trigger FullIndex (detect_index_mode hardening).
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    store.set_metadata("last_index_time", "1").unwrap();
    let mut rt = FocusRuntime::new(store, None);

    // Test all 8 variants return FullIndex when structural extraction exists
    let intents: Vec<QueryIntent> = vec![
        QueryIntent::Calls {
            symbol_name: "test".into(),
            file_id: Some(file_id),
            symbol_id: None,
            direction: None,
            depth: None,
        },
        QueryIntent::Path {
            from_name: "test".into(),
            to_name: "other".into(),
            max_depth: None,
        },
        QueryIntent::Impact {
            symbol_name: "test".into(),
            depth: None,
        },
        QueryIntent::Explore {
            symbol_name: "test".into(),
            file_id: Some(file_id),
            symbol_id: None,
        },
        QueryIntent::Search {
            query: "test".into(),
            scope: None,
        },
        QueryIntent::Context {
            symbol_name: "test".into(),
            file_id: Some(file_id),
            symbol_id: None,
        },
        QueryIntent::TracePoint {
            file_id,
            line: 1,
            column: 1,
        },
        QueryIntent::TraceVariable {
            file_id,
            line: 1,
            column: 1,
        },
    ];

    for intent in &intents {
        let result = rt.prepare(intent).unwrap();
        assert_eq!(
            result.mode,
            IndexMode::FullIndex,
            "all intents should return FullIndex when structural extraction exists"
        );
        assert!(
            result.precision.is_none(),
            "FullIndex mode should have no precision"
        );
    }
}

// ── Tests: Explore vs Calls same seed behavior ───────────────────────────────

#[test]
fn test_locate_seed_explore_vs_calls_same_behavior() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);

    let calls_intent = QueryIntent::Calls {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
        direction: None,
        depth: None,
    };
    let explore_intent = QueryIntent::Explore {
        symbol_name: "main".to_string(),
        file_id: Some(file_id),
        symbol_id: None,
    };

    let calls_result = rt.prepare(&calls_intent).unwrap();
    let explore_result = rt.prepare(&explore_intent).unwrap();

    assert_eq!(calls_result.mode, IndexMode::Focus);
    assert_eq!(explore_result.mode, IndexMode::Focus);
    assert_eq!(
        calls_result.seed_file_id, explore_result.seed_file_id,
        "Explore and Calls should produce same seed_file_id"
    );
}

// ── Tests: shared lazy dataflow ──────────────────────────────────────────

#[test]
fn test_shared_lazy_dataflow_passed_to_closure_engine() {
    let store = test_store();

    // Create a shared LazyDataflowService with a specific store
    let shared = crate::LazyDataflowService::new(store.clone(), None);

    let mut runtime = test_runtime_focus_mode(store);
    runtime.with_lazy_dataflow(shared);

    // Verify the shared service was stored
    assert!(runtime.shared_lazy_dataflow.is_some());
}

// ── Tests: Calls direction maps to CallGraph strategy ─────────────────────────

#[test]
fn calls_direction_incoming_maps_to_callgraph_strategy() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "test_func".into(),
        direction: Some("incoming".into()),
        depth: None,
        file_id: Some(file_id),
        symbol_id: None,
    };
    let result = rt.prepare(&intent);
    assert!(
        result.is_ok(),
        "prepare() should succeed for Calls with incoming direction"
    );
}

#[test]
fn calls_direction_outgoing_maps_to_callgraph_strategy() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "test_func".into(),
        direction: Some("outgoing".into()),
        depth: None,
        file_id: Some(file_id),
        symbol_id: None,
    };
    let result = rt.prepare(&intent);
    assert!(
        result.is_ok(),
        "prepare() should succeed for Calls with outgoing direction"
    );
}

#[test]
fn calls_direction_none_maps_to_callgraph_strategy() {
    let store = test_store();
    let file_id = insert_file_structural_complete(&store, "src/main.c");
    let mut rt = test_runtime_focus_mode(store);
    let intent = QueryIntent::Calls {
        symbol_name: "test_func".into(),
        direction: None,
        depth: None,
        file_id: Some(file_id),
        symbol_id: None,
    };
    let result = rt.prepare(&intent);
    assert!(
        result.is_ok(),
        "prepare() should succeed for Calls with None direction (maps to Both)"
    );
}
