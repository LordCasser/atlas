//! Extraction job tracking — query and update extraction build jobs.
//!
//! The `extraction_jobs` table records the lifecycle of extraction jobs
//! (queued → building → complete/failed) and provides in-flight deduplication:
//! two concurrent requests for the same file+unit+layer return the same job_id.

use super::Store;
use rusqlite::params;
use types::ids::FileId;
use types::lazy::AnalysisUnit;

/// A row from the extraction_jobs table.
#[derive(Debug, Clone)]
pub struct ExtractionJob {
    pub job_id: String,
    pub file_id: FileId,
    pub unit_id: Option<[u8; 16]>,
    pub layer: String,
    pub status: String, // queued/building/complete/failed
    pub trigger_query: Option<String>,
    pub depends_on: Option<String>, // JSON array of prerequisite job_ids
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub budget_ms: Option<i64>,
    pub error_msg: Option<String>,
}

/// Result of trying to claim an extraction job atomically.
pub enum ClaimResult {
    /// This caller owns the job and must execute the build.
    Claimed { job_id: String },
    /// Another caller is already building this file+layer.
    AlreadyBuilding { job_id: String },
}

impl Store {
    /// Create a new job in 'queued' state. Returns the job_id.
    ///
    /// If a job with the same file_id + layer is already 'building' or
    /// 'queued', returns the existing job_id instead of creating a duplicate.
    pub fn upsert_extraction_job_queued(
        &self,
        job_id: &str,
        file_id: &FileId,
        layer: &str,
        trigger_query: Option<&str>,
        depends_on: Option<&str>,
        budget_ms: Option<i64>,
    ) -> anyhow::Result<String> {
        // Check for existing active job (in-flight dedup)
        if let Some(active) = self.find_active_file_extraction_job(file_id, layer)? {
            return Ok(active.job_id);
        }

        let conn = self.lock();
        conn.execute(
            "INSERT INTO extraction_jobs
                (job_id, file_id, unit_id, layer, status, trigger_query, depends_on,
                 started_at, budget_ms)
             VALUES (?1, ?2, NULL, ?3, 'queued', ?4, ?5, datetime('now'), ?6)",
            params![job_id, file_id, layer, trigger_query, depends_on, budget_ms],
        )?;
        Ok(job_id.to_string())
    }

    /// Atomically claim an extraction build job or return the existing one.
    ///
    /// In a single transaction:
    /// 1. Checks if a 'queued' or 'building' job exists for this file+layer
    /// 2. If exists → returns AlreadyBuilding with that job_id
    /// 3. If not → inserts new row and returns Claimed with assigned job_id
    ///
    /// Only the owner may proceed to build. Non-owners must NOT build.
    pub fn claim_file_extraction_job(
        &self,
        file_id: &FileId,
        layer: &str,
        trigger_query: Option<&str>,
        depends_on: Option<&str>,
        budget_ms: Option<i64>,
    ) -> anyhow::Result<ClaimResult> {
        self.claim_extraction_job(file_id, None, layer, trigger_query, depends_on, budget_ms)
    }

    /// Atomically claim a dataflow build job for one analysis unit.
    pub fn claim_dataflow_extraction_job(
        &self,
        unit: &AnalysisUnit,
        trigger_query: Option<&str>,
        budget_ms: Option<i64>,
    ) -> anyhow::Result<ClaimResult> {
        self.claim_extraction_job(
            &unit.file_id,
            Some(&unit.unit_id),
            "dataflow",
            trigger_query,
            None,
            budget_ms,
        )
    }

    fn claim_extraction_job(
        &self,
        file_id: &FileId,
        unit_id: Option<&[u8; 16]>,
        layer: &str,
        trigger_query: Option<&str>,
        depends_on: Option<&str>,
        budget_ms: Option<i64>,
    ) -> anyhow::Result<ClaimResult> {
        self.with_transaction(|tx| {
            let existing: Option<String> = if let Some(unit_id) = unit_id {
                let unit_blob: &[u8] = unit_id;
                tx.query_row(
                    "SELECT job_id FROM extraction_jobs
                     WHERE file_id = ?1 AND unit_id = ?2 AND layer = ?3
                       AND status IN ('queued', 'building')
                     LIMIT 1",
                    params![file_id, unit_blob, layer],
                    |row| row.get(0),
                )
                .ok()
            } else {
                tx.query_row(
                    "SELECT job_id FROM extraction_jobs
                     WHERE file_id = ?1 AND unit_id IS NULL AND layer = ?2
                       AND status IN ('queued', 'building')
                     LIMIT 1",
                    params![file_id, layer],
                    |row| row.get(0),
                )
                .ok()
            };

            if let Some(job_id) = existing {
                return Ok(ClaimResult::AlreadyBuilding { job_id });
            }

            let job_id = format!(
                "extract_{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros()
            );
            let unit_blob: Option<&[u8]> = unit_id.map(|id| id.as_slice());
            tx.execute(
                "INSERT INTO extraction_jobs
                    (job_id, file_id, unit_id, layer, status, trigger_query, depends_on,
                     started_at, budget_ms)
                 VALUES (?1, ?2, ?3, ?4, 'building', ?5, ?6, datetime('now'), ?7)",
                params![
                    job_id,
                    file_id,
                    unit_blob,
                    layer,
                    trigger_query,
                    depends_on,
                    budget_ms
                ],
            )?;

            Ok(ClaimResult::Claimed { job_id })
        })
    }

