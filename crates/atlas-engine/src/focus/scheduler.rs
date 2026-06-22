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
use std::time::Instant;
use std::time::UNIX_EPOCH;
use std::time::{Duration, SystemTime};

use db::Store;
use types::ids::FileId;

use super::engine::ClosureEngine;
use super::job_tracker::JobTracker;
use super::types::{ClosureStrategy, FocusSeed, FocusWindow, WindowBudget};
use super::writer_coordinator::ProjectWriteCoordinator;

/// Global counter for focus job IDs.
static JOB_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_job_id() -> String {
    // Robust: SystemTime::now() is virtually always after UNIX_EPOCH; fall back to 0 on
    // pathological clock (pre-epoch or platform oddity). Counter provides uniqueness.
    let now = SystemTime::now();
    let ts = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
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
    pub closure_id: Option<String>,
}

impl FocusJob {
    pub fn new(window: FocusWindow, priority: FocusPriority) -> Self {
        FocusJob {
            id: next_job_id(),
            window,
            priority,
            closure_id: None,
        }
    }
}

/// Scheduler for background focus closure building.
pub struct FocusScheduler {
    #[allow(dead_code)] // reserved for future scheduler-initiated closure work
    store: Arc<Store>,
    engine: Option<ClosureEngine>,
    queues: Vec<VecDeque<FocusJob>>,
    pub(crate) coordinator: Arc<ProjectWriteCoordinator>,
    running: AtomicBool,
    /// Optional shared tracker for job completion notification.
    /// When set, every dequeued job reaches a terminal tracker state.
    job_tracker: Option<Arc<JobTracker>>,
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
            coordinator: Arc::new(ProjectWriteCoordinator::new()),
            running: AtomicBool::new(false),
            job_tracker: None,
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

