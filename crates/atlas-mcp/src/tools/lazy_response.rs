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
    /// Create diagnostics from a structural lazy extraction outcome.
    ///
    /// Used when only the structural layer was triggered (search, symbol,
    /// context, trace_point, trace_caller_path, trace_forward).
    pub(crate) fn from_structural(outcome: &LazyOutcome) -> Self {
        let structural = Some(LazyLayerDiagnostics {
            triggered: true,
            files_built: outcome.files_built,
            files_cached: outcome.files_cached,
            files_pending: outcome.files_pending,
            pending_job_ids: outcome.pending_job_ids.clone(),
            budget_exceeded: outcome.budget_exceeded,
        });
        let next_action = derive_next_action(
            outcome.files_pending,
            &outcome.pending_job_ids,
            outcome.budget_exceeded,
            outcome.precision_tier,
            false, // no dataflow budget exceeded
        );
        Self {
            structural,
            dataflow: None,
            next_action,
        }
    }

    /// Create diagnostics from a dataflow lazy extraction window.
    ///
    /// Used when only the dataflow layer was triggered.
    pub(crate) fn from_dataflow(window: &LazyWindow) -> Self {
        let dataflow = Some(LazyLayerDiagnostics {
            triggered: true,
            files_built: window.units_built,
            files_cached: window.units_cached,
            files_pending: window.units_pending,
            pending_job_ids: window.pending_job_ids.clone(),
            budget_exceeded: window.truncated,
        });
        let next_action = if window.units_pending > 0 && !window.pending_job_ids.is_empty() {
            "poll_jobs"
        } else if window.truncated {
            "narrow_scope"
        } else {
            "none"
        };
        Self {
            structural: None,
            dataflow,
            next_action,
        }
    }

    /// Create diagnostics from both structural and dataflow layers.
    ///
    /// Used when a single handler triggers both layers (e.g., `trace_variable`
    /// which ensures structural files and dataflow units).
    pub(crate) fn from_both(
        structural_outcome: Option<&LazyOutcome>,
        window: &LazyWindow,
    ) -> Self {
        let structural = structural_outcome.map(|outcome| LazyLayerDiagnostics {
            triggered: true,
            files_built: outcome.files_built,
            files_cached: outcome.files_cached,
            files_pending: outcome.files_pending,
            pending_job_ids: outcome.pending_job_ids.clone(),
            budget_exceeded: outcome.budget_exceeded,
        });

        let dataflow = Some(LazyLayerDiagnostics {
            triggered: true,
            files_built: window.units_built,
            files_cached: window.units_cached,
            files_pending: window.units_pending,
            pending_job_ids: window.pending_job_ids.clone(),
            budget_exceeded: window.truncated,
        });

        // Combined next action: prioritize the most urgent state.
        let structural_pending = structural
            .as_ref()
            .map_or(false, |s| s.files_pending > 0 && !s.pending_job_ids.is_empty());
        let dataflow_pending = window.units_pending > 0 && !window.pending_job_ids.is_empty();

        let next_action = if structural_pending || dataflow_pending {
            "poll_jobs"
        } else if structural.as_ref().map_or(false, |s| s.budget_exceeded) || window.truncated {
            "narrow_scope"
        } else if structural
            .as_ref()
            .map_or(false, |s| {
                s.files_built == 0
                    && s.files_cached == 0
                    && s.precision_tier() == PrecisionTier::Unavailable
            }) {
            "run_full_index"
        } else {
            "none"
        };

        Self {
            structural,
            dataflow,
            next_action,
        }
    }

    /// Create diagnostics when no lazy extraction ran (e.g., already indexed).
    pub(crate) fn none() -> Self {
        Self {
            structural: None,
            dataflow: None,
            next_action: "none",
        }
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

/// Derive the recommended next action from lazy extraction state.
fn derive_next_action(
    files_pending: usize,
    pending_job_ids: &[String],
    budget_exceeded: bool,
    precision_tier: PrecisionTier,
    dataflow_budget_exceeded: bool,
) -> &'static str {
    if files_pending > 0 && !pending_job_ids.is_empty() {
        "poll_jobs"
    } else if budget_exceeded && precision_tier != PrecisionTier::Exact {
        "narrow_scope"
    } else if precision_tier == PrecisionTier::Unavailable {
        "run_full_index"
    } else if dataflow_budget_exceeded {
        "narrow_scope"
    } else {
        "none"
    }
}
