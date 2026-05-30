//! Lazy job tracking — query and update lazy build jobs.
//!
//! The `lazy_jobs` table records the lifecycle of lazy extraction jobs
//! (queued → building → complete/failed) and provides in-flight deduplication:
//! two concurrent requests for the same file+layer return the same job_id.

use super::Store;
use rusqlite::params;
use types::ids::FileId;

/// A row from the lazy_jobs table.
#[derive(Debug, Clone)]
pub struct LazyJob {
    pub job_id: String,
    pub file_id: FileId,
    pub target_layer: String,
    pub status: String, // queued/building/complete/failed
    pub trigger_query: Option<String>,
    pub depends_on: Option<String>, // JSON array of prerequisite job_ids
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub budget_ms: Option<i64>,
    pub error_msg: Option<String>,
}

/// Result of trying to claim a lazy job atomically.
pub enum ClaimResult {
    /// This caller owns the job and must execute the build.
    Claimed { job_id: String },
    /// Another caller is already building this file+layer.
    AlreadyBuilding { job_id: String },
}

impl Store {
    /// Create a new job in 'queued' state. Returns the job_id.
    ///
    /// If a job with the same file_id + target_layer is already 'building' or
    /// 'queued', returns the existing job_id instead of creating a duplicate.
    pub fn upsert_lazy_job_queued(
        &self,
        job_id: &str,
        file_id: &FileId,
        target_layer: &str,
        trigger_query: Option<&str>,
        depends_on: Option<&str>,
        budget_ms: Option<i64>,
    ) -> anyhow::Result<String> {
        // Check for existing active job (in-flight dedup)
        if let Some(active) = self.find_active_lazy_job(file_id, target_layer)? {
            return Ok(active.job_id);
        }

        let conn = self.lock();
        conn.execute(
            "INSERT INTO lazy_jobs
                (job_id, file_id, target_layer, status, trigger_query, depends_on,
                 started_at, budget_ms)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5, datetime('now'), ?6)",
            params![job_id, file_id, target_layer, trigger_query, depends_on, budget_ms],
        )?;
        Ok(job_id.to_string())
    }

    /// Atomically claim a lazy build job or return the existing one.
    ///
    /// In a single transaction:
    /// 1. Checks if a 'queued' or 'building' job exists for this file+layer
    /// 2. If exists → returns AlreadyBuilding with that job_id
    /// 3. If not → inserts new row and returns Claimed with assigned job_id
    ///
    /// Only the owner may proceed to build. Non-owners must NOT build.
    pub fn claim_lazy_job(
        &self,
        file_id: &FileId,
        target_layer: &str,
        trigger_query: Option<&str>,
        depends_on: Option<&str>,
        budget_ms: Option<i64>,
    ) -> anyhow::Result<ClaimResult> {
        self.with_transaction(|tx| {
            // Check for existing active job inside the transaction
            let existing: Option<String> = tx
                .query_row(
                    "SELECT job_id FROM lazy_jobs
                     WHERE file_id = ?1 AND target_layer = ?2
                       AND status IN ('queued', 'building')
                     LIMIT 1",
                    params![file_id, target_layer],
                    |row| row.get(0),
                )
                .ok();

            if let Some(job_id) = existing {
                return Ok(ClaimResult::AlreadyBuilding { job_id });
            }

            // Generate job_id and insert
            let job_id = format!(
                "lazy_{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros()
            );

            tx.execute(
                "INSERT INTO lazy_jobs
                    (job_id, file_id, target_layer, status, trigger_query, depends_on,
                     started_at, budget_ms)
                 VALUES (?1, ?2, ?3, 'building', ?4, ?5, datetime('now'), ?6)",
                params![job_id, file_id, target_layer, trigger_query, depends_on, budget_ms],
            )?;

            Ok(ClaimResult::Claimed { job_id })
        })
    }

    /// Transition a job from 'queued' to 'building'. Returns the job.
    ///
    /// Returns `None` if no queued job exists for this file_id+layer.
    pub fn start_lazy_job(
        &self,
        file_id: &FileId,
        target_layer: &str,
    ) -> anyhow::Result<Option<LazyJob>> {
        let conn = self.lock();
        // Atomically select and update the queued job
        let updated = conn.execute(
            "UPDATE lazy_jobs
                SET status = 'building', started_at = datetime('now')
             WHERE file_id = ?1 AND target_layer = ?2 AND status = 'queued'",
            params![file_id, target_layer],
        )?;

        if updated == 0 {
            return Ok(None);
        }

        // Read back the now-building job
        drop(conn);
        let read_conn = self.lock_read();
        let mut stmt = read_conn.prepare(
            "SELECT job_id, file_id, target_layer, status, trigger_query,
                    depends_on, started_at, completed_at, budget_ms, error_msg
             FROM lazy_jobs
             WHERE file_id = ?1 AND target_layer = ?2 AND status = 'building'
             LIMIT 1",
        )?;
        let result = stmt
            .query_row(params![file_id, target_layer], row_to_lazy_job)
            .ok();
        Ok(result)
    }

    /// Mark a job as 'complete'.
    pub fn complete_lazy_job(&self, job_id: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE lazy_jobs
                SET status = 'complete', completed_at = datetime('now')
             WHERE job_id = ?1",
            params![job_id],
        )?;
        Ok(())
    }

    /// Mark a job as 'failed' with error message.
    pub fn fail_lazy_job(&self, job_id: &str, error_msg: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE lazy_jobs
                SET status = 'failed', completed_at = datetime('now'),
                    error_msg = ?2
             WHERE job_id = ?1",
            params![job_id, error_msg],
        )?;
        Ok(())
    }

    /// Find the currently active (queued or building) job for a file+layer, if any.
    pub fn find_active_lazy_job(
        &self,
        file_id: &FileId,
        target_layer: &str,
    ) -> anyhow::Result<Option<LazyJob>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT job_id, file_id, target_layer, status, trigger_query,
                    depends_on, started_at, completed_at, budget_ms, error_msg
             FROM lazy_jobs
             WHERE file_id = ?1 AND target_layer = ?2
               AND status IN ('queued', 'building')
             LIMIT 1",
        )?;
        let result = stmt.query_row(params![file_id, target_layer], row_to_lazy_job).ok();
        Ok(result)
    }

    /// Get a job by its ID.
    pub fn get_lazy_job(&self, job_id: &str) -> anyhow::Result<Option<LazyJob>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT job_id, file_id, target_layer, status, trigger_query,
                    depends_on, started_at, completed_at, budget_ms, error_msg
             FROM lazy_jobs
             WHERE job_id = ?1",
        )?;
        let result = stmt.query_row(params![job_id], row_to_lazy_job).ok();
        Ok(result)
    }
}

