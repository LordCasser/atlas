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

        (
            serde_json::to_string_pretty(&json!({
                "jobs": result,
                "total": jobs.len(),
            }))
            .unwrap_or_else(|e| e.to_string()),
            false,
        )
    }
}
