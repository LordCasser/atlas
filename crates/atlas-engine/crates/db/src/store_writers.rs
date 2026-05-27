//! Private write helpers that take `&Connection` to enable single-transaction
//! bulk writes.
//!
//! These are low-level INSERT/REPLACE functions called from `Store::insert_*`
//! methods (one transaction per call) and `Store::write_facts` (all writes
//! inside one batch transaction).
//!
//! Symbols and references use batched multi-row INSERT for reduced round-trips;
//! JSON serialization for empty arrays (the common case) is cached to avoid
//! per-row `serde_json::to_string()` overhead.

use rusqlite::{Connection, params};
use std::collections::HashSet;
use types::*;

/// Cached empty JSON array string — `Vec::new()` → `"[]"` is constant.
const EMPTY_JSON_ARRAY: &str = "[]";

/// Max rows per multi-row INSERT (limited by SQLite variable binding limit).
const BATCH_CHUNK_SIZE: usize = 50;

pub(crate) fn write_symbols(
    conn: &Connection,
    symbols: &[SymbolDef],
    layer: &str,
) -> anyhow::Result<()> {
    if symbols.is_empty() {
        return Ok(());
    }
    let valid_symbol_ids: HashSet<_> = symbols.iter().map(|s| s.id).collect();

    let base_sql = r#"INSERT OR REPLACE INTO symbols
        (symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
         language,
         range_start_byte, range_end_byte, range_start_line, range_start_column,
         range_end_line, range_end_column,
         name_start_byte, name_end_byte, name_start_line, name_start_column,
         name_end_line, name_end_column,
         signature, visibility, exported, static_, async_,
         container_id, scope_id, package_name, namespace_path_json,
         layer)
     VALUES "#;

    for chunk in symbols.chunks(BATCH_CHUNK_SIZE) {
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|i| {
                let o = i * 29;
                format!(
                    "(?{o1},?{o2},?{o3},?{o4},?{o5},?{o6},?{o7},\
                      ?{o8},?{o9},?{o10},?{o11},?{o12},?{o13},\
                      ?{o14},?{o15},?{o16},?{o17},?{o18},?{o19},\
                      ?{o20},?{o21},?{o22},?{o23},?{o24},\
                      ?{o25},?{o26},?{o27},?{o28},?{o29})",
                    o1 = o + 1,
                    o2 = o + 2,
                    o3 = o + 3,
                    o4 = o + 4,
                    o5 = o + 5,
                    o6 = o + 6,
                    o7 = o + 7,
                    o8 = o + 8,
                    o9 = o + 9,
                    o10 = o + 10,
                    o11 = o + 11,
                    o12 = o + 12,
                    o13 = o + 13,
                    o14 = o + 14,
                    o15 = o + 15,
                    o16 = o + 16,
                    o17 = o + 17,
                    o18 = o + 18,
                    o19 = o + 19,
                    o20 = o + 20,
                    o21 = o + 21,
                    o22 = o + 22,
                    o23 = o + 23,
                    o24 = o + 24,
                    o25 = o + 25,
                    o26 = o + 26,
                    o27 = o + 27,
                    o28 = o + 28,
                    o29 = o + 29,
                )
            })
            .collect();
        let sql = format!("{}{}", base_sql, placeholders.join(","));

        // Collect all params for this chunk
        let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> =
            Vec::with_capacity(chunk.len() * 29);
        for s in chunk {
            let path_json = if s.symbol_path.is_empty() {
                EMPTY_JSON_ARRAY.to_string()
            } else {
                serde_json::to_string(&s.symbol_path)?
            };
            let ns_json = if s.namespace_path.is_empty() {
                EMPTY_JSON_ARRAY.to_string()
            } else {
                serde_json::to_string(&s.namespace_path)?
            };
            let visibility = s.visibility.map(|v| v.as_str().to_string());
            let exported = s.exported as i32;
            let static_ = s.static_ as i32;
            let async_ = s.async_ as i32;

            all_params.push(Box::new(s.id));
            all_params.push(Box::new(s.file_id));
            all_params.push(Box::new(s.kind.as_str().to_string()));
            all_params.push(Box::new(s.name.clone()));
            all_params.push(Box::new(s.qualified_name.clone()));
            all_params.push(Box::new(path_json));
            all_params.push(Box::new(s.language.as_str().to_string()));
            all_params.push(Box::new(s.range.start_byte));
            all_params.push(Box::new(s.range.end_byte));
            all_params.push(Box::new(s.range.start_line));
            all_params.push(Box::new(s.range.start_column));
            all_params.push(Box::new(s.range.end_line));
            all_params.push(Box::new(s.range.end_column));
            all_params.push(Box::new(s.name_range.start_byte));
            all_params.push(Box::new(s.name_range.end_byte));
            all_params.push(Box::new(s.name_range.start_line));
            all_params.push(Box::new(s.name_range.start_column));
            all_params.push(Box::new(s.name_range.end_line));
            all_params.push(Box::new(s.name_range.end_column));
            all_params.push(Box::new(s.signature.clone()));
            all_params.push(Box::new(visibility));
            all_params.push(Box::new(exported));
            all_params.push(Box::new(static_));
            all_params.push(Box::new(async_));
            all_params.push(Box::new(None::<SymbolId>));
            all_params.push(Box::new(s.scope_id));
            all_params.push(Box::new(s.package_name.clone()));
            all_params.push(Box::new(ns_json));
            all_params.push(Box::new(layer.to_string()));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            all_params.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, param_refs.as_slice())?;
    }

    let mut update_container = conn.prepare(
        r#"UPDATE symbols
           SET container_id = ?2
           WHERE symbol_id = ?1"#,
    )?;
    for s in symbols {
        if let Some(container_id) = s.container.filter(|id| valid_symbol_ids.contains(id)) {
            update_container.execute(params![s.id, container_id])?;
        }
    }

    Ok(())
}

