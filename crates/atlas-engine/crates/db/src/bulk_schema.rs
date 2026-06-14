//! Bulk-load schema management: index groups and FTS trigger definitions.
//!
//! During full index rebuild, ALL indexes below (except PK autoindexes)
//! are dropped before Phase 6 write and recreated in stages.

/// FTS triggers on symbols table — drop before bulk write, recreate after.
pub const FTS_TRIGGERS: &[&str] = &["symbols_ai", "symbols_ad", "symbols_au"];

/// All non-PK indexes to DROP before Phase 6 bulk write.
/// This includes EVERY index except the implicit PK autoindexes.
/// Listed alphabetically by table for readability.
pub const ALL_WRITE_INDEXES: &[&str] = &[
    // binding_uses
    "idx_binding_uses_binding",
    "idx_binding_uses_file",
    "idx_binding_uses_reference",
    // bindings
    "idx_bindings_file",
    "idx_bindings_function",
    "idx_bindings_symbol",
    // callsites
    "idx_callsites_caller",
    "idx_callsites_reference",
    // cfg_edges
    "idx_cfg_edges_kind",
    "idx_cfg_edges_source",
    "idx_cfg_edges_target",
    // cfg_nodes
    "idx_cfg_nodes_function",
    "idx_cfg_nodes_kind",
    // data_nodes
    "idx_data_nodes_binding",
    "idx_data_nodes_file",
    "idx_data_nodes_function",
    // dataflow_edges
    "idx_dataflow_edges_kind",
    "idx_dataflow_edges_source",
    "idx_dataflow_edges_target",
    // files
    "idx_files_language",
    "idx_files_path",
    // fpa (function_path_aliases)
    "idx_fpa_source",
    "idx_fpa_source_field",
    "idx_fpa_target",
    // imports
    "idx_imports_file",
    "idx_imports_module",
    // references (ALL — Phase 7 will UPDATE these without index maintenance)
    "idx_references_file",
    "idx_references_resolved",
    "idx_references_source",
    "idx_references_unresolved",
    // scopes
    "idx_scopes_file",
    "idx_scopes_parent",
    // symbol_edges
    "idx_symbol_edges_kind",
    "idx_symbol_edges_source",
    "idx_symbol_edges_source_kind",
    "idx_symbol_edges_target",
    // symbols
    "idx_symbols_container",
    "idx_symbols_file",
    "idx_symbols_kind",
    "idx_symbols_name",
    "idx_symbols_qname",
];

/// Minimal indexes needed before Phase 7 resolution.
/// These are the indexes that resolve_all_parallel reads.
pub const RESOLUTION_INDEXES: &[&str] = &[
    "idx_files_path",
    "idx_symbols_file",
    "idx_symbols_qname",
    "idx_scopes_file",
    "idx_imports_file",
    "idx_imports_module",
];

/// Indexes needed for dataflow/CFG summary build (--analysis full only).
/// Created after Phase 7, before SummaryBuild.
pub const SUMMARY_INDEXES: &[&str] = &[
    "idx_cfg_nodes_function",
    "idx_cfg_nodes_kind",
    "idx_cfg_edges_source",
    "idx_cfg_edges_target",
    "idx_cfg_edges_kind",
    "idx_data_nodes_function",
    "idx_data_nodes_binding",
    "idx_data_nodes_file",
    "idx_dataflow_edges_source",
    "idx_dataflow_edges_target",
    "idx_dataflow_edges_kind",
];

/// All remaining indexes that should exist in a complete schema.
/// Created at Phase 10 finalize. This is the exhaustive list of ALL
/// indexes (minus resolution+summary indexes already created in earlier stages).
pub const FINAL_QUERY_INDEXES: &[&str] = &[
    // binding_uses
    "idx_binding_uses_binding",
    "idx_binding_uses_file",
    "idx_binding_uses_reference",
    // bindings
    "idx_bindings_file",
    "idx_bindings_function",
    "idx_bindings_symbol",
    // callsites
    "idx_callsites_caller",
    "idx_callsites_reference",
    // fpa
    "idx_fpa_source",
    "idx_fpa_source_field",
    "idx_fpa_target",
    // files
    "idx_files_language",
    // references
    "idx_references_file",
    "idx_references_resolved",
    "idx_references_source",
    "idx_references_unresolved",
    // scopes (non-resolution: parent index not needed for resolve_all_parallel)
    "idx_scopes_parent",
    // symbol_edges
    "idx_symbol_edges_kind",
    "idx_symbol_edges_source",
    "idx_symbol_edges_source_kind",
    "idx_symbol_edges_target",
    // symbols (non-resolution indexes)
    "idx_symbols_container",
    "idx_symbols_kind",
    "idx_symbols_name",
];

// ---------------------------------------------------------------------------
// FTS trigger SQL — matches schema.rs FTS5 virtual table columns (name,
// qualified_name only).
// ---------------------------------------------------------------------------

/// SQL to create FTS trigger on symbols INSERT.
pub const SYMBOLS_AI_TRIGGER: &str = r#"
CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
    INSERT INTO symbols_fts(rowid, name, qualified_name)
    VALUES (new.rowid, new.name, new.qualified_name);
END;
"#;

/// SQL to create FTS trigger on symbols DELETE.
pub const SYMBOLS_AD_TRIGGER: &str = r#"
CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, qualified_name)
    VALUES ('delete', old.rowid, old.name, old.qualified_name);
