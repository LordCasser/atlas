//! References, edges, callsites, and invalidation.

use atlas_types::*;
use rusqlite::params;

use super::Store;
use crate::store_rows::{
    REFERENCE_SELECT_NO_WHERE, REFERENCE_SELECT_WHERE, row_to_callsite, row_to_edge,
    row_to_reference,
};
use crate::store_writers::{write_callsites, write_edges, write_references};

impl Store {
    // ── References ──────────────────────────────────────────────────────────

    /// Batch-insert references inside a transaction.
    pub fn insert_references(&self, refs: &[ReferenceUse]) -> anyhow::Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_references(tx, refs))
    }

    /// Find all references belonging to a file.
    pub fn find_references_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<ReferenceUse>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(REFERENCE_SELECT_WHERE)?;
        let rows = stmt.query_map(params![file_id], row_to_reference)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find unresolved references (no resolved target).
    pub fn find_unresolved_references(&self) -> anyhow::Result<Vec<ReferenceUse>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{} WHERE resolved_symbol_id IS NULL",
            REFERENCE_SELECT_NO_WHERE
        ))?;
        let rows = stmt.query_map([], row_to_reference)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Update the resolved target of a specific reference.
    pub fn update_reference_resolution(
        &self,
        reference_id: &ReferenceId,
        target: &ResolvedTarget,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE \"references\" SET
                resolved_symbol_id = ?2,
                resolved_confidence = ?3,
                resolved_strategy = ?4,
                resolved_provenance = ?5
             WHERE reference_id = ?1",
            params![
                reference_id,
                target.symbol_id,
                target.confidence.as_f32(),
                target.strategy.as_str(),
                target.provenance.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Batch-update resolved targets for multiple references in a single transaction.
    ///
    /// This is significantly faster than calling `update_reference_resolution` per-reference
    /// because it amortizes the transaction overhead.
    pub fn batch_update_resolutions(
        &self,
        resolutions: &[(ReferenceId, ResolvedTarget)],
    ) -> anyhow::Result<()> {
        if resolutions.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| {
            let mut stmt = tx.prepare(
                "UPDATE \"references\" SET
                    resolved_symbol_id = ?2,
                    resolved_confidence = ?3,
                    resolved_strategy = ?4,
                    resolved_provenance = ?5
                 WHERE reference_id = ?1",
            )?;
            for (ref_id, target) in resolutions {
                stmt.execute(params![
                    ref_id,
                    target.symbol_id,
                    target.confidence.as_f32(),
                    target.strategy.as_str(),
                    target.provenance.as_str(),
                ])?;
            }
            Ok(())
        })
    }

    // ── Resolved fact invalidation (P2) ────────────────────────────────────

    /// Clear all resolution results for references belonging to a file.
    ///
    /// This is called when a file is modified — the references themselves
    /// remain (they are never deleted), but their resolved targets become
    /// stale and must be re-computed.
    ///
    /// Returns the number of references that were invalidated.
    pub fn invalidate_references_for_file(&self, file_id: &FileId) -> anyhow::Result<usize> {
        let conn = self.lock();
        let count = conn.execute(
            r#"UPDATE "references" SET
                resolved_symbol_id = NULL,
                resolved_confidence = NULL,
                resolved_strategy = NULL,
                resolved_provenance = NULL
               WHERE file_id = ?1 AND resolved_symbol_id IS NOT NULL"#,
            params![file_id],
        )?;
        Ok(count)
    }

    /// Delete all edges that were created from references belonging to a file.
    ///
    /// When a file is modified, the edges derived from its references become
    /// invalid. This deletes edges whose `ref_id` points to a reference in
    /// the given file.
    ///
    /// Returns the number of edges deleted.
    pub fn delete_edges_for_file_references(&self, file_id: &FileId) -> anyhow::Result<usize> {
        let conn = self.lock();
        // Find all reference IDs belonging to this file, then delete edges
        // whose ref_id matches any of them.
        let count = conn.execute(
            r#"DELETE FROM symbol_edges WHERE ref_id IN (
                SELECT reference_id FROM "references" WHERE file_id = ?1
            )"#,
            params![file_id],
        )?;
        Ok(count)
    }

    /// Invalidate ALL resolved references (clear resolution columns).
    ///
    /// Used when project-level configuration (e.g. tsconfig.json) changes,
    /// which can affect import resolution across all files.
    ///
    /// Returns the number of references invalidated.
    pub fn invalidate_all_references(&self) -> anyhow::Result<usize> {
        let conn = self.lock();
        let count = conn.execute(
            r#"UPDATE "references" SET
                resolved_symbol_id = NULL,
                resolved_confidence = NULL,
                resolved_strategy = NULL,
                resolved_provenance = NULL
             WHERE resolved_symbol_id IS NOT NULL"#,
            [],
        )?;
        Ok(count)
    }

    /// Delete ALL edges from the symbol graph.
    ///
    /// Used together with `invalidate_all_references` when project configuration
    /// changes require a full re-resolution and edge rebuild.
    ///
    /// Returns the number of edges deleted.
    pub fn delete_all_edges(&self) -> anyhow::Result<usize> {
        let conn = self.lock();
        let count = conn.execute("DELETE FROM symbol_edges", [])?;
        Ok(count)
    }

    // ── Edges ───────────────────────────────────────────────────────────────

    /// Batch-insert edges inside a transaction.
    pub fn insert_edges(&self, edges: &[RawEdge]) -> anyhow::Result<()> {
        if edges.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_edges(tx, edges))
    }

    /// Batch-insert edges inside a transaction (re-export with explicit name).
    ///
    /// This is the same as `insert_edges` but named for clarity in the
    /// resolution pipeline where we accumulate edges and flush them in batches.
    pub fn batch_insert_edges(&self, edges: &[RawEdge]) -> anyhow::Result<()> {
        self.insert_edges(edges)
    }

    /// Find edges originating from a symbol.
    pub fn find_edges_by_source(&self, source: &SymbolId) -> anyhow::Result<Vec<RawEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance,
                    ref_id, location_0, location_1, location_2, location_3, location_4, location_5,
                    metadata, resolved_by
             FROM symbol_edges WHERE source = ?1",
        )?;
        let rows = stmt.query_map(params![source], row_to_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find edges targeting a symbol.
    pub fn find_edges_by_target(&self, target: &SymbolId) -> anyhow::Result<Vec<RawEdge>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance,
                    ref_id, location_0, location_1, location_2, location_3, location_4, location_5,
                    metadata, resolved_by
             FROM symbol_edges WHERE target = ?1",
        )?;
        let rows = stmt.query_map(params![target], row_to_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Load ALL edges (for GraphSnapshot construction).
    /// Uses the shared connection via the mutex; long-running reads may
    //  block writes.  In the future this should use a separate read connection.
    pub fn get_all_edges(&self) -> anyhow::Result<Vec<RawEdge>> {
        let guard = self.lock();
        let conn: &rusqlite::Connection = &guard;
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance,
                    ref_id, location_0, location_1, location_2, location_3, location_4, location_5,
                    metadata, resolved_by FROM symbol_edges",
        )?;
        let rows = stmt.query_map([], row_to_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ── Callsites ───────────────────────────────────────────────────────────

    /// Batch-insert callsites inside a transaction.
    pub fn insert_callsites(&self, callsites: &[Callsite]) -> anyhow::Result<()> {
        if callsites.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_callsites(tx, callsites))
    }

    /// Find all callsites in a file.
    pub fn find_callsites_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<Callsite>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, callee, receiver, args_json,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    callee_start_line, callee_start_column, callee_end_line, callee_end_column,
                    callee_start_byte, callee_end_byte
             FROM callsites WHERE EXISTS (
                 SELECT 1 FROM symbols WHERE symbols.symbol_id = callsites.caller AND symbols.file_id = ?1
             )",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_callsite)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find all callsites that target a specific callee symbol.
    ///
    /// Used by summary-bridge trace to find callers of a function.
    pub fn find_callsites_by_callee(&self, callee: &SymbolId) -> anyhow::Result<Vec<Callsite>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, callee, receiver, args_json,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    callee_start_line, callee_start_column, callee_end_line, callee_end_column,
                    callee_start_byte, callee_end_byte
             FROM callsites WHERE callee = ?1",
        )?;
        let rows = stmt.query_map(params![callee], row_to_callsite)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find a single callsite by its ID.
    pub fn find_callsites_by_id(&self, callsite_id: &CallsiteId) -> anyhow::Result<Vec<Callsite>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, callee, receiver, args_json,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    callee_start_line, callee_start_column, callee_end_line, callee_end_column,
                    callee_start_byte, callee_end_byte
             FROM callsites WHERE callsite_id = ?1",
        )?;
        let rows = stmt.query_map(params![callsite_id], row_to_callsite)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find a callsite by its originating reference ID.
    pub fn find_callsite_by_reference_id(
        &self,
        ref_id: &ReferenceId,
    ) -> anyhow::Result<Option<Callsite>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, callee, receiver, args_json,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    callee_start_line, callee_start_column, callee_end_line, callee_end_column,
                    callee_start_byte, callee_end_byte
             FROM callsites WHERE reference_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![ref_id], row_to_callsite)?;
        match rows.next() {
            Some(Ok(cs)) => Ok(Some(cs)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Update the `callee` field of the callsite linked to a given reference.
    ///
    /// Called by GraphBuilder when a Calls/Instantiates edge is resolved,
    /// linking the callsite to the resolved target symbol.
    pub fn update_callsite_callee(
        &self,
        ref_id: &ReferenceId,
        callee: &SymbolId,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE callsites SET callee = ?1 WHERE reference_id = ?2",
            params![callee, ref_id],
        )?;
        Ok(())
    }
}
