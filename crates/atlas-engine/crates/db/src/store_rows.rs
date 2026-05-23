//! Row-mapping helpers: convert SQLite rows into Atlas domain types.
//!
//! Each `row_to_*` function deserialises a `rusqlite::Row` into the
//! corresponding type from `atlas_types`.  These are called from
//! `StoreReader` and `Store` query methods.
//!
//! ## Error handling
//! Enum and JSON deserialisation failures are NOT silently defaulted —
//! they propagate as `rusqlite::Error` so callers can detect DB corruption
//! or schema drift instead of silently returning wrong results.

use types::*;
use rusqlite::Row;

/// Build a `rusqlite::Error` from a parsing failure at a column index.
fn parse_err(idx: usize, value: &str, target: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        idx,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("cannot parse '{}' as {}", value, target),
        )),
    )
}

/// Shared SELECT column list for the `references` table (without WHERE clause).
pub(crate) const REFERENCE_SELECT_NO_WHERE: &str = r#"
    SELECT reference_id, file_id, source_symbol, scope_id, kind,
           text, name, receiver, arity,
           range_start_byte, range_end_byte, range_start_line,
           range_start_column, range_end_line, range_end_column,
           resolved_symbol_id, resolved_confidence, resolved_strategy,
           resolved_provenance, binding_id
    FROM "references""#;

/// Shared SELECT column list for the `references` table with `WHERE file_id = ?1`.
pub(crate) const REFERENCE_SELECT_WHERE: &str = r#"
    SELECT reference_id, file_id, source_symbol, scope_id, kind,
           text, name, receiver, arity,
           range_start_byte, range_end_byte, range_start_line,
           range_start_column, range_end_line, range_end_column,
           resolved_symbol_id, resolved_confidence, resolved_strategy,
           resolved_provenance, binding_id
    FROM "references" WHERE file_id = ?1"#;

pub(crate) fn row_to_file_info(row: &Row) -> rusqlite::Result<FileInfo> {
    let lang_str: String = row.get(2)?;
    let language =
        Language::from_str(&lang_str).ok_or_else(|| parse_err(2, &lang_str, "Language"))?;
    let status_str: String = row.get(4)?;
    let status = ParseStatus::from_str(&status_str)
        .ok_or_else(|| parse_err(4, &status_str, "ParseStatus"))?;
    Ok(FileInfo {
        file_id: row.get(0)?,
        path: row.get(1)?,
        language,
        content_hash: row.get(3)?,
        status,
    })
}

pub(crate) fn row_to_symbol(row: &Row) -> rusqlite::Result<SymbolDef> {
    let symbol_path_json: String = row.get(5)?;
    let ns_json: String = row.get(27)?;
    let kind_str: String = row.get(2)?;
    let kind =
        SymbolKind::from_str(&kind_str).ok_or_else(|| parse_err(2, &kind_str, "SymbolKind"))?;
    let lang_str: String = row.get(6)?;
    let language =
        Language::from_str(&lang_str).ok_or_else(|| parse_err(6, &lang_str, "Language"))?;
    Ok(SymbolDef {
        id: row.get(0)?,
        file_id: row.get(1)?,
        kind,
        name: row.get(3)?,
        qualified_name: row.get(4)?,
        symbol_path: serde_json::from_str(&symbol_path_json)
            .map_err(|e| parse_err(5, &symbol_path_json, &format!("symbol_path JSON: {e}")))?,
        language,
        range: TextRange {
            start_byte: row.get(7)?,
            end_byte: row.get(8)?,
            start_line: row.get(9)?,
            start_column: row.get(10)?,
            end_line: row.get(11)?,
            end_column: row.get(12)?,
        },
        name_range: TextRange {
            start_byte: row.get(13)?,
            end_byte: row.get(14)?,
            start_line: row.get(15)?,
            start_column: row.get(16)?,
            end_line: row.get(17)?,
            end_column: row.get(18)?,
        },
        signature: row.get(19)?,
        visibility: {
            let vis_str: Option<String> = row.get(20)?;
            match vis_str {
                Some(ref v) => {
                    Some(Visibility::from_str(v).ok_or_else(|| parse_err(20, v, "Visibility"))?)
                }
                None => None,
            }
        },
        exported: row.get::<_, i32>(21)? != 0,
        static_: row.get::<_, i32>(22)? != 0,
        async_: row.get::<_, i32>(23)? != 0,
        container: row.get(24)?,
        scope_id: row.get(25)?,
        package_name: row.get(26)?,
        namespace_path: serde_json::from_str(&ns_json)
            .map_err(|e| parse_err(27, &ns_json, &format!("namespace_path JSON: {e}")))?,
    })
}

