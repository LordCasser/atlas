//! FK guard utilities for dataflow payload validation.
//!
//! Provides reusable filtering logic that ensures every row written to
//! dataflow-related tables has valid foreign key references.  Two validation
//! modes are supported:
//!
//! | Mode | Use case | Check against |
//! |------|----------|---------------|
//! | **DB-resident** | `replace_dataflow_for_unit` (lazy build) | Queries `symbols`/`scopes` in DB |
//! | **Batch-resident** | `insert_file_facts_impl` (full index) | In-memory `HashSet` of symbols/scopes |
//!
//! The filtering functions themselves are shared between modes — only the
//! set construction differs.

use std::collections::HashSet;

use rusqlite::Connection;
use types::bindings::{BindingDef, BindingUse};
use types::cfg::{CfgEdge, CfgNode};
use types::dataflow::{DataFlowEdge, DataNode};
use types::ids::{BindingId, CfgNodeId, DataNodeId, ScopeId, SymbolId};

// ── DB-resident validation (for lazy build path) ─────────────────────────

/// Query the DB for existing symbol IDs referenced by bindings and data nodes.
///
/// Returns a `HashSet` of all `SymbolId`s that actually exist in the
/// `symbols` table — used as the allowlist for FK filtering.
pub(crate) fn query_existing_function_ids(
    conn: &Connection,
    bindings: &[BindingDef],
    data_nodes: &[DataNode],
) -> anyhow::Result<HashSet<SymbolId>> {
    let mut ids: HashSet<SymbolId> = HashSet::new();
    for b in bindings {
        if let Some(fid) = b.function_id {
            ids.insert(fid);
        }
        if let Some(sid) = b.symbol_id {
            ids.insert(sid);
        }
    }
    for dn in data_nodes {
        if let Some(fid) = dn.function_id {
            ids.insert(fid);
        }
    }
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let id_list: Vec<SymbolId> = ids.into_iter().collect();
    let existing = query_existing_ids(conn, "symbols", "symbol_id", &id_list)?;
    Ok(existing.into_iter().collect())
}

/// Query the DB for existing scope IDs referenced by bindings.
pub(crate) fn query_existing_scope_ids(
    conn: &Connection,
    bindings: &[BindingDef],
) -> anyhow::Result<HashSet<ScopeId>> {
    let ids: HashSet<ScopeId> = bindings.iter().map(|b| b.scope_id).collect();
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let id_list: Vec<ScopeId> = ids.into_iter().collect();
    Ok(query_existing_ids(conn, "scopes", "scope_id", &id_list)?
        .into_iter()
        .collect())
}

// ── Generic batched existence query ──────────────────────────────────────

/// Batch-query existence of IDs in a given table column.
///
/// Uses a single `IN (?, ?, ...)` query to avoid N+1 overhead.
/// Returns a `Vec` of IDs that exist — the caller typically wraps this
/// in a `HashSet` for O(1) lookups during filtering.
fn query_existing_ids<T>(
    conn: &Connection,
    table: &str,
    column: &str,
    ids: &[T],
) -> anyhow::Result<Vec<T>>
where
    T: rusqlite::types::ToSql + rusqlite::types::FromSql + Clone,
{
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT DISTINCT {} FROM {} WHERE {} IN ({})",
        column,
        table,
        column,
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, T>(0))?;
    let existing: Vec<T> = rows
        .filter_map(|r| match r {
            Ok(id) => Some(id),
            Err(e) => {
                // Row decode failure — skip this row rather than losing the
                // entire allowlist.  A malformed BLOB in the DB (e.g. wrong
                // length for the ID type) is a data-integrity concern that
                // callers should address separately.
                tracing::warn!(
                    ?e,
                    %table,
                    %column,
                    "Failed to decode existing ID in FK allowlist query"
                );
                None
            }
        })
        .collect();
    Ok(existing)
}

// ── Shared filtering functions (mode-agnostic) ───────────────────────────

/// Filter bindings to only those whose FK references are satisfied.
pub(crate) fn filter_bindings(
    bindings: &[BindingDef],
    valid_function_ids: &HashSet<SymbolId>,
    valid_scope_ids: &HashSet<ScopeId>,
) -> Vec<BindingDef> {
    bindings
        .iter()
        .filter(|b| {
            b.function_id
                .is_none_or(|fid| valid_function_ids.contains(&fid))
                && valid_scope_ids.contains(&b.scope_id)
                && b.symbol_id
                    .is_none_or(|sid| valid_function_ids.contains(&sid))
            // NOTE: symbol_id FK is ON DELETE SET NULL, so NULL is always valid.
            // A non-NULL symbol_id must pass the same check as function_id.
        })
        .cloned()
        .collect()
}

