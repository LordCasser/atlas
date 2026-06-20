//! Extraction state tracking over the `extraction_state` SQLite table.
//!
//! Covers both file-level (`unit_id IS NULL`) and unit-level (`unit_id IS NOT NULL`)
//! extraction state queries, plus lazy dataflow persistence helpers.

use super::Store;
use rusqlite::{OptionalExtension, params};
use tracing::debug_span;
use types::*;

use crate::store_rows::{UnitExtractionStateRecord, row_to_unit_extraction_state};
use crate::store_writers::{
    write_binding_uses, write_bindings, write_cfg_edges, write_cfg_nodes, write_data_nodes,
    write_dataflow_edges,
};

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
        let result = stmt
            .query_row(params![file_id, layer], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
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

    /// Detect stale structural facts where call references are owned by
    /// non-callable C/C++ symbols. This catches old extractor output where
    /// plain enum-typed local variables were misclassified as enum definitions
    /// and then stole ownership for the enclosing function body.
    pub fn file_has_non_callable_call_reference_sources(
        &self,
        file_id: &FileId,
    ) -> anyhow::Result<bool> {
        let conn = self.lock_read();
        let count: i64 = conn.query_row(
            r#"SELECT COUNT(*)
               FROM "references" r
               JOIN symbols s ON s.symbol_id = r.source_symbol
               JOIN files f ON f.file_id = r.file_id
               WHERE r.file_id = ?1
                 AND r.kind = 'call'
                 AND f.language IN ('c', 'cpp')
                 AND s.kind NOT IN ('function', 'method', 'constructor')
                 AND EXISTS (
                     SELECT 1 FROM symbols owner
                     WHERE owner.file_id = r.file_id
                       AND owner.kind IN ('function', 'method', 'constructor')
                       AND r.range_start_byte >= owner.range_start_byte
                       AND r.range_start_byte < owner.range_end_byte
                 )"#,
            params![file_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
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
    /// no hardcoded masks.  Aggregates `capability_mask` from fresh
    /// `extraction_state` rows (OR across all layers), with layer-string
    /// fallback for lazy-extraction records that write `capability_mask=0`.
    /// Also queries `symbol_edges` to detect CALL_EDGES when not already
    /// covered by stored masks.
    ///
    /// # Bits set based on actual store content
    ///
    /// | Bit          | Condition                                                      |
    /// |--------------|----------------------------------------------------------------|
    /// | `MANIFEST`   | Fresh `capability_mask` OR layer="manifest"                    |
    /// | `STRUCTURAL` | Fresh `capability_mask` OR layer="structural"                  |
    /// | `CALL_EDGES` | Fresh `capability_mask` OR `symbol_edges` for these files      |
    /// | `CFG`        | Fresh `capability_mask` from full-index rows                   |
    /// | `DATAFLOW`   | Fresh `capability_mask` from full-index rows                   |
    /// | `SUMMARIES`  | Fresh `capability_mask` from full-index rows                   |
    pub fn derive_capability_for_files(&self, file_ids: &[FileId]) -> CapabilityMask {
        if file_ids.is_empty() {
            return CapabilityMask::default();
        }

        let conn = self.lock_read();
        let mut aggregated = 0u16;

        // Build dynamic IN clause — FileIds are BLOBs so we need
        // placeholders for each file_id.
        let placeholders: Vec<String> =
            (0..file_ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let in_clause = placeholders.join(",");

        let params: Vec<&dyn rusqlite::types::ToSql> = file_ids
            .iter()
            .map(|fid| fid as &dyn rusqlite::types::ToSql)
            .collect();

        // ── Aggregate capability_mask from fresh extraction_state rows ──
        //
        // Joins with `files` on content_hash to skip stale rows.
        // Also applies `from_layers` on the layer string as a fallback
        // for lazy-extraction records that write `capability_mask=0`.
        let cap_sql = format!(
            "SELECT l.capability_mask, l.layer
             FROM extraction_state l
             JOIN files f ON f.file_id = l.file_id
             WHERE l.file_id IN ({in_clause})
               AND l.content_hash = f.content_hash
               AND l.unit_id IS NULL",
        );

        if let Ok(mut stmt) = conn.prepare(&cap_sql) {
            if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            }) {
                for (cap_mask_i64, layer) in rows.flatten() {
                    aggregated |= cap_mask_i64 as u16;
                    aggregated |= CapabilityMask::from_layers(&[layer.as_str()]).bits();
                }
            }
        }

        // ── Check symbol_edges for CALL_EDGES ───────────────────────────
        //
        // Only checked when CALL_EDGES is not already set by stored masks.
        // Any edge whose source or target belongs to one of the given files
        // implies the call-graph layer was built.
        if aggregated & CapabilityMask::CALL_EDGES == 0 {
            let edge_sql = format!(
                "SELECT 1 FROM symbol_edges
                  WHERE source IN (SELECT symbol_id FROM symbols WHERE file_id IN ({in_clause}))
                     OR target IN (SELECT symbol_id FROM symbols WHERE file_id IN ({in_clause}))
                  LIMIT 1",
            );

            if let Ok(mut stmt) = conn.prepare(&edge_sql) {
                if stmt.query_row(params.as_slice(), |_| Ok(())).is_ok() {
                    aggregated |= CapabilityMask::CALL_EDGES;
                }
            }
        }

        CapabilityMask::from_bits(aggregated)
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

// ── Unit-level extraction state ────────────────────────────────────────

impl Store {
    // ── Unit Extraction State CRUD ──────────────────────────────────────────

    /// Look up extraction state for a (file_id, unit_id, layer) triple.
    pub fn get_unit_extraction_state(
        &self,
        file_id: &FileId,
        unit_id: &[u8; 16],
        layer: &str,
    ) -> anyhow::Result<Option<UnitExtractionStateRecord>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT file_id, unit_id, layer, content_hash, status,
                    node_count, edge_count, budget_exceeded, updated_at, capability_mask
             FROM extraction_state
             WHERE file_id = ?1 AND unit_id = ?2 AND layer = ?3",
        )?;
        let unit_blob: &[u8] = unit_id;
        let mut rows = stmt.query_map(
            params![file_id, unit_blob, layer],
            row_to_unit_extraction_state,
        )?;
        match rows.next() {
            Some(Ok(a)) => Ok(Some(a)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Insert or update unit-level extraction state.
    pub fn upsert_unit_extraction_state(
        &self,
        record: &UnitExtractionStateRecord,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        let unit_blob: &[u8] = &record.unit_id;
        conn.execute(
            "DELETE FROM extraction_state
             WHERE file_id = ?1 AND unit_id = ?2 AND layer = ?3",
            params![record.file_id, unit_blob, record.layer],
        )?;
        conn.execute(
            "INSERT INTO extraction_state
             (file_id, unit_id, layer, content_hash, status,
              node_count, edge_count, budget_exceeded, capability_mask, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'))",
            params![
                record.file_id,
                unit_blob,
                record.layer,
                record.content_hash,
                record.status,
                record.node_count,
                record.edge_count,
                record.budget_exceeded as i32,
                record.capability_mask.bits() as i64,
            ],
        )?;
        Ok(())
    }

    /// Delete all unit-level extraction state for a file.
    pub fn delete_unit_extraction_state_for_file(&self, file_id: &FileId) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM extraction_state WHERE file_id = ?1 AND unit_id IS NOT NULL",
            params![file_id],
        )?;
        Ok(())
    }

    // ── Lazy dataflow write ─────────────────────────────────────────────────

    /// Replace all dataflow, binding, and CFG data for a single AnalysisUnit.
    ///
    /// Executes in a single transaction:
    /// 1. DELETE dataflow_edges targeting nodes in this unit
    /// 2. DELETE data_nodes belonging to this unit
    /// 3. DELETE cfg_edges for cfg_nodes in this unit
    /// 4. DELETE cfg_nodes belonging to this unit
    /// 5. DELETE binding_uses in this unit (by file + scope filter)
    /// 6. DELETE bindings in this unit
    /// 7. INSERT new rows
    ///
    /// For function units, the unit is identified by `function_id`.
    /// For top-level units, identified by `file_id + function_id IS NULL`.
    #[allow(clippy::too_many_arguments)]
    pub fn replace_dataflow_for_unit(
        &self,
        unit: &types::lazy::AnalysisUnit,
        data_nodes: &[DataNode],
        dataflow_edges: &[DataFlowEdge],
        bindings: &[BindingDef],
        binding_uses: &[BindingUse],
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
    ) -> anyhow::Result<()> {
        let _span = debug_span!(target: "atlas_db", "db.replace_dataflow").entered();
        // Always run the delete+insert transaction, even when the new payload
        // is empty.  An empty replacement means the unit no longer has any
        // dataflow — but stale rows from a previous build must still be
        // cleaned.  Skipping the delete phase would leave orphan dataflow
        // rows referencing structures that no longer exist.

        self.with_transaction(|tx| {
            // ── DELETE old rows ─────────────────────────────────────────
            // Order matters: edges first (FK → nodes), then nodes.

            // Identify data_node_ids to delete
            {
                let mut stmt = if unit.symbol_id.is_some() {
                    tx.prepare("SELECT data_node_id FROM data_nodes WHERE function_id = ?1")?
                } else {
                    tx.prepare(
                        "SELECT data_node_id FROM data_nodes WHERE file_id = ?1 AND function_id IS NULL",
                    )?
                };
                let dn_ids: Vec<DataNodeId> = if let Some(ref func_id) = unit.symbol_id {
                    stmt.query_map(params![func_id], |row| row.get::<_, DataNodeId>(0))?
                        .filter_map(|r| match r {
                            Ok(v) => Some(v),
                            Err(e) => {
                                tracing::warn!(?e, "DataNode ID decode error (by function), skipping");
                                None
                            }
                        })
                        .collect()
                } else {
                    stmt.query_map(params![unit.file_id], |row| row.get::<_, DataNodeId>(0))?
                        .filter_map(|r| match r {
                            Ok(v) => Some(v),
                            Err(e) => {
                                tracing::warn!(?e, "DataNode ID decode error (by file), skipping");
                                None
                            }
                        })
                        .collect()
                };

                // Delete dataflow_edges referencing these nodes
                for dn_id in &dn_ids {
                    tx.execute(
                        "DELETE FROM dataflow_edges WHERE source = ?1 OR target = ?1",
                        params![dn_id],
                    )?;
                }

                // Delete data_nodes
                if let Some(ref func_id) = unit.symbol_id {
                    tx.execute(
                        "DELETE FROM data_nodes WHERE function_id = ?1",
                        params![func_id],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM data_nodes WHERE file_id = ?1 AND function_id IS NULL",
                        params![unit.file_id],
                    )?;
                }
            }

            // Clean up cfg_edges → cfg_nodes
            {
                let mut stmt = if unit.symbol_id.is_some() {
                    tx.prepare("SELECT cfg_node_id FROM cfg_nodes WHERE function_id = ?1")?
                } else {
                    tx.prepare(
                        "SELECT cfg_node_id FROM cfg_nodes WHERE file_id = ?1 AND function_id IS NULL",
                    )?
                };
                let cn_ids: Vec<CfgNodeId> = if let Some(ref func_id) = unit.symbol_id {
                    stmt.query_map(params![func_id], |row| row.get::<_, CfgNodeId>(0))?
                        .filter_map(|r| match r {
                            Ok(v) => Some(v),
                            Err(e) => {
                                tracing::warn!(?e, "CfgNode ID decode error (by function), skipping");
                                None
                            }
                        })
                        .collect()
                } else {
                    stmt.query_map(params![unit.file_id], |row| row.get::<_, CfgNodeId>(0))?
                        .filter_map(|r| match r {
                            Ok(v) => Some(v),
                            Err(e) => {
                                tracing::warn!(?e, "CfgNode ID decode error (by file), skipping");
                                None
                            }
                        })
                        .collect()
                };

                for cn_id in &cn_ids {
                    tx.execute(
                        "DELETE FROM cfg_edges WHERE source_node = ?1 OR target_node = ?1",
                        params![cn_id],
                    )?;
                }

                if let Some(ref func_id) = unit.symbol_id {
                    tx.execute(
                        "DELETE FROM cfg_nodes WHERE function_id = ?1",
                        params![func_id],
                    )?;
                } else {
                    tx.execute("DELETE FROM cfg_nodes WHERE file_id = ?1 AND function_id IS NULL", params![unit.file_id])?;
                }
            }

            // Clean up bindings + binding_uses for this unit.
            // For function units, match by function_id.
            // For top-level, match by file_id with function_id IS NULL.
            {
                let binding_ids: Vec<BindingId> = if let Some(ref func_id) = unit.symbol_id {
                    let mut stmt = tx.prepare(
                        "SELECT binding_id FROM bindings WHERE function_id = ?1",
                    )?;
                    stmt.query_map(params![func_id], |row| row.get::<_, BindingId>(0))?
                        .filter_map(|r| match r {
                            Ok(v) => Some(v),
                            Err(e) => {
                                tracing::warn!(?e, "Binding ID decode error (by function), skipping");
                                None
                            }
                        })
                        .collect()
                } else {
                    let mut stmt = tx.prepare(
                        "SELECT binding_id FROM bindings WHERE file_id = ?1 AND function_id IS NULL",
                    )?;
                    stmt.query_map(params![unit.file_id], |row| row.get::<_, BindingId>(0))?
                        .filter_map(|r| match r {
                            Ok(v) => Some(v),
                            Err(e) => {
                                tracing::warn!(?e, "Binding ID decode error (by file), skipping");
                                None
                            }
                        })
                        .collect()
                };

                for bid in &binding_ids {
                    tx.execute(
                        "DELETE FROM binding_uses WHERE binding_id = ?1",
                        params![bid],
                    )?;
                }

                if let Some(ref func_id) = unit.symbol_id {
                    tx.execute(
                        "DELETE FROM bindings WHERE function_id = ?1",
                        params![func_id],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM bindings WHERE file_id = ?1 AND function_id IS NULL",
                        params![unit.file_id],
                    )?;
                }
            }

            // ── FK-guarded validation ───────────────────────────────
            // Before inserting, verify that every FK reference points to
            // an entity that exists in the DB (symbols, scopes) or in the
            // same batch (bindings, data_nodes, cfg_nodes).  Rows whose
            // FK references cannot be satisfied are silently dropped.
            // This mirrors the defensive FK guards in insert_file_facts_impl
            // but queries against DB state instead of in-memory batches.
            let validated = super::fk_guards::validate_dataflow_payload_db(
                tx,
                data_nodes,
                dataflow_edges,
                bindings,
                binding_uses,
                cfg_nodes,
                cfg_edges,
            )?;

            // ── INSERT new rows ─────────────────────────────────────────
            if !validated.bindings.is_empty() {
                write_bindings(tx, &validated.bindings)?;
            }
            if !validated.binding_uses.is_empty() {
                write_binding_uses(tx, &validated.binding_uses)?;
            }
            if !validated.data_nodes.is_empty() {
                write_data_nodes(tx, &validated.data_nodes)?;
            }
            if !validated.dataflow_edges.is_empty() {
                write_dataflow_edges(tx, &validated.dataflow_edges)?;
            }
            if !validated.cfg_nodes.is_empty() {
                write_cfg_nodes(tx, &validated.cfg_nodes)?;
            }
            if !validated.cfg_edges.is_empty() {
                write_cfg_edges(tx, &validated.cfg_edges)?;
            }

            Ok(())
        })
    }

    /// Backfill callsite argument `data_node_id` after lazy dataflow build.
    ///
    /// For each callsite whose `caller` is the unit's symbol, matches
    /// `CallArg` data nodes to arguments by byte offset and updates
    /// `args_json` in-place.
    pub fn update_callsite_arg_data_nodes(
        &self,
        unit: &types::lazy::AnalysisUnit,
        data_nodes: &[DataNode],
    ) -> anyhow::Result<()> {
        let caller = match unit.symbol_id {
            Some(sid) => sid,
            None => return Ok(()), // top-level — no per-function callsites to backfill
        };

        let conn = self.lock();
        let mut cs_stmt =
            conn.prepare("SELECT callsite_id, args_json FROM callsites WHERE caller = ?1")?;
        let cs_rows: Vec<(CallsiteId, String)> = cs_stmt
            .query_map(params![caller], |row| {
                Ok((row.get::<_, CallsiteId>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(?e, "Callsite row decode error, skipping");
                    None
                }
            })
            .collect();

        for (cs_id, args_json) in cs_rows {
            let mut args: Vec<ArgumentFact> =
                serde_json::from_str(&args_json).unwrap_or_else(|e| {
                    tracing::error!(
                        ?e,
                        callsite_id = %cs_id,
                        "Callsite args JSON corrupt, using empty vec"
                    );
                    Vec::new()
                });

            // Find CallArg data nodes for this callsite
            let arg_nodes: Vec<&DataNode> = data_nodes
                .iter()
                .filter(|dn| {
                    dn.kind == DataNodeKind::CallArg && dn.callsite_id.as_ref() == Some(&cs_id)
                })
                .collect();

            if arg_nodes.is_empty() {
                continue;
            }

            for arg in args.iter_mut() {
                if let Some(arg_range) = arg.range {
                    for dn in &arg_nodes {
                        if dn.range.start_byte == arg_range.start_byte {
                            arg.data_node_id = Some(dn.id);
                            break;
                        }
                    }
                }
            }

            // Write back updated args_json
            let new_json = serde_json::to_string(&args)?;
            conn.execute(
                "UPDATE callsites SET args_json = ?1 WHERE callsite_id = ?2",
                params![new_json, cs_id],
            )?;
        }

        Ok(())
    }

    /// Count data_nodes that belong to a specific AnalysisUnit.
    ///
    /// Used by the lazy loader to detect whether a full index
    /// (`atlas index --analysis full`) has already built dataflow for
    /// this unit, so it can skip lazy extraction and avoid deleting
    /// pre-built data.
    pub fn count_data_nodes_for_unit(
        &self,
        unit: &types::lazy::AnalysisUnit,
    ) -> anyhow::Result<usize> {
        let conn = self.lock_read();
        let count: i64 = if let Some(ref func_id) = unit.symbol_id {
            conn.query_row(
                "SELECT COUNT(*) FROM data_nodes WHERE function_id = ?1",
                params![func_id],
                |row| row.get(0),
            )?
        } else {
            conn.query_row(
                "SELECT COUNT(*) FROM data_nodes WHERE file_id = ?1 AND function_id IS NULL",
                params![unit.file_id],
                |row| row.get(0),
            )?
        };
        Ok(count as usize)
    }

    /// Get lazy dataflow statistics for status display.
    pub fn get_lazy_dataflow_stats(&self) -> anyhow::Result<LazyDataflowStats> {
        let conn = self.lock_read();
        let total_unit_states: i64 = conn.query_row(
            "SELECT COUNT(*) FROM extraction_state
             WHERE unit_id IS NOT NULL AND layer = 'dataflow'",
            [],
            |r| r.get(0),
        )?;
        let partial_unit_states: i64 = conn.query_row(
            "SELECT COUNT(*) FROM extraction_state
             WHERE unit_id IS NOT NULL AND layer = 'dataflow' AND budget_exceeded = 1",
            [],
            |r| r.get(0),
        )?;
        let has_dataflow: bool = conn
            .query_row("SELECT COUNT(*) FROM data_nodes LIMIT 1", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|c| c > 0)
            .unwrap_or_else(|e| {
                tracing::warn!(?e, "Failed to query data_nodes count, assuming false");
                false
            });
        Ok(LazyDataflowStats {
            total_unit_states,
            partial_unit_states,
            has_dataflow,
        })
    }
}

/// Summary of lazy dataflow state for atlas_status.
#[derive(Debug, Clone)]
pub struct LazyDataflowStats {
    pub total_unit_states: i64,
    pub partial_unit_states: i64,
    pub has_dataflow: bool,
}
