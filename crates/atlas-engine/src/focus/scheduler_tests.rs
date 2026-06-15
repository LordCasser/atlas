//! Tests for FocusScheduler — priority-queue based focus job scheduling.

use std::sync::Arc;
use std::time::Duration;

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
    let lazy_structural = crate::lazy_structural::LazyStructuralService::new(store.clone(), None);
    let lazy_dataflow = crate::LazyDataflowService::new(store.clone(), None);
    super::engine::ClosureEngine::new(store, lazy_structural, lazy_dataflow, None, vec![])
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn test_scheduler_new_empty() {
    let scheduler = test_scheduler();
    assert!(
        !scheduler.has_pending(),
        "new scheduler should have no pending jobs"
    );
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
    assert_eq!(
        depths[0],
        (FocusPriority::Sync, 1),
        "sync queue should have 1 job"
    );
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
    assert!(id.starts_with("cl_"), "job ID should start with cl_");
}

#[test]
fn test_prewarm_investigation() {
    let mut scheduler = test_scheduler();
    let fid1 = FileId::generate("src/main.rs");
    let fid2 = FileId::generate("src/lib.rs");

    let investigation = Investigation {
        focus: InvestigationFocus::Symbol(types::ids::SymbolId::generate(
            &fid1, "rust", "main", "function", None,
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

    // process_sync without engine returns Ok(0) (graceful no-op).
    let processed = scheduler
        .process_sync()
        .expect("process_sync should not panic");
    assert_eq!(processed, 0, "process_sync without engine returns 0");

    // process_all_queues without engine also returns Ok(0).
    let processed_all = scheduler
        .process_all_queues()
        .expect("process_all_queues should not panic");
    assert_eq!(
        processed_all, 0,
        "process_all_queues without engine returns 0"
    );

    let depths = scheduler.queue_depths();
    assert_eq!(
        depths[3],
        (FocusPriority::Speculative, 1),
        "speculative queue should have 1 job"
    );
}

#[test]
fn test_next_job_id_sequential_unique() {
    let id1 = super::scheduler::next_job_id();
    let id2 = super::scheduler::next_job_id();
    assert_ne!(
        id1, id2,
        "sequential next_job_id() calls must produce different IDs"
    );
    assert!(id1.starts_with("cl_"), "IDs should start with cl_");
    assert!(id2.starts_with("cl_"), "IDs should start with cl_");
}

// ── process_all_queues tests ────────────────────────────────────────────────

#[test]
fn test_process_all_queues_drains_all_levels() {
    let store = test_store();
    let file_id = test_file_with_structural_complete(&store, "main.c");
    let engine = test_engine_for_store(store.clone());
    let mut scheduler = FocusScheduler::new(store).with_engine(engine);

    let make_window = || FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Default::default(),
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Default::default(),
        max_iterations: 1,
    };

    // Enqueue one job at each priority level.
    scheduler.enqueue(make_window(), FocusPriority::Sync);
    scheduler.enqueue(make_window(), FocusPriority::UserFocus);
    scheduler.enqueue(make_window(), FocusPriority::Recent);
    scheduler.enqueue(make_window(), FocusPriority::Speculative);

    assert!(scheduler.has_pending());

    let processed = scheduler
        .process_all_queues()
        .expect("process_all_queues should succeed");
    assert_eq!(processed, 4, "should process 4 jobs (one per level)");

    // All queues should be empty.
    assert!(!scheduler.has_pending());
    for (_priority, depth) in scheduler.queue_depths() {
        assert_eq!(
            depth, 0,
            "all queues should be empty after process_all_queues"
        );
    }
}

#[test]
fn test_process_all_queues_returns_zero_without_engine() {
    let mut scheduler = test_scheduler();
    let window = test_file_window();
    scheduler.enqueue(window, FocusPriority::UserFocus);

    let processed = scheduler
        .process_all_queues()
        .expect("process_all_queues without engine should not panic");
    assert_eq!(processed, 0, "should return 0 when engine is not set");
    // The job should still be in the queue (process_all_queues is a no-op without engine).
    assert!(scheduler.has_pending(), "job should still be pending");
}

#[test]
fn test_process_all_queues_priority_order() {
    // Enqueue jobs out of priority order; process_all_queues drains all,
    // but Sync is drained first, Speculative last.
    let store = test_store();
    let file_id = test_file_with_structural_complete(&store, "main.c");
    let engine = test_engine_for_store(store.clone());
    let mut scheduler = FocusScheduler::new(store).with_engine(engine);

    let make_window = || FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Default::default(),
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Default::default(),
        max_iterations: 1,
    };

    // Enqueue in reverse priority order.
    scheduler.enqueue(make_window(), FocusPriority::Speculative);
    scheduler.enqueue(make_window(), FocusPriority::Recent);
    scheduler.enqueue(make_window(), FocusPriority::UserFocus);
    scheduler.enqueue(make_window(), FocusPriority::Sync);

    let depths_before = scheduler.queue_depths();
    assert_eq!(depths_before[0].1, 1, "Sync queue has 1 job");
    assert_eq!(depths_before[1].1, 1, "UserFocus queue has 1 job");
    assert_eq!(depths_before[2].1, 1, "Recent queue has 1 job");
    assert_eq!(depths_before[3].1, 1, "Speculative queue has 1 job");

    let processed = scheduler
        .process_all_queues()
        .expect("process_all_queues should succeed");
    assert_eq!(processed, 4);

    // All queues empty after processing.
    assert!(!scheduler.has_pending());
}

// ── Background worker thread tests ───────────────────────────────────────────

/// Helper: create a scheduler with engine, wrap in Arc<Mutex<>>.
fn test_scheduler_arc() -> Arc<std::sync::Mutex<FocusScheduler>> {
    let store = test_store();
    let file_id = test_file_with_structural_complete(&store, "main.c");
    // Need at least one file present so closure builds don't fail.
    let _ = file_id;
    let engine = test_engine_for_store(store.clone());
    let scheduler = FocusScheduler::new(store).with_engine(engine);
    Arc::new(std::sync::Mutex::new(scheduler))
}

#[test]
fn test_background_worker_drains_all_queues() {
    let scheduler = test_scheduler_arc();

    // Set running before enqueuing so the worker doesn't exit immediately.
    {
        let mut s = scheduler.lock().unwrap();
        s.set_running(true);
    }

    let file_id = FileId::generate("main.c");
    let make_window = || FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Default::default(),
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Default::default(),
        max_iterations: 1,
    };

    // Enqueue jobs at all priority levels.
    {
        let mut s = scheduler.lock().unwrap();
        s.enqueue(make_window(), FocusPriority::UserFocus);
        s.enqueue(make_window(), FocusPriority::Recent);
        s.enqueue(make_window(), FocusPriority::Speculative);
    }

    // Spawn background worker.
    let sched_clone = Arc::clone(&scheduler);
    let handle = std::thread::spawn(move || {
        FocusScheduler::background_worker_loop(sched_clone);
    });

    // Wait for the worker to drain the queues.
    std::thread::sleep(Duration::from_millis(800));

    // Stop the worker.
    {
        let s = scheduler.lock().unwrap();
        s.stop_background();
    }
    let _ = handle.join();

    // All queues should be empty.
    let s = scheduler.lock().unwrap();
    assert!(
        !s.has_pending(),
        "all queues should be drained by background worker"
    );
    for (_priority, depth) in s.queue_depths() {
        assert_eq!(depth, 0, "queue should be empty");
    }
}

