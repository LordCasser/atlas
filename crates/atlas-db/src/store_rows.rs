//! Row-mapping helpers: convert SQLite rows into Atlas domain types.
//!
//! Each `row_to_*` function deserialises a `rusqlite::Row` into the
//! corresponding type from `crate::types`.  These are called from
//! `StoreReader` and `Store` query methods.

use atlas_types::*;
use rusqlite::Row;

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
    Ok(FileInfo {
        file_id: row.get(0)?,
        path: row.get(1)?,
        language: Language::from_str(row.get::<_, String>(2)?.as_str()).unwrap_or_default(),
        content_hash: row.get(3)?,
        status: ParseStatus::from_str(row.get::<_, String>(4)?.as_str()).unwrap_or_default(),
    })
}

pub(crate) fn row_to_symbol(row: &Row) -> rusqlite::Result<SymbolDef> {
    let symbol_path_json: String = row.get(5)?;
    let ns_json: String = row.get(27)?;
    Ok(SymbolDef {
        id: row.get(0)?,
        file_id: row.get(1)?,
        kind: SymbolKind::from_str(row.get::<_, String>(2)?.as_str()).unwrap_or(SymbolKind::File),
        name: row.get(3)?,
        qualified_name: row.get(4)?,
        symbol_path: serde_json::from_str(&symbol_path_json).unwrap_or_default(),
        language: Language::from_str(row.get::<_, String>(6)?.as_str()).unwrap_or_default(),
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
        visibility: row
            .get::<_, Option<String>>(20)?
            .and_then(|v| Visibility::from_str(&v)),
        exported: row.get::<_, i32>(21)? != 0,
        static_: row.get::<_, i32>(22)? != 0,
        async_: row.get::<_, i32>(23)? != 0,
        container: row.get(24)?,
        scope_id: row.get(25)?,
        package_name: row.get(26)?,
        namespace_path: serde_json::from_str(&ns_json).unwrap_or_default(),
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
                Some(ResolvedTarget {
                    symbol_id: sid,
                    confidence: Confidence::new(conf.unwrap_or(0.5)),
                    strategy: ResolutionStrategy::from_str(strat_s.as_deref().unwrap_or(""))
                        .unwrap_or(ResolutionStrategy::ExactMatch),
                    provenance: Provenance::from_str(prov_s.as_deref().unwrap_or(""))
                        .unwrap_or_default(),
                })
            }
            None => None,
        }
    };
    Ok(ReferenceUse {
        id: row.get(0)?,
        file_id: row.get(1)?,
        source_symbol: row.get(2)?,
        scope_id: row.get(3)?,
        kind: ReferenceKind::from_str(row.get::<_, String>(4)?.as_str())
            .unwrap_or(ReferenceKind::Usage),
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
        sb.map(|start_byte| TextRange {
            start_byte,
            end_byte: row.get::<_, u32>(8).unwrap_or(0),
            start_line: row.get::<_, u32>(9).unwrap_or(0),
            start_column: row.get::<_, u32>(10).unwrap_or(0),
            end_line: row.get::<_, u32>(11).unwrap_or(0),
            end_column: row.get::<_, u32>(12).unwrap_or(0),
        })
    };
    let metadata: Option<String> = row.get(13)?;
    let resolved_by_str: Option<String> = row.get(14)?;
    let resolved_by = resolved_by_str
        .as_deref()
        .and_then(|s| ResolutionStrategy::from_str(s));

    Ok(RawEdge {
        id: row.get(0)?,
        source: row.get(1)?,
        target: row.get(2)?,
        kind: EdgeKind::from_str(row.get::<_, String>(3)?.as_str()).unwrap_or(EdgeKind::References),
        confidence: Confidence::new(row.get(4)?),
        provenance: Provenance::from_str(row.get::<_, String>(5)?.as_str()).unwrap_or_default(),
        ref_id,
        location,
        metadata,
        resolved_by,
    })
}

pub(crate) fn row_to_binding(row: &Row) -> rusqlite::Result<BindingDef> {
    Ok(BindingDef {
        id: row.get(0)?,
        file_id: row.get(1)?,
        function_id: row.get(2)?,
        scope_id: row.get(3)?,
        kind: BindingKind::from_str(row.get::<_, String>(4)?.as_str())
            .unwrap_or(BindingKind::Local),
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
    Ok(DataNode {
        id: row.get(0)?,
        file_id: row.get(1)?,
        function_id: row.get(2)?,
        kind: DataNodeKind::from_str(row.get::<_, String>(3)?.as_str())
            .unwrap_or(DataNodeKind::Unknown),
        binding_id: row.get(4)?,
        callsite_id: row.get(5)?,
        name: row.get(6)?,
        access_path: row.get(7)?,
        range: TextRange {
            start_byte: row.get(8)?,
            end_byte: row.get(9)?,
            start_line: row.get(10)?,
            start_column: row.get(11)?,
            end_line: row.get(12)?,
            end_column: row.get(13)?,
        },
    })
}

pub(crate) fn row_to_dataflow_edge(row: &Row) -> rusqlite::Result<DataFlowEdge> {
    let location = TextRange {
        start_byte: row.get::<_, u32>(4).unwrap_or(0),
        end_byte: row.get::<_, u32>(5).unwrap_or(0),
        start_line: row.get::<_, u32>(6).unwrap_or(0),
        start_column: row.get::<_, u32>(7).unwrap_or(0),
        end_line: row.get::<_, u32>(8).unwrap_or(0),
        end_column: row.get::<_, u32>(9).unwrap_or(0),
    };
    let conf: Option<f64> = row.get(10)?;
    Ok(DataFlowEdge {
        id: row.get(0)?,
        source: row.get(1)?,
        target: row.get(2)?,
        kind: DataFlowKind::from_str(row.get::<_, String>(3)?.as_str())
            .unwrap_or(DataFlowKind::Assign),
        location,
        confidence: conf.unwrap_or(0.8),
    })
}

pub(crate) fn row_to_callsite(row: &Row) -> rusqlite::Result<Callsite> {
    let args_str: String = row.get(5)?;
    let args: Vec<ArgumentFact> = serde_json::from_str(&args_str).unwrap_or_default();
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
    use atlas_types::enums::CfgNodeKind;
    let kind_str: String = row.get(2)?;
    let kind = CfgNodeKind::from_str(&kind_str).unwrap_or(CfgNodeKind::Statement);
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
    use atlas_types::enums::CfgEdgeKind;
    let kind_str: String = row.get(3)?;
    let kind = CfgEdgeKind::from_str(&kind_str).unwrap_or(CfgEdgeKind::Normal);
    Ok(CfgEdge {
        id: row.get(0)?,
        source: row.get(1)?,
        target: row.get(2)?,
        kind,
    })
}
