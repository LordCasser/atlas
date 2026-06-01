//! Unified lazy extraction diagnostics for MCP tool responses.
//!
//! Provides [`LazyDiagnostics`] — a consistent UX contract surfaced in every
//! tool response that triggers lazy extraction.  Handlers construct diagnostics
//! from [`LazyOutcome`] (structural) and [`LazyWindow`] (dataflow), and the
//! serialized JSON includes a `lazy_diagnostics` block with per-layer stats
//! and a recommended next action.

use atlas_engine::LazyOutcome;
use atlas_engine::LazyWindow;
use atlas_engine::structs::precision::PrecisionTier;
use serde::Serialize;

/// Unified lazy extraction diagnostics for MCP tool responses.
///
/// Every response from a handler that triggers lazy extraction surfaces a
/// `lazy_diagnostics` block so agents can understand extraction state and
/// decide what to do next (poll jobs, narrow scope, run a full index).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LazyDiagnostics {
    /// Structural layer diagnostics (None if no lazy structural ran).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) structural: Option<LazyLayerDiagnostics>,
    /// Dataflow layer diagnostics (None if no lazy dataflow ran).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dataflow: Option<LazyLayerDiagnostics>,
    /// Recommended next action for the user/agent.
    pub(crate) next_action: &'static str,
}

/// Per-layer lazy extraction diagnostics.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LazyLayerDiagnostics {
    /// Whether lazy extraction was triggered for this layer.
    pub(crate) triggered: bool,
    /// Number of files/units that were successfully built.
    pub(crate) files_built: usize,
    /// Number of files/units that were already cached (skipped).
    pub(crate) files_cached: usize,
    /// Number of files/units being built by another request (in-flight).
    pub(crate) files_pending: usize,
    /// Job IDs of pending extraction jobs (for polling).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) pending_job_ids: Vec<String>,
    /// Whether the budget was exceeded (results may be incomplete).
    pub(crate) budget_exceeded: bool,
}

impl LazyDiagnostics {
    /// Unified constructor accepting optional structural and dataflow outcomes.
    /// When both are None, returns None (caller should omit the diagnostics field).
    pub(crate) fn from_layers(
        structural_outcome: Option<&LazyOutcome>,
        dataflow_window: Option<&LazyWindow>,
    ) -> Option<Self> {
        if structural_outcome.is_none() && dataflow_window.is_none() {
            return None;
        }

        let structural = structural_outcome.map(|outcome| LazyLayerDiagnostics {
            triggered: true,
            files_built: outcome.files_built,
            files_cached: outcome.files_cached,
            files_pending: outcome.files_pending,
            pending_job_ids: outcome.pending_job_ids.clone(),
            budget_exceeded: outcome.budget_exceeded,
        });

        let dataflow = dataflow_window.map(|window| LazyLayerDiagnostics {
            triggered: true,
            files_built: window.units_built,
            files_cached: window.units_cached,
            files_pending: window.units_pending,
            pending_job_ids: window.pending_job_ids.clone(),
            budget_exceeded: window.truncated,
        });

        // Derive next_action from the combined state.
        let structural_pending = structural
            .as_ref()
            .map_or(false, |s| s.files_pending > 0 && !s.pending_job_ids.is_empty());
        let dataflow_pending = dataflow
            .as_ref()
            .map_or(false, |d| d.files_pending > 0 && !d.pending_job_ids.is_empty());

        let next_action = if structural_pending || dataflow_pending {
            "poll_jobs"
        } else if structural.as_ref().map_or(false, |s| s.budget_exceeded)
            || dataflow.as_ref().map_or(false, |d| d.budget_exceeded)
        {
            "narrow_scope"
        } else if structural
            .as_ref()
            .map_or(false, |s| {
                s.files_built == 0
                    && s.files_cached == 0
                    && s.precision_tier() == PrecisionTier::Unavailable
            })
        {
            "run_full_index"
        } else {
            "none"
        };

        Some(Self {
            structural,
            dataflow,
            next_action,
        })
    }

    /// Create diagnostics from a structural lazy extraction outcome.
    ///
    /// Thin wrapper around [`from_layers`].
    pub(crate) fn from_structural(outcome: &LazyOutcome) -> Self {
        Self::from_layers(Some(outcome), None)
            .expect("from_structural always has a structural outcome")
    }

    /// Create diagnostics from both structural and dataflow layers.
    ///
    /// Thin wrapper around [`from_layers`].
    ///
    /// Used when a single handler triggers both layers (e.g., `trace_variable`
    /// which ensures structural files and dataflow units).
    pub(crate) fn from_both(
        structural_outcome: Option<&LazyOutcome>,
        window: &LazyWindow,
    ) -> Self {
        Self::from_layers(structural_outcome, Some(window))
            .expect("from_both always has at least a dataflow window")
    }
}

impl LazyLayerDiagnostics {
    /// Approximate precision tier from layer diagnostics.
    fn precision_tier(&self) -> PrecisionTier {
        if self.files_built == 0 && self.files_cached == 0 {
            if self.budget_exceeded {
                PrecisionTier::ManifestOnly
            } else {
                PrecisionTier::Unavailable
            }
        } else if self.budget_exceeded {
            PrecisionTier::DegradedStructural
        } else {
            PrecisionTier::Exact
        }
    }
}
