//! Focus closure generation tracking — query and update closure lifecycle.
//!
//! The `closure_generations` table records the lifecycle of focus closures
//! (building → committed → stale). The `committed_generation` counter gates
//! visibility for MCP queries: only data at or below the committed generation
//! is visible.

use std::collections::HashMap;

use super::Store;
use rusqlite::params;

/// A row from the closure_generations table.
#[derive(Debug, Clone)]
pub struct ClosureGeneration {
    pub closure_id: String,
    pub committed_generation: i64,
    pub state: String,
    pub committed_at: Option<String>,
    pub created_at: String,
}

impl Store {
    /// Clear focus control-plane facts from an earlier MCP session.
    /// Query snapshots and in-flight jobs are process-local, while canonical
    /// graph/source facts have already been materialized elsewhere.
    pub fn reset_focus_session_state(&self) -> anyhow::Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM symbol_edge_candidates", [])?;
        tx.execute("DELETE FROM reference_resolutions", [])?;
        tx.execute("DELETE FROM closure_coverage", [])?;
        tx.execute("DELETE FROM closure_generations", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Insert a new closure generation in 'building' state.
    pub fn insert_closure_generation(&self, closure_id: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR IGNORE INTO closure_generations (closure_id) VALUES (?1)",
            params![closure_id],
        )?;
        Ok(())
    }

    /// Atomically commit a closure generation — sets state='committed' and
    /// increments committed_generation. Returns the new generation number.
    pub fn commit_closure_generation(&self, closure_id: &str) -> anyhow::Result<i64> {
        let conn = self.lock();
        let generation: i64 = conn.query_row(
            "UPDATE closure_generations
                SET committed_generation = committed_generation + 1,
                    state = 'committed',
                    committed_at = datetime('now')
             WHERE closure_id = ?1
             RETURNING committed_generation",
            params![closure_id],
            |row| row.get(0),
        )?;
        Ok(generation)
    }

    /// Mark a closure as stale.
    pub fn mark_closure_stale(&self, closure_id: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE closure_generations SET state = 'stale' WHERE closure_id = ?1",
            params![closure_id],
        )?;
        Ok(())
    }

    /// Get the committed generation for a closure.
    /// Returns `None` if the closure doesn't exist or isn't committed.
    pub fn get_committed_generation(&self, closure_id: &str) -> anyhow::Result<Option<i64>> {
        let conn = self.lock_read();
        let result = conn.query_row(
            "SELECT committed_generation FROM closure_generations
             WHERE closure_id = ?1 AND state = 'committed'",
            params![closure_id],
            |row| row.get(0),
        );
        match result {
            Ok(generation) => Ok(Some(generation)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get a closure generation by ID.
    pub fn get_closure_generation(
        &self,
        closure_id: &str,
    ) -> anyhow::Result<Option<ClosureGeneration>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT closure_id, committed_generation, state, committed_at, created_at
             FROM closure_generations
             WHERE closure_id = ?1",
        )?;
        let result = stmt
            .query_row(params![closure_id], row_to_closure_generation)
            .map_err(|e| {
                tracing::warn!(
                    ?e,
                    %closure_id,
                    "Failed to query closure generation"
                );
                e
            })
            .ok();
        Ok(result)
    }

    /// Count closures by state.
    ///
    /// Returns a map of state → count (e.g. `{"building": 3, "committed": 12, "stale": 0}`).
    pub fn get_closure_counts(&self) -> anyhow::Result<HashMap<String, usize>> {
        let conn = self.lock_read();
        let mut stmt =
            conn.prepare("SELECT state, COUNT(*) FROM closure_generations GROUP BY state")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = HashMap::new();
        for row in rows {
            let (state, count) = row?;
            counts.insert(state, count as usize);
        }
        Ok(counts)
    }

    /// Retain only the newest committed focus closures and atomically remove
    /// their transient coverage, scoped-resolution, and candidate-edge facts.
    /// Canonical graph edges have already been materialized and do not depend
    /// on these rows.
    pub fn prune_committed_closures(&self, retain: usize) -> anyhow::Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let old_closures = r#"
            SELECT closure_id
            FROM closure_generations
            WHERE state = 'committed'
            ORDER BY COALESCE(committed_at, created_at) DESC, rowid DESC
            LIMIT -1 OFFSET ?1
        "#;
        for table in [
            "symbol_edge_candidates",
            "reference_resolutions",
            "closure_coverage",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE closure_id IN ({old_closures})"),
                params![retain as i64],
            )?;
        }
        let deleted = tx.execute(
            &format!("DELETE FROM closure_generations WHERE closure_id IN ({old_closures})"),
            params![retain as i64],
        )?;
        tx.commit()?;
        Ok(deleted)
    }
}

// ── Row mapping ─────────────────────────────────────────────────────────────

fn row_to_closure_generation(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClosureGeneration> {
    Ok(ClosureGeneration {
        closure_id: row.get(0)?,
        committed_generation: row.get(1)?,
        state: row.get(2)?,
        committed_at: row.get(3)?,
        created_at: row.get(4)?,
    })
}
