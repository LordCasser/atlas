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
    pub fn batch_insert_candidate_edges(
        &self,
        edges: &[CandidateEdge],
    ) -> anyhow::Result<usize> {
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
