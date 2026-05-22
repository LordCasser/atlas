//! Private write helpers that take `&Connection` to enable single-transaction
//! bulk writes.
//!
//! These are low-level INSERT/REPLACE functions called from `Store::insert_*`
//! methods (one transaction per call) and `Store::write_facts` (all writes
//! inside one batch transaction).

use atlas_types::*;
use rusqlite::{Connection, params};

pub(crate) fn write_symbols(conn: &Connection, symbols: &[SymbolDef]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO symbols
           (symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
            language,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column,
            name_start_byte, name_end_byte, name_start_line, name_start_column,
            name_end_line, name_end_column,
            signature, visibility, exported, static_, async_,
            container_id, scope_id, package_name, namespace_path_json)
        VALUES (
            ?1,?2,?3,?4,?5,?6,?7,
            ?8,?9,?10,?11,?12,?13,
            ?14,?15,?16,?17,?18,?19,
            ?20,?21,?22,?23,?24,
            ?25,?26,?27,?28
        )"#,
    )?;
    for s in symbols {
        let path_json = serde_json::to_string(&s.symbol_path)?;
        let ns_json = serde_json::to_string(&s.namespace_path)?;
        stmt.execute(params![
            s.id,
            s.file_id,
            s.kind.as_str(),
            s.name,
            s.qualified_name,
            path_json,
            s.language.as_str(),
            s.range.start_byte,
            s.range.end_byte,
            s.range.start_line,
            s.range.start_column,
            s.range.end_line,
            s.range.end_column,
            s.name_range.start_byte,
            s.name_range.end_byte,
            s.name_range.start_line,
            s.name_range.start_column,
            s.name_range.end_line,
            s.name_range.end_column,
            s.signature,
            s.visibility.map(|v| v.as_str()),
            s.exported as i32,
            s.static_ as i32,
            s.async_ as i32,
            s.container,
            s.scope_id,
            s.package_name,
            ns_json,
        ])?;
    }
    Ok(())
}

pub(crate) fn write_scopes(conn: &Connection, scopes: &[ScopeDef]) -> anyhow::Result<()> {
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
            sc.parent_id,
            sc.range.start_byte,
            sc.range.end_byte,
            sc.range.start_line,
            sc.range.start_column,
            sc.range.end_line,
            sc.range.end_column,
        ])?;
    }
    Ok(())
}

pub(crate) fn write_references(conn: &Connection, refs: &[ReferenceUse]) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO "references"
            (reference_id, file_id, source_symbol, scope_id, kind, text, name,
            receiver, arity,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column,
            resolved_symbol_id, resolved_confidence, resolved_strategy, resolved_provenance,
            binding_id)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)"#,
    )?;
    for r in refs {
        stmt.execute(params![
            r.id,
            r.file_id,
            r.source_symbol,
            r.scope_id,
            r.kind.as_str(),
            r.text,
            r.name,
            r.receiver,
            r.arity,
            r.range.start_byte,
            r.range.end_byte,
            r.range.start_line,
            r.range.start_column,
            r.range.end_line,
            r.range.end_column,
            r.resolved.as_ref().map(|rt| &rt.symbol_id),
            r.resolved.as_ref().map(|rt| rt.confidence.as_f32()),
            r.resolved.as_ref().map(|rt| rt.strategy.as_str()),
            r.resolved.as_ref().map(|rt| rt.provenance.as_str()),
            r.binding_id,
        ])?;
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
            name, access_path,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"#,
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
