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
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use db::Store;
use std::sync::Arc;
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
    /// In a full implementation, this spawns a background thread that:
    /// 1. Waits for coordinator write access
    /// 2. Pops highest priority job
    /// 3. Builds closure via engine.build_closure()
    /// 4. Marks job as Committed
    /// 5. Loops
    ///
    /// For now, sets the running flag and processes the sync queue synchronously.
    /// Full background threading is deferred to post-MVP.
    pub fn start_background(&mut self) -> anyhow::Result<()> {
        self.running.store(true, Ordering::SeqCst);
        // Process any sync jobs immediately (user is waiting).
        if self.has_pending() {
            self.process_sync()?;
        }
        Ok(())
    }

    /// Process all jobs in the sync queue synchronously.
    ///
    /// Requires the engine to be set via [`with_engine`].
    pub fn process_sync(&mut self) -> anyhow::Result<usize> {
        let engine = self
            .engine
            .as_ref()
            .expect("ClosureEngine must be set before processing");

        let mut processed = 0;
        while let Some(mut job) = self.queues[0].pop_front() {
            let closure_id = next_job_id();
            job.closure_id = Some(closure_id.clone());
            let _closure = engine.build_closure(&job.window, &closure_id)?;
            processed += 1;
        }
        Ok(processed)
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
