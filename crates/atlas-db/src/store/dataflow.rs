//! Dataflow domain: bindings, binding uses, data nodes, dataflow edges.

use std::collections::HashMap;

use atlas_types::*;
use rusqlite::params;

use super::Store;
use crate::store_rows::{
    row_to_binding, row_to_binding_use, row_to_data_node, row_to_dataflow_edge,
};
use crate::store_writers::{
    write_binding_uses, write_bindings, write_data_nodes, write_dataflow_edges,
};

impl Store {
    // ── Bindings — write ───────────────────────────────────────────────────

    /// Batch-insert bindings.
    pub fn insert_bindings(&self, bindings: &[BindingDef]) -> anyhow::Result<()> {
        if bindings.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_bindings(tx, bindings))
    }

    /// Batch-insert binding uses.
    pub fn insert_binding_uses(&self, uses: &[BindingUse]) -> anyhow::Result<()> {
        if uses.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_binding_uses(tx, uses))
    }

    // ── Bindings — query ───────────────────────────────────────────────────

    /// Find bindings for a function.
    pub fn find_bindings_by_function(
        &self,
        function_id: &SymbolId,
    ) -> anyhow::Result<Vec<BindingDef>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT binding_id, file_id, function_id, scope_id, kind, name, symbol_id,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM bindings WHERE function_id = ?1",
        )?;
        let rows = stmt.query_map(params![function_id], row_to_binding)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find all bindings in a file.
    pub fn find_bindings_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<BindingDef>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT binding_id, file_id, function_id, scope_id, kind, name, symbol_id,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM bindings WHERE file_id = ?1",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_binding)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find binding uses for a specific binding.
    pub fn find_binding_uses_by_binding(
        &self,
        binding_id: &BindingId,
    ) -> anyhow::Result<Vec<BindingUse>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT binding_use_id, file_id, scope_id, binding_id, reference_id, name,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM binding_uses WHERE binding_id = ?1",
        )?;
        let rows = stmt.query_map(params![binding_id], row_to_binding_use)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find all binding uses in a file.
    pub fn find_binding_uses_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<BindingUse>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT binding_use_id, file_id, scope_id, binding_id, reference_id, name,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM binding_uses WHERE file_id = ?1",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_binding_use)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ── Data nodes — write ─────────────────────────────────────────────────

    /// Batch-insert data nodes.
    pub fn insert_data_nodes(&self, nodes: &[DataNode]) -> anyhow::Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_data_nodes(tx, nodes))
    }

    // ── Data nodes — query ─────────────────────────────────────────────────

    /// Find data nodes for a function.
    pub fn find_data_nodes_by_function(
        &self,
        function_id: &SymbolId,
    ) -> anyhow::Result<Vec<DataNode>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT data_node_id, file_id, function_id, kind, binding_id, callsite_id,
                    name, access_path, arg_index,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM data_nodes WHERE function_id = ?1
             ORDER BY range_start_byte ASC",
        )?;
        let rows = stmt.query_map(params![function_id], row_to_data_node)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get a single data node by ID.
    pub fn get_data_node(&self, node_id: &DataNodeId) -> anyhow::Result<Option<DataNode>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT data_node_id, file_id, function_id, kind, binding_id, callsite_id,
                    name, access_path, arg_index,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM data_nodes WHERE data_node_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![node_id], row_to_data_node)?;
        match rows.next() {
            Some(Ok(node)) => Ok(Some(node)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Batch-lookup data nodes by IDs in a single query.
    pub fn get_data_nodes(
        &self,
        ids: &[DataNodeId],
    ) -> anyhow::Result<HashMap<DataNodeId, DataNode>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.lock_read();
        let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT data_node_id, file_id, function_id, kind, binding_id, callsite_id,
                    name, access_path, arg_index,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM data_nodes WHERE data_node_id IN ({})",
            placeholders.join(","),
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            let node: DataNode = row_to_data_node(row)?;
            Ok((node.id, node))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (id, node) = row?;
            map.insert(id, node);
        }
        Ok(map)
    }

    /// Find all data nodes in a file.
    pub fn find_data_nodes_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<DataNode>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT data_node_id, file_id, function_id, kind, binding_id, callsite_id,
                    name, access_path, arg_index,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM data_nodes WHERE file_id = ?1",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_data_node)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find data nodes associated with a specific callsite (e.g. CallArg nodes).
    ///
    /// Used by summary-bridge trace to find call-arg data nodes for a given callsite.
    pub fn find_data_nodes_by_callsite(
        &self,
        callsite_id: &CallsiteId,
    ) -> anyhow::Result<Vec<DataNode>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT data_node_id, file_id, function_id, kind, binding_id, callsite_id,
                    name, access_path, arg_index,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column
             FROM data_nodes WHERE callsite_id = ?1",
        )?;
        let rows = stmt.query_map(params![callsite_id], row_to_data_node)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ── Dataflow edges — write ─────────────────────────────────────────────

    /// Batch-insert dataflow edges.
    pub fn insert_dataflow_edges(&self, edges: &[DataFlowEdge]) -> anyhow::Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_dataflow_edges(tx, edges))
    }

    // ── Dataflow edges — query ─────────────────────────────────────────────

    /// Find dataflow edges originating from a data node.
    pub fn find_dataflow_edges_by_source(
        &self,
        source: &DataNodeId,
    ) -> anyhow::Result<Vec<DataFlowEdge>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT dataflow_edge_id, source, target, kind,
                    location_0, location_1, location_2,
                    location_3, location_4, location_5, confidence
             FROM dataflow_edges WHERE source = ?1",
        )?;
        let rows = stmt.query_map(params![source], row_to_dataflow_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find dataflow edges targeting a data node.
    pub fn find_dataflow_edges_by_target(
        &self,
        target: &DataNodeId,
    ) -> anyhow::Result<Vec<DataFlowEdge>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT dataflow_edge_id, source, target, kind,
                    location_0, location_1, location_2,
                    location_3, location_4, location_5, confidence
             FROM dataflow_edges WHERE target = ?1",
        )?;
        let rows = stmt.query_map(params![target], row_to_dataflow_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Batch-lookup dataflow edges by source IDs in a single query.
    pub fn find_dataflow_edges_by_sources(
        &self,
        sources: &[DataNodeId],
    ) -> anyhow::Result<Vec<DataFlowEdge>> {
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock_read();
        let placeholders: Vec<String> = (0..sources.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT dataflow_edge_id, source, target, kind,
                    location_0, location_1, location_2,
                    location_3, location_4, location_5, confidence
             FROM dataflow_edges WHERE source IN ({})",
            placeholders.join(","),
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = sources
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), row_to_dataflow_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