pub(crate) fn write_scopes(conn: &Connection, scopes: &[ScopeDef]) -> anyhow::Result<()> {
    let valid_scope_ids: HashSet<_> = scopes.iter().map(|s| s.id).collect();
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO scopes
            (scope_id, file_id, kind, name, scope_path, parent_id,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"#,
    )?;
    for sc in scopes {
        stmt.execute(params![
            sc.id,
            sc.file_id,
            sc.kind.as_str(),
            sc.name,
            sc.scope_path,
            None::<ScopeId>,
            sc.range.start_byte,
            sc.range.end_byte,
            sc.range.start_line,
            sc.range.start_column,
            sc.range.end_line,
            sc.range.end_column,
        ])?;
    }
    let mut update_parent = conn.prepare(
        r#"UPDATE scopes
           SET parent_id = ?2
           WHERE scope_id = ?1"#,
    )?;
    for sc in scopes {
        if let Some(parent_id) = sc.parent_id.filter(|id| valid_scope_ids.contains(id)) {
            update_parent.execute(params![sc.id, parent_id])?;
        }
    }
    Ok(())
}

pub(crate) fn write_references(conn: &Connection, refs: &[ReferenceUse]) -> anyhow::Result<()> {
    if refs.is_empty() {
        return Ok(());
    }

    const REF_PARAMS: usize = 20;
    let base_sql = r#"INSERT OR REPLACE INTO "references"
        (reference_id, file_id, source_symbol, scope_id, kind, text, name,
        receiver, arity,
        range_start_byte, range_end_byte, range_start_line, range_start_column,
        range_end_line, range_end_column,
        resolved_symbol_id, resolved_confidence, resolved_strategy, resolved_provenance,
        binding_id)
     VALUES "#;

    for chunk in refs.chunks(BATCH_CHUNK_SIZE) {
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|i| {
                let o = i * REF_PARAMS;
                format!(
                    "(?{o1},?{o2},?{o3},?{o4},?{o5},?{o6},?{o7},?{o8},?{o9},\
                      ?{o10},?{o11},?{o12},?{o13},?{o14},?{o15},?{o16},?{o17},?{o18},?{o19},?{o20})",
                    o1 = o + 1, o2 = o + 2, o3 = o + 3, o4 = o + 4,
                    o5 = o + 5, o6 = o + 6, o7 = o + 7, o8 = o + 8,
                    o9 = o + 9, o10 = o + 10, o11 = o + 11, o12 = o + 12,
                    o13 = o + 13, o14 = o + 14, o15 = o + 15, o16 = o + 16,
                    o17 = o + 17, o18 = o + 18, o19 = o + 19, o20 = o + 20,
                )
            })
            .collect();
        let sql = format!("{}{}", base_sql, placeholders.join(","));

        let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> =
            Vec::with_capacity(chunk.len() * REF_PARAMS);
        for r in chunk {
            let strategy = r
                .resolved
                .as_ref()
                .map(|rt| rt.strategy.as_str().to_string());
            let provenance = r
                .resolved
                .as_ref()
                .map(|rt| rt.provenance.as_str().to_string());
            all_params.push(Box::new(r.id));
            all_params.push(Box::new(r.file_id));
            all_params.push(Box::new(r.source_symbol));
            all_params.push(Box::new(r.scope_id));
            all_params.push(Box::new(r.kind.as_str().to_string()));
            all_params.push(Box::new(r.text.clone()));
            all_params.push(Box::new(r.name.clone()));
            all_params.push(Box::new(r.receiver.clone()));
            all_params.push(Box::new(r.arity));
            all_params.push(Box::new(r.range.start_byte));
            all_params.push(Box::new(r.range.end_byte));
            all_params.push(Box::new(r.range.start_line));
            all_params.push(Box::new(r.range.start_column));
            all_params.push(Box::new(r.range.end_line));
            all_params.push(Box::new(r.range.end_column));
            all_params.push(Box::new(r.resolved.as_ref().map(|rt| rt.symbol_id)));
            all_params.push(Box::new(
                r.resolved.as_ref().map(|rt| rt.confidence.as_f32()),
            ));
            all_params.push(Box::new(strategy));
            all_params.push(Box::new(provenance));
            all_params.push(Box::new(r.binding_id));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            all_params.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, param_refs.as_slice())?;
    }

    Ok(())
}

