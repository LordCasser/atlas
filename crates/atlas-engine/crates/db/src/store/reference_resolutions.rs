//! Reference resolutions store — scoped reference resolution for focus closures.
//!
//! The `reference_resolutions` table records resolution results for references
//! within focus closures. Entries start as staged (is_visible=0) and are
//! atomically promoted to visible when the closure's resolution phase completes.

use super::Store;
use rusqlite::params;

/// A row from the reference_resolutions table.
#[derive(Debug, Clone)]
pub struct ReferenceResolution {
    pub id: i64,
    pub reference_id: Vec<u8>,
    pub closure_id: String,
    pub generation: i64,
    pub resolution_scope: String,
    pub target_symbol_id: Option<Vec<u8>>,
    pub coverage_tier: String,
    pub semantic_confidence: String,
    pub resolution_strategy: String,
    pub provenance: Option<String>,
    pub is_visible: bool,
    pub created_at: String,
}

/// Lightweight row for focus graph building — only the fields needed
/// to construct edges from reference_resolutions.
#[derive(Debug, Clone)]
pub struct ClosureResolution {
    pub reference_id: Vec<u8>,
    pub target_symbol_id: Vec<u8>,
    pub coverage_tier: String,
    pub semantic_confidence: String,
    pub resolution_strategy: String,
    pub resolution_scope: String,
    pub provenance: Option<String>,
}

impl Store {
    /// Insert a staged reference resolution (is_visible = 0).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_reference_resolution(
        &self,
        reference_id: &[u8],
        closure_id: &str,
        generation: i64,
        resolution_scope: &str,
        target_symbol_id: Option<&[u8]>,
        coverage_tier: &str,
        semantic_confidence: &str,
        resolution_strategy: &str,
        provenance: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO reference_resolutions
                (reference_id, closure_id, generation, resolution_scope,
                 target_symbol_id, coverage_tier, semantic_confidence,
                 resolution_strategy, provenance, is_visible)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
            params![
                reference_id,
                closure_id,
                generation,
                resolution_scope,
                target_symbol_id,
                coverage_tier,
                semantic_confidence,
                resolution_strategy,
                provenance,
            ],
        )?;
        Ok(())
    }

    /// Batch-insert staged reference resolutions in a single transaction.
    ///
    /// Parameters: `(closure_id, generation, reference_id_bytes, resolution_scope,
    /// target_symbol_id_bytes, coverage_tier, semantic_confidence, resolution_strategy,
    /// provenance)`
    ///
    /// All rows are inserted with `is_visible = 0` (staged). Call
    /// [`make_resolutions_visible`] to promote them to visible atomically.
    #[allow(clippy::type_complexity)]
    pub fn batch_insert_reference_resolutions(
        &self,
        resolutions: &[(
            String,
            i64,
            Vec<u8>,
            String,
            Option<Vec<u8>>,
            String,
            String,
            String,
            Option<String>,
        )],
    ) -> anyhow::Result<usize> {
        if resolutions.is_empty() {
            return Ok(0);
        }
        self.with_transaction(|tx| {
            let mut stmt = tx.prepare(
                "INSERT INTO reference_resolutions
                    (closure_id, generation, reference_id, resolution_scope,
                     target_symbol_id, coverage_tier, semantic_confidence,
                     resolution_strategy, provenance, is_visible)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
            )?;
            for (closure_id, generation, ref_id_bytes, scope, target_bytes, tier, confidence, strategy, provenance) in resolutions.iter() {
                stmt.execute(rusqlite::params![
                    closure_id,
                    generation,
                    ref_id_bytes,
                    scope,
                    target_bytes,
                    tier,
                    confidence,
                    strategy,
                    provenance,
                ])?;
            }
            let count = resolutions.len();
            Ok(count)
        })
    }

    /// Count all reference_resolutions for a closure+generation (including staged).
    /// Used for test verification.
    pub fn count_reference_resolutions(
        &self,
        closure_id: &str,
        generation: i64,
    ) -> anyhow::Result<i64> {
        let conn = self.lock_read();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM reference_resolutions WHERE closure_id = ?1 AND generation = ?2",
            rusqlite::params![closure_id, generation],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Make all staged resolutions for a closure+generation visible.
    /// Returns the number of rows updated.
    pub fn make_resolutions_visible(
        &self,
        closure_id: &str,
        generation: i64,
    ) -> anyhow::Result<usize> {
        let conn = self.lock();
        let updated = conn.execute(
            "UPDATE reference_resolutions
                SET is_visible = 1
             WHERE closure_id = ?1 AND generation = ?2 AND is_visible = 0",
            params![closure_id, generation],
        )?;
        Ok(updated)
    }

    /// Get all visible resolutions for a specific reference within a closure.
    pub fn get_visible_resolution(
        &self,
        reference_id: &[u8],
        closure_id: &str,
    ) -> anyhow::Result<Vec<ReferenceResolution>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT id, reference_id, closure_id, generation, resolution_scope,
                    target_symbol_id, coverage_tier, semantic_confidence,
                    resolution_strategy, provenance, is_visible, created_at
             FROM reference_resolutions
             WHERE reference_id = ?1 AND closure_id = ?2 AND is_visible = 1",
        )?;
        let rows =
            stmt.query_map(params![reference_id, closure_id], row_to_reference_resolution)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get ALL visible resolutions for a closure (every reference with a target).
    ///
    /// Returns only rows where `is_visible = 1` AND `target_symbol_id IS NOT NULL`.
    /// Used by [`FocusGraphBuilder`] to construct edges from closure resolutions.
    pub fn get_visible_resolutions_for_closure(
        &self,
        closure_id: &str,
    ) -> anyhow::Result<Vec<ClosureResolution>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT reference_id, target_symbol_id, coverage_tier, semantic_confidence,
                    resolution_strategy, resolution_scope, provenance
             FROM reference_resolutions
             WHERE closure_id = ?1 AND is_visible = 1 AND target_symbol_id IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![closure_id], |row| {
            Ok(ClosureResolution {
                reference_id: row.get(0)?,
                target_symbol_id: row.get(1)?,
                coverage_tier: row.get(2)?,
                semantic_confidence: row.get(3)?,
                resolution_strategy: row.get(4)?,
                resolution_scope: row.get(5)?,
                provenance: row.get::<_, Option<String>>(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get resolution counts grouped by resolution strategy for a closure.
    pub fn get_resolution_counts(
        &self,
        closure_id: &str,
    ) -> anyhow::Result<Vec<(String, i64)>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT resolution_strategy, COUNT(*) FROM reference_resolutions
             WHERE closure_id = ?1 AND is_visible = 1
             GROUP BY resolution_strategy",
        )?;
        let rows = stmt.query_map(params![closure_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

// ── Row mapping ─────────────────────────────────────────────────────────────

fn row_to_reference_resolution(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReferenceResolution> {
    Ok(ReferenceResolution {
        id: row.get(0)?,
        reference_id: row.get(1)?,
        closure_id: row.get(2)?,
        generation: row.get(3)?,
        resolution_scope: row.get(4)?,
        target_symbol_id: row.get(5)?,
        coverage_tier: row.get(6)?,
        semantic_confidence: row.get(7)?,
        resolution_strategy: row.get(8)?,
        provenance: row.get(9)?,
        is_visible: row.get::<_, i64>(10)? != 0,
        created_at: row.get(11)?,
    })
}