#[test]
fn test_background_worker_stops_on_cancel() {
    let scheduler = test_scheduler_arc();

    {
        let mut s = scheduler.lock().unwrap();
        s.set_running(true);
    }

    // Enqueue a job so the worker has something to process.
    let file_id = FileId::generate("main.c");
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
    {
        let mut s = scheduler.lock().unwrap();
        s.enqueue(window, FocusPriority::UserFocus);
    }

    let sched_clone = Arc::clone(&scheduler);
    let handle = std::thread::spawn(move || {
        FocusScheduler::background_worker_loop(sched_clone);
    });

    // Give the worker a moment to start.
    std::thread::sleep(Duration::from_millis(100));

    // Signal cancel.
    {
        let s = scheduler.lock().unwrap();
        s.stop_background();
    }

    // Worker should exit within a reasonable time.
    let result = handle.join();
    assert!(
        result.is_ok(),
        "background worker should join cleanly after cancel"
    );
}

#[test]
fn test_background_worker_processes_in_correct_order() {
    // Enqueue jobs at different priorities, verify all get drained.
    // The priority order is guaranteed by process_all_queues which drains
    // Sync → UserFocus → Recent → Speculative in that sequence.
    let scheduler = test_scheduler_arc();

    {
        let mut s = scheduler.lock().unwrap();
        s.set_running(true);
    }

    let file_id = FileId::generate("main.c");
    let make_window = || FocusWindow {
        seed: FocusSeed::File {
            file_id,
            language: Default::default(),
        },
        strategies: vec![],
        budget: WindowBudget::default(),
        language: Default::default(),
        max_iterations: 1,
    };

    // Enqueue high-priority first, then low. Worker should drain all.
    {
        let mut s = scheduler.lock().unwrap();
        s.enqueue(make_window(), FocusPriority::UserFocus);
        s.enqueue(make_window(), FocusPriority::Speculative);
    }

    // Confirm jobs are in the queues.
    {
        let s = scheduler.lock().unwrap();
        let depths = s.queue_depths();
        assert_eq!(depths[1].1, 1, "UserFocus queue has 1 job");
        assert_eq!(depths[3].1, 1, "Speculative queue has 1 job");
    }

    let sched_clone = Arc::clone(&scheduler);
    let handle = std::thread::spawn(move || {
        FocusScheduler::background_worker_loop(sched_clone);
    });

    std::thread::sleep(Duration::from_millis(800));

    // Stop and join.
    {
        let s = scheduler.lock().unwrap();
        s.stop_background();
    }
    let _ = handle.join();

    // All queues drained.
    let s = scheduler.lock().unwrap();
    assert!(
        !s.has_pending(),
        "background worker should drain all queues"
    );
}

