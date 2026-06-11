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
use std::time::Instant;
use tracing::debug_span;
use types::*;

/// Total wall-clock time of a `write_file_facts` call, in μs.
///
/// Collected only when the caller passes `Some(&mut timing)`; otherwise
/// the function has zero additional overhead.
#[derive(Debug, Default, Clone)]
pub struct DbWriteTiming {
    pub total_us: u64,
}

/// Cached empty JSON array string — `Vec::new()` → `"[]"` is constant.
const EMPTY_JSON_ARRAY: &str = "[]";

/// Max rows per multi-row INSERT (limited by SQLite variable binding limit).
const BATCH_CHUNK_SIZE: usize = 50;

pub(crate) fn write_symbols(
    conn: &Connection,
    symbols: &[SymbolDef],
    layer: &str,
) -> anyhow::Result<()> {
    let _span = debug_span!(target: "atlas_db", "db.write_symbols", count = symbols.len(), layer = %layer).entered();
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
    let _span = debug_span!(target: "atlas_db", "db.write_scopes", count = scopes.len()).entered();
    if scopes.is_empty() {
        return Ok(());
    }

    const PARAMS_PER_ROW: usize = 12;
    const CHUNK_SIZE: usize = 900 / PARAMS_PER_ROW; // 75

    let base_sql = r#"INSERT OR REPLACE INTO scopes
        (scope_id, file_id, kind, name, scope_path, parent_id,
         range_start_byte, range_end_byte, range_start_line, range_start_column,
         range_end_line, range_end_column)
     VALUES "#;

    for chunk in scopes.chunks(CHUNK_SIZE) {
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|i| {
                let o = i * PARAMS_PER_ROW;
                format!(
                    "(?{o1},?{o2},?{o3},?{o4},?{o5},?{o6},?{o7},?{o8},?{o9},?{o10},?{o11},?{o12})",
                    o1 = o + 1, o2 = o + 2, o3 = o + 3, o4 = o + 4,
                    o5 = o + 5, o6 = o + 6, o7 = o + 7, o8 = o + 8,
                    o9 = o + 9, o10 = o + 10, o11 = o + 11, o12 = o + 12,
                )
            })
            .collect();
        let sql = format!("{}{}", base_sql, placeholders.join(","));

        let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> =
            Vec::with_capacity(chunk.len() * PARAMS_PER_ROW);
        for sc in chunk {
            all_params.push(Box::new(sc.id));
            all_params.push(Box::new(sc.file_id));
            all_params.push(Box::new(sc.kind.as_str().to_string()));
            all_params.push(Box::new(sc.name.clone()));
            all_params.push(Box::new(sc.scope_path.clone()));
            all_params.push(Box::new(None::<ScopeId>));
            all_params.push(Box::new(sc.range.start_byte));
            all_params.push(Box::new(sc.range.end_byte));
            all_params.push(Box::new(sc.range.start_line));
            all_params.push(Box::new(sc.range.start_column));
            all_params.push(Box::new(sc.range.end_line));
            all_params.push(Box::new(sc.range.end_column));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            all_params.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, param_refs.as_slice())?;
    }

    let valid_scope_ids: HashSet<_> = scopes.iter().map(|s| s.id).collect();
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
    let _span = debug_span!(target: "atlas_db", "db.write_references", count = refs.len()).entered();
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
    if imports.is_empty() {
        return Ok(());
    }
    let _span = debug_span!(target: "atlas_db", "db.write_imports", count = imports.len()).entered();

    const PARAMS_PER_ROW: usize = 15;
    const CHUNK_SIZE: usize = 900 / PARAMS_PER_ROW; // 60

    let base_sql = r#"INSERT OR REPLACE INTO imports
        (import_id, file_id, kind, module, imported_name, local_name, alias,
         is_wildcard, is_relative,
         range_start_byte, range_end_byte, range_start_line, range_start_column,
         range_end_line, range_end_column)
     VALUES "#;

    for chunk in imports.chunks(CHUNK_SIZE) {
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|i| {
                let o = i * PARAMS_PER_ROW;
                format!(
                    "(?{o1},?{o2},?{o3},?{o4},?{o5},?{o6},?{o7},?{o8},?{o9},\
                      ?{o10},?{o11},?{o12},?{o13},?{o14},?{o15})",
                    o1 = o + 1, o2 = o + 2, o3 = o + 3, o4 = o + 4,
                    o5 = o + 5, o6 = o + 6, o7 = o + 7, o8 = o + 8,
                    o9 = o + 9, o10 = o + 10, o11 = o + 11, o12 = o + 12,
                    o13 = o + 13, o14 = o + 14, o15 = o + 15,
                )
            })
            .collect();
        let sql = format!("{}{}", base_sql, placeholders.join(","));

        let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> =
            Vec::with_capacity(chunk.len() * PARAMS_PER_ROW);
        for imp in chunk {
            all_params.push(Box::new(imp.id));
            all_params.push(Box::new(imp.file_id));
            all_params.push(Box::new(imp.kind.as_str().to_string()));
            all_params.push(Box::new(imp.module.clone()));
            all_params.push(Box::new(imp.imported_name.clone()));
            all_params.push(Box::new(imp.local_name.clone()));
            all_params.push(Box::new(imp.alias.clone()));
            all_params.push(Box::new(imp.is_wildcard as i32));
            all_params.push(Box::new(imp.is_relative as i32));
            all_params.push(Box::new(imp.range.start_byte));
            all_params.push(Box::new(imp.range.end_byte));
            all_params.push(Box::new(imp.range.start_line));
            all_params.push(Box::new(imp.range.start_column));
            all_params.push(Box::new(imp.range.end_line));
            all_params.push(Box::new(imp.range.end_column));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            all_params.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, param_refs.as_slice())?;
    }

    Ok(())
}