END;
"#;

/// SQL to create FTS trigger on symbols UPDATE.
pub const SYMBOLS_AU_TRIGGER: &str = r#"
CREATE TRIGGER IF NOT EXISTS symbols_au AFTER UPDATE ON symbols BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, qualified_name)
    VALUES ('delete', old.rowid, old.name, old.qualified_name);
    INSERT INTO symbols_fts(rowid, name, qualified_name)
    VALUES (new.rowid, new.name, new.qualified_name);
END;
"#;

/// SQL to rebuild FTS index after bulk loading is complete.
pub const FTS_REBUILD: &str = "INSERT INTO symbols_fts(symbols_fts) VALUES('rebuild');";

/// Indexes on extraction_state, extraction_jobs, and summary tables.
/// These are NOT dropped during bulk write (they are "kept"), but they are
/// still schema objects that `ensure_required_schema_objects` should check.
pub const EXTRACTION_AND_SUMMARY_INDEXES: &[&str] = &[
    // extraction_state
    "idx_extraction_state_file_layer",
    "idx_extraction_state_unit_layer",
    "idx_extraction_state_file",
    "idx_extraction_state_layer_status",
    // extraction_jobs
    "idx_extraction_jobs_file_layer_status",
    "idx_extraction_jobs_status",
    "idx_extraction_jobs_active_file_layer",
    "idx_extraction_jobs_active_unit_layer",
    // summary_param_reaches
    "idx_spr_function",
    "idx_spr_param",
    // summary_return_sources
    "idx_srs_function",
    "idx_srs_return",
    // summary_call_arg_sources
    "idx_scas_function",
    "idx_scas_callsite",
];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

use rusqlite::Connection;

/// Execute all SQL statements in `sqls` using `conn.execute_batch()`.
/// Each element is a single complete SQL statement (no trailing semicolon needed).
pub fn execute_batch_ddl(conn: &Connection, sqls: &[String]) -> anyhow::Result<()> {
    if sqls.is_empty() {
        return Ok(());
    }
    // join with semicolons, execute_batch handles multiple statements
    let batch = sqls
        .iter()
        .map(|s| s.trim().to_string())
        .collect::<Vec<_>>()
        .join(";\n");
    conn.execute_batch(&batch)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_write_indexes_contains_resolution_indexes() {
        let all: HashSet<_> = ALL_WRITE_INDEXES.iter().copied().collect();
        for &idx in RESOLUTION_INDEXES {
            assert!(
                all.contains(idx),
                "RESOLUTION_INDEXES member {idx} not found in ALL_WRITE_INDEXES"
            );
        }
    }

    #[test]
    fn no_overlap_between_resolution_and_final_query() {
        let res: HashSet<_> = RESOLUTION_INDEXES.iter().copied().collect();
        let fin: HashSet<_> = FINAL_QUERY_INDEXES.iter().copied().collect();
        let overlap: Vec<_> = res.intersection(&fin).copied().collect();
        assert!(
            overlap.is_empty(),
            "overlap between RESOLUTION_INDEXES and FINAL_QUERY_INDEXES: {overlap:?}"
        );
    }

    #[test]
    fn stage_groups_cover_all_write_indexes() {
        let all: HashSet<_> = ALL_WRITE_INDEXES.iter().copied().collect();
        let stage_indexes: HashSet<&str> = RESOLUTION_INDEXES
            .iter()
            .chain(SUMMARY_INDEXES.iter())
            .chain(FINAL_QUERY_INDEXES.iter())
            .copied()
            .collect();

        // Summary table indexes (idx_spr_*, idx_srs_*, idx_scas_*) are kept
        // during bulk write and are NOT in ALL_WRITE_INDEXES.
        let missing: Vec<_> = all.difference(&stage_indexes).copied().collect();
        assert!(
            missing.is_empty(),
            "ALL_WRITE_INDEXES has entries not covered by any stage group: {missing:?}"
        );
    }

    #[test]
    fn fts_triggers_has_three_entries() {
        assert_eq!(
            FTS_TRIGGERS.len(),
            3,
            "FTS_TRIGGERS must have exactly 3 entries (ai, ad, au)"
        );
        let set: HashSet<_> = FTS_TRIGGERS.iter().copied().collect();
        assert!(set.contains("symbols_ai"));
        assert!(set.contains("symbols_ad"));
        assert!(set.contains("symbols_au"));
    }

    #[test]
    fn execute_batch_ddl_zero_statements() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(execute_batch_ddl(&conn, &[]).is_ok());
    }

    #[test]
    fn execute_batch_ddl_one_statement() {
        let conn = Connection::open_in_memory().unwrap();
        let result = execute_batch_ddl(&conn, &["CREATE TABLE t1 (x INTEGER)".to_string()]);
        assert!(result.is_ok(), "expected ok, got {result:?}");
    }

    #[test]
    fn execute_batch_ddl_three_statements() {
        let conn = Connection::open_in_memory().unwrap();
        let sqls = [
            "CREATE TABLE t2 (x INTEGER)".to_string(),
            "INSERT INTO t2 VALUES (1)".to_string(),
            "INSERT INTO t2 VALUES (2)".to_string(),
        ];
        assert!(execute_batch_ddl(&conn, &sqls).is_ok());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM t2", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }
}
