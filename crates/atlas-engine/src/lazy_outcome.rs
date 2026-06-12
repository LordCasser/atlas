//! LazyOutcome — unified outcome from lazy extraction, consumed by MCP response builders.
//!
//! Extracted from `lazy_orchestrator.rs` when the orchestrator was removed in
//! favor of [`FocusRuntime`] as the single control plane.
//!
//! This type is still used by [`LazyDiagnostics`] in `atlas-mcp` to build
//! per-layer diagnostics and analysis contracts.  After the orchestrator
//! removal, callers always pass `None` for the `LazyOutcome` — the type is
//! retained for backward compatibility of the MCP response contract.

use types::ids::FileId;
use types::structs::CapabilityMask;
use types::structs::precision::PrecisionTier;

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

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