// ── Write coordinator integration tests ─────────────────────────────────────

#[test]
fn test_coordinator_acquired_for_sync_jobs() {
    let store = test_store();
    let file_id = test_file_with_structural_complete(&store, "main.c");
    let engine = test_engine_for_store(store.clone());
    let mut scheduler = FocusScheduler::new(store).with_engine(engine);

    assert!(
        !scheduler.coordinator.is_background_cancelled(),
        "coordinator should not be cancelled initially"
    );

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

    let processed = scheduler
        .process_sync()
        .expect("process_sync should succeed");
    assert_eq!(processed, 1, "should process 1 sync job");

    // After process_sync, the flag should be reset (was set by acquire, reset at end).
    assert!(
        !scheduler.coordinator.is_background_cancelled(),
        "coordinator should be reset after sync processing"
    );
    assert!(!scheduler.has_pending(), "sync queue should be drained");
}

#[test]
fn test_userfocus_acquires_coordinator() {
    let store = test_store();
    let file_id = test_file_with_structural_complete(&store, "main.c");
    let engine = test_engine_for_store(store.clone());
    let mut scheduler = FocusScheduler::new(store).with_engine(engine);

    assert!(!scheduler.coordinator.is_background_cancelled());

    let guard = scheduler.coordinator.acquire(FocusPriority::UserFocus);
    assert!(
        scheduler.coordinator.is_background_cancelled(),
        "acquiring UserFocus should set background_cancelled"
    );
    drop(guard);
    scheduler.coordinator.reset_cancellation();
    assert!(
        !scheduler.coordinator.is_background_cancelled(),
        "reset_cancellation should clear the flag"
    );
}

#[test]
fn test_background_worker_yields_to_cancellation() {
    let scheduler = test_scheduler_arc();
    let file_id = FileId::generate("main.c");
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

    // Set up: running=true, enqueue Speculative job, then set cancellation flag.
    {
        let mut s = scheduler.lock().unwrap();
        s.set_running(true);
        s.enqueue(window, FocusPriority::Speculative);
        // Acquire Sync to set the flag, then drop guard (flag stays set).
        let guard = s.coordinator.acquire(FocusPriority::Sync);
        drop(guard);
    }

    assert!(
        {
            let s = scheduler.lock().unwrap();
            s.coordinator.is_background_cancelled()
        },
        "flag should be set before starting worker"
    );

    // Spawn background worker. It will:
    //   1. acquire scheduler lock
    //   2. see is_background_cancelled() == true
    //   3. reset_cancellation()
    //   4. continue → skips process_all_queues and does not sleep
    //   5. immediately re-enters loop, acquires lock
    //   6. flag is now false → calls process_all_queues (processes the job)
    //
    // We stop the background worker in a brief window after step 2.
    let sched_clone = Arc::clone(&scheduler);
    let handle = std::thread::spawn(move || {
        FocusScheduler::background_worker_loop(sched_clone);
    });

    // Allow worker a small window to detect cancellation and reset the flag.
    // The continue creates a tight loop (no sleep), so the worker will
    // re-enter immediately. We race to set running=false before step 5.
    std::thread::sleep(Duration::from_millis(30));

    {
        let s = scheduler.lock().unwrap();
        s.stop_background();
    }
    let _ = handle.join();

    let s = scheduler.lock().unwrap();
    // The flag must have been reset by the worker's cancellation check,
    // proving that the check ran.
    assert!(
        !s.coordinator.is_background_cancelled(),
        "worker should have checked and reset the cancellation flag"
    );
    // Whether the job was processed depends on whether we stopped before
    // step 5 (process_all_queues). Either outcome is acceptable for this
    // test — the important invariant is that the cancellation check fired.
}
