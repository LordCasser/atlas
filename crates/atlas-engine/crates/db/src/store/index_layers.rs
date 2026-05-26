//! Per-file per-layer index status — separate from `analysis_artifacts` which is unit-level.

use super::Store;
use rusqlite::params;
use types::ids::FileId;

impl Store {
    /// Query the status and content_hash for a file at a given layer.
    ///
    /// Returns `None` if no record exists.  Layers: "manifest", "structural", "dataflow".
    pub fn get_file_index_layer(
        &self,
        file_id: &FileId,
        layer: &str,
    ) -> anyhow::Result<Option<(String, String)>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT status, content_hash FROM file_index_layers
             WHERE file_id = ?1 AND layer = ?2",
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
    pub fn upsert_file_index_layer(
        &self,
        file_id: &FileId,
        layer: &str,
        content_hash: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO file_index_layers
                (file_id, layer, content_hash, status, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))",
            params![file_id, layer, content_hash, status],
        )?;
        Ok(())
    }

    /// Delete all layer records for a file (used during clean-stale or re-index).
    pub fn delete_file_index_layers(&self, file_id: &FileId) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM file_index_layers WHERE file_id = ?1",
            params![file_id],
        )?;
        Ok(())
    }

    /// Count files by layer and status.
    ///
    /// Returns a Vec of `(layer, status, count)` tuples sorted by layer.
    pub fn count_file_index_layers(&self) -> anyhow::Result<Vec<(String, String, i64)>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT layer, status, COUNT(*) FROM file_index_layers
             GROUP BY layer, status ORDER BY layer, status",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