pub(crate) fn write_imports(conn: &Connection, imports: &[ImportDef]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO imports
           (import_id, file_id, kind, module, imported_name, local_name, alias,
            is_wildcard, is_relative,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"#,
    )?;
    for imp in imports {
        stmt.execute(params![
            imp.id,
            imp.file_id,
            imp.kind.as_str(),
            imp.module,
            imp.imported_name,
            imp.local_name,
            imp.alias,
            imp.is_wildcard as i32,
            imp.is_relative as i32,
            imp.range.start_byte,
            imp.range.end_byte,
            imp.range.start_line,
            imp.range.start_column,
            imp.range.end_line,
            imp.range.end_column,
        ])?;
    }
    Ok(())
}

pub(crate) fn write_edges(conn: &Connection, edges: &[RawEdge]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO symbol_edges
           (edge_id, source, target, kind, confidence, provenance,
            ref_id, location_0, location_1, location_2, location_3, location_4, location_5,
            metadata, resolved_by)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"#,
    )?;
    for e in edges {
        let (loc_0, loc_1, loc_2, loc_3, loc_4, loc_5) = match &e.location {
            Some(loc) => (
                Some(loc.start_byte),
                Some(loc.end_byte),
                Some(loc.start_line),
                Some(loc.start_column),
                Some(loc.end_line),
                Some(loc.end_column),
            ),
            None => (None, None, None, None, None, None),
        };
        stmt.execute(params![
            e.id,
            e.source,
            e.target,
            e.kind.as_str(),
            e.confidence.as_f32(),
            e.provenance.as_str(),
            e.ref_id,
            loc_0,
            loc_1,
            loc_2,
            loc_3,
            loc_4,
            loc_5,
            e.metadata,
            e.resolved_by.as_ref().map(|s| s.as_str()),
        ])?;
    }
    Ok(())
}

pub(crate) fn write_callsites(conn: &Connection, callsites: &[Callsite]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO callsites
           (callsite_id, reference_id, caller, callee, receiver, args_json,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column,
            callee_start_line, callee_start_column, callee_end_line, callee_end_column,
            callee_start_byte, callee_end_byte)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)"#,
    )?;
    for cs in callsites {
        let args_json = serde_json::to_string(&cs.args)?;
        let (cs_sl, cs_sc, cs_el, cs_ec, cs_sb, cs_eb) = match &cs.callee_range {
            Some(r) => (
                Some(r.start_line as i64),
                Some(r.start_column as i64),
                Some(r.end_line as i64),
                Some(r.end_column as i64),
                Some(r.start_byte as i64),
                Some(r.end_byte as i64),
            ),
            None => (None, None, None, None, None, None),
        };
        stmt.execute(params![
            cs.id,
            cs.reference_id,
            cs.caller,
            cs.callee,
            cs.receiver,
            args_json,
            cs.range.start_byte,
            cs.range.end_byte,
            cs.range.start_line,
            cs.range.start_column,
            cs.range.end_line,
            cs.range.end_column,
            cs_sl,
            cs_sc,
            cs_el,
            cs_ec,
            cs_sb,
            cs_eb,
        ])?;
    }
    Ok(())
}

