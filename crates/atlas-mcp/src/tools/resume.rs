//! `resume_query` — resume a previous query to get enhanced results after
//! lazy background extraction completes.
//!
//! Re-dispatches to the original handler after re-running lazy extraction on
//! the snapshot's window, so newly cached data is picked up.

use super::CallsDispatch;
use super::ToolRouter;
use super::query_snapshot::QueryStatus;
use super::resolve_calls_dispatch;
use super::tool_contract::{ToolContract, contract_for};
use serde_json::{Value, json};

impl ToolRouter {
    /// Handle `resume_query` — re-run lazy extraction then re-execute the
    /// original tool handler with the same arguments.
    pub(crate) fn handle_resume_query(&self, args: &Value) -> (String, bool) {
        let query_id = crate::tools::get_str(args, "query_id");
        if query_id.is_empty() {
            return (
                serde_json::to_string(&json!({"error": "missing query_id"})).unwrap(),
                true,
            );
        }

        // Prune expired snapshots before lookup
        self.project().job_runtime.prune_expired_snapshots();

        let snapshot = match self
            .project()
            .job_runtime
            .query_snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(query_id)
            .cloned()
        {
            Some(s) => s,
            None => {
                return (
                    serde_json::to_string(
                        &json!({"error": "query not found or expired; query snapshots expire 5 minutes after creation, so re-run the original tool to create a fresh snapshot"}),
                    )
                    .unwrap(),
                    true,
                );
            }
        };

        // Update snapshot status
        if let Some(s) = self
            .project()
            .job_runtime
            .query_snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(query_id)
        {
            s.status = QueryStatus::Refining;
        }

        // Only graph-backed replays need a GraphSnapshot refresh. Store-fact,
        // trace, and CFG/dataflow handlers prepare their own required state.
        if matches!(
            contract_for(&snapshot.tool_name, &snapshot.tool_args),
            ToolContract::SemanticGraphQuery(_)
        ) {
            if let Some(ref focus_result) = snapshot.focus_result {
                let materialized_files = focus_result.materialized_files();
                self.project()
                    .query_runtime
                    .lazy_refresh_queue
                    .record_lazy_writes(&materialized_files);
            }
            if let Err(e) = self.maybe_refresh_graph() {
                tracing::warn!("Graph refresh in resume handler failed: {e:#}");
            }
        }

        // Copy query_id before snapshot moves
        let original_query_id = query_id.to_string();
        let replay_router = ToolRouter::for_resume(self.project(), snapshot.focus_result.clone());

        // Re-dispatch to original handler using unified parameter names.
        // NOTE: Old snapshots with 'symbol_name'/'from_name'/'to_name' fields
        // are NOT auto-mapped — those snapshots must be recreated by the client
        // using the new unified 'symbol'/'from'/'to' parameters.
        let (resp_str, is_error) = match snapshot.tool_name.as_str() {
            "trace" => {
                let kind = snapshot
                    .tool_args
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match kind {
                    "point" => replay_router.handle_trace_point(&super::ToolCallContext::empty(), &snapshot.tool_args),
                    "variable" => replay_router.handle_trace_variable(&snapshot.tool_args),
                    "forward" => replay_router.handle_trace_forward(&snapshot.tool_args),
                    "callers" => replay_router.handle_trace_caller_path(&snapshot.tool_args),
                    _ => return (
                        serde_json::to_string(
                            &json!({"error": format!("resume not supported for trace kind '{}'", kind)}),
                        ).unwrap(),
                        true,
                    ),
                }
            }
            "calls" => match resolve_calls_dispatch(&snapshot.tool_args) {
                CallsDispatch::CallGraph(call_args) => replay_router.handle_callgraph(&call_args),
                CallsDispatch::Callers => replay_router.handle_callers(&snapshot.tool_args),
                CallsDispatch::Callees => replay_router.handle_callees(&snapshot.tool_args),
                CallsDispatch::Error(e) => (e, true),
            },
            "symbol" => {
                let view = snapshot
                    .tool_args
                    .get("view")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match view {
                    "detail" | "" => replay_router.handle_symbol_detail(&snapshot.tool_args),
                    "context" => replay_router.handle_context(&super::ToolCallContext::empty(), &snapshot.tool_args),
                    "usages" => replay_router.handle_usages(&snapshot.tool_args),
                    _ => return (
                        serde_json::to_string(
                            &json!({"error": format!("resume not supported for symbol view '{}'", view)}),
                        ).unwrap(),
                        true,
                    ),
                }
            }
            "search" => {
                replay_router.handle_search(&super::ToolCallContext::empty(), &snapshot.tool_args)
            }
            "path" => replay_router.handle_path(&snapshot.tool_args),
            "explore" => replay_router.handle_explore(&snapshot.tool_args),
            "impact" => replay_router.handle_impact(&snapshot.tool_args),
            "lifecycle" => replay_router.handle_lifecycle(&snapshot.tool_args),
            "branch_diff" => replay_router.handle_branch_diff(&snapshot.tool_args),
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

        // Patch response: keep original query_id so the client can correlate,
        // and add a `resumed_from` field to indicate this is a resume.
        let (patched, remains_partial, generated_query_id) =
            Self::patch_resume_response(&resp_str, &original_query_id)
                .unwrap_or((resp_str, false, None));

        if let Some(generated_query_id) = generated_query_id {
            if generated_query_id != original_query_id {
                self.project()
                    .job_runtime
                    .query_snapshots
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&generated_query_id);
            }
        }

        // Mark as Ready if the re-run completed successfully
        if let Some(s) = self
            .project()
            .job_runtime
            .query_snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&original_query_id)
        {
            s.status = if remains_partial {
                QueryStatus::Partial
            } else {
                QueryStatus::Ready
            };
        }

        (patched, is_error)
    }

    /// Patch a handler response to return the original query_id (not the
    /// one the handler generated internally) and add a `resumed_from` field.
    fn patch_resume_response(
        resp_str: &str,
        original_query_id: &str,
    ) -> Option<(String, bool, Option<String>)> {
        let mut resp: Value = serde_json::from_str(resp_str).ok()?;
        let generated_query_id = resp
            .get("query_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let remains_partial = resp
            .get("analysis")
            .and_then(|analysis| analysis.get("retry_after_ms"))
            .is_some();
        resp["query_id"] = json!(original_query_id);
        resp["resumed_from"] = json!(original_query_id);
        Some((
            serde_json::to_string_pretty(&resp).ok()?,
            remains_partial,
            generated_query_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_resume_response_preserves_partial_state_and_generated_id() {
        let response = json!({
            "query_id": "q_generated",
            "analysis": {"retry_after_ms": 5000},
            "result": "partial"
        })
        .to_string();

        let (patched, remains_partial, generated) =
            ToolRouter::patch_resume_response(&response, "q_original").unwrap();
        let patched: Value = serde_json::from_str(&patched).unwrap();
        assert!(remains_partial);
        assert_eq!(generated.as_deref(), Some("q_generated"));
        assert_eq!(patched["query_id"], "q_original");
        assert_eq!(patched["resumed_from"], "q_original");
    }

    #[test]
    fn patch_resume_response_marks_terminal_without_retry() {
        let response = json!({"query_id": "q_generated", "result": "ready"}).to_string();
        let (_, remains_partial, _) =
            ToolRouter::patch_resume_response(&response, "q_original").unwrap();
        assert!(!remains_partial);
    }
}
