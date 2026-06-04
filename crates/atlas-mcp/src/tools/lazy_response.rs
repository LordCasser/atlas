//! Unified lazy extraction diagnostics for MCP tool responses.
//!
//! Provides [`LazyDiagnostics`] — a consistent UX contract surfaced in every
//! tool response that triggers lazy extraction.  Handlers construct diagnostics
//! from [`LazyOutcome`] (structural) and [`LazyWindow`] (dataflow), and the
//! serialized JSON includes a `lazy_diagnostics` block with per-layer stats
//! and a recommended next action.

use atlas_engine::LazyOutcome;
use atlas_engine::LazyWindow;
use atlas_engine::structs::CapabilityMask;
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
    /// Analysis contract: what conclusions are safe/unsafe given current extraction state.
    pub(crate) analysis_contract: AnalysisContract,
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

// ── Analysis Contract ───────────────────────────────────────────────────

/// Analysis contract: what conclusions are safe/unsafe given current extraction state.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AnalysisContract {
    pub safe_conclusions: Vec<String>,
    pub unsafe_conclusions: Vec<String>,
    pub capability_summary: CapabilitySummary,
    pub refinement_jobs: Vec<RefinementJob>,
}

/// Summary of capability masks available across the project.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CapabilitySummary {
    pub mask_bits: u16,
    pub best_capability: String,
    pub total_files: usize,
    pub files_with_dataflow: usize,
    pub files_with_cfg: usize,
    pub files_structural_only: usize,
    pub files_manifest_only: usize,
}

/// Optional project-wide capability statistics populated from the DB.
/// When `None`, all counts default to 0 (the caller hasn't queried yet).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CapabilityStats {
    pub files_with_dataflow: usize,
    pub files_structural_only: usize,
    pub files_manifest_only: usize,
    pub files_with_cfg: usize,
}

/// A background job that would improve the analysis contract.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RefinementJob {
    pub description: String,
    pub capability_needed: String,
}

impl AnalysisContract {
    /// Build an AnalysisContract from a capability mask and optional LazyOutcome.
    /// `capability_stats` populates file-count breakdowns; pass `None` for zero defaults.
    pub(crate) fn from_capability(
        mask: CapabilityMask,
        outcome: Option<&LazyOutcome>,
        capability_stats: Option<CapabilityStats>,
    ) -> Self {
        let mut safe = Vec::new();
        let mut unsafe_conc = Vec::new();

        if mask.has(CapabilityMask::MANIFEST) {
            safe.push("can resolve symbol names and top-level declarations".into());
        } else {
            unsafe_conc.push("no symbol index available — cannot confirm any symbol exists".into());
        }

        if mask.has(CapabilityMask::STRUCTURAL) {
            safe.push("can confirm all AST-level references and scope relationships".into());
        }

        if mask.has(CapabilityMask::CALL_EDGES) {
            safe.push("can trace direct caller/callee relationships".into());
        } else {
            unsafe_conc
                .push("cannot confirm complete call graph — some calls may be missing".into());
        }

        if mask.has(CapabilityMask::CFG) {
            safe.push("can analyze branch-level control flow".into());
        } else {
            unsafe_conc.push(
                "cannot analyze branch-level control flow — path-sensitive questions are speculative"
                    .into(),
            );
        }

        if mask.has(CapabilityMask::DATAFLOW) {
            safe.push("can trace intra-procedural dataflow (def-use chains)".into());
        } else {
            unsafe_conc.push(
                "cannot confirm dataflow completeness — variable provenance may be incomplete"
                    .into(),
            );
        }

        if mask.has(CapabilityMask::SUMMARIES) {
            safe.push("can trace inter-procedural dataflow via function summaries".into());
        } else {
            unsafe_conc.push(
                "cannot trace dataflow across function boundaries — argument/return flows are not verified"
                    .into(),
            );
        }

        let stats = capability_stats.unwrap_or_default();
        let summary = CapabilitySummary {
            mask_bits: mask.bits(),
            best_capability: mask.best_capability_name().into(),
            total_files: outcome
                .map(|o| o.files_built + o.files_cached + o.files_pending)
                .unwrap_or(0),
            files_with_dataflow: stats.files_with_dataflow,
            files_with_cfg: stats.files_with_cfg,
            files_structural_only: stats.files_structural_only,
            files_manifest_only: stats.files_manifest_only,
        };

        let mut jobs = Vec::new();
        if !mask.has(CapabilityMask::CFG) {
            jobs.push(RefinementJob {
                description: "build CFG for functions in scope".into(),
                capability_needed: "cfg".into(),
            });
        }
        if !mask.has(CapabilityMask::DATAFLOW) {
            jobs.push(RefinementJob {
                description: "build intra-procedural dataflow for functions in scope".into(),
                capability_needed: "dataflow".into(),
            });
        }

        Self {
            safe_conclusions: safe,
            unsafe_conclusions: unsafe_conc,
            capability_summary: summary,
            refinement_jobs: jobs,
        }
    }
}

