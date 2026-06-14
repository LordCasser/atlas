//! Symbol edge candidates store — staged candidate edges for focus closures.
//!
//! The `symbol_edge_candidates` table stores Medium/Low confidence edges
//! produced by focus closure graph building.  Entries start as staged
//! (is_visible=0) and are promoted to visible when the closure build completes.

use super::Store;
use rusqlite::params;

/// A candidate edge row for batch insertion.
#[derive(Debug, Clone)]
pub struct CandidateEdge {
    pub source: Vec<u8>,
    pub target: Option<Vec<u8>>,
    pub kind: String,
    pub coverage_tier: String,
    pub semantic_confidence: String,
    pub candidate_count: Option<i64>,
    pub closure_id: String,
    pub generation: i64,
}

impl Store {
    /// Batch-insert candidate edges (staged, is_visible = 0).
    ///
    /// All rows are inserted in a single transaction.  Call
    /// [`make_candidate_edges_visible`] to promote them after the build
    /// completes.
    pub fn batch_insert_candidate_edges(&self, edges: &[CandidateEdge]) -> anyhow::Result<usize> {
        if edges.is_empty() {
            return Ok(0);
        }
        self.with_transaction(|tx| {
            let mut stmt = tx.prepare(
                "INSERT INTO symbol_edge_candidates
                    (source, target, kind, coverage_tier, semantic_confidence,
                     candidate_count, closure_id, generation, is_visible)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            )?;
            for e in edges {
                stmt.execute(params![
                    e.source,
                    e.target,
                    e.kind,
                    e.coverage_tier,
                    e.semantic_confidence,
                    e.candidate_count,
                    e.closure_id,
                    e.generation,
                ])?;
            }
            Ok(edges.len())
        })
    }

    /// Find visible candidate edges originating from a source symbol.
    ///
    /// Queries `symbol_edge_candidates` for rows where `source = ?1`
    /// and `is_visible = 1`.  Used by CallGraph closure expansion to
    /// discover Medium/Low confidence edges alongside canonical edges.
    pub fn find_visible_candidate_edges_by_source(
        &self,
        source_id: &types::ids::SymbolId,
    ) -> anyhow::Result<Vec<CandidateEdge>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT source, target, kind, coverage_tier, semantic_confidence,
                    candidate_count, closure_id, generation
             FROM symbol_edge_candidates
             WHERE source = ?1 AND is_visible = 1",
        )?;
        let rows = stmt.query_map(params![source_id], |row| {
            Ok(CandidateEdge {
                source: row.get(0)?,
                target: row.get(1)?,
                kind: row.get(2)?,
                coverage_tier: row.get(3)?,
                semantic_confidence: row.get(4)?,
                candidate_count: row.get(5)?,
                closure_id: row.get(6)?,
                generation: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find visible candidate edges targeting a symbol.
    ///
    /// Queries `symbol_edge_candidates` for rows where `target = ?1`
    /// and `is_visible = 1`.  Used by CallGraph closure expansion in
    /// `Direction::Incoming` to discover Medium/Low confidence caller
    /// edges alongside canonical incoming edges.
    pub fn find_visible_candidate_edges_by_target(
        &self,
        target_symbol_id: &types::ids::SymbolId,
    ) -> anyhow::Result<Vec<CandidateEdge>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT source, target, kind, coverage_tier, semantic_confidence,
                    candidate_count, closure_id, generation
             FROM symbol_edge_candidates
             WHERE target = ?1 AND is_visible = 1",
        )?;
        let rows = stmt.query_map(params![target_symbol_id], |row| {
            Ok(CandidateEdge {
                source: row.get(0)?,
                target: row.get(1)?,
                kind: row.get(2)?,
                coverage_tier: row.get(3)?,
                semantic_confidence: row.get(4)?,
                candidate_count: row.get(5)?,
                closure_id: row.get(6)?,
                generation: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Make all staged candidate edges for a closure+generation visible.
    /// Returns the number of rows updated.
    pub fn make_candidate_edges_visible(
        &self,
        closure_id: &str,
        generation: i64,
    ) -> anyhow::Result<usize> {
        let conn = self.lock();
        let updated = conn.execute(
            "UPDATE symbol_edge_candidates
                SET is_visible = 1
             WHERE closure_id = ?1 AND generation = ?2 AND is_visible = 0",
            params![closure_id, generation],
        )?;
        Ok(updated)
    }
}
