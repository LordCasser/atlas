//! DuckDB row-mapping helpers: convert DuckDB rows into Atlas domain types.
//!
//! Counterpart to `store_rows.rs` for the DuckDB backend. Deserialises
//! `duckdb::Row` into the corresponding types from `atlas_types`.
//!
//! ## Error handling
//! Enum and JSON deserialisation failures are NOT silently defaulted —
//! they propagate as `anyhow::Error` so callers can detect DB corruption
//! or schema drift instead of silently returning wrong results.

use anyhow::Context;
use duckdb::Row;
use types::*;

// ─── BLOB conversion helpers ──────────────────────────────────────────────

/// Convert a raw 32-byte Vec<u8> from DuckDB into a FileId.
/// This is the fast path for single-ID lookups (e.g. resolve_file_id).
pub(crate) fn blob_to_id_raw(bytes: Vec<u8>) -> FileId {
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    FileId::from_bytes(arr)
}

/// Extract a 32-byte BLOB column as a typed ID.
/// DuckDB returns BLOBs as `Vec<u8>`; this converts to `[u8; 32]` and
/// calls the ID's `from_bytes` constructor.
fn blob_to_32<T>(bytes: Vec<u8>, label: &str) -> anyhow::Result<T>
where
    T: FromBlob32,
{
    let len = bytes.len();
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|v: Vec<u8>| {
            let preview = if v.len() >= 8 {
                format!("{:02x?}", &v[..8])
            } else if v.is_empty() {
                "empty".to_string()
            } else {
                format!("{:02x?}", &v[..])
            };
            anyhow::anyhow!(
                "{label}: expected 32-byte blob, got {len} bytes. first bytes: {preview}"
            )
        })?;
    Ok(T::from_bytes(arr))
}

fn opt_blob_to_32<T>(bytes: Option<Vec<u8>>, label: &str) -> anyhow::Result<Option<T>>
where
    T: FromBlob32,
{
    match bytes {
        Some(b) => blob_to_32(b, label).map(Some),
        None => Ok(None),
    }
}

/// Trait for types that can be reconstructed from a 32-byte blob.
/// Implemented by all ID types in the `define_id!` macro.
trait FromBlob32: Sized {
    fn from_bytes(bytes: [u8; 32]) -> Self;
}

// Auto-impl for all ID types that have `from_bytes`.
macro_rules! impl_from_blob32 {
    ($($t:ty),* $(,)?) => {
        $(
            impl FromBlob32 for $t {
                fn from_bytes(bytes: [u8; 32]) -> Self { Self::from_bytes(bytes) }
            }
        )*
    };
}

impl_from_blob32!(
    FileId, SymbolId, ScopeId, ReferenceId, EdgeId,
    CallsiteId, ImportId, BindingId, BindingUseId,
    DataNodeId, DataFlowEdgeId, CfgNodeId, CfgEdgeId,
);

// ─── Enum fallback ────────────────────────────────────────────────────────

fn parse_err(col: usize, value: &str, target: &str) -> anyhow::Error {
    anyhow::anyhow!("column {col}: cannot parse '{value}' as {target}")
}

// ─── FileInfo ─────────────────────────────────────────────────────────────

pub(crate) fn row_to_file_info(row: &Row) -> anyhow::Result<FileInfo> {
    let lang_str: String = row.get(2)?;
    let language = Language::from_str(&lang_str)
        .ok_or_else(|| parse_err(2, &lang_str, "Language"))?;
    let status_str: String = row.get(4)?;
    let status = ParseStatus::from_str(&status_str)
        .ok_or_else(|| parse_err(4, &status_str, "ParseStatus"))?;
    Ok(FileInfo {
        file_id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "file_id")?,
        path: row.get(1)?,
        language,
        content_hash: row.get(3)?,
        status,
    })
}

// ─── SymbolDef ─────────────────────────────────────────────────────────────

