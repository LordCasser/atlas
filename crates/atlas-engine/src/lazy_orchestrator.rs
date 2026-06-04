//! LazyOrchestrator — unified lazy extraction orchestration.
//!
//! Provides a single API surface for MCP handlers, replacing direct
//! `LazyBudget` / `LazyCoordinator` construction with intent-based
//! [`LazyPolicy`] presets.  Callers choose an intent (e.g. foreground
//! structural, background preparse) and the orchestrator
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
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use db::Store;
use types::ids::FileId;
use types::structs::CapabilityMask;
use types::structs::precision::PrecisionTier;

use crate::closure_planner::IncludeRoot;
use crate::investigation::Investigation;
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
    pub capability_mask: CapabilityMask,
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
    /// Per-store prewarm guard: at most one background dataflow prewarm
    /// thread per store.  Injected by MCP's ToolRouter so concurrent
    /// requests to the same store share a single guard.
    prewarm_running: Arc<AtomicBool>,
    structural: LazyStructuralService,
    coordinator: LazyCoordinator,
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
            prewarm_running: Arc::new(AtomicBool::new(false)),
            structural,
            coordinator,
        }
    }

    /// Inject a shared prewarm guard.
    ///
    /// When set, the orchestrator will use this flag to deduplicate
    /// background dataflow prewarm threads across concurrent requests
    /// to the same store.  If not set, a fresh per-orchestrator guard
    /// is created in [`new`].
    pub fn with_prewarm_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.prewarm_running = flag;
        self
    }

    /// Ensure structural extraction for a list of files.
    ///
    /// Each file is expanded through its dependency closure (via
    /// [`ClosurePlanner`]), and the coordinator handles job tracking and
    /// in-flight deduplication.  The `policy` controls the time and file
    /// budget.
    ///
    /// When `investigation` is provided, files related to the investigation
    /// are prioritized before unrelated files.
    pub fn ensure_structural_for_files(
        &self,
        file_ids: &[FileId],
        policy: LazyPolicy,
        investigation: Option<&Investigation>,
        query_id: Option<&str>,
    ) -> Result<LazyOutcome> {
        // Create budget from policy
        let mut budget = match policy {
            LazyPolicy::ForegroundStructural => LazyBudget::structural(),
            LazyPolicy::BackgroundPreparse => LazyBudget::background_preparse(),
        };

        let mut outcome = LazyOutcome {
            files_built: 0,
            files_cached: 0,
            files_pending: 0,
            budget_exceeded: false,
            built_file_ids: vec![],
            pending_job_ids: vec![],
            precision_tier: PrecisionTier::Unavailable,
            capability_mask: CapabilityMask::default(),
        };

        // Prioritize files relevant to the active investigation first.
        let mut ordered: Vec<FileId> = file_ids.to_vec();
        if let Some(inv) = investigation {
            ordered.sort_by_key(|fid| {
                if inv.related_files.contains(fid) {
                    0u8
                }
                // highest priority
                else {
                    1u8
                }
            });
        }

        let mut cached_file_ids: Vec<FileId> = Vec::new();

        for file_id in &ordered {
            // Request-level budget check
            if !budget.can_continue() {
                outcome.budget_exceeded = true;
                break;
            }

            let (result, _job_id) = self.coordinator.ensure_structural_with_closure(
                &self.structural,
                file_id,
                &mut budget,
                query_id,
            )?;

            outcome.files_built += result.files_built;
            outcome.files_cached += result.files_cached;
            outcome.files_pending += result.files_pending;
            outcome.built_file_ids.extend(result.built_file_ids);
            cached_file_ids.extend(result.cached_file_ids);
            outcome.pending_job_ids.extend(result.pending_job_ids);
        }

        // Spawn background prewarm for files that just received structural
        // extraction, so subsequent trace queries hit pre-built dataflow.
        // The prewarm_running flag (injected via with_prewarm_flag) prevents
        // duplicate prewarm threads across concurrent MCP requests.
        if let Some(ref root) = self.project_root {
            if !outcome.built_file_ids.is_empty() {
                self.coordinator.spawn_background_prewarm(
                    &self.prewarm_running,
                    Arc::clone(&self.store),
                    root.clone(),
                    outcome.built_file_ids.clone(),
                );
            }
        }

        // Derive capability from actual persistent state instead of hardcoding.
        // Include both the caller's requested files (which after the loop
        // have structural data — either freshly built or cached) and the
        // closure files that were built or cached during the loop.
        {
            let mut all_ids: Vec<FileId> = ordered.clone();
            for fid in &outcome.built_file_ids {
                if !all_ids.contains(fid) {
                    all_ids.push(*fid);
                }
            }
            for fid in &cached_file_ids {
                if !all_ids.contains(fid) {
                    all_ids.push(*fid);
                }
            }
            outcome.capability_mask = self.store.derive_capability_for_files(&all_ids);
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
    pub fn ensure_structural_for_symbol(
        &self,
        name: &str,
        policy: LazyPolicy,
        _investigation: Option<&Investigation>,
        query_id: Option<&str>,
    ) -> Result<LazyOutcome> {
        // Create budget from policy
        let mut budget = match policy {
            LazyPolicy::ForegroundStructural => LazyBudget::structural(),
            LazyPolicy::BackgroundPreparse => LazyBudget::background_preparse(),
        };

        let result = self.coordinator.ensure_structural_for_symbol_with_closure(
            &self.structural,
            name,
            &mut budget,
            query_id,
        )?;

        let mut ids = result.built_file_ids.clone();
        for fid in &result.cached_file_ids {
            if !ids.contains(fid) {
                ids.push(*fid);
            }
        }
        let cap_mask = self.store.derive_capability_for_files(&ids);

        // Spawn background prewarm — same rationale as ensure_structural_for_files.
        if let Some(ref root) = self.project_root {
            if !result.built_file_ids.is_empty() {
                self.coordinator.spawn_background_prewarm(
                    &self.prewarm_running,
                    Arc::clone(&self.store),
                    root.clone(),
                    result.built_file_ids.clone(),
                );
            }
        }

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
            capability_mask: cap_mask,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
            capability_mask: CapabilityMask::default(),
        };
        assert_eq!(outcome.files_built, 0);
        assert_eq!(outcome.files_pending, 0);
        assert!(outcome.pending_job_ids.is_empty());
    }
}