impl LazyDiagnostics {
    /// Unified constructor accepting optional structural and dataflow outcomes.
    /// When both are None, returns None (caller should omit the diagnostics field).
    /// `capability_stats` is optional DB-sourced file counts; `None` defaults to zero.
    pub(crate) fn from_layers(
        structural_outcome: Option<&LazyOutcome>,
        dataflow_window: Option<&LazyWindow>,
        capability_stats: Option<CapabilityStats>,
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
            .is_some_and(|s| s.files_pending > 0 && !s.pending_job_ids.is_empty());
        let dataflow_pending = dataflow
            .as_ref()
            .is_some_and(|d| d.files_pending > 0 && !d.pending_job_ids.is_empty());

        let next_action = if structural_pending || dataflow_pending {
            "poll_jobs"
        } else if structural.as_ref().is_some_and(|s| s.budget_exceeded)
            || dataflow.as_ref().is_some_and(|d| d.budget_exceeded)
        {
            "narrow_scope"
        } else if structural.as_ref().is_some_and(|s| {
            s.files_built == 0
                && s.files_cached == 0
                && s.precision_tier() == PrecisionTier::Unavailable
        }) {
            "run_full_index"
        } else {
            "none"
        };

        // Compute analysis_contract from the combined capability mask.
        // Merge both structural and dataflow masks via bitwise OR so
        // the contract accurately reflects all built layers.
        let mut mask = CapabilityMask::default();
        if let Some(so) = structural_outcome {
            mask = CapabilityMask::new(mask.bits() | so.capability_mask.bits());
        }
        if let Some(dw) = dataflow_window {
            mask = CapabilityMask::new(mask.bits() | dw.capability_mask.bits());
        }

        Some(Self {
            structural,
            dataflow,
            next_action,
            analysis_contract: AnalysisContract::from_capability(
                mask,
                structural_outcome,
                capability_stats,
            ),
        })
    }

    /// Create diagnostics from a structural lazy extraction outcome.
    ///
    /// Thin wrapper around [`from_layers`].
    pub(crate) fn from_structural(outcome: &LazyOutcome) -> Self {
        Self::from_layers(Some(outcome), None, None)
            .expect("from_structural always has a structural outcome")
    }

    /// Create diagnostics from only a dataflow [`LazySummary`].
    /// Used when structural is already cached (no `LazyOutcome`) but the
    /// Engine triggered lazy dataflow loading.
    pub(crate) fn from_dataflow_summary(summary: &atlas_engine::LazySummary) -> Self {
        let dataflow = LazyLayerDiagnostics::from_lazy_summary(summary);
        let next_action = if dataflow.files_pending > 0 && !dataflow.pending_job_ids.is_empty() {
            "poll_jobs"
        } else if dataflow.budget_exceeded {
            "narrow_scope"
        } else {
            "none"
        };
        Self {
            structural: None,
            dataflow: Some(dataflow),
            next_action,
            // Dataflow extraction always provides at least manifest, structural,
            // call edges, and intra-procedural dataflow (but NOT CFG, which is
            // language-specific).
            analysis_contract: AnalysisContract::from_capability(
                CapabilityMask::new(
                    CapabilityMask::MANIFEST
                        | CapabilityMask::STRUCTURAL
                        | CapabilityMask::CALL_EDGES
                        | CapabilityMask::DATAFLOW,
                ),
                None,
                None,
            ),
        }
    }
}

impl LazyLayerDiagnostics {
    /// Build dataflow-layer diagnostics from Engine's [`LazySummary`].
    /// Used when the Engine handles lazy dataflow internally (P2#14 refactoring)
    /// and the MCP layer constructs combined diagnostics from structural +
    /// Engine-returned dataflow summary.
    pub(crate) fn from_lazy_summary(summary: &atlas_engine::LazySummary) -> Self {
        Self {
            triggered: summary.triggered,
            files_built: summary.units_built,
            files_cached: summary.units_cached,
            files_pending: summary.units_pending,
            pending_job_ids: summary.pending_job_ids.clone(),
            budget_exceeded: summary.truncated,
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::structs::CapabilityMask;

    #[test]
    fn test_analysis_contract_from_manifest_only() {
        let mask = CapabilityMask::new(CapabilityMask::MANIFEST);
        let contract = AnalysisContract::from_capability(mask, None, None);

        // With manifest only, should report limited capabilities
        assert!(!contract.safe_conclusions.is_empty());
        // Should have refinement suggestions for structural
        assert!(!contract.refinement_jobs.is_empty());
    }

    #[test]
    fn test_analysis_contract_from_full_dataflow() {
        let mask = CapabilityMask::new(
            CapabilityMask::MANIFEST
                | CapabilityMask::STRUCTURAL
                | CapabilityMask::CALL_EDGES
                | CapabilityMask::CFG
                | CapabilityMask::DATAFLOW,
        );
        let contract = AnalysisContract::from_capability(mask, None, None);

        // Should report full analysis
        assert!(!contract.safe_conclusions.is_empty());
        // Should acknowledge dataflow capability
        let has_dataflow_conclusion = contract
            .safe_conclusions
            .iter()
            .any(|c| c.contains("dataflow") || c.contains("Dataflow"));
        assert!(
            has_dataflow_conclusion,
            "Should mention dataflow in safe conclusions"
        );
    }

    #[test]
    fn test_analysis_contract_serialization() {
        let mask = CapabilityMask::new(CapabilityMask::STRUCTURAL);
        let contract = AnalysisContract::from_capability(mask, None, None);
        let json = serde_json::to_string(&contract).unwrap();
        assert!(json.contains("safe_conclusions"));
        assert!(json.contains("unsafe_conclusions"));
        assert!(json.contains("refinement_jobs"));
    }

    #[test]
    fn test_capability_summary_serialization() {
        let summary = CapabilitySummary {
            mask_bits: CapabilityMask::MANIFEST | CapabilityMask::STRUCTURAL,
            best_capability: "structural".into(),
            total_files: 10,
            files_with_dataflow: 0,
            files_with_cfg: 0,
            files_structural_only: 8,
            files_manifest_only: 2,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("structural"));
        assert!(json.contains("mask_bits"));
        assert!(json.contains("total_files"));
    }
}