pub(crate) fn row_to_symbol(row: &Row) -> anyhow::Result<SymbolDef> {
    let symbol_path_json: String = row.get(5)?;
    let ns_json: String = row.get(27)?;
    let kind_str: String = row.get(2)?;
    let kind = SymbolKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(2, &kind_str, "SymbolKind"))?;
    let lang_str: String = row.get(6)?;
    let language = Language::from_str(&lang_str)
        .ok_or_else(|| parse_err(6, &lang_str, "Language"))?;
    let vis_str: Option<String> = row.get(20)?;
    let visibility = match vis_str {
        Some(ref v) if v.is_empty() => None,
        Some(ref v) => Some(
            Visibility::from_str(v).ok_or_else(|| parse_err(20, v, "Visibility"))?,
        ),
        None => None,
    };
    let layer_str: Option<String> = row.get(28)?;
    Ok(SymbolDef {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "symbol_id")?,
        file_id: blob_to_32(row.get::<_, Vec<u8>>(1)?, "file_id")?,
        kind,
        name: row.get(3)?,
        qualified_name: row.get(4)?,
        symbol_path: serde_json::from_str(&symbol_path_json)
            .with_context(|| format!("symbol_path JSON at col 5: {symbol_path_json}"))?,
        language,
        range: build_range(row, 7)?,
        name_range: build_range(row, 13)?,
        signature: row.get(19)?,
        visibility,
        exported: row.get::<_, i32>(21)? != 0,
        static_: row.get::<_, i32>(22)? != 0,
        async_: row.get::<_, i32>(23)? != 0,
        container: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(24)?, "container_id")?,
        scope_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(25)?, "scope_id")?,
        package_name: row.get(26)?,
        namespace_path: serde_json::from_str(&ns_json)
            .with_context(|| format!("namespace_path JSON at col 27: {ns_json}"))?,
        layer: layer_str.unwrap_or_else(|| "structural".to_string()),
    })
}

// ─── SymbolDef (batch row without SymbolPath) ──────────────────────────────

/// Symbol row from the shared column list without SymbolPath column.
/// Used by `get_all_symbols()` which uses a shorter column list.
pub(crate) fn row_to_symbol_no_path(row: &Row) -> anyhow::Result<SymbolDef> {
    let kind_str: String = row.get(2)?;
    let kind = SymbolKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(2, &kind_str, "SymbolKind"))?;
    let lang_str: String = row.get(6)?;
    let language = Language::from_str(&lang_str)
        .ok_or_else(|| parse_err(6, &lang_str, "Language"))?;
    let vis_str: Option<String> = row.get(20)?;
    let visibility = match vis_str {
        Some(ref v) if v.is_empty() => None,
        Some(ref v) => Some(
            Visibility::from_str(v).ok_or_else(|| parse_err(20, v, "Visibility"))?,
        ),
        None => None,
    };
    let layer_str: Option<String> = row.get(28)?;
    Ok(SymbolDef {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "symbol_id")?,
        file_id: blob_to_32(row.get::<_, Vec<u8>>(1)?, "file_id")?,
        kind,
        name: row.get(3)?,
        qualified_name: row.get(4)?,
        symbol_path: vec![],
        language,
        range: build_range(row, 7)?,
        name_range: build_range(row, 13)?,
        signature: row.get(19)?,
        visibility,
        exported: row.get::<_, i32>(21)? != 0,
        static_: row.get::<_, i32>(22)? != 0,
        async_: row.get::<_, i32>(23)? != 0,
        container: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(24)?, "container_id")?,
        scope_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(25)?, "scope_id")?,
        package_name: row.get(26)?,
        namespace_path: vec![],
        layer: layer_str.unwrap_or_else(|| "structural".to_string()),
    })
}

// ─── RawEdge ───────────────────────────────────────────────────────────────

pub(crate) fn row_to_edge(row: &Row) -> anyhow::Result<RawEdge> {
    let kind_str: String = row.get(3)?;
    let kind = EdgeKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(3, &kind_str, "EdgeKind"))?;
    let prov_str: String = row.get(5)?;
    let provenance = Provenance::from_str(&prov_str)
        .ok_or_else(|| parse_err(5, &prov_str, "Provenance"))?;
    let location = build_opt_range(row, 7)?;
    let metadata: Option<String> = row.get(13)?;
    let resolved_by_str: Option<String> = row.get(14)?;
    let resolved_by = match resolved_by_str {
        Some(ref s) if s.is_empty() => None,
        Some(ref s) => Some(
            ResolutionStrategy::from_str(s)
                .ok_or_else(|| parse_err(14, s, "ResolutionStrategy"))?,
        ),
        None => None,
    };
    Ok(RawEdge {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "edge_id")?,
        source: blob_to_32(row.get::<_, Vec<u8>>(1)?, "source")?,
        target: blob_to_32(row.get::<_, Vec<u8>>(2)?, "target")?,
        kind,
        confidence: Confidence::new(row.get(4)?),
        provenance,
        ref_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(6)?, "ref_id")?,
        location,
        metadata,
        resolved_by,
    })
}

// ─── Callsite ──────────────────────────────────────────────────────────────