    /// Transition a job from 'queued' to 'building'. Returns the job.
    ///
    /// Returns `None` if no queued job exists for this file_id+layer.
    pub fn start_extraction_job(
        &self,
        file_id: &FileId,
        layer: &str,
    ) -> anyhow::Result<Option<ExtractionJob>> {
        let conn = self.lock();
        // Atomically select and update the queued job
        let updated = conn.execute(
            "UPDATE extraction_jobs
                SET status = 'building', started_at = datetime('now')
             WHERE file_id = ?1 AND unit_id IS NULL AND layer = ?2 AND status = 'queued'",
            params![file_id, layer],
        )?;

        if updated == 0 {
            return Ok(None);
        }

        // Read back the now-building job
        drop(conn);
        let read_conn = self.lock_read();
        let mut stmt = read_conn.prepare(
            "SELECT job_id, file_id, unit_id, layer, status, trigger_query,
                    depends_on, started_at, completed_at, budget_ms, error_msg
             FROM extraction_jobs
             WHERE file_id = ?1 AND unit_id IS NULL AND layer = ?2 AND status = 'building'
             LIMIT 1",
        )?;
        let result = stmt
            .query_row(params![file_id, layer], row_to_extraction_job)
            .ok();
        Ok(result)
    }

    /// Mark a job as 'complete'.
    pub fn complete_extraction_job(&self, job_id: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE extraction_jobs
                SET status = 'complete', completed_at = datetime('now')
             WHERE job_id = ?1",
            params![job_id],
        )?;
        Ok(())
    }

    /// Mark a job as 'failed' with error message.
    pub fn fail_extraction_job(&self, job_id: &str, error_msg: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE extraction_jobs
                SET status = 'failed', completed_at = datetime('now'),
                    error_msg = ?2
             WHERE job_id = ?1",
            params![job_id, error_msg],
        )?;
        Ok(())
    }

    /// Find the currently active (queued or building) job for a file+layer, if any.
    pub fn find_active_file_extraction_job(
        &self,
        file_id: &FileId,
        layer: &str,
    ) -> anyhow::Result<Option<ExtractionJob>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT job_id, file_id, unit_id, layer, status, trigger_query,
                    depends_on, started_at, completed_at, budget_ms, error_msg
             FROM extraction_jobs
             WHERE file_id = ?1 AND unit_id IS NULL AND layer = ?2
               AND status IN ('queued', 'building')
             LIMIT 1",
        )?;
        let result = stmt
            .query_row(params![file_id, layer], row_to_extraction_job)
            .ok();
        Ok(result)
    }

    /// Get a job by its ID.
    pub fn get_extraction_job(&self, job_id: &str) -> anyhow::Result<Option<ExtractionJob>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT job_id, file_id, unit_id, layer, status, trigger_query,
                    depends_on, started_at, completed_at, budget_ms, error_msg
             FROM extraction_jobs
             WHERE job_id = ?1",
        )?;
        let result = stmt.query_row(params![job_id], row_to_extraction_job).ok();
        Ok(result)
    }

    /// List active extraction jobs for MCP observability.
    pub fn list_active_extraction_jobs(&self) -> anyhow::Result<Vec<ExtractionJob>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT job_id, file_id, unit_id, layer, status, trigger_query,
                    depends_on, started_at, completed_at, budget_ms, error_msg
             FROM extraction_jobs
             WHERE status IN ('queued', 'building')
             ORDER BY started_at, job_id",
        )?;
        let rows = stmt.query_map([], row_to_extraction_job)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// List recent extraction jobs (all statuses), optionally filtered by
    /// trigger_query.  Used by `atlas_jobs` MCP tool for observability.
    pub fn list_extraction_jobs(
        &self,
        query_id_filter: Option<&str>,
    ) -> anyhow::Result<Vec<ExtractionJob>> {
        let conn = self.lock_read();
        if let Some(qid) = query_id_filter {
            let mut stmt = conn.prepare(
                "SELECT job_id, file_id, unit_id, layer, status, trigger_query,
                        depends_on, started_at, completed_at, budget_ms, error_msg
                 FROM extraction_jobs
                 WHERE trigger_query = ?1
                 ORDER BY started_at DESC
                 LIMIT 50",
            )?;
            let rows = stmt.query_map(params![qid], row_to_extraction_job)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        } else {
            let mut stmt = conn.prepare(
                "SELECT job_id, file_id, unit_id, layer, status, trigger_query,
                        depends_on, started_at, completed_at, budget_ms, error_msg
                 FROM extraction_jobs
                 ORDER BY started_at DESC
                 LIMIT 50",
            )?;
            let rows = stmt.query_map([], row_to_extraction_job)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        }
    }

    /// Count extraction jobs grouped by status for a given trigger query.
    /// Returns counts for queued, building, complete, and failed jobs.
    pub fn get_job_counts_by_trigger_query(
        &self,
        query_id: &str,
    ) -> anyhow::Result<QueryJobProgress> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT status, COUNT(*) FROM extraction_jobs
             WHERE trigger_query = ?1
             GROUP BY status",
        )?;
        let mut progress = QueryJobProgress::default();
        let rows = stmt.query_map(params![query_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (status, count) = row?;
            match status.as_str() {
                "queued" => progress.queued = count,
                "building" => progress.building = count,
                "complete" => progress.complete = count,
                "failed" => progress.failed = count,
                _ => {}
            }
        }
        Ok(progress)
    }
}