/// Filter binding uses to only those referencing existing bindings.
pub(crate) fn filter_binding_uses(
    uses: &[BindingUse],
    valid_binding_ids: &HashSet<BindingId>,
) -> Vec<BindingUse> {
    uses.iter()
        .filter(|u| {
            u.binding_id
                .map(|bid| valid_binding_ids.contains(&bid))
                .unwrap_or(true)
            // binding_id FK is ON DELETE SET NULL — NULL is always allowed
        })
        .cloned()
        .collect()
}

/// Filter data nodes to only those whose FK references are satisfied.
pub(crate) fn filter_data_nodes(
    nodes: &[DataNode],
    valid_function_ids: &HashSet<SymbolId>,
    valid_binding_ids: &HashSet<BindingId>,
) -> Vec<DataNode> {
    // Drop only on invalid function_id. For binding_id, clear the FK instead of
    // dropping the node — schema is ON DELETE SET NULL, and Focus materialize
    // re-extracts bindings with fresh ScopeIds that may not match structural
    // scopes already in the DB. Silent node drops made LazyDataflow unit facts
    // far thinner than Index full for the same function (N5).
    nodes
        .iter()
        .filter(|n| {
            n.function_id
                .is_none_or(|fid| valid_function_ids.contains(&fid))
        })
        .map(|n| {
            let mut n = n.clone();
            if let Some(bid) = n.binding_id {
                if !valid_binding_ids.contains(&bid) {
                    n.binding_id = None;
                }
            }
            n
        })
        .collect()
}

/// Filter dataflow edges to only those referencing existing data nodes.
pub(crate) fn filter_dataflow_edges(
    edges: &[DataFlowEdge],
    valid_node_ids: &HashSet<DataNodeId>,
) -> Vec<DataFlowEdge> {
    edges
        .iter()
        .filter(|e| valid_node_ids.contains(&e.source) && valid_node_ids.contains(&e.target))
        .cloned()
        .collect()
}

/// Filter CFG nodes to only those referencing existing functions.
pub(crate) fn filter_cfg_nodes(
    nodes: &[CfgNode],
    valid_function_ids: &HashSet<SymbolId>,
) -> Vec<CfgNode> {
    nodes
        .iter()
        .filter(|n| valid_function_ids.contains(&n.function_id))
        .cloned()
        .collect()
}

/// Filter CFG edges to only those referencing existing CFG nodes.
pub(crate) fn filter_cfg_edges(
    edges: &[CfgEdge],
    valid_cfg_ids: &HashSet<CfgNodeId>,
) -> Vec<CfgEdge> {
    edges
        .iter()
        .filter(|e| valid_cfg_ids.contains(&e.source) && valid_cfg_ids.contains(&e.target))
        .cloned()
        .collect()
}

// ── Full payload validation result ───────────────────────────────────────

/// Result of validating a complete dataflow payload.
///
/// Public fields allow the caller to inspect what was filtered (for
/// diagnostics/logging) and write only the safe subset.
#[derive(Debug, Clone)]
pub(crate) struct ValidatedDataflowPayload {
    pub bindings: Vec<BindingDef>,
    pub binding_uses: Vec<BindingUse>,
    pub data_nodes: Vec<DataNode>,
    pub dataflow_edges: Vec<DataFlowEdge>,
    pub cfg_nodes: Vec<CfgNode>,
    pub cfg_edges: Vec<CfgEdge>,
}