pub(crate) fn row_to_callsite(row: &Row) -> anyhow::Result<Callsite> {
    let args_str: String = row.get(5)?;
    let args: Vec<ArgumentFact> = serde_json::from_str(&args_str)
        .with_context(|| format!("callsite args JSON at col 5: {args_str}"))?;
    let callee_range = build_opt_range(row, 12)?;
    Ok(Callsite {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "callsite_id")?,
        reference_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(1)?, "reference_id")?,
        caller: blob_to_32(row.get::<_, Vec<u8>>(2)?, "caller")?,
        callee: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(3)?, "callee")?,
        receiver: row.get(4)?,
        args,
        range: build_range(row, 6)?,
        callee_range,
    })
}

// ─── DataNode ──────────────────────────────────────────────────────────────

pub(crate) fn row_to_data_node(row: &Row) -> anyhow::Result<DataNode> {
    let kind_str: String = row.get(3)?;
    let kind = DataNodeKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(3, &kind_str, "DataNodeKind"))?;
    Ok(DataNode {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "data_node_id")?,
        file_id: blob_to_32(row.get::<_, Vec<u8>>(1)?, "file_id")?,
        function_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(2)?, "function_id")?,
        kind,
        binding_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(4)?, "binding_id")?,
        callsite_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(5)?, "callsite_id")?,
        name: row.get(6)?,
        access_path: row.get(7)?,
        arg_index: row.get(8)?,
        range: build_range(row, 9)?,
    })
}

// ─── DataFlowEdge ──────────────────────────────────────────────────────────

pub(crate) fn row_to_dataflow_edge(row: &Row) -> anyhow::Result<DataFlowEdge> {
    let kind_str: String = row.get(3)?;
    let kind = DataFlowKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(3, &kind_str, "DataFlowKind"))?;
    let conf: Option<f64> = row.get(10)?;
    Ok(DataFlowEdge {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "dataflow_edge_id")?,
        source: blob_to_32(row.get::<_, Vec<u8>>(1)?, "source")?,
        target: blob_to_32(row.get::<_, Vec<u8>>(2)?, "target")?,
        kind,
        location: build_range(row, 4)?,
        confidence: conf.unwrap_or(0.8),
    })
}

// ─── BindingDef / BindingUse ───────────────────────────────────────────────

pub(crate) fn row_to_binding(row: &Row) -> anyhow::Result<BindingDef> {
    let kind_str: String = row.get(4)?;
    let kind = BindingKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(4, &kind_str, "BindingKind"))?;
    Ok(BindingDef {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "binding_id")?,
        file_id: blob_to_32(row.get::<_, Vec<u8>>(1)?, "file_id")?,
        function_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(2)?, "function_id")?,
        scope_id: blob_to_32(row.get::<_, Vec<u8>>(3)?, "scope_id")?,
        kind,
        name: row.get(5)?,
        symbol_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(6)?, "symbol_id")?,
        range: build_range(row, 7)?,
    })
}

pub(crate) fn row_to_binding_use(row: &Row) -> anyhow::Result<BindingUse> {
    Ok(BindingUse {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "binding_use_id")?,
        file_id: blob_to_32(row.get::<_, Vec<u8>>(1)?, "file_id")?,
        scope_id: blob_to_32(row.get::<_, Vec<u8>>(2)?, "scope_id")?,
        binding_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(3)?, "binding_id")?,
        reference_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(4)?, "reference_id")?,
        name: row.get(5)?,
        range: build_range(row, 6)?,
    })
}

// ─── CfgNode / CfgEdge ─────────────────────────────────────────────────────

pub(crate) fn row_to_cfg_node(row: &Row) -> anyhow::Result<CfgNode> {
    let kind_str: String = row.get(2)?;
    let kind = CfgNodeKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(2, &kind_str, "CfgNodeKind"))?;
    Ok(CfgNode {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "cfg_node_id")?,
        function_id: blob_to_32(row.get::<_, Vec<u8>>(1)?, "function_id")?,
        kind,
        stmt_range: build_range(row, 3)?,
    })
}

pub(crate) fn row_to_cfg_edge(row: &Row) -> anyhow::Result<CfgEdge> {
    let kind_str: String = row.get(3)?;
    let kind = CfgEdgeKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(3, &kind_str, "CfgEdgeKind"))?;
    Ok(CfgEdge {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "cfg_edge_id")?,
        source: blob_to_32(row.get::<_, Vec<u8>>(1)?, "source")?,
        target: blob_to_32(row.get::<_, Vec<u8>>(2)?, "target")?,
        kind,
    })
}

// ─── ScopeDef ──────────────────────────────────────────────────────────────

