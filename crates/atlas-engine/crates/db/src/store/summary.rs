//! Function summary persistence — store & query pre-computed intraprocedural
//! reachability data.
//!
//! ## Tables (Schema v3)
//!
//! - `function_summaries`        — per-function metadata + cache key
//! - `summary_param_reaches`     — parameter → downstream target
//! - `summary_return_sources`    — return node → upstream source
//! - `summary_call_arg_sources`  — call argument → upstream source
//!
//! ## Design
//!
//! `build_all` / `build_for_function` accept a **builder closure** so the
//! db crate does not depend on the analysis crate.  The caller supplies
//! `SummaryBuilder::build` or equivalent.
//!
//! Query methods (`query_param_reaches`, `query_return_sources`,
//! `query_call_arg_sources`) provide O(1) lookups for the
//! `CrossFunctionBridge` in the analysis layer.

use std::time::Instant;

use rusqlite::params;
use types::enums::SymbolKind;
use types::ids::{CallsiteId, DataNodeId, SymbolId};
#[allow(unused_imports)]
use types::summary::{CallArgFlow, FunctionSummary, ParameterFlow, ReturnFlow};

use crate::readers::TraceStore;
use crate::store::Store;

// ---------------------------------------------------------------------------
// Persisted row types — lightweight query results
// ---------------------------------------------------------------------------

/// A row from `summary_param_reaches`.
#[derive(Debug, Clone)]
pub struct ParamReachRow {
    pub function_id: SymbolId,
    pub param_id: DataNodeId,
    pub param_index: i64,
    pub param_name: String,
    pub target_kind: String,
    pub target_node_id: DataNodeId,
    pub confidence: f64,
    pub provenance: String,
}

/// A row from `summary_return_sources`.
#[derive(Debug, Clone)]
pub struct ReturnSourceRow {
    pub function_id: SymbolId,
    pub return_id: DataNodeId,
    pub source_node_id: DataNodeId,
    pub confidence: f64,
    pub provenance: String,
}

/// A row from `summary_call_arg_sources`.
#[derive(Debug, Clone)]
pub struct CallArgSourceRow {
    pub function_id: SymbolId,
    pub callsite_id: CallsiteId,
    pub arg_index: i64,
    pub arg_node_id: DataNodeId,
    pub source_node_id: DataNodeId,
    pub confidence: f64,
    pub provenance: String,
}

// ---------------------------------------------------------------------------
// Build stats
// ---------------------------------------------------------------------------

/// Statistics returned by [`SummaryStore::build_all`].
#[derive(Debug, Clone, Default)]
pub struct SummaryBuildStats {
    /// Number of functions processed.
    pub functions_processed: usize,
    /// Number of functions skipped (no data nodes).
    pub functions_skipped: usize,
    /// Number of functions that produced a summary.
    pub functions_summarized: usize,
    /// Total param_reach rows written.
    pub param_reach_rows: usize,
    /// Total return_source rows written.
    pub return_source_rows: usize,
    /// Total call_arg_source rows written.
    pub call_arg_source_rows: usize,
    /// Wall-clock duration.
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// SummaryStore — persistence layer
// ---------------------------------------------------------------------------

/// Persists and queries function summary data in SQLite.
pub struct SummaryStore;

impl SummaryStore {
    // ── Build ────────────────────────────────────────────────────────────