    /// Attach a shared JobTracker for completion notification.
    /// When set, every background closure build records either `mark_done` or
    /// `mark_failed`, so query polling always converges.
    pub fn with_job_tracker(mut self, tracker: Arc<JobTracker>) -> Self {
        self.job_tracker = Some(tracker);
        self
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
            let work = {
                let mut s = scheduler.lock().unwrap();
                if !s.running.load(Ordering::SeqCst) {
                    break;
                }
                // Check cancellation before processing — a Sync job may have
                // been enqueued from another thread.
                if s.coordinator.is_background_cancelled() {
                    s.coordinator.reset_cancellation();
                    continue; // let Sync preempt
                }

                match s.engine.take() {
                    Some(engine) => {
                        if let Some(job) = s.pop_next_job() {
                            Some((
                                engine,
                                job,
                                Arc::clone(&s.coordinator),
                                s.job_tracker.clone(),
                            ))
                        } else {
                            s.engine = Some(engine);
                            None
                        }
                    }
                    None => None,
                }
            };

            if let Some((engine, job, coordinator, tracker)) = work {
                let result =
                    Self::process_detached_job(&engine, job, &coordinator, tracker.as_deref());
                {
                    let mut s = scheduler.lock().unwrap();
                    s.engine = Some(engine);
                }
                if let Err(e) = result {
                    tracing::warn!("FocusScheduler background worker error: {e:#}");
                }
            } else {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }

    /// Process all jobs in the sync queue synchronously.
    ///
    /// Requires the engine to be set via [`with_engine`] or [`set_engine`].
    /// Returns 0 (no-op) silently if the engine is not yet initialized.
    ///
    /// Acquires the write coordinator with [`FocusPriority::Sync`] so that
    /// background workers are preempted.
    pub fn process_sync(&mut self) -> anyhow::Result<usize> {
        let engine = match self.engine.as_ref() {
            Some(e) => e,
            None => return Ok(0),
        };
        let _guard = self.coordinator.acquire(FocusPriority::Sync);
        let mut processed = 0;
        while let Some(mut job) = self.queues[0].pop_front() {
            let (closure_id, closure) =
                Self::build_tracked_closure(engine, &mut job, self.job_tracker.as_deref())?;
            processed += 1;
            Self::build_dataflow_for_sync(engine, &closure_id, &closure.files);
            if let Some(ref tracker) = self.job_tracker {
                tracker.mark_done(&job.id);
            }
        }
        self.coordinator.reset_cancellation();
        Ok(processed)
    }

    /// Process all jobs across ALL priority levels.
    ///
    /// Drains queues in priority order: Sync → UserFocus → Recent → Speculative.
    /// Each priority level acquires its own write guard so that a Sync job
    /// enqueued from another thread can preempt lower-priority processing.
    ///
    /// Returns 0 (no-op) silently if the engine is not yet initialized.
    pub fn process_all_queues(&mut self) -> anyhow::Result<usize> {
        let engine = match self.engine.as_ref() {
            Some(e) => e,
            None => return Ok(0),
        };

        let mut processed = 0;

        // Drain Sync first (highest priority)
        {
            let _guard = self.coordinator.acquire(FocusPriority::Sync);
            while let Some(mut job) = self.queues[0].pop_front() {
                let (closure_id, closure) =
                    Self::build_tracked_closure(engine, &mut job, self.job_tracker.as_deref())?;
                processed += 1;
                Self::build_dataflow_for_sync(engine, &closure_id, &closure.files);
                if let Some(ref tracker) = self.job_tracker {
                    tracker.mark_done(&job.id);
                }
            }
        }
        self.coordinator.reset_cancellation();

        // Check cancellation before UserFocus
        if self.coordinator.is_background_cancelled() {
            return Ok(processed);
        }

        // Then UserFocus
        {
            let _guard = self.coordinator.acquire(FocusPriority::UserFocus);
            while let Some(mut job) = self.queues[1].pop_front() {
                Self::build_tracked_closure(engine, &mut job, self.job_tracker.as_deref())?;
                processed += 1;
                if let Some(ref tracker) = self.job_tracker {
                    tracker.mark_done(&job.id);
                }
            }
        }
        // UserFocus acquire sets the flag; clear it so lower levels
        // are only preempted by a truly new cancellation.
        self.coordinator.reset_cancellation();

        // Check cancellation before Recent
        if self.coordinator.is_background_cancelled() {
            return Ok(processed);
        }

        // Then Recent
        {
            let _guard = self.coordinator.acquire(FocusPriority::Recent);
            while let Some(mut job) = self.queues[2].pop_front() {
                Self::build_tracked_closure(engine, &mut job, self.job_tracker.as_deref())?;
                processed += 1;
                if let Some(ref tracker) = self.job_tracker {
                    tracker.mark_done(&job.id);
                }
            }
        }

        // Check cancellation before Speculative
        if self.coordinator.is_background_cancelled() {
            return Ok(processed);
        }

        // Then Speculative (lowest priority)
        {
            let _guard = self.coordinator.acquire(FocusPriority::Speculative);
            while let Some(mut job) = self.queues[3].pop_front() {
                Self::build_tracked_closure(engine, &mut job, self.job_tracker.as_deref())?;
                processed += 1;
                if let Some(ref tracker) = self.job_tracker {
                    tracker.mark_done(&job.id);
                }
            }
        }

        Ok(processed)
    }

    fn pop_next_job(&mut self) -> Option<FocusJob> {
        for queue in &mut self.queues {
            if let Some(job) = queue.pop_front() {
                return Some(job);
            }
        }
        None
    }

    fn process_detached_job(
        engine: &ClosureEngine,
        mut job: FocusJob,
        coordinator: &ProjectWriteCoordinator,
        job_tracker: Option<&JobTracker>,
    ) -> anyhow::Result<()> {
        let should_reset_cancellation =
            matches!(job.priority, FocusPriority::Sync | FocusPriority::UserFocus);
        let start = Instant::now();
        let _guard = coordinator.acquire(job.priority);
        let closure_id = next_job_id();
        job.closure_id = Some(closure_id.clone());
        let closure_result = engine.build_closure(&job.window, &closure_id);
        let closure = match closure_result {
            Ok(closure) => closure,
            Err(err) => {
                if should_reset_cancellation {
                    coordinator.reset_cancellation();
                }
                if let Some(tracker) = job_tracker {
                    tracker.record_elapsed(start.elapsed().as_millis() as u64);
                    tracker.mark_failed(&job.id, format!("{err:#}"));
                }
                return Err(err);
            }
        };
        if let Some(tracker) = job_tracker {
            tracker.record_built_files(&job.id, closure.files.iter().copied());
        }
        if job.priority == FocusPriority::Sync {
            Self::build_dataflow_for_sync(engine, &closure_id, &closure.files);
            coordinator.reset_cancellation();
        } else if job.priority == FocusPriority::UserFocus {
            coordinator.reset_cancellation();
        }
        if let Some(tracker) = job_tracker {
            tracker.record_elapsed(start.elapsed().as_millis() as u64);
            tracker.mark_done(&job.id);
        }
        Ok(())
    }

    fn build_tracked_closure(
        engine: &ClosureEngine,
        job: &mut FocusJob,
        job_tracker: Option<&JobTracker>,
    ) -> anyhow::Result<(String, super::types::FocusClosure)> {
        let closure_id = next_job_id();
        job.closure_id = Some(closure_id.clone());
        match engine.build_closure(&job.window, &closure_id) {
            Ok(closure) => {
                if let Some(tracker) = job_tracker {
                    tracker.record_built_files(&job.id, closure.files.iter().copied());
                }
                Ok((closure_id, closure))
            }
            Err(error) => {
                if let Some(tracker) = job_tracker {
                    tracker.mark_failed(&job.id, format!("{error:#}"));
                }
                Err(error)
            }
        }
    }

    fn build_dataflow_for_sync(
        engine: &ClosureEngine,
        closure_id: &str,
        files: &std::collections::HashSet<FileId>,
    ) {
        // Dataflow extraction can take tens of seconds on large C files.
        // Keep it out of background focus/prewarm queues; tools that need
        // dataflow request it explicitly through their own lazy path.
        let _ = engine.build_dataflow_for_closure(closure_id, files);
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
    pub fn prewarm_investigation(&mut self, investigation: &crate::investigation::Investigation) {
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