pub(crate) fn write_edges(conn: &Connection, edges: &[RawEdge]) -> anyhow::Result<()> {
    if edges.is_empty() {
        return Ok(());
    }
    let _span = debug_span!(target: "atlas_db", "db.write_edges", count = edges.len()).entered();

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
    if callsites.is_empty() {
        return Ok(());
    }
    let _span = debug_span!(target: "atlas_db", "db.write_callsites", count = callsites.len()).entered();

    const PARAMS_PER_ROW: usize = 17;
    const CHUNK_SIZE: usize = 900 / PARAMS_PER_ROW; // 50

    let base_sql = r#"INSERT OR REPLACE INTO callsites
        (callsite_id, reference_id, caller, receiver, args_json,
         range_start_byte, range_end_byte, range_start_line, range_start_column,
         range_end_line, range_end_column,
         callee_start_line, callee_start_column, callee_end_line, callee_end_column,
         callee_start_byte, callee_end_byte)
     VALUES "#;

    for chunk in callsites.chunks(CHUNK_SIZE) {
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|i| {
                let o = i * PARAMS_PER_ROW;
                format!(
                    "(?{o1},?{o2},?{o3},?{o4},?{o5},?{o6},?{o7},?{o8},?{o9},\
                      ?{o10},?{o11},?{o12},?{o13},?{o14},?{o15},?{o16},?{o17})",
                    o1 = o + 1, o2 = o + 2, o3 = o + 3, o4 = o + 4,
                    o5 = o + 5, o6 = o + 6, o7 = o + 7, o8 = o + 8,
                    o9 = o + 9, o10 = o + 10, o11 = o + 11, o12 = o + 12,
                    o13 = o + 13, o14 = o + 14, o15 = o + 15, o16 = o + 16,
                    o17 = o + 17,
                )
            })
            .collect();
        let sql = format!("{}{}", base_sql, placeholders.join(","));

        let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> =
            Vec::with_capacity(chunk.len() * PARAMS_PER_ROW);
        for cs in chunk {
            let args_json = if cs.args.is_empty() {
                EMPTY_JSON_ARRAY.to_string()
            } else {
                serde_json::to_string(&cs.args)?
            };
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
            all_params.push(Box::new(cs.id));
            all_params.push(Box::new(cs.reference_id));
            all_params.push(Box::new(cs.caller));
            all_params.push(Box::new(cs.receiver.clone()));
            all_params.push(Box::new(args_json));
            all_params.push(Box::new(cs.range.start_byte));
            all_params.push(Box::new(cs.range.end_byte));
            all_params.push(Box::new(cs.range.start_line));
            all_params.push(Box::new(cs.range.start_column));
            all_params.push(Box::new(cs.range.end_line));
            all_params.push(Box::new(cs.range.end_column));
            all_params.push(Box::new(cs_sl));
            all_params.push(Box::new(cs_sc));
            all_params.push(Box::new(cs_el));
            all_params.push(Box::new(cs_ec));
            all_params.push(Box::new(cs_sb));
            all_params.push(Box::new(cs_eb));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            all_params.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, param_refs.as_slice())?;
    }

    Ok(())
}

pub(crate) fn write_bindings(conn: &Connection, bindings: &[BindingDef]) -> anyhow::Result<()> {
    if bindings.is_empty() {
        return Ok(());
    }
    let _span = debug_span!(target: "atlas_db", "db.write_bindings", count = bindings.len()).entered();

    const PARAMS_PER_ROW: usize = 13;
    const CHUNK_SIZE: usize = 900 / PARAMS_PER_ROW; // 69

    let base_sql = r#"INSERT OR REPLACE INTO bindings
        (binding_id, file_id, function_id, scope_id, kind, name, symbol_id,
         range_start_byte, range_end_byte, range_start_line, range_start_column,
         range_end_line, range_end_column)
     VALUES "#;

    for chunk in bindings.chunks(CHUNK_SIZE) {
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|i| {
                let o = i * PARAMS_PER_ROW;
                format!(
                    "(?{o1},?{o2},?{o3},?{o4},?{o5},?{o6},?{o7},?{o8},?{o9},\
                      ?{o10},?{o11},?{o12},?{o13})",
                    o1 = o + 1, o2 = o + 2, o3 = o + 3, o4 = o + 4,
                    o5 = o + 5, o6 = o + 6, o7 = o + 7, o8 = o + 8,
                    o9 = o + 9, o10 = o + 10, o11 = o + 11, o12 = o + 12,
                    o13 = o + 13,
                )
            })
            .collect();
        let sql = format!("{}{}", base_sql, placeholders.join(","));

        let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> =
            Vec::with_capacity(chunk.len() * PARAMS_PER_ROW);
        for b in chunk {
            all_params.push(Box::new(b.id));
            all_params.push(Box::new(b.file_id));
            all_params.push(Box::new(b.function_id));
            all_params.push(Box::new(b.scope_id));
            all_params.push(Box::new(b.kind.as_str().to_string()));
            all_params.push(Box::new(b.name.clone()));
            all_params.push(Box::new(b.symbol_id));
            all_params.push(Box::new(b.range.start_byte));
            all_params.push(Box::new(b.range.end_byte));
            all_params.push(Box::new(b.range.start_line));
            all_params.push(Box::new(b.range.start_column));
            all_params.push(Box::new(b.range.end_line));
            all_params.push(Box::new(b.range.end_column));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            all_params.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, param_refs.as_slice())?;
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
    let _span = debug_span!(target: "atlas_db", "db.write_data_nodes", count = nodes.len()).entered();
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
    if edges.is_empty() {
        return Ok(());
    }
    let _span = debug_span!(target: "atlas_db", "db.write_dataflow_edges", count = edges.len()).entered();

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
    let _span = debug_span!(target: "atlas_db", "db.write_cfg_nodes", count = nodes.len()).entered();
    let mut stmt = conn.prepare(
        r#"INSERT OR REPLACE INTO cfg_nodes
           (cfg_node_id, function_id, kind,
            range_start_byte, range_end_byte, range_start_line, range_start_column,
            range_end_line, range_end_column,
            semantic_effects_json, call_context)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"#,
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
            if n.semantic_effects.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&n.semantic_effects).map_err(|e| {
                    tracing::error!(
                        ?e,
                        fn_id = %n.function_id,
                        "CFG node semantic_effects serialization failed, storing NULL"
                    );
                    e
                })?)
            },
            n.call_context.as_str(),
        ])?;
    }
    Ok(())
}

