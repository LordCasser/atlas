//! References, edges, callsites, and invalidation.

use rusqlite::params;
use types::*;

use super::Store;
use super::files::{normalize_scope, scope_child_bounds};
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
        let conn = self.lock_read();
        let mut stmt = conn.prepare(REFERENCE_SELECT_WHERE)?;
        let rows = stmt.query_map(params![file_id], row_to_reference)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find references in a file filtered by kind.
    ///
    /// Used by TypeGraph strategy to discover type-related references
    /// (Usage, Inheritance, Implementation) from closure files.
    pub fn find_references_by_file_and_kinds(
        &self,
        file_id: &FileId,
        kinds: &[ReferenceKind],
    ) -> anyhow::Result<Vec<ReferenceUse>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock_read();
        let placeholders: Vec<String> = kinds.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "{REFERENCE_SELECT_NO_WHERE} WHERE file_id = ?1 AND kind IN ({})",
            placeholders.join(",")
        );
        let kind_strs: Vec<String> = kinds.iter().map(|k| k.as_str().to_string()).collect();
        let mut params: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(1 + kinds.len());
        params.push(file_id as &dyn rusqlite::types::ToSql);
        for k in &kind_strs {
            params.push(k as &dyn rusqlite::types::ToSql);
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), row_to_reference)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find unresolved references (no resolved target).
    pub fn find_unresolved_references(&self) -> anyhow::Result<Vec<ReferenceUse>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(&format!(
            "{REFERENCE_SELECT_NO_WHERE} WHERE resolved_symbol_id IS NULL"
        ))?;
        let rows = stmt.query_map([], row_to_reference)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Count unresolved references with an index-only scan over the partial
    /// index `idx_references_unresolved`, without materializing reference rows.
    pub fn count_unresolved_references(&self) -> anyhow::Result<u64> {
        let conn = self.lock_read();
        let mut stmt =
            conn.prepare("SELECT COUNT(*) FROM \"references\" WHERE resolved_symbol_id IS NULL")?;
        stmt.query_row([], |row| row.get::<_, i64>(0))
            .map(|c| c as u64)
            .map_err(Into::into)
    }

    /// Find unresolved call references inside a source symbol.
    ///
    /// These are calls whose callee token was extracted but could not be
    /// resolved to a local symbol. They are useful for C/C++ kernel-style
    /// helper, macro, and externally-defined sink names such as
    /// `copy_from_user`.
    pub fn find_unresolved_call_references_by_source(
        &self,
        source_symbol: &SymbolId,
    ) -> anyhow::Result<Vec<ReferenceUse>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(&format!(
            "{REFERENCE_SELECT_NO_WHERE} \
             WHERE source_symbol = ?1 AND kind = ?2 AND resolved_symbol_id IS NULL \
               AND NOT EXISTS ( \
                   SELECT 1 FROM reference_resolutions rr \
                   WHERE rr.reference_id = \"references\".reference_id \
                     AND rr.is_visible = 1 \
                     AND rr.target_symbol_id IS NOT NULL \
               )"
        ))?;
        let rows = stmt.query_map(
            params![source_symbol, ReferenceKind::Call.as_str()],
            row_to_reference,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Return the newest visible closure-scoped target for a reference.
    pub fn find_latest_visible_reference_target(
        &self,
        reference_id: &ReferenceId,
    ) -> anyhow::Result<Option<SymbolId>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT target_symbol_id
             FROM reference_resolutions
             WHERE reference_id = ?1
               AND is_visible = 1
               AND target_symbol_id IS NOT NULL
             ORDER BY id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![reference_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let bytes: Vec<u8> = row.get(0)?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
            anyhow::anyhow!("invalid target_symbol_id length in reference_resolutions")
        })?;
        Ok(Some(SymbolId::from_bytes(bytes)))
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

    /// Clear resolved targets on OTHER files' references that point to
    /// symbols in this file.  Called before deleting a file's symbols so that
    /// cross-file references become unresolved instead of dangling.
    pub fn invalidate_references_to_symbols_in_file(
        &self,
        file_id: &FileId,
    ) -> anyhow::Result<usize> {
        self.with_transaction(|tx| {
            // The importing file did not change, but its resolution context
            // did. Clear its fingerprint before nulling the target so a later
            // Index cannot mistake it for a clean canonical resolution.
            tx.execute(
                r#"UPDATE extraction_state SET resolution_fingerprint = NULL
                   WHERE layer = 'resolution' AND unit_id IS NULL
                     AND file_id IN (
                       SELECT DISTINCT r.file_id FROM "references" r
                       WHERE r.resolved_symbol_id IN (
                           SELECT symbol_id FROM symbols WHERE file_id = ?1
                       )
                     )"#,
                params![file_id],
            )?;
            // Find all symbol IDs belonging to this file, then clear any
            // reference in ANY file whose resolved_symbol_id matches.
            let count = tx.execute(
                r#"UPDATE "references" SET
                    resolved_symbol_id = NULL,
                    resolved_confidence = NULL,
                    resolved_strategy = NULL,
                    resolved_provenance = NULL
                   WHERE resolved_symbol_id IN (
                       SELECT symbol_id FROM symbols WHERE file_id = ?1
                   )"#,
                params![file_id],
            )?;
            Ok(count)
        })
    }

    /// Delete all edges that were created from references belonging to a file.
    ///
    /// When a file is modified, the edges derived from its references become
    /// invalid.  This deletes edges whose `ref_id` points to a reference in
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

    /// Delete previously materialized focus edges for references resolved by
    /// this closure. A later closure may select a better target for the same
    /// reference; retaining the older edge would expose both as canonical.
    pub fn delete_superseded_focus_edges(&self, closure_id: &str) -> anyhow::Result<usize> {
        let conn = self.lock();
        let count = conn.execute(
            r#"WITH latest AS (
                   SELECT reference_id, MAX(id) AS id
                   FROM reference_resolutions
                   WHERE closure_id = ?1
                     AND is_visible = 1
                     AND target_symbol_id IS NOT NULL
                   GROUP BY reference_id
               )
               DELETE FROM symbol_edges
               WHERE provenance = 'focus_closure'
                 AND ref_id IN (SELECT reference_id FROM latest)
                 AND NOT EXISTS (
                     SELECT 1
                     FROM latest
                     JOIN reference_resolutions current ON current.id = latest.id
                     WHERE latest.reference_id = symbol_edges.ref_id
                       AND current.target_symbol_id = symbol_edges.target
                 )"#,
            params![closure_id],
        )?;
        Ok(count)
    }

    /// Atomically clean stale file facts: invalidate cross-file references,
    /// delete outgoing edges, delete incoming edges targeting this file's
    /// symbols, delete file records, and delete extraction state
    /// — all within a single transaction.
    ///
    /// This replaces the per-file, per-operation pattern that previously
    /// required 4N+1 individual transactions for N stale files.
    pub fn clean_stale_file_facts(&self, file_ids: &[FileId]) -> anyhow::Result<()> {
        if file_ids.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| {
            for fid in file_ids {
                // P3: Clear resolution fingerprints for files whose references
                // are resolved to symbols in this deleted file.  The references
                // themselves are nullified below — but if we don't clear the
                // fingerprint, the owning files will enter the clean-file fast
                // path (resolve_global_only) on the next resolution run and
                // may match a wrong symbol via global name search (strategy 6).
                //
                // Clearing the fingerprint forces full-resolution (strategies
                // 1–6 with import/scope context) so the stale cross-file
                // dependency is handled correctly.
                tx.execute(
                    r#"UPDATE extraction_state SET resolution_fingerprint = NULL
                       WHERE layer = 'resolution' AND unit_id IS NULL
                         AND file_id IN (
                           SELECT DISTINCT r.file_id FROM "references" r
                           WHERE r.resolved_symbol_id IN (
                               SELECT symbol_id FROM symbols WHERE file_id = ?1
                           )
                       )"#,
                    params![fid],
                )?;
                tx.execute(
                    r#"UPDATE "references" SET
                        resolved_symbol_id = NULL,
                        resolved_confidence = NULL,
                        resolved_strategy = NULL,
                        resolved_provenance = NULL
                       WHERE resolved_symbol_id IN (
                           SELECT symbol_id FROM symbols WHERE file_id = ?1
                       )"#,
                    params![fid],
                )?;
                tx.execute(
                    r#"DELETE FROM symbol_edges WHERE ref_id IN (
                        SELECT reference_id FROM "references" WHERE file_id = ?1
                    )"#,
                    params![fid],
                )?;
                // Delete incoming edges that target symbols belonging to this
                // file. The target column has no FK (schema allows external /
                // not-yet-indexed targets), so CASCADE from files→symbols does
                // not reach these rows.
                tx.execute(
                    r#"DELETE FROM symbol_edges WHERE target IN (
                        SELECT symbol_id FROM symbols WHERE file_id = ?1
                    )"#,
                    params![fid],
                )?;
            }
            for fid in file_ids {
                tx.execute("DELETE FROM files WHERE file_id = ?1", params![fid])?;
            }
            for fid in file_ids {
                tx.execute(
                    "DELETE FROM extraction_state WHERE file_id = ?1 AND unit_id IS NULL",
                    params![fid],
                )?;
            }
            Ok(())
        })
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

    /// Delete edges by provenance string (e.g., "user_annotation").
    ///
    /// Used by `materialize_annotations` to clean stale annotation edges
    /// before re-materializing. Returns the number of edges deleted.
    pub fn delete_edges_by_provenance(&self, provenance: &str) -> anyhow::Result<usize> {
        let conn = self.lock();
        let count = conn.execute(
            "DELETE FROM symbol_edges WHERE provenance = ?1",
            rusqlite::params![provenance],
        )?;
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

    /// Find edges originating from a symbol.
    pub fn find_edges_by_source(&self, source: &SymbolId) -> anyhow::Result<Vec<RawEdge>> {
        let conn = self.lock_read();
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
        let conn = self.lock_read();
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
        let guard = self.lock_read();
        let conn: &rusqlite::Connection = &guard;
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance,
                    ref_id, location_0, location_1, location_2, location_3, location_4, location_5,
                    metadata, resolved_by FROM symbol_edges",
        )?;
        let rows = stmt.query_map([], row_to_edge)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find symbol edges whose source or target is in any of the given files.
    ///
    /// Uses a subquery to find symbol_ids belonging to `file_ids`, then
    /// selects edges where source or target matches.  Used by delta graph
    /// refresh to scope edge loading to only the files affected by lazy
    /// structural extraction.
    pub fn find_edges_for_files(&self, file_ids: &[FileId]) -> anyhow::Result<Vec<RawEdge>> {
        if file_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock_read();
        let file_placeholders: Vec<String> = file_ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT e.edge_id, e.source, e.target, e.kind, e.confidence, e.provenance,
                    e.ref_id, e.location_0, e.location_1, e.location_2,
                    e.location_3, e.location_4, e.location_5,
                    e.metadata, e.resolved_by
             FROM symbol_edges e
             WHERE e.source IN (SELECT symbol_id FROM symbols WHERE file_id IN ({}))
                OR e.target IN (SELECT symbol_id FROM symbols WHERE file_id IN ({}))
             ORDER BY e.kind",
            file_placeholders.join(","),
            file_placeholders.join(","),
        );
        let mut stmt = conn.prepare(&sql)?;
        // Both IN clauses use same file_ids — bind twice
        let mut params: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(file_ids.len() * 2);
        for fid in file_ids {
            params.push(fid as &dyn rusqlite::types::ToSql);
        }
        for fid in file_ids {
            params.push(fid as &dyn rusqlite::types::ToSql);
        }
        let rows = stmt.query_map(params.as_slice(), row_to_edge)?;
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
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, receiver, args_json,
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

    /// Find callsites by unresolved/resolved callee name and receiver text.
    ///
    /// This is a syntactic lookup: it intentionally does not require symbol
    /// resolution, which makes it suitable for framework APIs defined outside
    /// the indexed project.
    pub fn find_callsites_by_name_and_receiver(
        &self,
        name: &str,
        receiver: &str,
        language: Language,
    ) -> anyhow::Result<Vec<Callsite>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT cs.callsite_id, cs.reference_id, cs.caller, cs.receiver, cs.args_json,
                    cs.range_start_byte, cs.range_end_byte, cs.range_start_line, cs.range_start_column,
                    cs.range_end_line, cs.range_end_column,
                    cs.callee_start_line, cs.callee_start_column, cs.callee_end_line, cs.callee_end_column,
                    cs.callee_start_byte, cs.callee_end_byte
             FROM callsites cs
             JOIN \"references\" r ON r.reference_id = cs.reference_id
             JOIN files f ON f.file_id = r.file_id
             WHERE r.name = ?1 AND cs.receiver = ?2 AND f.language = ?3
             ORDER BY r.file_id, cs.range_start_byte",
        )?;
        let rows = stmt.query_map(params![name, receiver, language.as_str()], row_to_callsite)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find all callsites whose resolved callee is the given symbol.
    ///
    /// JOINs `callsites` with `references` to filter by `resolved_symbol_id`.
    pub fn find_resolved_callsites_by_callee(
        &self,
        callee: &SymbolId,
    ) -> anyhow::Result<Vec<ResolvedCallsite>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT cs.callsite_id, cs.reference_id, cs.caller, cs.receiver, cs.args_json,
                    cs.range_start_byte, cs.range_end_byte, cs.range_start_line, cs.range_start_column,
                    cs.range_end_line, cs.range_end_column,
                    cs.callee_start_line, cs.callee_start_column, cs.callee_end_line, cs.callee_end_column,
                    cs.callee_start_byte, cs.callee_end_byte,
                    r.resolved_symbol_id
             FROM callsites cs
             JOIN \"references\" r ON r.reference_id = cs.reference_id
             WHERE r.resolved_symbol_id = ?1",
        )?;
        let rows = stmt.query_map(params![callee], |row| {
            let cs = row_to_callsite(row)?;
            let callee: SymbolId = row.get(17)?;
            Ok(ResolvedCallsite {
                callsite: cs,
                callee,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find a single callsite by its ID.
    pub fn find_callsites_by_id(&self, callsite_id: &CallsiteId) -> anyhow::Result<Vec<Callsite>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, receiver, args_json,
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
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT callsite_id, reference_id, caller, receiver, args_json,
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

    /// Find all callsites with resolved callee, filtered by callsite ID.
    pub fn find_resolved_callsites_by_id(
        &self,
        callsite_id: &CallsiteId,
    ) -> anyhow::Result<Vec<ResolvedCallsite>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT cs.callsite_id, cs.reference_id, cs.caller, cs.receiver, cs.args_json,
                    cs.range_start_byte, cs.range_end_byte, cs.range_start_line, cs.range_start_column,
                    cs.range_end_line, cs.range_end_column,
                    cs.callee_start_line, cs.callee_start_column, cs.callee_end_line, cs.callee_end_column,
                    cs.callee_start_byte, cs.callee_end_byte,
                    r.resolved_symbol_id
             FROM callsites cs
             JOIN \"references\" r ON r.reference_id = cs.reference_id
             WHERE cs.callsite_id = ?1",
        )?;
        let rows = stmt.query_map(params![callsite_id], |row| {
            let cs = row_to_callsite(row)?;
            let callee: SymbolId = row.get(17)?;
            Ok(ResolvedCallsite {
                callsite: cs,
                callee,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find a single callsite with resolved callee, by reference ID.
    pub fn find_resolved_callsite_by_reference_id(
        &self,
        ref_id: &ReferenceId,
    ) -> anyhow::Result<Option<ResolvedCallsite>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT cs.callsite_id, cs.reference_id, cs.caller, cs.receiver, cs.args_json,
                    cs.range_start_byte, cs.range_end_byte, cs.range_start_line, cs.range_start_column,
                    cs.range_end_line, cs.range_end_column,
                    cs.callee_start_line, cs.callee_start_column, cs.callee_end_line, cs.callee_end_column,
                    cs.callee_start_byte, cs.callee_end_byte,
                    r.resolved_symbol_id
             FROM callsites cs
             JOIN \"references\" r ON r.reference_id = cs.reference_id
             WHERE cs.reference_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![ref_id], |row| {
            let cs = row_to_callsite(row)?;
            let callee: SymbolId = row.get(17)?;
            Ok(ResolvedCallsite {
                callsite: cs,
                callee,
            })
        })?;
        match rows.next() {
            Some(Ok(cs)) => Ok(Some(cs)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Find references by name and kind within a project-relative scope.
    ///
    /// Empty scope searches the whole project. Non-empty directory and file
    /// scopes use the same exact-or-descendant bounds as symbol search.
    pub fn find_references_by_name_and_kind_in_scope(
        &self,
        name: &str,
        kind: ReferenceKind,
        scope: &str,
    ) -> anyhow::Result<Vec<ReferenceUse>> {
        let scope = normalize_scope(scope);
        let conn = self.lock_read();
        let sql = if scope.is_empty() {
            format!("{REFERENCE_SELECT_NO_WHERE} WHERE name = ?1 AND kind = ?2")
        } else {
            format!(
                "{REFERENCE_SELECT_NO_WHERE}
                 WHERE name = ?1 AND kind = ?2
                   AND file_id IN (
                       SELECT file_id FROM files
                       WHERE path = ?3 OR (path >= ?4 AND path < ?5)
                   )"
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = if scope.is_empty() {
            stmt.query_map(params![name, kind.as_str()], row_to_reference)?
        } else {
            let (lower, upper) = scope_child_bounds(&scope);
            stmt.query_map(
                params![name, kind.as_str(), scope, lower, upper],
                row_to_reference,
            )?
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find all references that currently resolve to a given symbol.
    ///
    /// A full index stores the canonical target on `references`. Focus keeps
    /// closure-local targets in `reference_resolutions`; when no canonical
    /// target exists, the newest visible Focus target is the current answer.
    pub fn find_references_by_symbol(
        &self,
        symbol_id: &SymbolId,
    ) -> anyhow::Result<Vec<ReferenceUse>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(&format!(
            "{REFERENCE_SELECT_NO_WHERE}
             WHERE resolved_symbol_id = ?1
                OR (
                    resolved_symbol_id IS NULL
                    AND EXISTS (
                        SELECT 1
                        FROM reference_resolutions current
                        WHERE current.reference_id = \"references\".reference_id
                          AND current.is_visible = 1
                          AND current.target_symbol_id = ?1
                          AND current.id = (
                              SELECT MAX(latest.id)
                              FROM reference_resolutions latest
                              WHERE latest.reference_id = \"references\".reference_id
                                AND latest.is_visible = 1
                          )
                    )
                )"
        ))?;
        let rows = stmt.query_map(params![symbol_id], row_to_reference)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Look up a single reference by its ID.
    pub fn get_reference_by_id(&self, reference_id: &[u8]) -> anyhow::Result<Option<ReferenceUse>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(&format!(
            "{REFERENCE_SELECT_NO_WHERE} WHERE reference_id = ?1"
        ))?;
        let mut rows = stmt.query_map(params![reference_id], row_to_reference)?;
        match rows.next() {
            Some(Ok(r)) => Ok(Some(r)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Find a specific edge by (source, target, kind).  Returns the first match
    /// if any exists; used for conflict detection during focus graph building.
    pub fn find_edge_by_source_target_kind(
        &self,
        source: &SymbolId,
        target: &SymbolId,
        kind: &EdgeKind,
    ) -> anyhow::Result<Option<RawEdge>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT edge_id, source, target, kind, confidence, provenance,
                    ref_id, location_0, location_1, location_2, location_3, location_4, location_5,
                    metadata, resolved_by
             FROM symbol_edges
             WHERE source = ?1 AND target = ?2 AND kind = ?3
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![source, target, kind.as_str()], row_to_edge)?;
        match rows.next() {
            Some(Ok(e)) => Ok(Some(e)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }
}
