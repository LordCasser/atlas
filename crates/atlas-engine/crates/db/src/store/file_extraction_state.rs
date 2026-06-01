//! File-level extraction state facade.

use super::Store;
use rusqlite::params;
use types::ids::FileId;
use types::structs::CapabilityMask;

impl Store {
    /// Query the status and content_hash for a file at a given layer.
    ///
    /// Returns `None` if no record exists.  Layers: "manifest", "structural", "dataflow".
    pub fn get_file_extraction_state(
        &self,
        file_id: &FileId,
        layer: &str,
    ) -> anyhow::Result<Option<(String, String)>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT status, content_hash FROM extraction_state
             WHERE file_id = ?1 AND unit_id IS NULL AND layer = ?2",
        )?;
        let result: Option<(String, String)> = stmt
            .query_row(params![file_id, layer], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .ok();
        Ok(result)
    }

    /// Record (or update) the index status for a file at a given layer.
    ///
    /// Layers: "manifest", "structural", "dataflow".
    /// Status: "complete", "partial", "failed".
    pub fn upsert_file_extraction_state(
        &self,
        file_id: &FileId,
        layer: &str,
        content_hash: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM extraction_state
             WHERE file_id = ?1 AND unit_id IS NULL AND layer = ?2",
            params![file_id, layer],
        )?;
        conn.execute(
            "INSERT INTO extraction_state
                (file_id, unit_id, layer, content_hash, status, updated_at)
             VALUES (?1, NULL, ?2, ?3, ?4, datetime('now'))",
            params![file_id, layer, content_hash, status],
        )?;
        Ok(())
    }

    /// Delete all layer records for a file (used during clean-stale or re-index).
    pub fn delete_file_extraction_state(&self, file_id: &FileId) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM extraction_state WHERE file_id = ?1 AND unit_id IS NULL",
            params![file_id],
        )?;
        Ok(())
    }

    /// Count files by layer and status.
    ///
    /// Returns a Vec of `(layer, status, count)` tuples sorted by layer.
    pub fn count_file_extraction_state(&self) -> anyhow::Result<Vec<(String, String, i64)>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT layer, status, COUNT(*) FROM extraction_state
             WHERE unit_id IS NULL
             GROUP BY layer, status ORDER BY layer, status",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Count fresh per-file layers whose recorded hash still matches `files`.
    ///
    /// This is the status/tooling boundary for file-level extraction state:
    /// stale rows remain useful for cache invalidation diagnostics, but they
    /// must not make the project look more precisely indexed than it is.
    pub fn count_fresh_file_extraction_state(&self) -> anyhow::Result<Vec<(String, String, i64)>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT l.layer, l.status, COUNT(*)
             FROM extraction_state l
             JOIN files f ON f.file_id = l.file_id
             WHERE l.unit_id IS NULL AND l.content_hash = f.content_hash
             GROUP BY l.layer, l.status
             ORDER BY l.layer, l.status",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Query the aggregate capability mask for a file across all layers.
    /// Returns the bitwise OR of all `capability_mask` values for the file.
    pub fn get_capability_mask(&self, file_id: &FileId) -> anyhow::Result<CapabilityMask> {
        let conn = self.lock_read();
        // Aggregate from both file-level (unit_id IS NULL) and unit-level layers.
        let mask: Option<i64> = conn
            .query_row(
                "SELECT MAX(capability_mask) FROM extraction_state
                 WHERE file_id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        Ok(CapabilityMask::new(mask.unwrap_or(0) as u16))
    }

    /// Query the capability mask for a specific unit within a file.
    pub fn get_capability_mask_for_unit(
        &self,
        file_id: &FileId,
        unit_id: &[u8; 16],
    ) -> anyhow::Result<CapabilityMask> {
        let conn = self.lock_read();
        let unit_blob: &[u8] = unit_id;
        let mask: Option<i64> = conn
            .query_row(
                "SELECT capability_mask FROM extraction_state
                 WHERE file_id = ?1 AND unit_id = ?2",
                params![file_id, unit_blob],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        Ok(CapabilityMask::new(mask.unwrap_or(0) as u16))
    }
}