    /// Build summaries for **all** function symbols in the database.
    ///
    /// Iterates every symbol with `kind = 'function'`, calls `build_fn` to
    /// compute its `FunctionSummary`, and batch-inserts the results into
    /// the 4 summary tables inside a single transaction.
    ///
    /// `build_fn` receives `&dyn TraceStore` and a function `SymbolId` and
    /// must return a `FunctionSummary`.
    pub fn build_all<F>(store: &Store, build_fn: F) -> anyhow::Result<SummaryBuildStats>
    where
        F: Fn(&dyn TraceStore, &SymbolId) -> anyhow::Result<FunctionSummary>,
    {
        let start = Instant::now();

        // Find all function symbols
        let all_symbols = store.get_all_symbols()?;
        let function_symbols: Vec<_> = all_symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();

        let total = function_symbols.len();
        let mut stats = SummaryBuildStats {
            functions_processed: total,
            ..Default::default()
        };

        if total == 0 {
            stats.elapsed_ms = start.elapsed().as_millis() as u64;
            return Ok(stats);
        }

        // ── Phase 1: Build summaries WITHOUT holding the write lock ─────
        //
        // We must NOT hold store.lock() while calling build_fn because
        // SummaryBuilder::build internally calls TraceStore methods that
        // acquire lock_read().  For in-memory databases (open_in_memory),
        // lock_read() falls back to the same std::sync::Mutex as lock(),
        // causing a reentrant deadlock on the same thread.
        //
        // By building summaries first and collecting results, we can then
        // acquire the write lock in Phase 2 solely for persisting them —
        // without any nested lock_read() calls.
        let mut results: Vec<(SymbolId, FunctionSummary)> = Vec::with_capacity(total);
        for sym in &function_symbols {
            match build_fn(store, &sym.id) {
                Ok(summary) => {
                    if summary.is_empty() {
                        stats.functions_skipped += 1;
                    } else {
                        results.push((sym.id, summary));
                    }
                }
                Err(_) => {
                    stats.functions_skipped += 1;
                }
            }
        }

        // ── Phase 2: Write summaries inside a single transaction ────────
        //
        // Now we hold the write lock exclusively for persistence — no
        // nested store reads happen inside write_one_summary, so there
        // is no risk of reentrant locking.
        if !results.is_empty() {
            let conn = store.lock();
            let tx = conn.unchecked_transaction()?;
            for (id, summary) in &results {
                write_one_summary(&tx, id, summary, &mut stats)?;
                stats.functions_summarized += 1;
            }
            tx.commit()?;
        }

        stats.elapsed_ms = start.elapsed().as_millis() as u64;
        Ok(stats)
    }

    /// Build (or rebuild) the summary for a **single** function.
    ///
    /// Invalidates any existing summary rows for this function first, then
    /// computes a new summary via `build_fn` and persists it.
    pub fn build_for_function<F>(
        store: &Store,
        function_id: &SymbolId,
        build_fn: F,
    ) -> anyhow::Result<FunctionSummary>
    where
        F: Fn(&dyn TraceStore, &SymbolId) -> anyhow::Result<FunctionSummary>,
    {
        // Invalidate existing data first
        Self::invalidate_function(store, function_id)?;

        let summary = build_fn(store, function_id)?;
        if summary.is_empty() {
            return Ok(summary);
        }

        let conn = store.lock();
        let tx = conn.unchecked_transaction()?;
        let mut _stats = SummaryBuildStats::default();
        write_one_summary(&tx, function_id, &summary, &mut _stats)?;
        tx.commit()?;

        Ok(summary)
    }

    // ── Invalidation ─────────────────────────────────────────────────────

    /// Delete all summary rows for a function from all 4 tables.
    ///
    /// The `function_summaries` row deletion cascades to the other 3 tables
    /// via `ON DELETE CASCADE`, but we also explicitly delete from all 4
    /// tables to be safe.
    pub fn invalidate_function(store: &Store, function_id: &SymbolId) -> anyhow::Result<()> {
        let conn = store.lock();
        conn.execute(
            "DELETE FROM summary_call_arg_sources WHERE function_id = ?1",
            params![function_id],
        )?;
        conn.execute(
            "DELETE FROM summary_return_sources WHERE function_id = ?1",
            params![function_id],
        )?;
        conn.execute(
            "DELETE FROM summary_param_reaches WHERE function_id = ?1",
            params![function_id],
        )?;
        conn.execute(
            "DELETE FROM function_summaries WHERE function_id = ?1",
            params![function_id],
        )?;
        Ok(())
    }

    // ── Query ────────────────────────────────────────────────────────────