/// Progress summary for jobs triggered by a specific query.
#[derive(Debug, Clone, Default)]
pub struct QueryJobProgress {
    pub queued: i64,
    pub building: i64,
    pub complete: i64,
    pub failed: i64,
}

// ── Row mapping ─────────────────────────────────────────────────────────────

fn row_to_extraction_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExtractionJob> {
    Ok(ExtractionJob {
        job_id: row.get(0)?,
        file_id: row.get(1)?,
        unit_id: {
            let blob: Option<Vec<u8>> = row.get(2)?;
            blob.map(|bytes| {
                let mut arr = [0u8; 16];
                arr.copy_from_slice(&bytes);
                arr
            })
        },
        layer: row.get(3)?,
        status: row.get(4)?,
        trigger_query: row.get(5)?,
        depends_on: row.get(6)?,
        started_at: row.get(7)?,
        completed_at: row.get(8)?,
        budget_ms: row.get(9)?,
        error_msg: row.get(10)?,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use types::ids::FileId;

    fn test_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
    }

    #[test]
    fn extraction_job_create_and_complete() {
        let store = test_store();
        let file_id = FileId::generate("src/main.c");

        // Register the file first (FK constraint)
        let file_info = types::FileInfo {
            file_id,
            path: "src/main.c".into(),
            language: types::Language::C,
            content_hash: "abc".into(),
            status: types::ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();

        // Create job
        let job_id = store
            .upsert_extraction_job_queued("test_job_1", &file_id, "structural", None, None, None)
            .unwrap();
        assert_eq!(job_id, "test_job_1");

        // Verify queued
        let active = store
            .find_active_file_extraction_job(&file_id, "structural")
            .unwrap();
        assert!(active.is_some());
        assert_eq!(active.as_ref().unwrap().status, "queued");

        // Start job
        let started = store.start_extraction_job(&file_id, "structural").unwrap();
        assert!(started.is_some());
        assert_eq!(started.as_ref().unwrap().status, "building");

        // Complete job
        store.complete_extraction_job(&job_id).unwrap();

        // Verify complete — no longer active
        let active = store
            .find_active_file_extraction_job(&file_id, "structural")
            .unwrap();
        assert!(active.is_none());

        // Verify status via get
        let job = store.get_extraction_job(&job_id).unwrap();
        assert!(job.is_some());
        assert_eq!(job.as_ref().unwrap().status, "complete");
    }

    #[test]
    fn extraction_job_fail_records_error() {
        let store = test_store();
        let file_id = FileId::generate("src/broken.c");

        let file_info = types::FileInfo {
            file_id,
            path: "src/broken.c".into(),
            language: types::Language::C,
            content_hash: "abc".into(),
            status: types::ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();

        let job_id = store
            .upsert_extraction_job_queued("fail_job", &file_id, "structural", None, None, None)
            .unwrap();

        store.start_extraction_job(&file_id, "structural").unwrap();
        store
            .fail_extraction_job(&job_id, "parse error: unexpected token")
            .unwrap();

        let job = store.get_extraction_job(&job_id).unwrap().unwrap();
        assert_eq!(job.status, "failed");
        assert!(job.error_msg.unwrap().contains("parse error"));
    }

    #[test]
    fn extraction_job_dedup_returns_existing_active() {
        let store = test_store();
        let file_id = FileId::generate("src/dup.c");

        let file_info = types::FileInfo {
            file_id,
            path: "src/dup.c".into(),
            language: types::Language::C,
            content_hash: "abc".into(),
            status: types::ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();

        // First upsert creates the job
        let id1 = store
            .upsert_extraction_job_queued("first_job", &file_id, "structural", None, None, None)
            .unwrap();
        assert_eq!(id1, "first_job");

        // Second upsert for same file+layer returns existing job_id (dedup)
        let id2 = store
            .upsert_extraction_job_queued("second_job", &file_id, "structural", None, None, None)
            .unwrap();
        assert_eq!(id2, "first_job", "dedup should return first job's id");

        // Different layer should create a new job
        let id3 = store
            .upsert_extraction_job_queued("third_job", &file_id, "dataflow", None, None, None)
            .unwrap();
        assert_eq!(id3, "third_job");
    }

    #[test]
    fn extraction_job_start_nonexistent_returns_none() {
        let store = test_store();
        let file_id = FileId::generate("src/nonexistent.c");
        let result = store.start_extraction_job(&file_id, "structural").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn claim_file_extraction_job_atomic_dedup() {
        let store = test_store();
        let file_id = FileId::generate("src/atomic.c");

        // Register the file first (FK constraint)
        let file_info = types::FileInfo {
            file_id,
            path: "src/atomic.c".into(),
            language: types::Language::C,
            content_hash: "abc".into(),
            status: types::ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();

        // First claim succeeds
        let claim1 = store
            .claim_file_extraction_job(&file_id, "structural", None, None, None)
            .unwrap();
        let job_id1 = match &claim1 {
            ClaimResult::Claimed { job_id } => job_id.clone(),
            _ => panic!("expected Claimed, got AlreadyBuilding"),
        };

        // Second claim returns AlreadyBuilding
        let claim2 = store
            .claim_file_extraction_job(&file_id, "structural", None, None, None)
            .unwrap();
        assert!(
            matches!(claim2, ClaimResult::AlreadyBuilding { .. }),
            "second claim should return AlreadyBuilding"
        );

        // Complete the first job
        store.complete_extraction_job(&job_id1).unwrap();

        // After completion, claim succeeds again (no active job)
        let claim3 = store
            .claim_file_extraction_job(&file_id, "structural", None, None, None)
            .unwrap();
        assert!(
            matches!(claim3, ClaimResult::Claimed { .. }),
            "claim after completion should succeed"
        );
    }

    #[test]
    fn claim_dataflow_extraction_job_is_unit_scoped() {
        let store = test_store();
        let file_id = FileId::generate("src/dataflow.ts");
        let file_info = types::FileInfo {
            file_id,
            path: "src/dataflow.ts".into(),
            language: types::Language::TypeScript,
            content_hash: "abc".into(),
            status: types::ParseStatus::Success,
        };
        store.upsert_file(&file_info).unwrap();

        let range = types::TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 0,
            start_column: 0,
            end_line: 1,
            end_column: 0,
        };
        let sym_a = types::SymbolId::generate(&file_id, "typescript", "a", "function", None);
        let sym_b = types::SymbolId::generate(&file_id, "typescript", "b", "function", None);
        let unit_a = AnalysisUnit::from_function(file_id, sym_a, range);
        let unit_b = AnalysisUnit::from_function(file_id, sym_b, range);

        assert!(matches!(
            store
                .claim_dataflow_extraction_job(&unit_a, Some("test"), Some(25_000))
                .unwrap(),
            ClaimResult::Claimed { .. }
        ));
        assert!(matches!(
            store
                .claim_dataflow_extraction_job(&unit_a, Some("test"), Some(25_000))
                .unwrap(),
            ClaimResult::AlreadyBuilding { .. }
        ));
        assert!(matches!(
            store
                .claim_dataflow_extraction_job(&unit_b, Some("test"), Some(25_000))
                .unwrap(),
            ClaimResult::Claimed { .. }
        ));
    }
}
