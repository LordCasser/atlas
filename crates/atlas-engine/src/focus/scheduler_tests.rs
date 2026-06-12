//! Tests for FocusScheduler — priority-queue based focus job scheduling.

use std::sync::Arc;

use db::Store;
use types::ids::FileId;
use types::structs::CapabilityMask;

use crate::investigation::{Investigation, InvestigationFocus};

use super::scheduler::{FocusJob, FocusPriority, FocusScheduler};
use super::types::{ClosureStrategy, FocusSeed, FocusWindow, WindowBudget};

// ── Helpers ─────────────────────────────────────────────────────────────────

fn test_store() -> Arc<Store> {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    Arc::new(store)
}

fn test_scheduler() -> FocusScheduler {
    FocusScheduler::new(test_store())
}

fn test_file_window() -> FocusWindow {
    let file_id = FileId::generate("test.rs");
    FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Default::default(),
        },
        strategies: vec![ClosureStrategy::ImportNeighborhood { depth: 1 }],
        budget: WindowBudget::default(),
        language: Default::default(),
        max_iterations: 1,
    }
}

// ── Extended helpers (for process_sync tests) ──────────────────────────────

fn test_file_with_structural_complete(store: &Store, path: &str) -> types::ids::FileId {
    let file_id = types::ids::FileId::generate(path);
    let file_info = types::structs::FileInfo {
        file_id,
        path: path.to_string(),
        language: types::enums::Language::C,
        content_hash: "abc123".to_string(),
        status: types::enums::ParseStatus::Success,
    };
    store.upsert_file(&file_info).unwrap();
    store
        .upsert_file_extraction_state(
            &file_id,
            types::layer::STRUCTURAL,
            "abc123",
            types::status::COMPLETE,
            CapabilityMask::default(),
        )
        .unwrap();
    file_id
}

fn test_engine_for_store(store: Arc<Store>) -> super::engine::ClosureEngine {
    let lazy_structural =
        crate::lazy_structural::LazyStructuralService::new(store.clone(), None);
    super::engine::ClosureEngine::new(store, lazy_structural, None, vec![])
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn test_scheduler_new_empty() {
    let scheduler = test_scheduler();
    assert!(!scheduler.has_pending(), "new scheduler should have no pending jobs");
    let depths = scheduler.queue_depths();
    for (_priority, depth) in depths {
        assert_eq!(depth, 0, "all queues should be empty in new scheduler");
    }
}

#[test]
fn test_enqueue_sync() {
    let mut scheduler = test_scheduler();
    let window = test_file_window();
    scheduler.enqueue(window, FocusPriority::Sync);

    assert!(scheduler.has_pending(), "should have pending after enqueue");
    let depths = scheduler.queue_depths();
    assert_eq!(depths[0], (FocusPriority::Sync, 1), "sync queue should have 1 job");
    assert_eq!(depths[1], (FocusPriority::UserFocus, 0));
    assert_eq!(depths[2], (FocusPriority::Recent, 0));
    assert_eq!(depths[3], (FocusPriority::Speculative, 0));
}

#[test]
fn test_enqueue_multiple_priorities() {
    let mut scheduler = test_scheduler();
    let window = test_file_window();

    scheduler.enqueue(window.clone(), FocusPriority::Sync);
    scheduler.enqueue(window.clone(), FocusPriority::Speculative);
    scheduler.enqueue(window, FocusPriority::Speculative);

    let depths = scheduler.queue_depths();
    assert_eq!(depths[0], (FocusPriority::Sync, 1));
    assert_eq!(depths[1], (FocusPriority::UserFocus, 0));
    assert_eq!(depths[2], (FocusPriority::Recent, 0));
    assert_eq!(depths[3], (FocusPriority::Speculative, 2));
}

#[test]
fn test_enqueue_returns_job_id() {
    let mut scheduler = test_scheduler();
    let window = test_file_window();
    let id = scheduler.enqueue(window, FocusPriority::Recent);

    assert!(!id.is_empty(), "enqueue should return a non-empty job ID");
    assert!(id.starts_with("fj_"), "job ID should start with fj_");
}

#[test]
fn test_prewarm_investigation() {
    let mut scheduler = test_scheduler();
    let fid1 = FileId::generate("src/main.rs");
    let fid2 = FileId::generate("src/lib.rs");

    let investigation = Investigation {
        focus: InvestigationFocus::Symbol(types::ids::SymbolId::generate(
            &fid1,
            "rust",
            "main",
            "function",
            None,
        )),
        related_symbols: vec![],
        related_files: vec![fid1, fid2],
        desired_capabilities: CapabilityMask::default(),
    };

    scheduler.prewarm_investigation(&investigation);

    let depths = scheduler.queue_depths();
    assert_eq!(
        depths[2],
        (FocusPriority::Recent, 2),
        "prewarm should create one Recent job per related file"
    );
}

#[test]
fn test_on_file_read() {
    let mut scheduler = test_scheduler();
    let file_id = FileId::generate("src/foo.rs");

    scheduler.on_file_read(file_id);

    let depths = scheduler.queue_depths();
    assert_eq!(
        depths[2],
        (FocusPriority::Recent, 1),
        "on_file_read should create one Recent job"
    );
    assert!(scheduler.has_pending());
}

#[test]
fn test_queue_depths() {
    let mut scheduler = test_scheduler();
    let window = test_file_window();

    // Initially all zero
    assert!(scheduler.queue_depths().iter().all(|(_, d)| *d == 0));

    // Enqueue to various queues
    scheduler.enqueue(window.clone(), FocusPriority::Sync);
    scheduler.enqueue(window.clone(), FocusPriority::UserFocus);
    scheduler.enqueue(window.clone(), FocusPriority::Recent);
    scheduler.enqueue(window, FocusPriority::Speculative);

    let depths = scheduler.queue_depths();
    assert_eq!(depths[0], (FocusPriority::Sync, 1));
    assert_eq!(depths[1], (FocusPriority::UserFocus, 1));
    assert_eq!(depths[2], (FocusPriority::Recent, 1));
    assert_eq!(depths[3], (FocusPriority::Speculative, 1));
}

#[test]
fn test_has_pending_true() {
    let mut scheduler = test_scheduler();
    let window = test_file_window();
    scheduler.enqueue(window, FocusPriority::Speculative);

    assert!(scheduler.has_pending());
}

#[test]
fn test_has_pending_false() {
    let scheduler = test_scheduler();
    assert!(!scheduler.has_pending());
}

// ── FocusJob tests ──────────────────────────────────────────────────────────

#[test]
fn test_focus_job_creation() {
    let window = test_file_window();
    let job = FocusJob::new(window, FocusPriority::UserFocus);

    assert!(!job.id.is_empty());
    assert_eq!(job.priority, FocusPriority::UserFocus);
    assert_eq!(job.state, super::types::FocusJobState::Planned);
    assert!(job.closure_id.is_none());
}

#[test]
fn test_focus_priority_ordering() {
    // Lower numeric value = higher priority
    assert!(FocusPriority::Sync < FocusPriority::UserFocus);
    assert!(FocusPriority::UserFocus < FocusPriority::Recent);
    assert!(FocusPriority::Recent < FocusPriority::Speculative);
}

// ── Supplementary: process_sync, priority ordering, no-panic safety ─────

#[test]
fn test_process_sync_drains_queue() {
    let store = test_store();
    let file_id = test_file_with_structural_complete(&store, "main.c");
    let engine = test_engine_for_store(store.clone());
    let mut scheduler = FocusScheduler::new(store).with_engine(engine);

    let window = FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Default::default(),
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Default::default(),
        max_iterations: 1,
    };
    scheduler.enqueue(window, FocusPriority::Sync);
    assert!(
        scheduler.has_pending(),
        "should have pending after enqueue Sync"
    );

    let processed = scheduler
        .process_sync()
        .expect("process_sync should succeed");
    assert_eq!(processed, 1, "process_sync should process 1 job");
    assert!(
        !scheduler.has_pending(),
        "queue should be empty after processing sync jobs"
    );
}

