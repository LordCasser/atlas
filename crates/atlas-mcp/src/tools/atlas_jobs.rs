//! Active extraction job listing — used by the `tasks` tool.
//!
//! Provides observability into in-flight lazy extraction work. Completed
//! structural jobs may disappear when file facts are atomically replaced, so
//! this module reports raw job rows and active pending counts, not durable
//! query completion.

use super::{ToolRouter, get_str_opt};
use serde_json::{Value, json};

impl ToolRouter {
    /// Handle `atlas_jobs` — list recent extraction jobs, optionally filtered
    /// by query_id.
    pub(crate) fn handle_atlas_jobs(&self, args: &Value) -> (String, bool) {
        let query_id = crate::tools::get_str_opt(args, "query_id");
        let jobs = match self.project().store.list_extraction_jobs(query_id) {
            Ok(j) => j,
            Err(e) => {
                return (
                    serde_json::to_string(&json!({"error": format!("Error listing jobs: {}", e)}))
                        .unwrap(),
                    true,
                );
            }
        };

        let result: Vec<Value> = jobs
            .iter()
            .map(|j| {
                json!({
                    "job_id": j.job_id,
                    "file": self.project().store_query_runtime.resolve_file_path(&j.file_id),
                    "layer": j.layer,
                    "status": j.status,
                    "capability": j.layer,
                    "trigger_query": j.trigger_query,
                    "started_at": j.started_at,
                    "completed_at": j.completed_at,
                })
            })
            .collect();

        // Compute progress when filtering by query_id
        let (progress, pending_jobs, message) = if let Some(qid) = query_id {
            match self.project().store.get_job_counts_by_trigger_query(qid) {
                Ok(prog) => {
                    let pending = prog.queued + prog.building;
                    let msg = if pending > 0 {
                        format!("{pending} extraction job(s) pending")
                    } else {
                        "no active extraction jobs".to_string()
                    };
                    (Some(prog), pending, msg)
                }
                Err(_) => (None, 0i64, "unavailable".to_string()),
            }
        } else {
            (None, 0i64, String::new())
        };

        let mut resp = json!({
            "jobs": result,
            "total": jobs.len(),
        });

        if let Some(prog) = progress {
            resp["progress"] = json!({
                "queued": prog.queued,
                "building": prog.building,
                "complete": prog.complete,
                "failed": prog.failed,
            });
            resp["pending_jobs"] = json!(pending_jobs);
            resp["message"] = json!(message);
        }

        (
            serde_json::to_string_pretty(&resp).unwrap_or_else(|e| e.to_string()),
            false,
        )
    }

    /// Handle `tasks` tool - aggregate active jobs + atlas jobs.
    pub(crate) fn handle_tasks(&self, args: &Value) -> (String, bool) {
        let query_id = get_str_opt(args, "query_id");

        let query = query_id.map(|qid| {
            self.project().job_runtime.prune_expired_snapshots();
            let snapshot = self
                .project()
                .job_runtime
                .query_snapshots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(qid)
                .cloned();
            let Some(snapshot) = snapshot else {
                return json!({
                    "query_id": qid,
                    "status": "not_found_or_expired",
                    "pending_jobs": 0,
                });
            };

            let failures = snapshot
                .focus_result
                .as_ref()
                .map(|result| {
                    let mut failures = result
                        .job_tracker
                        .as_ref()
                        .map(|tracker| tracker.failures_for(&result.pending_closure_ids))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(job_id, reason)| format!("{job_id}: {reason}"))
                        .collect::<Vec<_>>();
                    for job_id in &result.pending_extraction_job_ids {
                        if let Ok(Some(job)) = self.project().store.get_extraction_job(job_id)
                            && job.status == "failed"
                        {
                            failures.push(format!(
                                "{}: {}",
                                job.job_id,
                                job.error_msg
                                    .as_deref()
                                    .unwrap_or("background extraction failed")
                            ));
                        }
                    }
                    failures
                })
                .unwrap_or_default();
            if !failures.is_empty() {
                return json!({
                    "query_id": qid,
                    "tool": snapshot.tool_name,
                    "status": "failed",
                    "pending_jobs": 0,
                    "detail": failures.join("; "),
                });
            }

            let (pending, retry_after_ms) = snapshot
                .focus_result
                .as_ref()
                .map(|result| self.focus_pending_count_and_eta_ms(result))
                .unwrap_or((0, 0));
            let mut state = json!({
                "query_id": qid,
                "tool": snapshot.tool_name,
                "status": if pending == 0 { "ready" } else { "refining" },
                "pending_jobs": pending,
            });
            if pending > 0 {
                state["retry_after_ms"] = json!(retry_after_ms);
            }
            state
        });

        let (jobs_str, jobs_err) = self.handle_jobs();
        let atlas_args = if let Some(qid) = query_id {
            let mut m = serde_json::Map::new();
            m.insert("query_id".into(), Value::String(qid.to_string()));
            Value::Object(m)
        } else {
            Value::Object(serde_json::Map::new())
        };
        let (atlas_str, atlas_err) = self.handle_atlas_jobs(&atlas_args);

        let mut result = json!({
            "active_extraction_jobs": serde_json::from_str::<Value>(&jobs_str).unwrap_or_default(),
            "atlas_jobs": serde_json::from_str::<Value>(&atlas_str).unwrap_or_default(),
        });
        if let Some(query) = query {
            result["query"] = query;
        }
        (
            serde_json::to_string_pretty(&result).unwrap_or_default(),
            jobs_err || atlas_err,
        )
    }
}
