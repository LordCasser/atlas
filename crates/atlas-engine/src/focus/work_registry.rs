//! Background work aggregation registry.
//!
//! The [`WorkRegistry`] collects work items from multiple [`WorkSource`]
//! implementations into a unified [`WorkView`] suitable for MCP responses.
//! Sources are registered dynamically and polled on each [`WorkRegistry::collect`]
//! call.  The trait-based design keeps the engine crate independent of the MCP
//! crate — the MCP layer implements [`WorkSource`] for its task manager and
//! other background work producers.

use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single background work item ready for the MCP response.
/// Uses PUBLIC vocabulary only — no internal names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItem {
    /// Public task/work id (e.g. "task-00012"). NOT a closure_id or job row id.
    pub id: String,
    /// Kind of work: "full_index" | "lazy_extraction" | "focus_refinement" | "graph_refresh" | "project_activation"
    pub kind: String,
    /// Current state: "running" | "completed" | "failed"
    pub state: String,
    /// Scope: "repo" | "local" | "file"
    pub scope: String,
    /// Human-readable reason for the work.
    pub reason: String,
    /// Progress percentage 0-100.
    pub percent: Option<u8>,
    /// Whether this work can be waited on via the public task API.
    pub waitable: bool,
    /// Suggested retry interval in ms (only when waitable=false).
    pub retry_after_ms: Option<u64>,
}

/// Overall work view status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkStatus {
    Idle,
    Running,
    Completed,
}

/// Aggregated work view returned by [`WorkRegistry::collect`].
#[derive(Debug, Clone)]
pub struct WorkView {
    /// Whether this work is relevant to a specific response (vs. global background).
    pub relevant: bool,
    pub status: WorkStatus,
    pub items: Vec<WorkItem>,
}

