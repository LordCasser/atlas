//! LazyOrchestrator — unified lazy extraction orchestration.
//!
//! Provides a single API surface for MCP handlers, replacing direct
//! `LazyBudget` / `LazyCoordinator` construction with intent-based
//! [`LazyPolicy`] presets.  Callers choose an intent (e.g. foreground
//! structural, background preparse, dataflow trace) and the orchestrator
//! handles budget creation, coordination, and precision reporting.
//!
//! # Usage
//!
//! ```ignore
//! let orchestrator = LazyOrchestrator::new(store, Some(root), vec![]);
//! let outcome = orchestrator.ensure_structural_for_files(
//!     &file_ids,
//!     LazyPolicy::ForegroundStructural,
//! )?;
//! // outcome.built_file_ids, outcome.pending_job_ids, outcome.precision_tier …
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use db::Store;
use types::ids::FileId;
use types::structs::precision::PrecisionTier;

use crate::closure_planner::IncludeRoot;
use crate::lazy_budget::LazyBudget;
use crate::lazy_coordinator::LazyCoordinator;
use crate::lazy_structural::LazyStructuralService;

// ── LazyPolicy ────────────────────────────────────────────────────────────

/// Public policy preset for lazy extraction — callers choose intent, not numbers.
/// Replaces direct `LazyBudget` construction in MCP handler code.
#[derive(Debug, Clone, Copy)]
pub enum LazyPolicy {
    /// Standard foreground structural: 18s / 30 files per MCP request.
    ForegroundStructural,
    /// Background preparse: 60s / 100 files, shared across seeds.
    BackgroundPreparse,
    /// Dataflow trace: 20s / 32 units.
    DataflowTrace,
}

// ── LazyOutcome ───────────────────────────────────────────────────────────

/// Unified outcome from lazy extraction, consumed by MCP response builders.
#[derive(Debug, Clone)]
pub struct LazyOutcome {
    pub files_built: usize,
    pub files_cached: usize,
    pub files_pending: usize,
    pub budget_exceeded: bool,
    pub built_file_ids: Vec<FileId>,
    pub pending_job_ids: Vec<String>,
    pub precision_tier: PrecisionTier,
}

// ── LazyOrchestrator ──────────────────────────────────────────────────────

/// Orchestrates lazy structural extraction with a single API surface.
///
/// MCP handlers call [`ensure_structural_for_files`] or
/// [`ensure_structural_for_symbol`] with a [`LazyPolicy`] — no direct
/// `LazyBudget` or `LazyCoordinator` construction.
pub struct LazyOrchestrator {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
    structural: LazyStructuralService,
    coordinator: LazyCoordinator,
    include_roots: Vec<IncludeRoot>,
}

impl LazyOrchestrator {
    /// Create a new orchestrator with the given store, project root, and
    /// request-scoped include roots (for C/C++ angle-bracket resolution).
    pub fn new(
        store: Arc<Store>,
        project_root: Option<PathBuf>,
        include_roots: Vec<IncludeRoot>,
    ) -> Self {
        let structural = LazyStructuralService::new(store.clone(), project_root.clone());
        let coordinator = if let Some(ref root) = project_root {
            LazyCoordinator::with_project_root(store.clone(), root.clone())
                .with_include_roots(include_roots.clone())
        } else {
            LazyCoordinator::new(store.clone()).with_include_roots(include_roots.clone())
        };
        Self {
            store,
            project_root,
            structural,
            coordinator,
            include_roots,
        }
    }

