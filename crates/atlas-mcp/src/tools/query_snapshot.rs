//! Query snapshot and investigation state — MCP session-scoped query tracking.
//!
//! QuerySnapshot records the original tool call for `resume_query` recovery.
//! InvestigationState tracks the active investigation focus for lazy job
//! prioritization. Both are in-memory (lost on server restart).

use std::time::Instant;

use atlas_engine::{Investigation, InvestigationFocus};
use serde_json::Value;

/// TTL for query snapshots before they expire (default 5 minutes).
pub(crate) const QUERY_SNAPSHOT_TTL_SECS: u64 = 300;

/// Snapshot of an MCP tool call, stored for potential `resume_query`.
#[derive(Debug, Clone)]
pub(crate) struct QuerySnapshot {
    pub query_id: String,
    pub tool_name: String,
    pub tool_args: Value,
    /// Focus state captured by the original query. Its tracker remains live
    /// so resume can observe completion without scheduling new closures.
    pub focus_result: Option<atlas_engine::focus::runtime::FocusResult>,
    pub created_at: Instant,
    pub status: QueryStatus,
}

/// Lifecycle status of a query snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueryStatus {
    /// Result is non-terminal and should be resumed after analysis.retry_after_ms.
    Retryable,
    /// Background refinement is in progress.
    Refining,
    /// All extraction is complete — result is final.
    Ready,
}

/// MCP-session-scoped investigation state and TTL tracking.
#[derive(Debug, Clone)]
pub(crate) struct InvestigationState {
    pub active_investigation: Option<Investigation>,
    pub last_activity: Instant,
}

impl Default for InvestigationState {
    fn default() -> Self {
        Self {
            active_investigation: None,
            last_activity: Instant::now(),
        }
    }
}

impl InvestigationState {
    /// Check whether the investigation has expired (no activity within TTL).
    pub fn is_expired(&self) -> bool {
        self.last_activity.elapsed().as_secs() > QUERY_SNAPSHOT_TTL_SECS
    }

    /// Reset the investigation if it has expired.
    pub fn maybe_clear_expired(&mut self) {
        if self.is_expired() {
            self.active_investigation = None;
        }
    }

    /// Update or create the active investigation based on a tool call focus.
    pub fn update(&mut self, focus: InvestigationFocus) {
        self.maybe_clear_expired();
        self.last_activity = Instant::now();

        let inv = self
            .active_investigation
            .get_or_insert_with(|| Investigation {
                focus: focus.clone(),
                related_symbols: vec![],
                related_files: vec![],
                desired_capabilities: atlas_engine::CapabilityMask::default(),
            });

        match &focus {
            InvestigationFocus::Symbol(sid) => {
                if !inv.related_symbols.contains(sid) {
                    inv.related_symbols.push(*sid);
                }
                inv.desired_capabilities
                    .set(atlas_engine::CapabilityMask::CFG);
                inv.desired_capabilities
                    .set(atlas_engine::CapabilityMask::DATAFLOW);
            }
            InvestigationFocus::Position { file_id, .. } => {
                if !inv.related_files.contains(file_id) {
                    inv.related_files.push(*file_id);
                }
            }
            InvestigationFocus::Field {
                struct_sym,
                field_path: _,
            } => {
                if !inv.related_symbols.contains(struct_sym) {
                    inv.related_symbols.push(*struct_sym);
                }
                inv.desired_capabilities
                    .set(atlas_engine::CapabilityMask::CFG);
                inv.desired_capabilities
                    .set(atlas_engine::CapabilityMask::DATAFLOW);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::{CapabilityMask, InvestigationFocus};
    use std::time::Duration;

    #[test]
    fn test_query_status_default() {
        let status = QueryStatus::Retryable;
        match status {
            QueryStatus::Retryable => {}
            QueryStatus::Refining => {}
            QueryStatus::Ready => {}
        }
    }

    #[test]
    fn test_investigation_state_update_symbol() {
        let mut state = InvestigationState {
            active_investigation: None,
            last_activity: std::time::Instant::now(),
        };
        state.update(InvestigationFocus::Symbol(Default::default()));
        assert!(state.active_investigation.is_some());
        let inv = state.active_investigation.unwrap();
        assert!(matches!(inv.focus, InvestigationFocus::Symbol(_)));
    }

    #[test]
    fn test_investigation_state_update_position() {
        let mut state = InvestigationState {
            active_investigation: None,
            last_activity: std::time::Instant::now(),
        };
        state.update(InvestigationFocus::Position {
            file_id: Default::default(),
            line: 42,
            col: 10,
        });
        assert!(state.active_investigation.is_some());
        let inv = state.active_investigation.unwrap();
        assert!(matches!(
            inv.focus,
            InvestigationFocus::Position { line: 42, .. }
        ));
    }

    #[test]
    fn test_investigation_desired_capabilities() {
        let mut state = InvestigationState {
            active_investigation: None,
            last_activity: std::time::Instant::now(),
        };
        state.update(InvestigationFocus::Symbol(Default::default()));
        let inv = state.active_investigation.as_ref().unwrap();
        // Investigation should request CFG and dataflow
        assert!(inv.desired_capabilities.has(CapabilityMask::CFG));
        assert!(inv.desired_capabilities.has(CapabilityMask::DATAFLOW));
    }

    #[test]
    fn test_query_snapshot_construction() {
        let snapshot = QuerySnapshot {
            query_id: "q_test".into(),
            tool_name: "trace".into(),
            tool_args: serde_json::json!({"line": 1}),
            focus_result: None,
            created_at: std::time::Instant::now(),
            status: QueryStatus::Retryable,
        };
        assert_eq!(snapshot.query_id, "q_test");
        assert_eq!(snapshot.tool_name, "trace");
    }

    #[test]
    fn test_snapshot_ttl_constant() {
        assert_eq!(QUERY_SNAPSHOT_TTL_SECS, 300);
    }

    #[test]
    fn test_snapshot_expiry() {
        let old = QuerySnapshot {
            query_id: "q_old".into(),
            tool_name: "trace".into(),
            tool_args: serde_json::json!({}),
            focus_result: None,
            created_at: std::time::Instant::now() - Duration::from_secs(400),
            status: QueryStatus::Ready,
        };
        let cutoff = std::time::Instant::now() - Duration::from_secs(300);
        assert!(old.created_at <= cutoff, "Old snapshot should be expired");
    }
}