pub(crate) fn write_bindings(conn: &Connection, bindings: &[BindingDef]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO bindings
           (binding_id, file_id, function_id, scope_id, kind, name, symbol_id,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"#,
    )?;
    for b in bindings {
        stmt.execute(params![
            b.id,
            b.file_id,
            b.function_id,
            b.scope_id,
            b.kind.as_str(),
            b.name,
            b.symbol_id,
            b.range.start_byte,
            b.range.end_byte,
            b.range.start_line,
            b.range.start_column,
            b.range.end_line,
            b.range.end_column,
        ])?;
    }
    Ok(())
}

pub(crate) fn write_binding_uses(conn: &Connection, uses: &[BindingUse]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO binding_uses
           (binding_use_id, file_id, scope_id, binding_id, reference_id, name,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"#,
    )?;
    for u in uses {
        stmt.execute(params![
            u.id,
            u.file_id,
            u.scope_id,
            u.binding_id,
            u.reference_id,
            u.name,
            u.range.start_byte,
            u.range.end_byte,
            u.range.start_line,
            u.range.start_column,
            u.range.end_line,
            u.range.end_column,
        ])?;
    }
    Ok(())
}

pub(crate) fn write_data_nodes(conn: &Connection, nodes: &[DataNode]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO data_nodes
           (data_node_id, file_id, function_id, kind, binding_id, callsite_id,
            name, access_path, arg_index,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"#,
    )?;
    for n in nodes {
        stmt.execute(params![
            n.id,
            n.file_id,
            n.function_id,
            n.kind.as_str(),
            n.binding_id,
            n.callsite_id,
            n.name,
            n.access_path,
            n.arg_index,
            n.range.start_byte,
            n.range.end_byte,
            n.range.start_line,
            n.range.start_column,
            n.range.end_line,
            n.range.end_column,
        ])?;
    }
    Ok(())
}

pub(crate) fn write_dataflow_edges(
    conn: &Connection,
    edges: &[DataFlowEdge],
) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO dataflow_edges
           (dataflow_edge_id, source, target, kind,
            location_0, location_1, location_2,
            location_3, location_4, location_5,
            confidence)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"#,
    )?;
    for e in edges {
        stmt.execute(params![
            e.id,
            e.source,
            e.target,
            e.kind.as_str(),
            e.location.start_byte,
            e.location.end_byte,
            e.location.start_line,
            e.location.start_column,
            e.location.end_line,
            e.location.end_column,
            e.confidence,
        ])?;
    }
    Ok(())
}

pub(crate) fn write_cfg_nodes(conn: &Connection, nodes: &[CfgNode]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO cfg_nodes
           (cfg_node_id, function_id, kind,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"#,
    )?;
    for n in nodes {
        stmt.execute(params![
            n.id,
            n.function_id,
            n.kind.as_str(),
            n.stmt_range.start_byte,
            n.stmt_range.end_byte,
            n.stmt_range.start_line,
            n.stmt_range.start_column,
            n.stmt_range.end_line,
            n.stmt_range.end_column,
        ])?;
    }
    Ok(())
}

pub(crate) fn write_cfg_edges(conn: &Connection, edges: &[CfgEdge]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO cfg_edges
           (cfg_edge_id, source_node, target_node, kind)
        VALUES (?1,?2,?3,?4)"#,
    )?;
    for e in edges {
        stmt.execute(params![e.id, e.source, e.target, e.kind.as_str()])?;
    }
    Ok(())
}