    /// Return the set of distinct file_ids that have at least one function
    /// summary, paired with the file's current content_hash.
    ///
    /// Used by the indexing pipeline to record the "summaries" layer in
    /// extraction_state so capability queries can detect SUMMARIES.
    pub fn files_with_summaries(
        store: &Store,
    ) -> anyhow::Result<Vec<(types::ids::FileId, String)>> {
        let conn = store.lock_read();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT s.file_id, f.content_hash
             FROM function_summaries fs
             JOIN symbols s ON s.symbol_id = fs.function_id
             JOIN files f ON f.file_id = s.file_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, types::ids::FileId>(0)?,
                    row.get::<_, String>(1)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Query all `summary_param_reaches` rows for a given parameter node.
    pub fn query_param_reaches(
        store: &Store,
        param_id: &DataNodeId,
    ) -> anyhow::Result<Vec<ParamReachRow>> {
        let conn = store.lock_read();
        let mut stmt = conn.prepare(
            "SELECT function_id, param_id, param_index, param_name,
                    target_kind, target_node_id, confidence, provenance
             FROM summary_param_reaches WHERE param_id = ?1",
        )?;
        let rows = stmt.query_map(params![param_id], |row| {
            Ok(ParamReachRow {
                function_id: row.get(0)?,
                param_id: row.get(1)?,
                param_index: row.get(2)?,
                param_name: row.get(3)?,
                target_kind: row.get(4)?,
                target_node_id: row.get(5)?,
                confidence: row.get(6)?,
                provenance: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Query all `summary_return_sources` rows for a given return node.
    pub fn query_return_sources(
        store: &Store,
        return_id: &DataNodeId,
    ) -> anyhow::Result<Vec<ReturnSourceRow>> {
        let conn = store.lock_read();
        let mut stmt = conn.prepare(
            "SELECT function_id, return_id, source_node_id, confidence, provenance
             FROM summary_return_sources WHERE return_id = ?1",
        )?;
        let rows = stmt.query_map(params![return_id], |row| {
            Ok(ReturnSourceRow {
                function_id: row.get(0)?,
                return_id: row.get(1)?,
                source_node_id: row.get(2)?,
                confidence: row.get(3)?,
                provenance: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Query all `summary_call_arg_sources` rows for a given call-argument node.
    pub fn query_call_arg_sources(
        store: &Store,
        arg_node_id: &DataNodeId,
    ) -> anyhow::Result<Vec<CallArgSourceRow>> {
        let conn = store.lock_read();
        let mut stmt = conn.prepare(
            "SELECT function_id, callsite_id, arg_index, arg_node_id,
                    source_node_id, confidence, provenance
             FROM summary_call_arg_sources WHERE arg_node_id = ?1",
        )?;
        let rows = stmt.query_map(params![arg_node_id], |row| {
            Ok(CallArgSourceRow {
                function_id: row.get(0)?,
                callsite_id: row.get(1)?,
                arg_index: row.get(2)?,
                arg_node_id: row.get(3)?,
                source_node_id: row.get(4)?,
                confidence: row.get(5)?,
                provenance: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// SummaryReader impl for Store — trait-based access for cross-crate bridges
// ---------------------------------------------------------------------------

impl crate::readers::SummaryReader for Store {
    fn query_param_reaches(&self, param_id: &DataNodeId) -> anyhow::Result<Vec<ParamReachRow>> {
        SummaryStore::query_param_reaches(self, param_id)
    }

    fn query_return_sources(&self, return_id: &DataNodeId) -> anyhow::Result<Vec<ReturnSourceRow>> {
        SummaryStore::query_return_sources(self, return_id)
    }

    fn query_call_arg_sources(
        &self,
        arg_node_id: &DataNodeId,
    ) -> anyhow::Result<Vec<CallArgSourceRow>> {
        SummaryStore::query_call_arg_sources(self, arg_node_id)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Write one function's summary rows into the 4 tables.
fn write_one_summary(
    tx: &rusqlite::Transaction,
    function_id: &SymbolId,
    summary: &FunctionSummary,
    stats: &mut SummaryBuildStats,
) -> anyhow::Result<()> {
    // Deterministic content hash for cache-key / invalidation.
    // Concatenates function_id (deterministic blake3 bytes from types),
    // node_count, and edge_count into a hex string.  No hashing needed
    // here — the input bytes are already a stable unique key.
    let content_hash = {
        let fn_bytes = function_id.as_bytes();
        let mut buf = Vec::with_capacity(fn_bytes.len() + 16);
        buf.extend_from_slice(fn_bytes);
        buf.extend_from_slice(&(summary.node_count as u64).to_le_bytes());
        buf.extend_from_slice(&(summary.edge_count as u64).to_le_bytes());
        hex::encode(&buf)
    };

    // 1. function_summaries
    tx.execute(
        "INSERT OR REPLACE INTO function_summaries
         (function_id, node_count, edge_count, content_hash)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            function_id,
            summary.node_count as i64,
            summary.edge_count as i64,
            content_hash,
        ],
    )?;

    // 2. summary_param_reaches
    for pf in &summary.param_flows {
        // Emit a row for each downstream target kind
        for target_id in &pf.reaches_call_args {
            tx.execute(
                "INSERT INTO summary_param_reaches
                 (function_id, param_id, param_index, param_name,
                  target_kind, target_node_id, confidence, provenance)
                 VALUES (?1, ?2, ?3, ?4, 'call_arg', ?5, ?6, ?7)",
                params![
                    function_id,
                    pf.param_id,
                    pf.param_index as i64,
                    pf.param_name.as_str(),
                    target_id,
                    pf.confidence,
                    pf.provenance.as_str(),
                ],
            )?;
            stats.param_reach_rows += 1;
        }
        for target_id in &pf.reaches_returns {
            tx.execute(
                "INSERT INTO summary_param_reaches
                 (function_id, param_id, param_index, param_name,
                  target_kind, target_node_id, confidence, provenance)
                 VALUES (?1, ?2, ?3, ?4, 'return', ?5, ?6, ?7)",
                params![
                    function_id,
                    pf.param_id,
                    pf.param_index as i64,
                    pf.param_name.as_str(),
                    target_id,
                    pf.confidence,
                    pf.provenance.as_str(),
                ],
            )?;
            stats.param_reach_rows += 1;
        }
        for target_id in &pf.reaches_fields {
            tx.execute(
                "INSERT INTO summary_param_reaches
                 (function_id, param_id, param_index, param_name,
                  target_kind, target_node_id, confidence, provenance)
                 VALUES (?1, ?2, ?3, ?4, 'field', ?5, ?6, ?7)",
                params![
                    function_id,
                    pf.param_id,
                    pf.param_index as i64,
                    pf.param_name.as_str(),
                    target_id,
                    pf.confidence,
                    pf.provenance.as_str(),
                ],
            )?;
            stats.param_reach_rows += 1;
        }
    }

    // 3. summary_return_sources
    for rf in &summary.return_flows {
        for source_id in &rf.sources {
            tx.execute(
                "INSERT INTO summary_return_sources
                 (function_id, return_id, source_node_id, confidence, provenance)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    function_id,
                    rf.return_id,
                    source_id,
                    rf.confidence,
                    rf.provenance.as_str(),
                ],
            )?;
            stats.return_source_rows += 1;
        }
    }

    // 4. summary_call_arg_sources
    for cf in &summary.call_arg_flows {
        for source_id in &cf.sources {
            tx.execute(
                "INSERT INTO summary_call_arg_sources
                 (function_id, callsite_id, arg_index, arg_node_id,
                  source_node_id, confidence, provenance)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    function_id,
                    cf.callsite_id,
                    cf.arg_index as i64,
                    cf.arg_node_id,
                    source_id,
                    cf.confidence,
                    cf.provenance.as_str(),
                ],
            )?;
            stats.call_arg_source_rows += 1;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
    }

    /// Helper: create a minimal FunctionSummary for testing.
    fn test_summary(function_id: &SymbolId, file_id: &types::ids::FileId) -> FunctionSummary {
        let param_id =
            DataNodeId::generate(file_id, Some(function_id), "parameter", Some("x"), None, 0);
        let return_id = DataNodeId::generate(file_id, Some(function_id), "return", None, None, 100);
        let arg_node_id =
            DataNodeId::generate(file_id, Some(function_id), "call_arg", None, None, 50);
        let callsite_id = CallsiteId::generate(
            &types::ids::ReferenceId::generate(
                file_id,
                None,
                30,
                35,
                "call",
                types::enums::ReferenceKind::Call,
            ),
            Some(function_id),
            30,
        );

        #[allow(deprecated)]
        FunctionSummary {
            function_id: *function_id,
            node_count: 5,
            edge_count: 4,
            param_flows: vec![ParameterFlow {
                param_id,
                param_index: 0,
                param_name: "x".to_string(),
                reaches_call_args: vec![arg_node_id],
                reaches_returns: vec![return_id],
                reaches_fields: vec![],
                confidence: 0.85,
                provenance: "intraprocedural_dataflow".to_string(),
            }],
            return_flows: vec![ReturnFlow {
                return_id,
                sources: vec![param_id],
                confidence: 1.0,
                provenance: "intraprocedural_dataflow".to_string(),
            }],
            call_arg_flows: vec![CallArgFlow {
                callsite_id,
                arg_index: 0,
                arg_node_id,
                sources: vec![param_id],
                confidence: 1.0,
                provenance: "intraprocedural_dataflow".to_string(),
            }],
            return_sources: vec![param_id],
        }
    }

    #[test]
    fn test_build_and_query_roundtrip() -> anyhow::Result<()> {
        let store = test_store();

        // Set up: insert a file + function symbol so FK constraints pass
        let file_id = types::ids::FileId::generate("src/test.ts");
        let file_info = types::structs::FileInfo {
            file_id,
            path: "src/test.ts".into(),
            language: types::enums::Language::TypeScript,
            content_hash: "abc".into(),
            status: types::enums::ParseStatus::Success,
        };
        store.upsert_file(&file_info)?;

        let range = types::structs::TextRange {
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 1,
            end_line: 10,
            end_column: 1,
        };
        let func_sym = types::structs::SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", "myFunc", "function", None),
            kind: SymbolKind::Function,
            name: "myFunc".into(),
            qualified_name: "myFunc".into(),
            symbol_path: vec!["myFunc".into()],
            file_id,
            language: types::enums::Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[func_sym.clone()])?;

        // Build summary via build_for_function
        let function_id = func_sym.id;
        let summary = test_summary(&function_id, &file_id);

        let _result = SummaryStore::build_for_function(&store, &function_id, |_, fid| {
            assert_eq!(*fid, function_id);
            Ok(summary.clone())
        })?;

        // Query: param_reaches
        let param_id = summary.param_flows[0].param_id;
        let param_rows = SummaryStore::query_param_reaches(&store, &param_id)?;
        assert!(!param_rows.is_empty(), "should have param_reach rows");
        let call_arg_row = param_rows.iter().find(|r| r.target_kind == "call_arg");
        assert!(call_arg_row.is_some(), "should have call_arg reach row");
        let return_row = param_rows.iter().find(|r| r.target_kind == "return");
        assert!(return_row.is_some(), "should have return reach row");

        // Query: return_sources
        let return_id = summary.return_flows[0].return_id;
        let return_rows = SummaryStore::query_return_sources(&store, &return_id)?;
        assert!(!return_rows.is_empty(), "should have return_source rows");

        // Query: call_arg_sources
        let arg_node_id = summary.call_arg_flows[0].arg_node_id;
        let arg_rows = SummaryStore::query_call_arg_sources(&store, &arg_node_id)?;
        assert!(!arg_rows.is_empty(), "should have call_arg_source rows");

        Ok(())
    }

    #[test]
    fn test_invalidate_clears_all_rows() -> anyhow::Result<()> {
        let store = test_store();

        let file_id = types::ids::FileId::generate("src/test2.ts");
        store.upsert_file(&types::structs::FileInfo {
            file_id,
            path: "src/test2.ts".into(),
            language: types::enums::Language::TypeScript,
            content_hash: "abc".into(),
            status: types::enums::ParseStatus::Success,
        })?;

        let range = types::structs::TextRange {
            start_byte: 0,
            end_byte: 50,
            start_line: 1,
            start_column: 1,
            end_line: 5,
            end_column: 1,
        };
        let func_sym = types::structs::SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", "toInvalidate", "function", None),
            kind: SymbolKind::Function,
            name: "toInvalidate".into(),
            qualified_name: "toInvalidate".into(),
            symbol_path: vec!["toInvalidate".into()],
            file_id,
            language: types::enums::Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[func_sym.clone()])?;

        let function_id = func_sym.id;
        let summary = test_summary(&function_id, &file_id);

        // Build
        SummaryStore::build_for_function(&store, &function_id, |_, _fid| Ok(summary.clone()))?;

        // Verify rows exist
        let param_rows =
            SummaryStore::query_param_reaches(&store, &summary.param_flows[0].param_id)?;
        assert!(!param_rows.is_empty());

        // Invalidate
        SummaryStore::invalidate_function(&store, &function_id)?;

        // Verify rows are gone
        let param_rows =
            SummaryStore::query_param_reaches(&store, &summary.param_flows[0].param_id)?;
        assert!(
            param_rows.is_empty(),
            "param_reaches should be empty after invalidate"
        );

        let return_rows =
            SummaryStore::query_return_sources(&store, &summary.return_flows[0].return_id)?;
        assert!(
            return_rows.is_empty(),
            "return_sources should be empty after invalidate"
        );

        let arg_rows =
            SummaryStore::query_call_arg_sources(&store, &summary.call_arg_flows[0].arg_node_id)?;
        assert!(
            arg_rows.is_empty(),
            "call_arg_sources should be empty after invalidate"
        );

        Ok(())
    }

    #[test]
    fn test_build_all_with_multiple_functions() -> anyhow::Result<()> {
        let store = test_store();

        let file_id = types::ids::FileId::generate("src/multi.ts");
        store.upsert_file(&types::structs::FileInfo {
            file_id,
            path: "src/multi.ts".into(),
            language: types::enums::Language::TypeScript,
            content_hash: "abc".into(),
            status: types::enums::ParseStatus::Success,
        })?;

        let range = types::structs::TextRange {
            start_byte: 0,
            end_byte: 50,
            start_line: 1,
            start_column: 1,
            end_line: 5,
            end_column: 1,
        };

        let fn_a = types::structs::SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", "fnA", "function", None),
            kind: SymbolKind::Function,
            name: "fnA".into(),
            qualified_name: "fnA".into(),
            symbol_path: vec!["fnA".into()],
            file_id,
            language: types::enums::Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        let fn_b = types::structs::SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", "fnB", "function", None),
            kind: SymbolKind::Function,
            name: "fnB".into(),
            qualified_name: "fnB".into(),
            symbol_path: vec!["fnB".into()],
            file_id,
            language: types::enums::Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        let not_a_fn = types::structs::SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", "MyClass", "class", None),
            kind: SymbolKind::Class,
            name: "MyClass".into(),
            qualified_name: "MyClass".into(),
            symbol_path: vec!["MyClass".into()],
            file_id,
            language: types::enums::Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[fn_a.clone(), fn_b.clone(), not_a_fn])?;

        let stats = SummaryStore::build_all(&store, |_s, fid| Ok(test_summary(fid, &file_id)))?;

        // Should process exactly 2 functions, skipping the Class
        assert_eq!(
            stats.functions_processed, 2,
            "should only count Function symbols"
        );
        assert_eq!(stats.functions_summarized, 2);

        // Verify rows exist for fnA
        let summary_a = test_summary(&fn_a.id, &file_id);
        let rows = SummaryStore::query_param_reaches(&store, &summary_a.param_flows[0].param_id)?;
        assert!(!rows.is_empty(), "fnA should have param_reach rows");

        Ok(())
    }

    /// `build_all` must not deadlock on in-memory databases when the
    /// builder closure reads from the store.
    ///
    /// In-memory databases use a single `std::sync::Mutex` for both
    /// `lock()` (write) and `lock_read()` (read).  The old single-phase
    /// `build_all` held `store.lock()` while calling `build_fn`, which
    /// internally calls `lock_read()` — a reentrant deadlock on the same
    /// thread.  The two-phase design (build without lock, then write with
    /// lock) avoids this.
    #[test]
    fn build_all_no_deadlock_in_memory() -> anyhow::Result<()> {
        let store = test_store();

        let file_id = types::ids::FileId::generate("src/deadlock_test.ts");
        store.upsert_file(&types::structs::FileInfo {
            file_id,
            path: "src/deadlock_test.ts".into(),
            language: types::enums::Language::TypeScript,
            content_hash: "abc".into(),
            status: types::enums::ParseStatus::Success,
        })?;

        let range = types::structs::TextRange {
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 1,
            end_line: 10,
            end_column: 1,
        };
        let func_sym = types::structs::SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", "deadlockFn", "function", None),
            kind: SymbolKind::Function,
            name: "deadlockFn".into(),
            qualified_name: "deadlockFn".into(),
            symbol_path: vec!["deadlockFn".into()],
            file_id,
            language: types::enums::Language::TypeScript,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        };
        store.insert_symbols(&[func_sym.clone()])?;

        // build_fn reads from the store via &dyn TraceStore — internally
        // calls lock_read().  On in-memory DBs this would deadlock if the
        // write lock were already held.
        let stats = SummaryStore::build_all(&store, |ts, _fid| {
            let _sym = ts.find_symbol_by_id(_fid)?;
            Ok(test_summary(_fid, &file_id))
        })?;

        assert_eq!(stats.functions_processed, 1);
        assert_eq!(stats.functions_summarized, 1);
        Ok(())
    }
}
