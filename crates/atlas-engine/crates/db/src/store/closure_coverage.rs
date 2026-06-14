//! Closure coverage tracking — map closures to covered files.
//!
//! The `closure_coverage` table records which files belong to which closure,
//! with per-generation visibility gating. Coverage entries start as 'staged'
//! and are atomically promoted to 'visible' when the closure is committed.

use super::Store;
use rusqlite::params;

/// A row from the closure_coverage table.
#[derive(Debug, Clone)]
pub struct ClosureCoverage {
    pub closure_id: String,
    pub file_id: Vec<u8>,
    pub source: String,
    pub visibility_state: String,
    pub generation: i64,
    pub content_hash: Option<String>,
    pub extracted_at: String,
}

impl Store {
    /// Insert a staged coverage entry.
    pub fn insert_closure_coverage(
        &self,
        closure_id: &str,
        file_id: &[u8],
        source: &str,
        generation: i64,
        content_hash: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        // precision_tier is deprecated (vestigial column), write empty string
        // for backwards compat with old DBs that still have the NOT NULL column.
        conn.execute(
            "INSERT INTO closure_coverage
                (closure_id, file_id, source, visibility_state, generation,
                 content_hash, precision_tier)
             VALUES (?1, ?2, ?3, 'staged', ?4, ?5, '')",
            params![closure_id, file_id, source, generation, content_hash],
        )?;
        Ok(())
    }

    /// Make all staged entries for a closure+generation visible.
    /// Returns the number of rows updated.
    pub fn make_coverage_visible(
        &self,
        closure_id: &str,
        generation: i64,
    ) -> anyhow::Result<usize> {
        let conn = self.lock();
        let updated = conn.execute(
            "UPDATE closure_coverage
                SET visibility_state = 'visible'
             WHERE closure_id = ?1 AND generation = ?2 AND visibility_state = 'staged'",
            params![closure_id, generation],
        )?;
        Ok(updated)
    }

    /// Make ALL staged entries for a closure visible, regardless of generation.
    /// This is used at commit time when a closure may have files spread across
    /// multiple generations (seed=0, iteration 1, iteration 2, ...).
    /// Returns the number of rows updated.
    pub fn make_all_staged_coverage_visible(&self, closure_id: &str) -> anyhow::Result<usize> {
        let conn = self.lock();
        let updated = conn.execute(
            "UPDATE closure_coverage
                SET visibility_state = 'visible'
             WHERE closure_id = ?1 AND visibility_state = 'staged'",
            params![closure_id],
        )?;
        Ok(updated)
    }

    /// Get all visible files for a closure.
    pub fn get_visible_coverage(&self, closure_id: &str) -> anyhow::Result<Vec<ClosureCoverage>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT closure_id, file_id, source, visibility_state, generation,
                    content_hash, extracted_at
             FROM closure_coverage
             WHERE closure_id = ?1 AND visibility_state = 'visible'",
        )?;
        let rows = stmt.query_map(params![closure_id], row_to_closure_coverage)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Check if a file is covered by any committed closure.
    pub fn is_file_covered(&self, file_id: &[u8]) -> anyhow::Result<bool> {
        let conn = self.lock_read();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM closure_coverage
             WHERE file_id = ?1 AND visibility_state = 'visible'",
            params![file_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Get counts by source for a closure.
    pub fn get_coverage_counts(&self, closure_id: &str) -> anyhow::Result<Vec<(String, i64)>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT source, COUNT(*) FROM closure_coverage
             WHERE closure_id = ?1 AND visibility_state = 'visible'
             GROUP BY source",
        )?;
        let rows = stmt.query_map(params![closure_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

// ── Row mapping ─────────────────────────────────────────────────────────────

fn row_to_closure_coverage(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClosureCoverage> {
    Ok(ClosureCoverage {
        closure_id: row.get(0)?,
        file_id: row.get(1)?,
        source: row.get(2)?,
        visibility_state: row.get(3)?,
        generation: row.get(4)?,
        content_hash: row.get(5)?,
        extracted_at: row.get(6)?,
    })
}