/// Validate a full dataflow payload against DB-resident entities.
///
/// Used by `replace_dataflow_for_unit` (lazy build path).  Queries the
/// `symbols` and `scopes` tables to verify FK references, then filters
/// each entity type.  Returns only rows whose FK references are satisfied.
pub(crate) fn validate_dataflow_payload_db(
    conn: &Connection,
    data_nodes: &[DataNode],
    dataflow_edges: &[DataFlowEdge],
    bindings: &[BindingDef],
    binding_uses: &[BindingUse],
    cfg_nodes: &[CfgNode],
    cfg_edges: &[CfgEdge],
) -> anyhow::Result<ValidatedDataflowPayload> {
    let valid_function_ids = query_existing_function_ids(conn, bindings, data_nodes)?;
    let valid_scope_ids = query_existing_scope_ids(conn, bindings)?;

    let safe_bindings = filter_bindings(bindings, &valid_function_ids, &valid_scope_ids);

    let valid_binding_ids: HashSet<BindingId> = safe_bindings.iter().map(|b| b.id).collect();

    let safe_binding_uses = filter_binding_uses(binding_uses, &valid_binding_ids);

    let safe_data_nodes = filter_data_nodes(data_nodes, &valid_function_ids, &valid_binding_ids);
    let valid_data_node_ids: HashSet<DataNodeId> = safe_data_nodes.iter().map(|n| n.id).collect();

    let safe_dataflow_edges = filter_dataflow_edges(dataflow_edges, &valid_data_node_ids);

    let safe_cfg_nodes = filter_cfg_nodes(cfg_nodes, &valid_function_ids);
    let valid_cfg_node_ids: HashSet<CfgNodeId> = safe_cfg_nodes.iter().map(|n| n.id).collect();

    let safe_cfg_edges = filter_cfg_edges(cfg_edges, &valid_cfg_node_ids);

    Ok(ValidatedDataflowPayload {
        bindings: safe_bindings,
        binding_uses: safe_binding_uses,
        data_nodes: safe_data_nodes,
        dataflow_edges: safe_dataflow_edges,
        cfg_nodes: safe_cfg_nodes,
        cfg_edges: safe_cfg_edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::enums::DataNodeKind;
    use types::ids::{DataNodeId, FileId, ScopeId};
    use types::structs::TextRange;

    fn range(start: u32, end: u32) -> TextRange {
        TextRange {
            start_byte: start,
            end_byte: end,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        }
    }

    fn node(
        name: &str,
        function_id: Option<SymbolId>,
        binding_id: Option<BindingId>,
        start: u32,
    ) -> DataNode {
        let file_id = FileId::generate("fk_guard_test.ts");
        let id = DataNodeId::generate(
            &file_id,
            function_id.as_ref(),
            "local",
            Some(name),
            None,
            start,
        );
        DataNode {
            id,
            file_id,
            function_id,
            kind: DataNodeKind::Local,
            binding_id,
            callsite_id: None,
            name: Some(name.to_string()),
            access_path: Some(name.to_string()),
            arg_index: None,
            range: range(start, start + 1),
        }
    }

    fn binding_id(tag: &str, start: u32) -> BindingId {
        let file_id = FileId::generate("fk_guard_test.ts");
        let scope = ScopeId::generate(&file_id, None, "function", start);
        BindingId::generate(&file_id, &scope, "local", tag, start)
    }

    #[test]
    fn filter_data_nodes_clears_invalid_binding_id_keeps_node() {
        let fid = SymbolId::generate(
            &FileId::generate("fk_guard_test.ts"),
            "typescript",
            "f",
            "function",
            None,
        );
        let good_binding = binding_id("good", 10);
        let bad_binding = binding_id("bad", 99);

        let mut valid_fns = HashSet::new();
        valid_fns.insert(fid);
        let mut valid_bindings = HashSet::new();
        valid_bindings.insert(good_binding);

        let nodes = vec![
            node("with_good", Some(fid), Some(good_binding), 1),
            node("with_bad", Some(fid), Some(bad_binding), 2),
            node("no_binding", Some(fid), None, 3),
        ];
        let out = filter_data_nodes(&nodes, &valid_fns, &valid_bindings);
        assert_eq!(out.len(), 3, "nodes must not be dropped for bad binding_id");
        assert_eq!(out[0].binding_id, Some(good_binding));
        assert_eq!(
            out[1].binding_id, None,
            "invalid binding_id must SET NULL, not drop row"
        );
        assert_eq!(out[2].binding_id, None);
        assert_eq!(out[1].name.as_deref(), Some("with_bad"));
    }

    #[test]
    fn filter_data_nodes_drops_invalid_function_id() {
        let good_fn = SymbolId::generate(
            &FileId::generate("fk_guard_test.ts"),
            "typescript",
            "good",
            "function",
            None,
        );
        let bad_fn = SymbolId::generate(
            &FileId::generate("fk_guard_test.ts"),
            "typescript",
            "bad",
            "function",
            None,
        );
        let mut valid_fns = HashSet::new();
        valid_fns.insert(good_fn);
        let valid_bindings = HashSet::new();

        let nodes = vec![
            node("ok", Some(good_fn), None, 1),
            node("orphan", Some(bad_fn), None, 2),
            node("no_fn", None, None, 3),
        ];
        let out = filter_data_nodes(&nodes, &valid_fns, &valid_bindings);
        assert_eq!(out.len(), 2);
        let names: Vec<_> = out.iter().map(|n| n.name.as_deref()).collect();
        assert!(names.contains(&Some("ok")));
        assert!(names.contains(&Some("no_fn")));
        assert!(!names.contains(&Some("orphan")));
    }
}