    /// Ensure structural extraction for a list of files.
    ///
    /// Each file is expanded through its dependency closure (via
    /// [`ClosurePlanner`]), and the coordinator handles job tracking and
    /// in-flight deduplication.  The `policy` controls the time and file
    /// budget.
    ///
    /// Returns an error if `DataflowTrace` policy is passed (this method
    /// only supports structural policies).
    pub fn ensure_structural_for_files(
        &self,
        file_ids: &[FileId],
        policy: LazyPolicy,
    ) -> Result<LazyOutcome> {
        // Validate policy
        match policy {
            LazyPolicy::ForegroundStructural | LazyPolicy::BackgroundPreparse => {}
            LazyPolicy::DataflowTrace => {
                anyhow::bail!(
                    "LazyPolicy::DataflowTrace is not valid for ensure_structural_for_files; \
                     use ForegroundStructural or BackgroundPreparse"
                );
            }
        }

        // Create budget from policy
        let mut budget = match policy {
            LazyPolicy::ForegroundStructural => LazyBudget::structural(),
            LazyPolicy::BackgroundPreparse => LazyBudget::background_preparse(),
            _ => unreachable!(),
        };

        let mut outcome = LazyOutcome {
            files_built: 0,
            files_cached: 0,
            files_pending: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            pending_job_ids: vec![],
            precision_tier: PrecisionTier::Unavailable,
        };

        for file_id in file_ids {
            // Request-level budget check
            if !budget.can_continue() {
                outcome.budget_exceeded = true;
                break;
            }

            let (result, _job_id) =
                self.coordinator
                    .ensure_structural_with_closure(&self.structural, file_id, &mut budget)?;

            outcome.files_built += result.files_built;
            outcome.files_cached += result.files_cached;
            outcome.files_pending += result.files_pending;
            outcome.built_file_ids.extend(result.built_file_ids);
            outcome.pending_job_ids.extend(result.pending_job_ids);

            if result.files_built > 0 {
                budget.consume_file();
            }
        }

        // Compute final precision tier
        outcome.precision_tier = crate::precision::structural_precision(
            outcome.files_built,
            outcome.files_cached,
            outcome.budget_exceeded,
        );

        Ok(outcome)
    }

    /// Ensure structural extraction for a symbol by name.
    ///
    /// Uses the candidate provider to find files likely containing the
    /// symbol, then builds the best candidate's dependency closure.
    /// Returns an error if `DataflowTrace` policy is passed.
    pub fn ensure_structural_for_symbol(
        &self,
        name: &str,
        policy: LazyPolicy,
    ) -> Result<LazyOutcome> {
        // Validate policy
        match policy {
            LazyPolicy::ForegroundStructural | LazyPolicy::BackgroundPreparse => {}
            LazyPolicy::DataflowTrace => {
                anyhow::bail!(
                    "LazyPolicy::DataflowTrace is not valid for ensure_structural_for_symbol; \
                     use ForegroundStructural or BackgroundPreparse"
                );
            }
        }

        // Create budget from policy
        let mut budget = match policy {
            LazyPolicy::ForegroundStructural => LazyBudget::structural(),
            LazyPolicy::BackgroundPreparse => LazyBudget::background_preparse(),
            _ => unreachable!(),
        };

        let result = self.coordinator.ensure_structural_for_symbol_with_closure(
            &self.structural,
            name,
            &mut budget,
        )?;

        // Map EnsureStructuralResult → LazyOutcome
        let precision_tier = crate::precision::structural_precision(
            result.files_built,
            result.files_cached,
            result.budget_exceeded,
        );

        Ok(LazyOutcome {
            files_built: result.files_built,
            files_cached: result.files_cached,
            files_pending: result.files_pending,
            budget_exceeded: result.budget_exceeded,
            built_file_ids: result.built_file_ids,
            pending_job_ids: result.pending_job_ids,
            precision_tier,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use types::ids::FileId;

    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    #[test]
    fn lazy_policy_is_copy() {
        let p = LazyPolicy::ForegroundStructural;
        let _copy = p; // must compile
        assert!(matches!(p, LazyPolicy::ForegroundStructural));
    }

    #[test]
    fn lazy_outcome_defaults() {
        let outcome = LazyOutcome {
            files_built: 0,
            files_cached: 0,
            files_pending: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            pending_job_ids: vec![],
            precision_tier: PrecisionTier::Unavailable,
        };
        assert_eq!(outcome.files_built, 0);
        assert_eq!(outcome.files_pending, 0);
        assert!(outcome.pending_job_ids.is_empty());
    }

    #[test]
    fn ensure_structural_for_files_rejects_dataflow_policy() {
        let store = test_store();
        let orchestrator = LazyOrchestrator::new(store, None, vec![]);
        let fid = FileId::generate("test.rs");
        let err = orchestrator
            .ensure_structural_for_files(&[fid], LazyPolicy::DataflowTrace)
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("DataflowTrace"),
            "error should mention DataflowTrace, got: {msg}"
        );
    }

    #[test]
    fn ensure_structural_for_symbol_rejects_dataflow_policy() {
        let store = test_store();
        let orchestrator = LazyOrchestrator::new(store, None, vec![]);
        let err = orchestrator
            .ensure_structural_for_symbol("test_func", LazyPolicy::DataflowTrace)
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("DataflowTrace"),
            "error should mention DataflowTrace, got: {msg}"
        );
    }
}