#[test]
fn test_priority_ordering_sync_before_speculative() {
    let store = test_store();
    let file_id = test_file_with_structural_complete(&store, "main.c");
    let engine = test_engine_for_store(store.clone());
    let mut scheduler = FocusScheduler::new(store).with_engine(engine);

    let spec_window = FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Default::default(),
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Default::default(),
        max_iterations: 1,
    };
    let sync_window = FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Default::default(),
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Default::default(),
        max_iterations: 1,
    };

    // Enqueue Speculative first, Sync second
    scheduler.enqueue(spec_window, FocusPriority::Speculative);
    scheduler.enqueue(sync_window, FocusPriority::Sync);

    // process_sync only drains the Sync queue
    let processed = scheduler
        .process_sync()
        .expect("process_sync should succeed");
    assert_eq!(processed, 1, "should process exactly 1 sync job");

    let depths = scheduler.queue_depths();
    assert_eq!(
        depths[0],
        (FocusPriority::Sync, 0),
        "sync queue must be drained"
    );
    assert_eq!(
        depths[3],
        (FocusPriority::Speculative, 1),
        "speculative queue must still have 1 job"
    );
}

#[test]
fn test_scheduler_no_store_panic() {
    // Creating a scheduler with no engine and enqueuing should not panic.
    let mut scheduler = test_scheduler();
    let window = test_file_window();
    scheduler.enqueue(window, FocusPriority::Speculative);

    // has_pending() should not panic — it just checks queue emptiness.
    assert!(scheduler.has_pending(), "should have a pending job");

    // process_sync without engine panics (expect), so we don't call it here.
    // But has_pending and enqueue should be safe without an engine.
    let depths = scheduler.queue_depths();
    assert_eq!(
        depths[3],
        (FocusPriority::Speculative, 1),
        "speculative queue should have 1 job"
    );
}
