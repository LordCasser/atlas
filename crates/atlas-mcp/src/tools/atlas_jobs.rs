//! `atlas_jobs` — list background extraction jobs with status.
//!
//! Provides observability into lazy extraction progress. Can filter by
//! query_id to see jobs triggered by a specific query.

use super::ToolRouter;
use serde_json::{Value, json};

impl ToolRouter {
    /// Handle `atlas_jobs` — list recent extraction jobs, optionally filtered
    /// by query_id.
    pub(crate) fn handle_atlas_jobs(&self, args: &Value) -> (String, bool) {
        let query_id = crate::tools::get_str_opt(args, "query_id");
        let jobs = match self.store.list_extraction_jobs(query_id) {
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
                    "file": self.resolve_file_path(&j.file_id),
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
            match self.store.get_job_counts_by_trigger_query(qid) {
                Ok(prog) => {
                    let pending = prog.queued + prog.building;
                    let msg = if pending > 0 {
                        format!("{} jobs pending", pending)
                    } else {
                        "all jobs complete".to_string()
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
}
