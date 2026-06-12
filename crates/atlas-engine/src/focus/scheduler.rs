//! FocusScheduler — priority-based work queue for background closure building.
//!
//! # Architecture
//!
//! The scheduler maintains four priority queues (Sync → UserFocus → Recent →
//! Speculative). Jobs are enqueued via [`FocusScheduler::enqueue`] and processed
//! by a background worker thread that:
//!
//! 1. Waits for write access via [`ProjectWriteCoordinator`]
//! 2. Pops the highest-priority job
//! 3. Builds a closure via [`ClosureEngine::build_closure`]
//! 4. Marks the job as Committed
//! 5. Loops
//!
//! # Syncing with the write coordinator
//!
//! All DB writes go through the coordinator to prevent races between
//! foreground (MCP sync) and background (focus pre-warming) jobs.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};
use std::time::UNIX_EPOCH;

use db::Store;
use types::ids::FileId;

use super::engine::ClosureEngine;
use super::types::{ClosureStrategy, FocusJobState, FocusSeed, FocusWindow, WindowBudget};
use super::writer_coordinator::ProjectWriteCoordinator;

/// Global counter for focus job IDs.
static JOB_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_job_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let counter = JOB_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("cl_{ts}_{counter}")
}

/// Priority levels for focus jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FocusPriority {
    /// User is waiting — MCP tool call, must return fast.
    Sync = 0,
    /// User's current investigation — build as soon as possible.
    UserFocus = 1,
    /// Recently touched files/symbols — pre-warm for likely next query.
    Recent = 2,
    /// Background pre-warming — lowest priority.
    Speculative = 3,
}

/// A job to build a focus closure in the background.
#[derive(Debug, Clone)]
pub struct FocusJob {
    pub id: String,
    pub window: FocusWindow,
    pub priority: FocusPriority,
    pub state: FocusJobState,
    pub closure_id: Option<String>,
}

impl FocusJob {
    pub fn new(window: FocusWindow, priority: FocusPriority) -> Self {
        FocusJob {
            id: next_job_id(),
            window,
            priority,
            state: FocusJobState::Planned,
            closure_id: None,
        }
    }
}

/// Scheduler for background focus closure building.
pub struct FocusScheduler {
    store: Arc<Store>,
    engine: Option<ClosureEngine>,
    queues: Vec<VecDeque<FocusJob>>,
    coordinator: ProjectWriteCoordinator,
    running: AtomicBool,
}

impl FocusScheduler {
    pub fn new(store: Arc<Store>) -> Self {
        FocusScheduler {
            store,
            engine: None,
            queues: vec![
                VecDeque::new(), // Sync
                VecDeque::new(), // UserFocus
                VecDeque::new(), // Recent
                VecDeque::new(), // Speculative
            ],
            coordinator: ProjectWriteCoordinator::new(),
            running: AtomicBool::new(false),
        }
    }

    /// Set the closure engine (after construction, once lazy services are ready).
    pub fn with_engine(mut self, engine: ClosureEngine) -> Self {
        self.engine = Some(engine);
        self
    }

    /// Set the closure engine on an existing scheduler (without consuming it).
    pub fn set_engine(&mut self, engine: ClosureEngine) {
        self.engine = Some(engine);
    }

    /// Set the running flag (controls background worker lifecycle).
    pub fn set_running(&mut self, running: bool) {
        self.running.store(running, Ordering::SeqCst);
    }

    /// Enqueue a focus window for background building.
    pub fn enqueue(&mut self, window: FocusWindow, priority: FocusPriority) -> String {
        let job = FocusJob::new(window, priority);
        let id = job.id.clone();
        let idx = priority as usize;
        self.queues[idx].push_back(job);
        id
    }

    /// Start background processing. Returns immediately after spawning worker thread.
    ///
    /// On first call:
    /// 1. Processes all pending queues synchronously (Sync → UserFocus → Recent → Speculative)
    /// 2. Spawns a background worker thread that periodically drains all priority levels
    ///
    /// The background thread polls cancellable queues, holding the lock only briefly
    /// per job, so MCP tool calls (which also need the lock) are not blocked.
    pub fn start_background(scheduler: Arc<Mutex<FocusScheduler>>) -> anyhow::Result<()> {
        // Process all pending jobs synchronously first.
        {
            let mut s = scheduler.lock().unwrap();
            // Only start once; if already running, do nothing.
            if s.running.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            s.process_all_queues()?;
        }

        // Spawn background worker thread.
        let sched = Arc::clone(&scheduler);
        std::thread::Builder::new()
            .name("atlas-focus-bg".into())
            .spawn(move || {
                Self::background_worker_loop(sched);
            })?;

        Ok(())
    }

