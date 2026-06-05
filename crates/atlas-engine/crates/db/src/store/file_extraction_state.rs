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
            .map_err(|e| {
                tracing::warn!(
                    ?e,
                    %file_id,
                    %layer,
                    "Failed to query file extraction state, returning None"
                );
                e
            })
            .ok();
        Ok(result)
    }

    /// Record (or update) the index status for a file at a given layer.
    ///
    /// Layers: "manifest", "structural", "dataflow".
    /// Status: "complete", "partial", "failed".
    /// `capability_mask` is written to the DB so `get_capability_mask`
    /// can compute the aggregate mask via bitwise OR across all layers.
    pub fn upsert_file_extraction_state(
        &self,
        file_id: &FileId,
        layer: &str,
        content_hash: &str,
        status: &str,
        capability_mask: CapabilityMask,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM extraction_state
             WHERE file_id = ?1 AND unit_id IS NULL AND layer = ?2",
            params![file_id, layer],
        )?;
        conn.execute(
            "INSERT INTO extraction_state
                (file_id, unit_id, layer, content_hash, status, capability_mask, updated_at)
             VALUES (?1, NULL, ?2, ?3, ?4, ?5, datetime('now'))",
            params![
                file_id,
                layer,
                content_hash,
                status,
                capability_mask.bits() as i64,
            ],
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

    /// Return the effective precision of the fresh project index.
    ///
    /// This is the shared status boundary used by CLI, MCP, and TUI entry
    /// points. Stale extraction rows are ignored so an old structural/full
    /// layer cannot make a downgraded or modified project look protected.
    pub fn read_index_mode(&self) -> anyhow::Result<String> {
        let stats = self.get_stats()?;
        let lazy_stats = self.get_lazy_dataflow_stats().ok();
        let layer_counts = self.count_fresh_file_extraction_state().unwrap_or_default();

        Ok(compute_index_mode(
            stats.total_files,
            complete_count(&layer_counts, "manifest"),
            complete_count(&layer_counts, "structural"),
            complete_count(&layer_counts, "dataflow"),
            lazy_stats
                .as_ref()
                .map(|l| l.total_unit_states)
                .unwrap_or(0),
            lazy_stats.as_ref().is_some_and(|l| l.has_dataflow),
        )
        .to_string())
    }

    /// Return true when a file has fresh, complete file-level extraction state
    /// covering every bit in `required`.
    pub fn file_has_fresh_complete_capability(
        &self,
        file_id: &FileId,
        content_hash: &str,
        required: CapabilityMask,
    ) -> anyhow::Result<bool> {
        if required.is_zero() {
            return Ok(true);
        }

        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT layer, capability_mask FROM extraction_state
             WHERE file_id = ?1
               AND unit_id IS NULL
               AND content_hash = ?2
               AND status = 'complete'",
        )?;
        let rows = stmt.query_map(params![file_id, content_hash], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        let mut bits = 0u16;
        for row in rows {
            let (layer, capability_mask) = row?;
            bits |= capability_mask as u16;
            bits |= CapabilityMask::from_layers(&[layer.as_str()]).bits();
        }

        Ok(CapabilityMask::from_bits(bits).has_all(required.bits()))
    }

    /// Query the aggregate capability mask for a file across all layers.
    /// Returns the bitwise OR of all `capability_mask` values for the file.
    pub fn get_capability_mask(&self, file_id: &FileId) -> anyhow::Result<CapabilityMask> {
        let conn = self.lock_read();
        let mut stmt =
            conn.prepare("SELECT capability_mask FROM extraction_state WHERE file_id = ?1")?;
        let rows: Vec<i64> = stmt
            .query_map(params![file_id], |row| row.get(0))?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(?e, %file_id, "Capability mask row decode error, skipping");
                    None
                }
            })
            .collect();
        let mask = rows.iter().fold(0u16, |acc, &m| acc | (m as u16));
        Ok(CapabilityMask::new(mask))
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
            .map_err(|e| {
                tracing::warn!(
                    ?e,
                    %file_id,
                    "Unit capability mask query failed, defaulting to 0"
                );
                e
            })
            .ok()
            .flatten();
        Ok(CapabilityMask::new(mask.unwrap_or(0) as u16))
    }

    /// Return project-wide counts for capability analytics.
    ///
    /// Counts only fresh layers (content_hash still matches `files`):
    /// - `files_with_dataflow`: files with a fresh complete dataflow layer
    /// - `files_structural_only`: files with fresh structural but no dataflow
    /// - `files_manifest_only`: files with fresh manifest but no structural
    /// - `files_with_cfg`: count distinct files where fresh capability_mask has CFG bit
    pub fn get_capability_counts(&self) -> anyhow::Result<(usize, usize, usize, usize)> {
        let conn = self.lock_read();

        // Files with a fresh complete dataflow layer (file-level).
        let mut stmt = conn.prepare(
            "SELECT COUNT(DISTINCT l.file_id)
             FROM extraction_state l
             JOIN files f ON f.file_id = l.file_id
             WHERE l.unit_id IS NULL AND l.layer = 'dataflow'
               AND l.status = 'complete' AND l.content_hash = f.content_hash",
        )?;
        let files_with_dataflow: usize =
            stmt.query_row([], |row| row.get::<_, i64>(0))?.max(0) as usize;

        // Files with fresh structural but no dataflow layer.
        let mut stmt = conn.prepare(
            "SELECT COUNT(DISTINCT l.file_id)
             FROM extraction_state l
             JOIN files f ON f.file_id = l.file_id
             WHERE l.unit_id IS NULL AND l.layer = 'structural'
               AND l.content_hash = f.content_hash
               AND l.file_id NOT IN (
                 SELECT d.file_id FROM extraction_state d
                 JOIN files fd ON fd.file_id = d.file_id
                 WHERE d.unit_id IS NULL AND d.layer = 'dataflow'
                   AND d.status = 'complete' AND d.content_hash = fd.content_hash
               )",
        )?;
        let files_structural_only: usize =
            stmt.query_row([], |row| row.get::<_, i64>(0))?.max(0) as usize;

        // Files with fresh manifest but no structural layer.
        let mut stmt = conn.prepare(
            "SELECT COUNT(DISTINCT l.file_id)
             FROM extraction_state l
             JOIN files f ON f.file_id = l.file_id
             WHERE l.unit_id IS NULL AND l.layer = 'manifest'
               AND l.content_hash = f.content_hash
               AND l.file_id NOT IN (
                 SELECT d.file_id FROM extraction_state d
                 JOIN files fd ON fd.file_id = d.file_id
                 WHERE d.unit_id IS NULL AND d.layer = 'structural'
                   AND d.content_hash = fd.content_hash
               )",
        )?;
        let files_manifest_only: usize =
            stmt.query_row([], |row| row.get::<_, i64>(0))?.max(0) as usize;

        // Files where the aggregated capability_mask has CFG bit set.
        // Uses MAX(capability_mask) as a rough heuristic; a true answer
        // requires reading and OR-ing each file's rows (O(files) work).
        let mut stmt = conn.prepare(
            "SELECT COUNT(DISTINCT l.file_id)
             FROM extraction_state l
             JOIN files f ON f.file_id = l.file_id
             WHERE l.content_hash = f.content_hash
               AND (l.capability_mask & ?1) != 0",
        )?;
        let cfg_bit = CapabilityMask::CFG as i64;
        let files_with_cfg: usize = stmt
            .query_row(params![cfg_bit], |row| row.get::<_, i64>(0))?
            .max(0) as usize;

        Ok((
            files_with_dataflow,
            files_structural_only,
            files_manifest_only,
            files_with_cfg,
        ))
    }

    /// Derive actual capability for a set of files by querying the store.
    ///
    /// This is the **single source of truth** for extraction capability —
    /// no hardcoded masks.  Queries `extraction_state` and `symbol_edges`
    /// to determine what capabilities are actually available.
    ///
    /// # Bits set based on actual store content
    ///
    /// | Bit          | Condition                                                    |
    /// |--------------|--------------------------------------------------------------|
    /// | `MANIFEST`   | Any file has a `manifest` layer with status `complete`       |
    /// | `STRUCTURAL` | Any file has a `structural` layer with status `complete`     |
    /// | `CALL_EDGES` | Any `symbol_edges` exist for symbols belonging to these files|
    /// | `CFG`        | Always false (lazy structural doesn't build CFG)             |
    /// | `DATAFLOW`   | Always false (lazy structural doesn't build dataflow)        |
    /// | `SUMMARIES`  | Always false (lazy structural doesn't build summaries)       |
    ///
    /// The last three bits are extension points — when future lazy
    /// extraction layers are added, this function should query the
    /// corresponding tables (e.g. `cfg_nodes`, `dataflow_edges`,
    /// `function_summaries`) and set the bits accordingly.
    pub fn derive_capability_for_files(&self, file_ids: &[FileId]) -> CapabilityMask {
        if file_ids.is_empty() {
            return CapabilityMask::default();
        }

        let mut mask = CapabilityMask::default();
        let conn = self.lock_read();

        // ── Query extraction_state for manifest + structural layers ──────
        //
        // Follows the same layer→capability semantics as CapabilityMask::from_layers:
        // - "manifest" layer → MANIFEST_BIT
        // - "structural" layer → MANIFEST_BIT | STRUCTURAL_BIT (structural implies manifest)

        for file_id in file_ids {
            let has_manifest: bool = conn
                .query_row(
                    "SELECT 1 FROM extraction_state
                     WHERE file_id = ?1 AND layer = 'manifest' AND status = 'complete'",
                    params![file_id],
                    |_| Ok(()),
                )
                .is_ok();

            if has_manifest {
                mask.set(CapabilityMask::MANIFEST);
                break;
            }
        }

        for file_id in file_ids {
            let has_structural: bool = conn
                .query_row(
                    "SELECT 1 FROM extraction_state
                     WHERE file_id = ?1 AND layer = 'structural' AND status = 'complete'",
                    params![file_id],
                    |_| Ok(()),
                )
                .is_ok();

            if has_structural {
                // structural implies manifest (same as from_layers convention)
                mask.set(CapabilityMask::MANIFEST);
                mask.set(CapabilityMask::STRUCTURAL);
                break;
            }
        }

        // ── Query symbol_edges joined with symbols ───────────────────────
        //
        // We check whether any edges exist whose source or target symbol
        // belongs to one of the given files.  A single edge is enough to
        // set CALL_EDGES — the presence of any call-graph edge implies the
        // call-graph layer was built.

        // Build dynamic IN clause.  FileIds are BLOBs so we need
        // placeholders for each file_id.
        let placeholders: Vec<String> =
            (0..file_ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let in_clause = placeholders.join(",");

        let sql = format!(
            "SELECT 1 FROM symbol_edges
              WHERE source IN (SELECT symbol_id FROM symbols WHERE file_id IN ({in_clause}))
                 OR target IN (SELECT symbol_id FROM symbols WHERE file_id IN ({in_clause}))
              LIMIT 1",
        );

        let params: Vec<&dyn rusqlite::types::ToSql> = file_ids
            .iter()
            .map(|fid| fid as &dyn rusqlite::types::ToSql)
            .collect();

        let has_edges: bool = conn
            .prepare(&sql)
            .ok()
            .and_then(|mut stmt| stmt.query_row(params.as_slice(), |_| Ok(())).ok())
            .is_some();

        if has_edges {
            mask.set(CapabilityMask::CALL_EDGES);
        }

        // CFG, DATAFLOW, SUMMARIES — not built by lazy structural.
        // These are extension points; when future lazy extraction layers
        // are added, query `cfg_nodes`, `dataflow_edges`, and
        // `function_summaries` respectively and set the bits here.

        mask
    }
}