pub(crate) fn write_cfg_edges(conn: &Connection, edges: &[CfgEdge]) -> anyhow::Result<()> {
    let _span = debug_span!(target: "atlas_db", "db.write_cfg_edges", count = edges.len()).entered();
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
    timing: Option<&mut DbWriteTiming>,
) -> anyhow::Result<()> {
    let _timer = timing.as_ref().map(|_| Instant::now());
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
    let _fk_span = debug_span!(target: "atlas_db", "db.fk_guard", file = %facts.file.path).entered();
    let valid_sources: HashSet<_> = facts.symbols.iter().map(|s| s.id).collect();
    if !facts.raw_edges.is_empty() {
        if facts.raw_edges.iter().all(|e| valid_sources.contains(&e.source)) {
            write_edges(conn, &facts.raw_edges)?;
        } else {
            let valid_edges: Vec<_> = facts
                .raw_edges
                .iter()
                .filter(|e| valid_sources.contains(&e.source))
                .cloned()
                .collect();
            if !valid_edges.is_empty() {
                write_edges(conn, &valid_edges)?;
            }
        }
    }
    if !facts.callsites.is_empty() {
        if facts.callsites.iter().all(|cs| valid_sources.contains(&cs.caller)) {
            write_callsites(conn, &facts.callsites)?;
        } else {
            let valid_callsites: Vec<_> = facts
                .callsites
                .iter()
                .filter(|cs| valid_sources.contains(&cs.caller))
                .cloned()
                .collect();
            if !valid_callsites.is_empty() {
                write_callsites(conn, &valid_callsites)?;
            }
        }
    }

    // Binding data — FK guarded
    let valid_scope_ids: HashSet<_> = facts.scopes.iter().map(|s| s.id).collect();
    let (_valid_bindings, valid_binding_ids): (Vec<_>, HashSet<BindingId>) =
        if facts.bindings.iter().all(|b| {
            b.function_id.is_none_or(|fid| valid_sources.contains(&fid))
                && valid_scope_ids.contains(&b.scope_id)
                && b.symbol_id.is_none_or(|sid| valid_sources.contains(&sid))
        }) {
            if !facts.bindings.is_empty() {
                write_bindings(conn, &facts.bindings)?;
            }
            let ids: HashSet<BindingId> = facts.bindings.iter().map(|b| b.id).collect();
            (Vec::new(), ids)
        } else {
            let valid: Vec<_> = facts
                .bindings
                .iter()
                .filter(|b| {
                    b.function_id.is_none_or(|fid| valid_sources.contains(&fid))
                        && valid_scope_ids.contains(&b.scope_id)
                        && b.symbol_id.is_none_or(|sid| valid_sources.contains(&sid))
                })
                .cloned()
                .collect();
            if !valid.is_empty() {
                write_bindings(conn, &valid)?;
            }
            let ids: HashSet<BindingId> = valid.iter().map(|b| b.id).collect();
            (valid, ids)
        };
    if !facts.binding_uses.is_empty() {
        if facts.binding_uses.iter().all(|bu| {
            bu.binding_id.is_some_and(|bid| valid_binding_ids.contains(&bid))
                && valid_scope_ids.contains(&bu.scope_id)
        }) {
            write_binding_uses(conn, &facts.binding_uses)?;
        } else {
            let valid_uses: Vec<_> = facts
                .binding_uses
                .iter()
                .filter(|bu| {
                    bu.binding_id.is_some_and(|bid| valid_binding_ids.contains(&bid))
                        && valid_scope_ids.contains(&bu.scope_id)
                })
                .cloned()
                .collect();
            if !valid_uses.is_empty() {
                write_binding_uses(conn, &valid_uses)?;
            }
        }
    }

    // Dataflow + CFG data — FK guarded
    if !facts.data_nodes.is_empty() {
        if facts.data_nodes.iter().all(|dn| {
            dn.function_id.is_none_or(|fid| valid_sources.contains(&fid))
                && dn.binding_id.is_none_or(|bid| valid_binding_ids.contains(&bid))
        }) {
            write_data_nodes(conn, &facts.data_nodes)?;
        } else {
            let safe_nodes: Vec<_> = facts
                .data_nodes
                .iter()
                .filter(|dn| {
                    dn.function_id.is_none_or(|fid| valid_sources.contains(&fid))
                        && dn.binding_id.is_none_or(|bid| valid_binding_ids.contains(&bid))
                })
                .cloned()
                .collect();
            if !safe_nodes.is_empty() {
                write_data_nodes(conn, &safe_nodes)?;
            }
        }
    }
    if !facts.dataflow_edges.is_empty() {
        let valid_node_ids: HashSet<_> = if facts.data_nodes.is_empty() {
            HashSet::new()
        } else {
            facts.data_nodes.iter()
                .filter(|dn| {
                    dn.function_id.is_none_or(|fid| valid_sources.contains(&fid))
                        && dn.binding_id.is_none_or(|bid| valid_binding_ids.contains(&bid))
                })
                .map(|dn| dn.id)
                .collect()
        };
        if facts.dataflow_edges.iter().all(|e| {
            valid_node_ids.contains(&e.source) && valid_node_ids.contains(&e.target)
        }) {
            write_dataflow_edges(conn, &facts.dataflow_edges)?;
        } else {
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
    }
    if !facts.cfg_nodes.is_empty() {
        if facts.cfg_nodes.iter().all(|cn| valid_sources.contains(&cn.function_id)) {
            write_cfg_nodes(conn, &facts.cfg_nodes)?;
        } else {
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
    }
    if !facts.cfg_edges.is_empty() {
        let valid_cfg_ids: HashSet<_> = facts
            .cfg_nodes
            .iter()
            .filter(|cn| valid_sources.contains(&cn.function_id))
            .map(|cn| cn.id)
            .collect();
        if facts.cfg_edges.iter().all(|e| {
            valid_cfg_ids.contains(&e.source) && valid_cfg_ids.contains(&e.target)
        }) {
            write_cfg_edges(conn, &facts.cfg_edges)?;
        } else {
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
    }
    drop(_fk_span);

    // Record per-file per-layer index status.
    let status = if facts.budget_exceeded {
        "partial"
    } else {
        "complete"
    };
    conn.execute(
        "DELETE FROM extraction_state
         WHERE file_id = ?1 AND unit_id IS NULL AND layer = ?2",
        params![facts.file.file_id, facts.layer],
    )?;
    let mut capability_mask = CapabilityMask::from_layers(&[&facts.layer]);
    if !facts.cfg_nodes.is_empty() {
        capability_mask.set(CapabilityMask::CFG);
    }
    conn.execute(
        "INSERT INTO extraction_state
            (file_id, unit_id, layer, content_hash, status, capability_mask, updated_at)
         VALUES (?1, NULL, ?2, ?3, ?4, ?5, datetime('now'))",
        params![
            facts.file.file_id,
            facts.layer,
            facts.file.content_hash,
            status,
            capability_mask.bits() as i64,
        ],
    )?;

    if let Some(t) = timing {
        if let Some(start) = _timer {
            t.total_us += start.elapsed().as_micros() as u64;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn in_memory_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbols (
                symbol_id TEXT PRIMARY KEY, file_id TEXT NOT NULL, kind TEXT NOT NULL,
                name TEXT NOT NULL, qualified_name TEXT NOT NULL,
                symbol_path_json TEXT NOT NULL DEFAULT '[]',
                language TEXT NOT NULL,
                range_start_byte INTEGER NOT NULL, range_end_byte INTEGER NOT NULL,
                range_start_line INTEGER NOT NULL, range_start_column INTEGER NOT NULL,
                range_end_line INTEGER NOT NULL, range_end_column INTEGER NOT NULL,
                name_start_byte INTEGER NOT NULL, name_end_byte INTEGER NOT NULL,
                name_start_line INTEGER NOT NULL, name_start_column INTEGER NOT NULL,
                name_end_line INTEGER NOT NULL, name_end_column INTEGER NOT NULL,
                signature TEXT, visibility TEXT, exported INTEGER NOT NULL DEFAULT 0,
                static_ INTEGER NOT NULL DEFAULT 0, async_ INTEGER NOT NULL DEFAULT 0,
                container_id TEXT, scope_id TEXT, package_name TEXT,
                namespace_path_json TEXT NOT NULL DEFAULT '[]',
                layer TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS dataflow_edges (
                dataflow_edge_id TEXT PRIMARY KEY, source TEXT NOT NULL,
                target TEXT NOT NULL, kind TEXT NOT NULL,
                location_0 INTEGER, location_1 INTEGER, location_2 INTEGER,
                location_3 INTEGER, location_4 INTEGER, location_5 INTEGER,
                confidence REAL
            );"
        ).unwrap();
        conn
    }

    #[test]
    fn tracing_spans_do_not_panic_write_symbols() {
        let subscriber = tracing_subscriber::fmt()
            .with_test_writer()
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let conn = in_memory_conn();
            let result = write_symbols(&conn, &[], "structural");
            assert!(result.is_ok());
        });
    }

    #[test]
    fn tracing_spans_do_not_panic_write_dataflow_edges() {
        let subscriber = tracing_subscriber::fmt()
            .with_test_writer()
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let conn = in_memory_conn();
            let result = write_dataflow_edges(&conn, &[]);
            assert!(result.is_ok());
        });
    }

    // ── Batch INSERT tests for write_dataflow_edges ──────────────────────

    fn make_dataflow_edge(i: u32) -> DataFlowEdge {
        let file_id = FileId::generate(&format!("src/test_{i}.c"));
        let func_id =
            SymbolId::generate(&file_id, "c", "main", "function", None);
        let src = DataNodeId::generate(
            &file_id,
            Some(&func_id),
            "parameter",
            Some(&format!("src_{i}")),
            None,
            i * 10,
        );
        let tgt = DataNodeId::generate(
            &file_id,
            Some(&func_id),
            "local",
            Some(&format!("tgt_{i}")),
            None,
            i * 10 + 5,
        );
        let edge_id = DataFlowEdgeId::generate(&src, &tgt, "assign");
        DataFlowEdge {
            id: edge_id,
            source: src,
            target: tgt,
            kind: DataFlowKind::Assign,
            location: TextRange {
                start_byte: i * 10,
                end_byte: i * 10 + 5,
                start_line: i + 1,
                start_column: 1,
                end_line: i + 1,
                end_column: 6,
            },
            confidence: 0.9,
        }
    }

    #[test]
    fn write_dataflow_edges_empty_input() {
        let conn = in_memory_conn();
        let result = write_dataflow_edges(&conn, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn write_dataflow_edges_batch_insert_various_counts() {
        let conn = in_memory_conn();
        //                        0, 1, chunk-1, chunk, chunk+1, 2*chunk
        for count in [0usize, 1, 80, 81, 82, 162] {
            conn.execute("DELETE FROM dataflow_edges", []).unwrap();

            let edges: Vec<DataFlowEdge> = (0..count as u32).map(make_dataflow_edge).collect();
            write_dataflow_edges(&conn, &edges).unwrap();

            let stored: i64 = conn
                .query_row("SELECT COUNT(*) FROM dataflow_edges", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                stored,
                count as i64,
                "count mismatch for {count} edges"
            );
        }
    }

    #[test]
    fn write_dataflow_edges_stores_correct_values() {
        let conn = in_memory_conn();
        let edges: Vec<DataFlowEdge> = (0..3).map(make_dataflow_edge).collect();
        write_dataflow_edges(&conn, &edges).unwrap();

        // Read back by edge_id and verify each edge
        let mut stmt = conn
            .prepare(
                "SELECT source, target, kind, confidence
                 FROM dataflow_edges WHERE dataflow_edge_id = ?1",
            )
            .unwrap();
        for edge in &edges {
            let row = stmt
                .query_row(params![edge.id], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                })
                .unwrap();
            assert_eq!(&row.0, edge.source.as_bytes());
            assert_eq!(&row.1, edge.target.as_bytes());
            assert_eq!(row.2, edge.kind.as_str());
            assert!((row.3 - edge.confidence).abs() < 1e-10);
        }
    }

    // ── Batch INSERT tests for write_edges ──────────────────────────────

    fn in_memory_conn_with_edges() -> Connection {
        let conn = in_memory_conn();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbol_edges (
                edge_id BLOB PRIMARY KEY, source BLOB NOT NULL, target BLOB NOT NULL,
                kind TEXT NOT NULL, confidence REAL NOT NULL DEFAULT 0.5,
                provenance TEXT NOT NULL DEFAULT 'tree_sitter',
                ref_id BLOB,
                location_0 INTEGER, location_1 INTEGER, location_2 INTEGER,
                location_3 INTEGER, location_4 INTEGER, location_5 INTEGER,
                metadata TEXT, resolved_by TEXT
            );"
        ).unwrap();
        conn
    }

    fn make_raw_edge(i: u32, with_location: bool) -> RawEdge {
        let file_id = FileId::generate(&format!("src/test_e{i}.ts"));
        let src =
            SymbolId::generate(&file_id, "typescript", &format!("Src{i}"), "function", None);
        let tgt =
            SymbolId::generate(&file_id, "typescript", &format!("Tgt{i}"), "function", None);
        let edge_id = EdgeId::generate(&src, &tgt, "calls", None, "tree_sitter");
        RawEdge {
            id: edge_id,
            source: src,
            target: tgt,
            kind: EdgeKind::Calls,
            confidence: Confidence::new(0.85),
            provenance: Provenance::TreeSitter,
            ref_id: None,
            location: if with_location {
                Some(TextRange {
                    start_byte: i * 20,
                    end_byte: i * 20 + 10,
                    start_line: i + 1,
                    start_column: 1,
                    end_line: i + 1,
                    end_column: 11,
                })
            } else {
                None
            },
            metadata: Some(format!("meta_{i}")),
            resolved_by: Some(ResolutionStrategy::ExactMatch),
        }
    }

    #[test]
    fn write_edges_empty_input() {
        let conn = in_memory_conn_with_edges();
        let result = write_edges(&conn, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn write_edges_batch_insert_various_counts() {
        let conn = in_memory_conn_with_edges();
        //                        0, 1, chunk-1, chunk, chunk+1, 2*chunk
        for count in [0usize, 1, 59, 60, 61, 120] {
            conn.execute("DELETE FROM symbol_edges", []).unwrap();

            let edges: Vec<RawEdge> =
                (0..count as u32).map(|i| make_raw_edge(i, true)).collect();
            write_edges(&conn, &edges).unwrap();

            let stored: i64 = conn
                .query_row("SELECT COUNT(*) FROM symbol_edges", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                stored,
                count as i64,
                "count mismatch for {count} edges"
            );
        }
    }

    #[test]
    fn write_edges_handles_none_location() {
        let conn = in_memory_conn_with_edges();

        let edges: Vec<RawEdge> = vec![
            make_raw_edge(0, true),  // has location
            make_raw_edge(1, false), // no location
            make_raw_edge(2, true),  // has location
        ];
        write_edges(&conn, &edges).unwrap();

        // Verify location columns for the None edge are NULL
        let mut stmt = conn
            .prepare(
                "SELECT location_0, location_1, location_2, location_3, location_4, location_5
                 FROM symbol_edges WHERE edge_id = ?1",
            )
            .unwrap();

        // Edge 1 has no location — all NULL
        let row = stmt
            .query_row(params![edges[1].id], |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            })
            .unwrap();
        assert!(row.0.is_none(), "location_0 should be NULL");
        assert!(row.1.is_none(), "location_1 should be NULL");

        // Edge 0 has location — non-NULL
        let row = stmt
            .query_row(params![edges[0].id], |row| {
                Ok(row.get::<_, Option<i64>>(0)?)
            })
            .unwrap();
        assert!(row.is_some(), "location_0 should be non-NULL for edge with location");
    }

    #[test]
    fn write_edges_stores_correct_values() {
        let conn = in_memory_conn_with_edges();

        let edge = make_raw_edge(0, true);
        write_edges(&conn, &[edge.clone()]).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT edge_id, kind, confidence, provenance, metadata, resolved_by
                 FROM symbol_edges WHERE edge_id = ?1",
            )
            .unwrap();
        let row = stmt
            .query_row(params![edge.id], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .unwrap();

        assert_eq!(&row.0, edge.id.as_bytes());
        assert_eq!(row.1, edge.kind.as_str());
        assert_eq!(row.2, edge.confidence.as_f32() as f64);
        assert_eq!(row.3, edge.provenance.as_str());
        assert_eq!(row.4.as_deref(), Some("meta_0"));
        assert_eq!(row.5.as_deref(), Some(ResolutionStrategy::ExactMatch.as_str()));
    }

    #[test]
    fn tracing_spans_do_not_panic_write_cfg_nodes() {
        let subscriber = tracing_subscriber::fmt()
            .with_test_writer()
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS cfg_nodes (
                    cfg_node_id TEXT PRIMARY KEY, function_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    range_start_byte INTEGER NOT NULL, range_end_byte INTEGER NOT NULL,
                    range_start_line INTEGER NOT NULL, range_start_column INTEGER NOT NULL,
                    range_end_line INTEGER NOT NULL, range_end_column INTEGER NOT NULL,
                    semantic_effects_json TEXT, call_context TEXT
                );"
            ).unwrap();
            let result = write_cfg_nodes(&conn, &[]);
            assert!(result.is_ok());
        });
    }

    // ── Batch INSERT tests for write_scopes ──────────────────────────────

    fn scopes_table_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scopes (
                scope_id TEXT PRIMARY KEY, file_id INTEGER, kind TEXT, name TEXT,
                scope_path TEXT, parent_id TEXT,
                range_start_byte INTEGER, range_end_byte INTEGER,
                range_start_line INTEGER, range_start_column INTEGER,
                range_end_line INTEGER, range_end_column INTEGER
            );"
        ).unwrap();
        conn
    }

    fn make_scope_def(i: u32) -> ScopeDef {
        let file_id = FileId::generate(&format!("src/test_s{i}.ts"));
        let scope_id = ScopeId::generate(&file_id, None, "block", i * 100);
        ScopeDef {
            id: scope_id,
            file_id,
            kind: ScopeKind::Block,
            name: format!("scope_{i}"),
            scope_path: format!("module:scope_{i}"),
            range: TextRange {
                start_byte: i * 100,
                end_byte: i * 100 + 50,
                start_line: i + 1,
                start_column: 0,
                end_line: i + 1,
                end_column: 50,
            },
            parent_id: None,
        }
    }

    #[test]
    fn test_write_scopes_multi_row_insert() {
        // Empty input
        {
            let conn = scopes_table_conn();
            write_scopes(&conn, &[]).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM scopes", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "empty input should produce 0 rows");
        }

        const CHUNK_SIZE: usize = 75;

        // Various counts: 0, 1, chunk-1, chunk, chunk+1, 2*chunk+1
        for count in [0usize, 1, CHUNK_SIZE - 1, CHUNK_SIZE, CHUNK_SIZE + 1, CHUNK_SIZE * 2 + 1] {
            let conn = scopes_table_conn();

            let scopes: Vec<ScopeDef> = (0..count as u32).map(make_scope_def).collect();
            write_scopes(&conn, &scopes).unwrap();

            let stored: i64 = conn
                .query_row("SELECT COUNT(*) FROM scopes", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                stored,
                count as i64,
                "count mismatch for {count} scopes"
            );
        }
    }

    #[test]
    fn test_write_scopes_stores_correct_values() {
        let conn = scopes_table_conn();
        let scopes: Vec<ScopeDef> = (0..3).map(make_scope_def).collect();
        write_scopes(&conn, &scopes).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT scope_id, kind, name, scope_path, range_start_byte
                 FROM scopes WHERE scope_id = ?1",
            )
            .unwrap();
        for sc in &scopes {
            let row = stmt
                .query_row(params![sc.id], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })
                .unwrap();
            assert_eq!(&row.0, sc.id.as_bytes());
            assert_eq!(row.1, sc.kind.as_str());
            assert_eq!(row.2, sc.name);
            assert_eq!(row.3, sc.scope_path);
            assert_eq!(row.4, sc.range.start_byte as i64);
        }
    }

    // ── Batch INSERT tests for write_imports ─────────────────────────────

    fn imports_table_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS imports (
                import_id TEXT PRIMARY KEY, file_id INTEGER, kind TEXT, module TEXT,
                imported_name TEXT, local_name TEXT, alias TEXT,
                is_wildcard INTEGER, is_relative INTEGER,
                range_start_byte INTEGER, range_end_byte INTEGER,
                range_start_line INTEGER, range_start_column INTEGER,
                range_end_line INTEGER, range_end_column INTEGER
            );"
        ).unwrap();
        conn
    }

    fn make_import_def(i: u32) -> ImportDef {
        let file_id = FileId::generate(&format!("src/test_i{i}.ts"));
        let import_id =
            ImportId::generate(&file_id, "import", &format!("mod_{i}"), Some(&format!("name_{i}")), i * 20);
        ImportDef {
            id: import_id,
            file_id,
            kind: ImportKind::Import,
            module: format!("mod_{i}"),
            imported_name: format!("name_{i}"),
            local_name: None,
            alias: None,
            is_wildcard: false,
            is_relative: false,
            range: TextRange {
                start_byte: i * 20,
                end_byte: i * 20 + 15,
                start_line: i + 1,
                start_column: 0,
                end_line: i + 1,
                end_column: 15,
            },
        }
    }

    #[test]
    fn test_write_imports_multi_row_insert() {
        // Empty input
        {
            let conn = imports_table_conn();
            write_imports(&conn, &[]).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM imports", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "empty input should produce 0 rows");
        }

        const CHUNK_SIZE: usize = 60;

        for count in [0usize, 1, CHUNK_SIZE - 1, CHUNK_SIZE, CHUNK_SIZE + 1, CHUNK_SIZE * 2 + 1] {
            let conn = imports_table_conn();

            let imports: Vec<ImportDef> = (0..count as u32).map(make_import_def).collect();
            write_imports(&conn, &imports).unwrap();

            let stored: i64 = conn
                .query_row("SELECT COUNT(*) FROM imports", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                stored,
                count as i64,
                "count mismatch for {count} imports"
            );
        }
    }

    #[test]
    fn test_write_imports_stores_correct_values() {
        let conn = imports_table_conn();
        let imports: Vec<ImportDef> = (0..3).map(make_import_def).collect();
        write_imports(&conn, &imports).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT import_id, kind, module, imported_name, is_wildcard, is_relative
                 FROM imports WHERE import_id = ?1",
            )
            .unwrap();
        for imp in &imports {
            let row = stmt
                .query_row(params![imp.id], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                })
                .unwrap();
            assert_eq!(&row.0, imp.id.as_bytes());
            assert_eq!(row.1, imp.kind.as_str());
            assert_eq!(row.2, imp.module);
            assert_eq!(row.3, imp.imported_name);
            assert_eq!(row.4, imp.is_wildcard as i64);
            assert_eq!(row.5, imp.is_relative as i64);
        }
    }

    // ── Batch INSERT tests for write_callsites ───────────────────────────

    fn callsites_table_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS callsites (
                callsite_id TEXT PRIMARY KEY, reference_id INTEGER,
                caller TEXT, receiver TEXT, args_json TEXT,
                range_start_byte INTEGER, range_end_byte INTEGER,
                range_start_line INTEGER, range_start_column INTEGER,
                range_end_line INTEGER, range_end_column INTEGER,
                callee_start_line INTEGER, callee_start_column INTEGER,
                callee_end_line INTEGER, callee_end_column INTEGER,
                callee_start_byte INTEGER, callee_end_byte INTEGER
            );"
        ).unwrap();
        conn
    }

    fn make_callsite(i: u32) -> Callsite {
        let file_id = FileId::generate(&format!("src/test_cs{i}.ts"));
        let caller = SymbolId::generate(
            &file_id,
            "typescript",
            &format!("caller_{i}"),
            "function",
            None,
        );
        let ref_id = ReferenceId::generate(
            &file_id,
            Some(&caller),
            i * 10,
            i * 10 + 8,
            &format!("callee_{i}"),
            ReferenceKind::Call,
        );
        let callsite_id = CallsiteId::generate(&ref_id, Some(&caller), i * 10);
        Callsite {
            id: callsite_id,
            reference_id: Some(ref_id),
            caller,
            receiver: None,
            args: vec![],
            range: TextRange {
                start_byte: i * 10,
                end_byte: i * 10 + 8,
                start_line: i + 1,
                start_column: 0,
                end_line: i + 1,
                end_column: 8,
            },
            callee_range: Some(TextRange {
                start_byte: i * 10,
                end_byte: i * 10 + 8,
                start_line: i + 1,
                start_column: 0,
                end_line: i + 1,
                end_column: 8,
            }),
        }
    }

    #[test]
    fn test_write_callsites_multi_row_insert() {
        // Empty input
        {
            let conn = callsites_table_conn();
            write_callsites(&conn, &[]).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM callsites", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "empty input should produce 0 rows");
        }

        const CHUNK_SIZE: usize = 50;

        for count in [0usize, 1, CHUNK_SIZE - 1, CHUNK_SIZE, CHUNK_SIZE + 1, CHUNK_SIZE * 2 + 1] {
            let conn = callsites_table_conn();

            let callsites: Vec<Callsite> = (0..count as u32).map(make_callsite).collect();
            write_callsites(&conn, &callsites).unwrap();

            let stored: i64 = conn
                .query_row("SELECT COUNT(*) FROM callsites", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                stored,
                count as i64,
                "count mismatch for {count} callsites"
            );
        }
    }

    #[test]
    fn test_write_callsites_stores_correct_values() {
        let conn = callsites_table_conn();
        let callsites: Vec<Callsite> = (0..3).map(make_callsite).collect();
        write_callsites(&conn, &callsites).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT callsite_id, caller, args_json, range_start_byte
                 FROM callsites WHERE callsite_id = ?1",
            )
            .unwrap();
        for cs in &callsites {
            let row = stmt
                .query_row(params![cs.id], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .unwrap();
            assert_eq!(&row.0, cs.id.as_bytes());
            assert_eq!(&row.1, cs.caller.as_bytes());
            assert_eq!(row.2, "[]");
            assert_eq!(row.3, cs.range.start_byte as i64);
        }
    }

    // ── Batch INSERT tests for write_bindings ────────────────────────────

    fn bindings_table_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS bindings (
                binding_id TEXT PRIMARY KEY, file_id INTEGER,
                function_id TEXT, scope_id TEXT,
                kind TEXT, name TEXT, symbol_id TEXT,
                range_start_byte INTEGER, range_end_byte INTEGER,
                range_start_line INTEGER, range_start_column INTEGER,
                range_end_line INTEGER, range_end_column INTEGER
            );"
        ).unwrap();
        conn
    }

    fn make_binding_def(i: u32) -> BindingDef {
        let file_id = FileId::generate(&format!("src/test_b{i}.ts"));
        let function_id = SymbolId::generate(
            &file_id,
            "typescript",
            &format!("func_{i}"),
            "function",
            None,
        );
        let scope_id = ScopeId::generate(&file_id, None, "function", i * 100);
        let binding_id = BindingId::generate(&file_id, &scope_id, "local", &format!("var_{i}"), i * 10);
        BindingDef {
            id: binding_id,
            file_id,
            function_id: Some(function_id),
            scope_id,
            kind: BindingKind::Local,
            name: format!("var_{i}"),
            symbol_id: None,
            range: TextRange {
                start_byte: i * 10,
                end_byte: i * 10 + 5,
                start_line: i + 1,
                start_column: 0,
                end_line: i + 1,
                end_column: 5,
            },
        }
    }

    #[test]
    fn test_write_bindings_multi_row_insert() {
        // Empty input
        {
            let conn = bindings_table_conn();
            write_bindings(&conn, &[]).unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM bindings", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "empty input should produce 0 rows");
        }

        const CHUNK_SIZE: usize = 69;

        for count in [0usize, 1, CHUNK_SIZE - 1, CHUNK_SIZE, CHUNK_SIZE + 1, CHUNK_SIZE * 2 + 1] {
            let conn = bindings_table_conn();

            let bindings: Vec<BindingDef> = (0..count as u32).map(make_binding_def).collect();
            write_bindings(&conn, &bindings).unwrap();

            let stored: i64 = conn
                .query_row("SELECT COUNT(*) FROM bindings", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                stored,
                count as i64,
                "count mismatch for {count} bindings"
            );
        }
    }

    #[test]
    fn test_write_bindings_stores_correct_values() {
        let conn = bindings_table_conn();
        let bindings: Vec<BindingDef> = (0..3).map(make_binding_def).collect();
        write_bindings(&conn, &bindings).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT binding_id, function_id, scope_id, kind, name, symbol_id, range_start_byte
                 FROM bindings WHERE binding_id = ?1",
            )
            .unwrap();
        for b in &bindings {
            let row = stmt
                .query_row(params![b.id], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<Vec<u8>>>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                })
                .unwrap();
            assert_eq!(&row.0, b.id.as_bytes());
            assert_eq!(&row.1, b.function_id.as_ref().unwrap().as_bytes());
            assert_eq!(&row.2, b.scope_id.as_bytes());
            assert_eq!(row.3, b.kind.as_str());
            assert_eq!(row.4, b.name);
            assert!(row.5.is_none(), "symbol_id should be NULL");
            assert_eq!(row.6, b.range.start_byte as i64);
        }
    }

    // ── write_file_facts FK filter tests ─────────────────────────────────

    fn facts_test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::SCHEMA_DDL).unwrap();
        conn
    }

    fn facts_help_range(idx: u32, len: u32) -> TextRange {
        TextRange {
            start_byte: idx * 100,
            end_byte: idx * 100 + len,
            start_line: idx + 1,
            start_column: 0,
            end_line: idx + 1,
            end_column: len,
        }
    }

    fn facts_help_sym(file_id: FileId, name: &str, idx: u32) -> SymbolDef {
        SymbolDef {
            id: SymbolId::generate(&file_id, "rust", name, "function", None),
            kind: SymbolKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            symbol_path: vec![name.to_string()],
            file_id,
            language: Language::Rust,
            range: facts_help_range(idx, 50),
            name_range: facts_help_range(idx, 20),
            signature: Some(format!("fn {name}()")),
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        }
    }

    fn facts_help_scope(file_id: FileId, idx: u32) -> ScopeDef {
        ScopeDef {
            id: ScopeId::generate(&file_id, None, "function", idx * 100),
            file_id,
            kind: ScopeKind::Function,
            name: format!("scope_{idx}"),
            scope_path: format!("scope_{idx}"),
            range: facts_help_range(idx, 80),
            parent_id: None,
        }
    }

    fn facts_help_edge(_file_id: FileId, source: SymbolId, target: SymbolId, idx: u32) -> RawEdge {
        RawEdge {
            id: EdgeId::generate(&source, &target, "calls", None, "tree_sitter"),
            source,
            target,
            kind: EdgeKind::Calls,
            confidence: Confidence::new(0.9),
            provenance: Provenance::TreeSitter,
            ref_id: None,
            location: Some(facts_help_range(idx, 20)),
            metadata: None,
            resolved_by: None,
        }
    }

    fn facts_help_cs(file_id: FileId, caller: SymbolId, _callee: SymbolId, idx: u32) -> Callsite {
        let ref_id = ReferenceId::generate(
            &file_id, Some(&caller), idx * 100, idx * 100 + 20,
            "target", ReferenceKind::Call,
        );
        let rng = facts_help_range(idx, 20);
        Callsite {
            id: CallsiteId::generate(&ref_id, Some(&caller), idx * 100),
            reference_id: Some(ref_id),
            caller,
            receiver: None,
            args: vec![],
            range: rng,
            callee_range: Some(rng),
        }
    }

    fn facts_help_binding(file_id: FileId, function_id: Option<SymbolId>,
                           scope_id: ScopeId, symbol_id: Option<SymbolId>, idx: u32) -> BindingDef {
        BindingDef {
            id: BindingId::generate(&file_id, &scope_id, "local", &format!("v{idx}"), idx * 10),
            file_id,
            function_id,
            scope_id,
            kind: BindingKind::Local,
            name: format!("v{idx}"),
            symbol_id,
            range: facts_help_range(idx, 5),
        }
    }

    fn facts_help_bu(file_id: FileId, binding_id: BindingId, scope_id: ScopeId, idx: u32) -> BindingUse {
        BindingUse {
            id: BindingUseId::generate(&file_id, Some(&binding_id), None, &format!("v{idx}"), idx * 20),
            file_id,
            scope_id,
            binding_id: Some(binding_id),
            reference_id: None,
            name: format!("v{idx}"),
            range: facts_help_range(idx, 5),
        }
    }

    fn facts_help_dn(file_id: FileId, function_id: Option<SymbolId>,
                      binding_id: Option<BindingId>, idx: u32) -> DataNode {
        DataNode {
            id: DataNodeId::generate(&file_id, function_id.as_ref(), "local", Some("x"), Some("x"), idx * 30),
            file_id,
            function_id,
            kind: DataNodeKind::Local,
            binding_id,
            callsite_id: None,
            name: Some("x".to_string()),
            access_path: Some("x".to_string()),
            arg_index: None,
            range: facts_help_range(idx, 10),
        }
    }

    fn facts_help_dfe(source: DataNodeId, target: DataNodeId, idx: u32) -> DataFlowEdge {
        DataFlowEdge {
            id: DataFlowEdgeId::generate(&source, &target, "assign"),
            source,
            target,
            kind: DataFlowKind::Assign,
            location: facts_help_range(idx, 20),
            confidence: 0.95,
        }
    }

    fn facts_help_cn(_file_id: FileId, function_id: SymbolId, idx: u32) -> CfgNode {
        CfgNode::new(&function_id, CfgNodeKind::Statement, facts_help_range(idx, 30))
    }

    fn facts_help_ce(source: CfgNodeId, target: CfgNodeId) -> CfgEdge {
        CfgEdge::new(&source, &target, CfgEdgeKind::Normal)
    }

    fn count_rows(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn test_write_file_facts_all_valid_no_clone_fallback() {
        let conn = facts_test_conn();
        let file_id = FileId::generate("test.rs");

        let sym0 = facts_help_sym(file_id, "func_a", 0);
        let sym1 = facts_help_sym(file_id, "func_b", 1);
        let scope = facts_help_scope(file_id, 0);
        let edge = facts_help_edge(file_id, sym0.id, sym1.id, 0);
        let cs = facts_help_cs(file_id, sym0.id, sym1.id, 0);
        let binding = facts_help_binding(file_id, Some(sym1.id), scope.id, Some(sym0.id), 0);
        let bu = facts_help_bu(file_id, binding.id, scope.id, 0);
        let dn0 = facts_help_dn(file_id, Some(sym1.id), Some(binding.id), 0);
        let dn1 = facts_help_dn(file_id, Some(sym1.id), Some(binding.id), 1);
        let dfe = facts_help_dfe(dn0.id, dn1.id, 0);
        let cn0 = facts_help_cn(file_id, sym1.id, 0);
        let cn1 = facts_help_cn(file_id, sym1.id, 1);
        let ce = facts_help_ce(cn0.id, cn1.id);

        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "test.rs".into(),
                language: Language::Rust,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![sym0.clone(), sym1.clone()],
            scopes: vec![scope.clone()],
            references: vec![],
            imports: vec![],
            exports: vec![],
            raw_edges: vec![edge.clone()],
            callsites: vec![cs.clone()],
            diagnostics: vec![],
            bindings: vec![binding.clone()],
            binding_uses: vec![bu.clone()],
            data_nodes: vec![dn0.clone(), dn1.clone()],
            dataflow_edges: vec![dfe.clone()],
            cfg_nodes: vec![cn0.clone(), cn1.clone()],
            cfg_edges: vec![ce],
            layer: "structural".to_string(),
            ..Default::default()
        };

        write_file_facts(&conn, &facts, None).unwrap();

        // Verify all data was written (fast-path = no filtering needed)
        assert_eq!(count_rows(&conn, "files"), 1);
        assert_eq!(count_rows(&conn, "symbols"), 2);
        assert_eq!(count_rows(&conn, "scopes"), 1);
        assert_eq!(count_rows(&conn, "symbol_edges"), 1);
        assert_eq!(count_rows(&conn, "callsites"), 1);
        assert_eq!(count_rows(&conn, "bindings"), 1);
        assert_eq!(count_rows(&conn, "binding_uses"), 1);
        assert_eq!(count_rows(&conn, "data_nodes"), 2);
        assert_eq!(count_rows(&conn, "dataflow_edges"), 1);
        assert_eq!(count_rows(&conn, "cfg_nodes"), 2);
        assert_eq!(count_rows(&conn, "cfg_edges"), 1);
    }

    #[test]
    fn test_write_file_facts_fallback_on_invalid_fk() {
        let conn = facts_test_conn();
        let file_id = FileId::generate("test.rs");

        let sym0 = facts_help_sym(file_id, "func_a", 0);
        // An orphan symbol ID not present in facts.symbols
        let orphan_id = SymbolId::generate(&FileId::generate("ghost.rs"), "rust", "ghost", "function", None);
        let valid_edge = facts_help_edge(file_id, sym0.id, sym0.id, 0);
        let invalid_edge = facts_help_edge(file_id, orphan_id, sym0.id, 1);

        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "test.rs".into(),
                language: Language::Rust,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![sym0],
            scopes: vec![],
            references: vec![],
            imports: vec![],
            exports: vec![],
            raw_edges: vec![valid_edge.clone(), invalid_edge],
            callsites: vec![],
            diagnostics: vec![],
            bindings: vec![],
            binding_uses: vec![],
            data_nodes: vec![],
            dataflow_edges: vec![],
            cfg_nodes: vec![],
            cfg_edges: vec![],
            layer: "structural".to_string(),
            ..Default::default()
        };

        write_file_facts(&conn, &facts, None).unwrap();

        // Only the valid edge should be written; no error for invalid one
        assert_eq!(count_rows(&conn, "symbol_edges"), 1);
    }

    #[test]
    fn test_write_file_facts_scope_lookup_empty_scopes() {
        let conn = facts_test_conn();
        let file_id = FileId::generate("test.rs");

        let sym = facts_help_sym(file_id, "main", 0);
        let scope_id = ScopeId::generate(&file_id, None, "function", 100);
        // Binding refers to scope_id which is NOT in facts.scopes
        let binding = facts_help_binding(file_id, Some(sym.id), scope_id, None, 0);

        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "test.rs".into(),
                language: Language::Rust,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![sym],
            scopes: vec![], // empty!
            references: vec![],
            imports: vec![],
            exports: vec![],
            raw_edges: vec![],
            callsites: vec![],
            diagnostics: vec![],
            bindings: vec![binding],
            binding_uses: vec![],
            data_nodes: vec![],
            dataflow_edges: vec![],
            cfg_nodes: vec![],
            cfg_edges: vec![],
            layer: "structural".to_string(),
            ..Default::default()
        };

        write_file_facts(&conn, &facts, None).unwrap();

        // valid_scope_ids is empty → all bindings filtered
        assert_eq!(count_rows(&conn, "bindings"), 0);
    }

    #[test]
    fn test_write_file_facts_scope_lookup_with_scopes() {
        let conn = facts_test_conn();
        let file_id = FileId::generate("test.rs");

        let sym = facts_help_sym(file_id, "main", 0);
        let scope0 = facts_help_scope(file_id, 0);
        let scope1 = facts_help_scope(file_id, 1);
        // scope_not_present is not in facts.scopes → invalid FK
        let phantom_scope = ScopeId::generate(&file_id, None, "block", 9999);

        let b0 = facts_help_binding(file_id, Some(sym.id), scope0.id, None, 0); // valid
        let b1 = facts_help_binding(file_id, Some(sym.id), scope1.id, None, 1); // valid
        let b2 = facts_help_binding(file_id, Some(sym.id), phantom_scope, None, 2); // invalid FK

        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "test.rs".into(),
                language: Language::Rust,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![sym],
            scopes: vec![scope0, scope1],
            references: vec![],
            imports: vec![],
            exports: vec![],
            raw_edges: vec![],
            callsites: vec![],
            diagnostics: vec![],
            bindings: vec![b0, b1, b2],
            binding_uses: vec![],
            data_nodes: vec![],
            dataflow_edges: vec![],
            cfg_nodes: vec![],
            cfg_edges: vec![],
            layer: "structural".to_string(),
            ..Default::default()
        };

        write_file_facts(&conn, &facts, None).unwrap();

        // Only 2 bindings with valid scope FK should be written
        assert_eq!(count_rows(&conn, "bindings"), 2);
    }
}