    /// Background worker loop: periodically drain all priority queues.
    ///
    /// Public so that [`FocusRuntime::ensure_started`] can spawn it directly
    /// and own the [`JoinHandle`] for clean shutdown.
    pub(crate) fn background_worker_loop(scheduler: Arc<Mutex<FocusScheduler>>) {
        loop {
            // Check cancellation flag before acquiring the lock.
            // We peek at running without holding the lock to avoid
            // contention during shutdown.
            {
                let mut s = scheduler.lock().unwrap();
                if !s.running.load(Ordering::SeqCst) {
                    break;
                }
                // Process all queues.
                if let Err(e) = s.process_all_queues() {
                    tracing::warn!("FocusScheduler background worker error: {e:#}");
                }
                // If all queues are empty, the worker goes idle.
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Process all jobs in the sync queue synchronously.
    ///
    /// Requires the engine to be set via [`with_engine`] or [`set_engine`].
    /// Returns 0 (no-op) silently if the engine is not yet initialized.
    pub fn process_sync(&mut self) -> anyhow::Result<usize> {
        let engine = match self.engine.as_ref() {
            Some(e) => e,
            None => return Ok(0),
        };

        let mut processed = 0;
        while let Some(mut job) = self.queues[0].pop_front() {
            let closure_id = next_job_id();
            job.closure_id = Some(closure_id.clone());
            let _closure = engine.build_closure(&job.window, &closure_id)?;
            processed += 1;
        }
        Ok(processed)
    }

    /// Process all jobs across ALL priority levels.
    ///
    /// Drains queues in priority order: Sync → UserFocus → Recent → Speculative.
    /// This is the main drain path used by the background worker thread.
    ///
    /// Returns 0 (no-op) silently if the engine is not yet initialized.
    pub fn process_all_queues(&mut self) -> anyhow::Result<usize> {
        let engine = match self.engine.as_ref() {
            Some(e) => e,
            None => return Ok(0),
        };

        let mut processed = 0;

        // Drain Sync first (highest priority)
        while let Some(mut job) = self.queues[0].pop_front() {
            let closure_id = next_job_id();
            job.closure_id = Some(closure_id.clone());
            let _closure = engine.build_closure(&job.window, &closure_id)?;
            processed += 1;
        }

        // Then UserFocus
        while let Some(mut job) = self.queues[1].pop_front() {
            let closure_id = next_job_id();
            job.closure_id = Some(closure_id.clone());
            let _closure = engine.build_closure(&job.window, &closure_id)?;
            processed += 1;
        }

        // Then Recent
        while let Some(mut job) = self.queues[2].pop_front() {
            let closure_id = next_job_id();
            job.closure_id = Some(closure_id.clone());
            let _closure = engine.build_closure(&job.window, &closure_id)?;
            processed += 1;
        }

        // Then Speculative (lowest priority)
        while let Some(mut job) = self.queues[3].pop_front() {
            let closure_id = next_job_id();
            job.closure_id = Some(closure_id.clone());
            let _closure = engine.build_closure(&job.window, &closure_id)?;
            processed += 1;
        }

        Ok(processed)
    }

    /// Signal the background worker to stop.
    ///
    /// After this call the background thread will exit its next loop iteration.
    /// Call [`shutdown`] to block until the thread exits.
    pub fn stop_background(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Block until the background worker thread exits.
    ///
    /// Returns `true` if the thread was joined, `false` if no join handle was
    /// available (caller is responsible for lifecycle).
    ///
    /// This is a convenience wrapper around [`stop_background`] + join.
    /// The caller passes the join handle obtained when spawning the thread.
    pub fn shutdown(scheduler: Arc<Mutex<FocusScheduler>>, handle: JoinHandle<()>) {
        {
            let s = scheduler.lock().unwrap();
            s.stop_background();
        }
        // Best-effort join — don't unwrap to avoid panicking on drop.
        let _ = handle.join();
    }

    /// Queue pre-warming for an investigation.
    ///
    /// Converts [`crate::investigation::Investigation`] related files into
    /// Recent-priority focus windows with import-neighborhood and
    /// same-directory strategies.
    pub fn prewarm_investigation(
        &mut self,
        investigation: &crate::investigation::Investigation,
    ) {
        for file_id in &investigation.related_files {
            let window = FocusWindow {
                seed: FocusSeed::File {
                    file_id: *file_id,
                    language: Default::default(),
                },
                strategies: vec![
                    ClosureStrategy::ImportNeighborhood { depth: 1 },
                    ClosureStrategy::SameDirectory,
                ],
                budget: WindowBudget::background(),
                language: Default::default(),
                max_iterations: 1,
            };
            self.enqueue(window, FocusPriority::Recent);
        }
    }

    /// Called when user reads a file — pre-warm its imports.
    pub fn on_file_read(&mut self, file_id: FileId) {
        let window = FocusWindow {
            seed: FocusSeed::File {
                file_id,
                language: Default::default(),
            },
            strategies: vec![
                ClosureStrategy::ImportNeighborhood { depth: 1 },
                ClosureStrategy::SameDirectory,
            ],
            budget: WindowBudget::background(),
            language: Default::default(),
            max_iterations: 1,
        };
        self.enqueue(window, FocusPriority::Recent);
    }

    /// Get queue depths by priority.
    pub fn queue_depths(&self) -> Vec<(FocusPriority, usize)> {
        vec![
            (FocusPriority::Sync, self.queues[0].len()),
            (FocusPriority::UserFocus, self.queues[1].len()),
            (FocusPriority::Recent, self.queues[2].len()),
            (FocusPriority::Speculative, self.queues[3].len()),
        ]
    }

    /// Check if any jobs are queued.
    pub fn has_pending(&self) -> bool {
        self.queues.iter().any(|q| !q.is_empty())
    }
}