// ── Row mapping ─────────────────────────────────────────────────────────────

fn row_to_lazy_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<LazyJob> {
    Ok(LazyJob {
        job_id: row.get(0)?,
        file_id: row.get(1)?,
        target_layer: row.get(2)?,
        status: row.get(3)?,
        trigger_query: row.get(4)?,
        depends_on: row.get(5)?,
        started_at: row.get(6)?,
        completed_at: row.get(7)?,
        budget_ms: row.get(8)?,
        error_msg: row.get(9)?,
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
    fn lazy_job_create_and_complete() {
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
            .upsert_lazy_job_queued("test_job_1", &file_id, "structural", None, None, None)
            .unwrap();
        assert_eq!(job_id, "test_job_1");

        // Verify queued
        let active = store.find_active_lazy_job(&file_id, "structural").unwrap();
        assert!(active.is_some());
        assert_eq!(active.as_ref().unwrap().status, "queued");

        // Start job
        let started = store.start_lazy_job(&file_id, "structural").unwrap();
        assert!(started.is_some());
        assert_eq!(started.as_ref().unwrap().status, "building");

        // Complete job
        store.complete_lazy_job(&job_id).unwrap();

        // Verify complete — no longer active
        let active = store.find_active_lazy_job(&file_id, "structural").unwrap();
        assert!(active.is_none());

        // Verify status via get
        let job = store.get_lazy_job(&job_id).unwrap();
        assert!(job.is_some());
        assert_eq!(job.as_ref().unwrap().status, "complete");
    }

    #[test]
    fn lazy_job_fail_records_error() {
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
            .upsert_lazy_job_queued("fail_job", &file_id, "structural", None, None, None)
            .unwrap();

        store.start_lazy_job(&file_id, "structural").unwrap();
        store.fail_lazy_job(&job_id, "parse error: unexpected token").unwrap();

        let job = store.get_lazy_job(&job_id).unwrap().unwrap();
        assert_eq!(job.status, "failed");
        assert!(job.error_msg.unwrap().contains("parse error"));
    }

    #[test]
    fn lazy_job_dedup_returns_existing_active() {
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
            .upsert_lazy_job_queued("first_job", &file_id, "structural", None, None, None)
            .unwrap();
        assert_eq!(id1, "first_job");

        // Second upsert for same file+layer returns existing job_id (dedup)
        let id2 = store
            .upsert_lazy_job_queued("second_job", &file_id, "structural", None, None, None)
            .unwrap();
        assert_eq!(id2, "first_job", "dedup should return first job's id");

        // Different layer should create a new job
        let id3 = store
            .upsert_lazy_job_queued("third_job", &file_id, "dataflow", None, None, None)
            .unwrap();
        assert_eq!(id3, "third_job");
    }

    #[test]
    fn lazy_job_start_nonexistent_returns_none() {
        let store = test_store();
        let file_id = FileId::generate("src/nonexistent.c");
        let result = store.start_lazy_job(&file_id, "structural").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn claim_lazy_job_atomic_dedup() {
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
            .claim_lazy_job(&file_id, "structural", None, None, None)
            .unwrap();
        let job_id1 = match &claim1 {
            ClaimResult::Claimed { job_id } => job_id.clone(),
            _ => panic!("expected Claimed, got AlreadyBuilding"),
        };

        // Second claim returns AlreadyBuilding
        let claim2 = store
            .claim_lazy_job(&file_id, "structural", None, None, None)
            .unwrap();
        assert!(
            matches!(claim2, ClaimResult::AlreadyBuilding { .. }),
            "second claim should return AlreadyBuilding"
        );

        // Complete the first job
        store.complete_lazy_job(&job_id1).unwrap();

        // After completion, claim succeeds again (no active job)
        let claim3 = store
            .claim_lazy_job(&file_id, "structural", None, None, None)
            .unwrap();
        assert!(
            matches!(claim3, ClaimResult::Claimed { .. }),
            "claim after completion should succeed"
        );
    }
}