pub(crate) fn row_to_scope(row: &Row) -> anyhow::Result<ScopeDef> {
    let kind_str: String = row.get(2)?;
    let kind = ScopeKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(2, &kind_str, "ScopeKind"))?;
    Ok(ScopeDef {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "scope_id")?,
        file_id: blob_to_32(row.get::<_, Vec<u8>>(1)?, "file_id")?,
        kind,
        name: row.get(3)?,
        scope_path: row.get(4)?,
        range: build_range(row, 5)?,
        parent_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(11)?, "parent_id")?,
    })
}

// ─── ImportDef ─────────────────────────────────────────────────────────────

pub(crate) fn row_to_import(row: &Row) -> anyhow::Result<ImportDef> {
    let kind_str: String = row.get(2)?;
    let kind = ImportKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(2, &kind_str, "ImportKind"))?;
    Ok(ImportDef {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "import_id")?,
        file_id: blob_to_32(row.get::<_, Vec<u8>>(1)?, "file_id")?,
        kind,
        module: row.get(3)?,
        imported_name: row.get(4)?,
        local_name: row.get(5)?,
        is_wildcard: row.get::<_, i32>(6)? != 0,
        is_relative: row.get::<_, i32>(7)? != 0,
        range: build_range(row, 8)?,
        alias: row.get(14)?,
    })
}

// ─── ReferenceUse ──────────────────────────────────────────────────────────

pub(crate) fn row_to_reference(row: &Row) -> anyhow::Result<ReferenceUse> {
    let kind_str: String = row.get(4)?;
    let kind = ReferenceKind::from_str(&kind_str)
        .ok_or_else(|| parse_err(4, &kind_str, "ReferenceKind"))?;
    let resolved = match row.get::<_, Option<Vec<u8>>>(15)? {
        Some(sid_blob) => {
            let sid = blob_to_32(sid_blob, "resolved_symbol_id")?;
            let conf: Option<f32> = row.get(16)?;
            let strat_s: Option<String> = row.get(17)?;
            let prov_s: Option<String> = row.get(18)?;
            let strategy = match strat_s {
                Some(ref s) if s.is_empty() => ResolutionStrategy::ExactMatch,
                Some(ref s) => ResolutionStrategy::from_str(s)
                    .ok_or_else(|| parse_err(17, s, "ResolutionStrategy"))?,
                None => ResolutionStrategy::ExactMatch,
            };
            let provenance = match prov_s {
                Some(ref s) if s.is_empty() => Provenance::default(),
                Some(ref s) => Provenance::from_str(s)
                    .ok_or_else(|| parse_err(18, s, "Provenance"))?,
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
    };
    Ok(ReferenceUse {
        id: blob_to_32(row.get::<_, Vec<u8>>(0)?, "reference_id")?,
        file_id: blob_to_32(row.get::<_, Vec<u8>>(1)?, "file_id")?,
        source_symbol: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(2)?, "source_symbol")?,
        scope_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(3)?, "scope_id")?,
        kind,
        text: row.get(5)?,
        name: row.get(6)?,
        receiver: row.get(7)?,
        arity: row.get(8)?,
        range: build_range(row, 9)?,
        resolved,
        binding_id: opt_blob_to_32(row.get::<_, Option<Vec<u8>>>(19)?, "binding_id")?,
    })
}

// ─── Helpers ───────────────────────────────────────────────────────────────

fn build_range(row: &Row, start_col: usize) -> anyhow::Result<TextRange> {
    Ok(TextRange {
        start_byte: col_u32(row, start_col)?,
        end_byte: col_u32(row, start_col + 1)?,
        start_line: col_u32(row, start_col + 2)?,
        start_column: col_u32(row, start_col + 3)?,
        end_line: col_u32(row, start_col + 4)?,
        end_column: col_u32(row, start_col + 5)?,
    })
}

fn build_opt_range(row: &Row, start_col: usize) -> anyhow::Result<Option<TextRange>> {
    // Check if the first column of the range is NULL.
    let sb: Option<i64> = row.get(start_col)?;
    match sb {
        None => Ok(None),
        Some(start_byte) => Ok(Some(TextRange {
            start_byte: start_byte as u32,
            end_byte: row.get::<_, i64>(start_col + 1)? as u32,
            start_line: row.get::<_, i64>(start_col + 2)? as u32,
            start_column: row.get::<_, i64>(start_col + 3)? as u32,
            end_line: row.get::<_, i64>(start_col + 4)? as u32,
            end_column: row.get::<_, i64>(start_col + 5)? as u32,
        })),
    }
}

fn col_u32(row: &Row, col: usize) -> anyhow::Result<u32> {
    let v: i64 = row.get(col)?;
    Ok(v as u32)
}