pub(crate) fn row_to_reference(row: &Row) -> rusqlite::Result<ReferenceUse> {
    let resolved = {
        let sym: Option<SymbolId> = row.get(15)?;
        match sym {
            Some(sid) => {
                let conf: Option<f32> = row.get(16)?;
                let strat_s: Option<String> = row.get(17)?;
                let prov_s: Option<String> = row.get(18)?;
                let strategy = match strat_s {
                    Some(ref s) => ResolutionStrategy::from_str(s)
                        .ok_or_else(|| parse_err(17, s, "ResolutionStrategy"))?,
                    None => ResolutionStrategy::ExactMatch,
                };
                let provenance = match prov_s {
                    Some(ref s) => {
                        Provenance::from_str(s).ok_or_else(|| parse_err(18, s, "Provenance"))?
                    }
                    None => Provenance::default(),
                };
                Some(ResolvedTarget {
                    symbol_id: sid,
                    confidence: Confidence::new(conf.unwrap_or(0.5)),
                    strategy,
                    provenance,
                })
            }
            None => None,
        }
    };
    let ref_kind_str: String = row.get(4)?;
    let ref_kind = ReferenceKind::from_str(&ref_kind_str)
        .ok_or_else(|| parse_err(4, &ref_kind_str, "ReferenceKind"))?;
    Ok(ReferenceUse {
        id: row.get(0)?,
        file_id: row.get(1)?,
        source_symbol: row.get(2)?,
        scope_id: row.get(3)?,
        kind: ref_kind,
        text: row.get(5)?,
        name: row.get(6)?,
        receiver: row.get(7)?,
        arity: row.get(8)?,
        range: TextRange {
            start_byte: row.get(9)?,
            end_byte: row.get(10)?,
            start_line: row.get(11)?,
            start_column: row.get(12)?,
            end_line: row.get(13)?,
            end_column: row.get(14)?,
        },
        resolved,
        binding_id: row.get(19)?,
    })
}

pub(crate) fn row_to_edge(row: &Row) -> rusqlite::Result<RawEdge> {
    let ref_id: Option<ReferenceId> = row.get(6)?;
    let location: Option<TextRange> = {
        let sb: Option<u32> = row.get(7)?;
        match sb {
            Some(start_byte) => Some(TextRange {
                start_byte,
                end_byte: row.get::<_, u32>(8)?,
                start_line: row.get::<_, u32>(9)?,
                start_column: row.get::<_, u32>(10)?,
                end_line: row.get::<_, u32>(11)?,
                end_column: row.get::<_, u32>(12)?,
            }),
            None => None,
        }
    };
    let metadata: Option<String> = row.get(13)?;
    let resolved_by_str: Option<String> = row.get(14)?;
    let resolved_by = match resolved_by_str {
        Some(ref s) => Some(
            ResolutionStrategy::from_str(s)
                .ok_or_else(|| parse_err(14, s, "ResolutionStrategy (resolved_by)"))?,
        ),
        None => None,
    };
    let kind_str: String = row.get(3)?;
    let kind = EdgeKind::from_str(&kind_str).ok_or_else(|| parse_err(3, &kind_str, "EdgeKind"))?;
    let prov_str: String = row.get(5)?;
    let provenance =
        Provenance::from_str(&prov_str).ok_or_else(|| parse_err(5, &prov_str, "Provenance"))?;

    Ok(RawEdge {
        id: row.get(0)?,
        source: row.get(1)?,
        target: row.get(2)?,
        kind,
        confidence: Confidence::new(row.get(4)?),
        provenance,
        ref_id,
        location,
        metadata,
        resolved_by,
    })
}

pub(crate) fn row_to_binding(row: &Row) -> rusqlite::Result<BindingDef> {
    let kind_str: String = row.get(4)?;
    let kind =
        BindingKind::from_str(&kind_str).ok_or_else(|| parse_err(4, &kind_str, "BindingKind"))?;
    Ok(BindingDef {
        id: row.get(0)?,
        file_id: row.get(1)?,
        function_id: row.get(2)?,
        scope_id: row.get(3)?,
        kind,
        name: row.get(5)?,
        symbol_id: row.get(6)?,
        range: TextRange {
            start_byte: row.get(7)?,
            end_byte: row.get(8)?,
            start_line: row.get(9)?,
            start_column: row.get(10)?,
            end_line: row.get(11)?,
            end_column: row.get(12)?,
        },
    })
}

pub(crate) fn row_to_binding_use(row: &Row) -> rusqlite::Result<BindingUse> {
    Ok(BindingUse {
        id: row.get(0)?,
        file_id: row.get(1)?,
        scope_id: row.get(2)?,
        binding_id: row.get(3)?,
        reference_id: row.get(4)?,
        name: row.get(5)?,
        range: TextRange {
            start_byte: row.get(6)?,
            end_byte: row.get(7)?,
            start_line: row.get(8)?,
            start_column: row.get(9)?,
            end_line: row.get(10)?,
            end_column: row.get(11)?,
        },
    })
}

