//! `atlas_resume` — resume a previous query to get enhanced results after
//! lazy background extraction completes.
//!
//! Re-dispatches to the original handler after re-running lazy extraction on
//! the snapshot's window, so newly cached data is picked up.

use super::query_snapshot::QueryStatus;
use super::ToolRouter;
use serde_json::{Value, json};

impl ToolRouter {
    /// Handle `atlas_resume` — re-run lazy extraction then re-execute the
    /// original tool handler with the same arguments.
    pub(crate) fn handle_resume(&mut self, args: &Value) -> (String, bool) {
        let query_id = crate::tools::get_str(args, "query_id");
        if query_id.is_empty() {
            return (
                serde_json::to_string(&json!({"error": "missing query_id"})).unwrap(),
                true,
            );
        }

        // Prune expired snapshots before lookup
        self.prune_expired_snapshots();

        let snapshot = match self.query_snapshots.get(query_id).cloned() {
            Some(s) => s,
            None => {
                return (
                    serde_json::to_string(
                        &json!({"error": "query not found or expired", "hint": "Query snapshots expire after 5 minutes of inactivity. Re-run the original tool to create a fresh snapshot."}),
                    )
                    .unwrap(),
                    true,
                );
            }
        };

        // Update snapshot status
        if let Some(s) = self.query_snapshots.get_mut(query_id) {
            s.status = QueryStatus::Refining;
        }

        // Re-run lazy extraction for the snapshot's window to pick up newly
        // cached data. The extraction services skip already-cached units.
        if let Some(ref window) = snapshot.lazy_window {
            let include_roots = self.include_roots_from_args(&snapshot.tool_args).0;
            let file_ids: Vec<atlas_engine::FileId> =
                window.units.iter().map(|u| u.file_id).collect();
            let investigation = self.investigation_state.active_investigation.clone();
            if !file_ids.is_empty() {
                let _ = self.ensure_structural_for_files(
                    file_ids,
                    include_roots,
                    investigation.as_ref(),
                    None,
                );
            }
        }

        // Trigger graph refresh so re-dispatched handler sees fresh data.
        let _ = self.maybe_refresh_graph();

        // Re-dispatch to original handler
        let result = match snapshot.tool_name.as_str() {
            "trace_variable" => self.handle_trace_variable(&snapshot.tool_args),
            "trace_point" => self.handle_trace_point(&snapshot.tool_args),
            "trace_caller_path" => self.handle_trace_caller_path(&snapshot.tool_args),
            "trace_forward" => self.handle_trace_forward(&snapshot.tool_args),
            "usages" => self.handle_usages(&snapshot.tool_args),
            "callers" => self.handle_callers(&snapshot.tool_args),
            "callees" => self.handle_callees(&snapshot.tool_args),
            "neighbors" => self.handle_neighbors(&snapshot.tool_args),
            "path" => self.handle_path(&snapshot.tool_args),
            "context" => self.handle_context(&snapshot.tool_args),
            "search" => self.handle_search(&snapshot.tool_args),
            "symbol" => self.handle_symbol(&snapshot.tool_args),
            _ => {
                return (
                    serde_json::to_string(
                        &json!({"error": format!("resume not supported for {}", snapshot.tool_name)}),
                    )
                    .unwrap(),
                    true,
                );
            }
        };

        // Mark as Ready if the re-run completed successfully
        if let Some(s) = self.query_snapshots.get_mut(query_id) {
            s.status = QueryStatus::Ready;
        }

        result
    }
}