fn complete_count(layer_counts: &[(String, String, i64)], layer: &str) -> i64 {
    layer_counts
        .iter()
        .filter(|(l, s, _)| l == layer && s == "complete")
        .map(|(_, _, c)| *c)
        .sum()
}

fn compute_index_mode(
    total_files: i64,
    manifest_complete: i64,
    structural_complete: i64,
    dataflow_file_complete: i64,
    lazy_total_unit_states: i64,
    lazy_has_dataflow: bool,
) -> &'static str {
    let structural_or_better_complete = structural_complete.max(dataflow_file_complete);

    if total_files == 0 {
        "none"
    } else if dataflow_file_complete >= total_files {
        // `--analysis full` writes a per-file `dataflow` layer. That layer
        // implies structural facts even when no separate structural row exists.
        "full"
    } else if structural_or_better_complete == 0 {
        if manifest_complete > 0 {
            "manifest"
        } else {
            "unknown"
        }
    } else if structural_or_better_complete < total_files {
        if lazy_total_unit_states > 0 {
            "partial_structural+lazy"
        } else {
            "partial_structural"
        }
    } else if lazy_total_unit_states > 0 {
        "structural+lazy"
    } else if dataflow_file_complete >= total_files || lazy_has_dataflow {
        "full"
    } else {
        "structural"
    }
}