pub(crate) fn row_to_data_node(row: &Row) -> rusqlite::Result<DataNode> {
    let kind_str: String = row.get(3)?;
    let kind =
        DataNodeKind::from_str(&kind_str).ok_or_else(|| parse_err(3, &kind_str, "DataNodeKind"))?;
    Ok(DataNode {
        id: row.get(0)?,
        file_id: row.get(1)?,
        function_id: row.get(2)?,
        kind,
        binding_id: row.get(4)?,
        callsite_id: row.get(5)?,
        name: row.get(6)?,
        access_path: row.get(7)?,
        arg_index: row.get(8)?,
        range: TextRange {
            start_byte: row.get(9)?,
            end_byte: row.get(10)?,
            start_line: row.get(11)?,
            start_column: row.get(12)?,
            end_line: row.get(13)?,
            end_column: row.get(14)?,
        },
    })
}

pub(crate) fn row_to_dataflow_edge(row: &Row) -> rusqlite::Result<DataFlowEdge> {
    let location = TextRange {
        start_byte: row.get::<_, u32>(4)?,
        end_byte: row.get::<_, u32>(5)?,
        start_line: row.get::<_, u32>(6)?,
        start_column: row.get::<_, u32>(7)?,
        end_line: row.get::<_, u32>(8)?,
        end_column: row.get::<_, u32>(9)?,
    };
    let conf: Option<f64> = row.get(10)?;
    let kind_str: String = row.get(3)?;
    let kind =
        DataFlowKind::from_str(&kind_str).ok_or_else(|| parse_err(3, &kind_str, "DataFlowKind"))?;
    Ok(DataFlowEdge {
        id: row.get(0)?,
        source: row.get(1)?,
        target: row.get(2)?,
        kind,
        location,
        confidence: conf.unwrap_or(0.8),
    })
}

pub(crate) fn row_to_callsite(row: &Row) -> rusqlite::Result<Callsite> {
    let args_str: String = row.get(5)?;
    let args: Vec<ArgumentFact> = serde_json::from_str(&args_str)
        .map_err(|e| parse_err(5, &args_str, &format!("Callsite args JSON: {e}")))?;
    let callee_start_line: Option<u32> = row.get(12).ok();
    let callee_start_column: Option<u32> = row.get(13).ok();
    let callee_end_line: Option<u32> = row.get(14).ok();
    let callee_end_column: Option<u32> = row.get(15).ok();
    let callee_start_byte: Option<i64> = row.get(16).ok();
    let callee_end_byte: Option<i64> = row.get(17).ok();
    let callee_range = match (
        callee_start_line,
        callee_start_column,
        callee_end_line,
        callee_end_column,
        callee_start_byte,
        callee_end_byte,
    ) {
        (Some(sl), Some(sc), Some(el), Some(ec), Some(sb), Some(eb)) => Some(TextRange {
            start_line: sl,
            start_column: sc,
            end_line: el,
            end_column: ec,
            start_byte: sb as u32,
            end_byte: eb as u32,
        }),
        _ => None,
    };
    Ok(Callsite {
        id: row.get(0)?,
        reference_id: row.get(1)?,
        caller: row.get(2)?,
        callee: row.get(3)?,
        receiver: row.get(4)?,
        args,
        range: TextRange {
            start_byte: row.get(6)?,
            end_byte: row.get(7)?,
            start_line: row.get(8)?,
            start_column: row.get(9)?,
            end_line: row.get(10)?,
            end_column: row.get(11)?,
        },
        callee_range,
    })
}

pub(crate) fn row_to_cfg_node(row: &Row) -> rusqlite::Result<CfgNode> {
    use types::enums::CfgNodeKind;
    let kind_str: String = row.get(2)?;
    let kind =
        CfgNodeKind::from_str(&kind_str).ok_or_else(|| parse_err(2, &kind_str, "CfgNodeKind"))?;
    Ok(CfgNode {
        id: row.get(0)?,
        function_id: row.get(1)?,
        kind,
        stmt_range: TextRange {
            start_byte: row.get::<_, u32>(3)? as u32,
            end_byte: row.get::<_, u32>(4)? as u32,
            start_line: row.get::<_, u32>(5)? as u32,
            start_column: row.get::<_, u32>(6)? as u32,
            end_line: row.get::<_, u32>(7)? as u32,
            end_column: row.get::<_, u32>(8)? as u32,
        },
    })
}

pub(crate) fn row_to_cfg_edge(row: &Row) -> rusqlite::Result<CfgEdge> {
    use types::enums::CfgEdgeKind;
    let kind_str: String = row.get(3)?;
    let kind =
        CfgEdgeKind::from_str(&kind_str).ok_or_else(|| parse_err(3, &kind_str, "CfgEdgeKind"))?;
    Ok(CfgEdge {
        id: row.get(0)?,
        source: row.get(1)?,
        target: row.get(2)?,
        kind,
    })
}