impl WorkView {
    /// Create an idle work view with no items.
    pub fn idle() -> Self {
        Self {
            relevant: false,
            status: WorkStatus::Idle,
            items: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// WorkSource trait
// ---------------------------------------------------------------------------

/// Trait for querying work from a specific source.
///
/// Implementations live in the MCP crate (or any layer that produces
/// background work).  The engine crate only knows about this trait — it
/// never references scheduler internals, closure IDs, or extraction-job row
/// IDs directly.
pub trait WorkSource: Send + Sync {
    /// Return the current work items from this source.
    fn current_work(&self) -> Vec<WorkItem>;

    /// Identify the source for deduplication / logging.
    fn source_name(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// WorkRegistry
// ---------------------------------------------------------------------------

/// Registry that aggregates work from multiple [`WorkSource`] implementations.
///
/// Sources are registered once (typically during MCP server startup) and
/// polled on every [`collect`](WorkRegistry::collect) call.
pub struct WorkRegistry {
    sources: Mutex<Vec<Box<dyn WorkSource>>>,
}

impl Default for WorkRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkRegistry {
    pub fn new() -> Self {
        Self {
            sources: Mutex::new(Vec::new()),
        }
    }

    /// Register a work source.  Sources are polled in registration order.
    pub fn register(&self, source: Box<dyn WorkSource>) {
        self.sources.lock().unwrap().push(source);
    }

    /// Collect work from all registered sources into a single [`WorkView`].
    pub fn collect(&self) -> WorkView {
        let sources = self.sources.lock().unwrap();
        let mut items = Vec::new();
        for source in sources.iter() {
            items.extend(source.current_work());
        }
        let status = if items.iter().any(|i| i.state == "running") {
            WorkStatus::Running
        } else if items.is_empty() {
            WorkStatus::Idle
        } else {
            WorkStatus::Completed
        };
        WorkView {
            relevant: false,
            status,
            items,
        }
    }

    /// Quick check: any work running?
    pub fn has_running(&self) -> bool {
        self.collect().status == WorkStatus::Running
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock source that returns a configurable list of items.
    struct MockWorkSource {
        name: &'static str,
        items: Vec<WorkItem>,
        poll_count: AtomicUsize,
    }

    impl MockWorkSource {
        fn new(name: &'static str, items: Vec<WorkItem>) -> Self {
            Self {
                name,
                items,
                poll_count: AtomicUsize::new(0),
            }
        }
    }

    impl WorkSource for MockWorkSource {
        fn current_work(&self) -> Vec<WorkItem> {
            self.poll_count.fetch_add(1, Ordering::SeqCst);
            self.items.clone()
        }

        fn source_name(&self) -> &'static str {
            self.name
        }
    }

    fn make_item(id: &str, kind: &str, state: &str, reason: &str) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            kind: kind.to_string(),
            state: state.to_string(),
            scope: "local".to_string(),
            reason: reason.to_string(),
            percent: None,
            waitable: true,
            retry_after_ms: None,
        }
    }

    // -- WorkView ------------------------------------------------------------

    #[test]
    fn test_work_view_idle() {
        let view = WorkView::idle();
        assert_eq!(view.status, WorkStatus::Idle);
        assert!(view.items.is_empty());
    }

    // -- WorkRegistry empty --------------------------------------------------

    #[test]
    fn test_work_registry_empty() {
        let registry = WorkRegistry::new();
        let view = registry.collect();
        assert_eq!(view.status, WorkStatus::Idle);
        assert!(view.items.is_empty());
    }

    // -- WorkRegistry single source ------------------------------------------

    #[test]
    fn test_work_registry_single_source() {
        let registry = WorkRegistry::new();
        let items = vec![
            make_item(
                "task-00001",
                "lazy_extraction",
                "completed",
                "extracting deps",
            ),
            make_item(
                "task-00002",
                "focus_refinement",
                "running",
                "building focus closure",
            ),
        ];
        let mock = MockWorkSource::new("test-source", items.clone());
        registry.register(Box::new(mock));

        let view = registry.collect();
        assert_eq!(view.items.len(), 2);
        assert_eq!(view.items[0].id, "task-00001");
        assert_eq!(view.items[1].id, "task-00002");
        // One item is "running" → status Running
        assert_eq!(view.status, WorkStatus::Running);
    }

    // -- WorkRegistry running status -----------------------------------------

    #[test]
    fn test_work_registry_running_status() {
        let registry = WorkRegistry::new();
        let items = vec![make_item("t1", "full_index", "running", "indexing")];
        registry.register(Box::new(MockWorkSource::new("s", items)));

        let view = registry.collect();
        assert_eq!(view.status, WorkStatus::Running);
        assert!(registry.has_running());
    }

    #[test]
    fn test_work_registry_completed_status() {
        let registry = WorkRegistry::new();
        let items = vec![make_item("t1", "full_index", "completed", "done")];
        registry.register(Box::new(MockWorkSource::new("s", items)));

        let view = registry.collect();
        assert_eq!(view.status, WorkStatus::Completed);
        assert!(!registry.has_running());
    }

    // -- WorkRegistry multiple sources ---------------------------------------

    #[test]
    fn test_work_registry_multiple_sources() {
        let registry = WorkRegistry::new();
        let src1 = MockWorkSource::new(
            "a",
            vec![make_item("task-00001", "full_index", "running", "idx")],
        );
        let src2 = MockWorkSource::new(
            "b",
            vec![make_item(
                "task-00002",
                "lazy_extraction",
                "completed",
                "lazy",
            )],
        );
        registry.register(Box::new(src1));
        registry.register(Box::new(src2));

        let view = registry.collect();
        assert_eq!(view.items.len(), 2);
        assert_eq!(view.status, WorkStatus::Running);
    }

    // -- WorkRegistry empty source → Idle ------------------------------------

    #[test]
    fn test_work_registry_empty_items_is_idle() {
        let registry = WorkRegistry::new();
        registry.register(Box::new(MockWorkSource::new("empty", vec![])));

        let view = registry.collect();
        assert_eq!(view.status, WorkStatus::Idle);
        assert!(view.items.is_empty());
    }

    // -- WorkRegistry Default ------------------------------------------------

    #[test]
    fn test_work_registry_default() {
        let registry = WorkRegistry::default();
        let view = registry.collect();
        assert_eq!(view.status, WorkStatus::Idle);
    }
}
