//! `resume_task` — resume a previous query to get enhanced results after
//! lazy background extraction completes.
//!
//! Re-dispatches to the original handler after re-running lazy extraction on
//! the snapshot's window, so newly cached data is picked up.

use super::ToolRouter;
use super::query_snapshot::QueryStatus;
use serde_json::{Value, json};

impl ToolRouter {
    /// Handle `resume_task` — re-run lazy extraction then re-execute the
    /// original tool handler with the same arguments.
    pub(crate) fn handle_resume_task(&mut self, args: &Value) -> (String, bool) {
        let query_id = crate::tools::get_str(args, "query_id");
        if query_id.is_empty() {
            return (
                serde_json::to_string(&json!({"error": "missing query_id"})).unwrap(),
                true,
            );
        }

        // Prune expired snapshots before lookup
        self.prune_expired_snapshots();

        let snapshot = match self.query_snapshots.lock().unwrap().get(query_id).cloned() {
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
        if let Some(s) = self.query_snapshots.lock().unwrap().get_mut(query_id) {
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

            // Re-trigger dataflow extraction for each function-scoped unit
            // in the lazy window, so re-dispatched handlers see fresh
            // dataflow results. Top-level units (symbol_id is None) are
            // skipped because LazyDataflowService only supports
            // function-level dataflow via `ensure_for_function`.
            for unit in &window.units {
                if let Some(ref sid) = unit.symbol_id {
                    let _ = self.lazy_service.ensure_for_function(sid, None);
                }
            }
        }

        // Trigger graph refresh so re-dispatched handler sees fresh data.
        let _ = self.maybe_refresh_graph();

        // Copy query_id before snapshot moves
        let original_query_id = query_id.to_string();

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
                    "point" => self.handle_trace_point(&super::ToolCallContext::empty(), &snapshot.tool_args),
                    "variable" => self.handle_trace_variable(&snapshot.tool_args),
                    "forward" => self.handle_trace_forward(&snapshot.tool_args),
                    "callers" => self.handle_trace_caller_path(&snapshot.tool_args),
                    _ => return (
                        serde_json::to_string(
                            &json!({"error": format!("resume not supported for trace kind '{}'", kind)}),
                        ).unwrap(),
                        true,
                    ),
                }
            }
            "calls" => {
                let direction = snapshot
                    .tool_args
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let depth = snapshot
                    .tool_args
                    .get("depth")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                // Distinguish "not provided" (→ default) from explicit "[]" (→ wildcard).
                let raw_kinds = snapshot
                    .tool_args
                    .get("edge_kinds")
                    .and_then(|v| v.as_array());
                let (is_wildcard, edge_kinds_is_custom): (bool, bool) = match raw_kinds {
                    Some(arr) => {
                        let kinds: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                        let wildcard = kinds.is_empty();
                        let custom = !wildcard && kinds != ["calls", "instantiates", "implements"];
                        (wildcard, custom)
                    }
                    None => (false, false),
                };
                // Custom edge_kinds, wildcard (explicit []), multi-hop, or bidirectional
                // → callgraph (handles all properly)
                if is_wildcard
                    || edge_kinds_is_custom
                    || depth > 1
                    || direction == "both"
                    || direction.is_empty()
                {
                    // handle_callgraph internally defaults depth to 3, but our
                    // schema default is 1. Inject depth when not user-specified.
                    let call_args = if snapshot.tool_args.get("depth").is_none() {
                        let mut m = serde_json::Map::new();
                        if let Some(obj) = snapshot.tool_args.as_object() {
                            m.clone_from(obj);
                        }
                        m.insert(
                            "depth".into(),
                            serde_json::Value::Number(serde_json::Number::from(depth)),
                        );
                        serde_json::Value::Object(m)
                    } else {
                        snapshot.tool_args.clone()
                    };
                    self.handle_callgraph(&call_args)
                } else {
                    match direction {
                        "incoming" => self.handle_callers(&snapshot.tool_args),
                        "outgoing" => self.handle_callees(&snapshot.tool_args),
                        _ => return (
                            serde_json::to_string(
                                &json!({"error": format!("resume not supported for calls direction '{}'", direction)}),
                            ).unwrap(),
                            true,
                        ),
                    }
                }
            }
            "symbol" => {
                let view = snapshot
                    .tool_args
                    .get("view")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match view {
                    "detail" | "" => self.handle_symbol_detail(&snapshot.tool_args),
                    "context" => self.handle_context(&super::ToolCallContext::empty(), &snapshot.tool_args),
                    "usages" => self.handle_usages(&snapshot.tool_args),
                    _ => return (
                        serde_json::to_string(
                            &json!({"error": format!("resume not supported for symbol view '{}'", view)}),
                        ).unwrap(),
                        true,
                    ),
                }
            }
            "search" => self.handle_search(&super::ToolCallContext::empty(), &snapshot.tool_args),
            "path" => self.handle_path(&snapshot.tool_args),
            "explore" => self.handle_explore(&snapshot.tool_args),
            "impact" => self.handle_impact(&snapshot.tool_args),
            "lifecycle" => self.handle_lifecycle(&snapshot.tool_args),
            "branch_diff" => self.handle_branch_diff(&snapshot.tool_args),
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
        let patched =
            Self::patch_resume_response(&resp_str, &original_query_id).unwrap_or(resp_str);

        // Mark as Ready if the re-run completed successfully
        if let Some(s) = self
            .query_snapshots
            .lock()
            .unwrap()
            .get_mut(&original_query_id)
        {
            s.status = QueryStatus::Ready;
        }

        (patched, is_error)
    }

    /// Patch a handler response to return the original query_id (not the
    /// one the handler generated internally) and add a `resumed_from` field.
    fn patch_resume_response(resp_str: &str, original_query_id: &str) -> Option<String> {
        let mut resp: Value = serde_json::from_str(resp_str).ok()?;
        resp["query_id"] = json!(original_query_id);
        resp["resumed_from"] = json!(original_query_id);
        serde_json::to_string_pretty(&resp).ok()
    }
}