/// Write all components of a single [`FileFacts`] to the given connection.
///
/// Used by both `insert_file_facts_impl` (batch insert) and
/// `replace_file_facts` (atomic delete+insert for lazy structural).
pub(crate) fn write_file_facts(
    conn: &Connection,
    facts: &FileFacts,
) -> anyhow::Result<()> {
    // File info
    conn.execute(
        r#"INSERT OR REPLACE INTO files
           (file_id, path, language, content_hash, status, index_time)
           VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))"#,
        params![
            facts.file.file_id,
            facts.file.path,
            facts.file.language.as_str(),
            facts.file.content_hash,
            facts.file.status.as_str(),
        ],
    )?;

    if !facts.symbols.is_empty() {
        write_symbols(conn, &facts.symbols, &facts.layer)?;
    }
    if !facts.scopes.is_empty() {
        write_scopes(conn, &facts.scopes)?;
    }
    if !facts.references.is_empty() {
        write_references(conn, &facts.references)?;
    }
    if !facts.imports.is_empty() {
        write_imports(conn, &facts.imports)?;
    }
    // Defensive FK guard
    let valid_sources: HashSet<_> = facts.symbols.iter().map(|s| s.id).collect();
    if !facts.raw_edges.is_empty() {
        let valid_edges: Vec<_> = facts
            .raw_edges
            .iter()
            .filter(|edge| valid_sources.contains(&edge.source))
            .cloned()
            .collect();
        if !valid_edges.is_empty() {
            write_edges(conn, &valid_edges)?;
        }
    }
    if !facts.callsites.is_empty() {
        let valid_callsites: Vec<_> = facts
            .callsites
            .iter()
            .filter(|callsite| {
                valid_sources.contains(&callsite.caller)
                    && callsite
                        .callee
                        .map_or(true, |callee| valid_sources.contains(&callee))
            })
            .cloned()
            .collect();
        if !valid_callsites.is_empty() {
            write_callsites(conn, &valid_callsites)?;
        }
    }

    // Binding data — FK guarded
    let valid_bindings: Vec<_> = facts
        .bindings
        .iter()
        .filter(|b| {
            b.function_id
                .map_or(true, |fid| valid_sources.contains(&fid))
                && facts.scopes.iter().any(|s| s.id == b.scope_id)
                && b.symbol_id.map_or(true, |sid| valid_sources.contains(&sid))
        })
        .cloned()
        .collect();
    if !valid_bindings.is_empty() {
        write_bindings(conn, &valid_bindings)?;
    }
    let valid_binding_ids: HashSet<_> = valid_bindings.iter().map(|b| b.id).collect();
    if !facts.binding_uses.is_empty() {
        let valid_uses: Vec<_> = facts
            .binding_uses
            .iter()
            .filter(|bu| {
                bu.binding_id
                    .map_or(false, |bid| valid_binding_ids.contains(&bid))
                    && facts.scopes.iter().any(|s| s.id == bu.scope_id)
            })
            .cloned()
            .collect();
        if !valid_uses.is_empty() {
            write_binding_uses(conn, &valid_uses)?;
        }
    }

    // Dataflow + CFG data — FK guarded
    if !facts.data_nodes.is_empty() {
        let safe_nodes: Vec<_> = facts
            .data_nodes
            .iter()
            .filter(|dn| {
                dn.function_id
                    .map_or(true, |fid| valid_sources.contains(&fid))
                    && dn
                        .binding_id
                        .map_or(true, |bid| valid_binding_ids.contains(&bid))
            })
            .cloned()
            .collect();
        if !safe_nodes.is_empty() {
            write_data_nodes(conn, &safe_nodes)?;
        }
    }
    if !facts.dataflow_edges.is_empty() {
        let valid_node_ids: HashSet<_> = facts
            .data_nodes
            .iter()
            .filter(|dn| {
                dn.function_id
                    .map_or(true, |fid| valid_sources.contains(&fid))
                    && dn
                        .binding_id
                        .map_or(true, |bid| valid_binding_ids.contains(&bid))
            })
            .map(|dn| dn.id)
            .collect();
        let safe_edges: Vec<_> = facts
            .dataflow_edges
            .iter()
            .filter(|e| {
                valid_node_ids.contains(&e.source) && valid_node_ids.contains(&e.target)
            })
            .cloned()
            .collect();
        if !safe_edges.is_empty() {
            write_dataflow_edges(conn, &safe_edges)?;
        }
    }
    if !facts.cfg_nodes.is_empty() {
        let safe_cfg: Vec<_> = facts
            .cfg_nodes
            .iter()
            .filter(|cn| valid_sources.contains(&cn.function_id))
            .cloned()
            .collect();
        if !safe_cfg.is_empty() {
            write_cfg_nodes(conn, &safe_cfg)?;
        }
    }
    if !facts.cfg_edges.is_empty() {
        let valid_cfg_ids: HashSet<_> = facts
            .cfg_nodes
            .iter()
            .filter(|cn| valid_sources.contains(&cn.function_id))
            .map(|cn| cn.id)
            .collect();
        let safe_cfg_edges: Vec<_> = facts
            .cfg_edges
            .iter()
            .filter(|e| {
                valid_cfg_ids.contains(&e.source) && valid_cfg_ids.contains(&e.target)
            })
            .cloned()
            .collect();
        if !safe_cfg_edges.is_empty() {
            write_cfg_edges(conn, &safe_cfg_edges)?;
        }
    }

    // Record per-file per-layer index status.
    let status = if facts.budget_exceeded {
        "partial"
    } else {
        "complete"
    };
    conn.execute(
        "INSERT OR REPLACE INTO file_index_layers
            (file_id, layer, content_hash, status, updated_at)
         VALUES (?1, ?2, ?3, ?4, datetime('now'))",
        params![
            facts.file.file_id,
            facts.layer,
            facts.file.content_hash,
            status
        ],
    )?;

    Ok(())
}
