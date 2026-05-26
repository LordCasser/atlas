//! CFG domain: control-flow graph nodes and edges.

use rusqlite::params;
use types::*;

use super::Store;
use crate::store_rows::{row_to_cfg_edge, row_to_cfg_node};
use crate::store_writers::{write_cfg_edges, write_cfg_nodes};

impl Store {
    // ── CFG — write APIs ───────────────────────────────────────────────────

    /// Batch-insert CFG nodes.
    pub fn insert_cfg_nodes(&self, nodes: &[CfgNode]) -> anyhow::Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_cfg_nodes(tx, nodes))
    }

    /// Batch-insert CFG edges.
    pub fn insert_cfg_edges(&self, edges: &[CfgEdge]) -> anyhow::Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_cfg_edges(tx, edges))
    }

    // ── CFG — query APIs ───────────────────────────────────────────────────

    /// Find all CFG nodes for a function.
    pub fn find_cfg_nodes_by_function(
        &self,
        function_id: &SymbolId,
    ) -> anyhow::Result<Vec<CfgNode>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT cfg_node_id, function_id, kind,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM cfg_nodes WHERE function_id = ?1",
        )?;
        let rows = stmt.query_map(params![function_id], row_to_cfg_node)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find CFG edges originating from a CFG node.
    pub fn find_cfg_edges_by_source(&self, source: &CfgNodeId) -> anyhow::Result<Vec<CfgEdge>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT cfg_edge_id, source_node, target_node, kind FROM cfg_edges WHERE source_node = ?1",
        )?;
        let rows = stmt.query_map(params![source], row_to_cfg_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
