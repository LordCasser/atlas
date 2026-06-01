//! Unit-level extraction state tracking: CRUD over unit-level `extraction_state`,
//! plus `replace_dataflow_for_unit` and `update_callsite_arg_data_nodes`.

use rusqlite::params;
use types::*;

use super::Store;
use crate::store_rows::{UnitExtractionStateRecord, row_to_unit_extraction_state};
use crate::store_writers::{
    write_binding_uses, write_bindings, write_cfg_edges, write_cfg_nodes, write_data_nodes,
    write_dataflow_edges,
};

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
                    node_count, edge_count, budget_exceeded, updated_at
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
              node_count, edge_count, budget_exceeded, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))",
            params![
                record.file_id,
                unit_blob,
                record.layer,
                record.content_hash,
                record.status,
                record.node_count,
                record.edge_count,
                record.budget_exceeded as i32,
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
                        .filter_map(|r| r.ok())
                        .collect()
                } else {
                    stmt.query_map(params![unit.file_id], |row| row.get::<_, DataNodeId>(0))?
                        .filter_map(|r| r.ok())
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
                        .filter_map(|r| r.ok())
                        .collect()
                } else {
                    stmt.query_map(params![unit.file_id], |row| row.get::<_, CfgNodeId>(0))?
                        .filter_map(|r| r.ok())
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
                        .filter_map(|r| r.ok())
                        .collect()
                } else {
                    let mut stmt = tx.prepare(
                        "SELECT binding_id FROM bindings WHERE file_id = ?1 AND function_id IS NULL",
                    )?;
                    stmt.query_map(params![unit.file_id], |row| row.get::<_, BindingId>(0))?
                        .filter_map(|r| r.ok())
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
            .filter_map(|r| r.ok())
            .collect();

        for (cs_id, args_json) in cs_rows {
            let mut args: Vec<ArgumentFact> = serde_json::from_str(&args_json).unwrap_or_default();

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
            .unwrap_or(false);
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
