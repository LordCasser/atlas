//! Focus closure generation tracking — query and update closure lifecycle.
//!
//! The `closure_generations` table records the lifecycle of focus closures
//! (building → committed → stale). The `committed_generation` counter gates
//! visibility for MCP queries: only data at or below the committed generation
//! is visible.

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
    /// Insert a new closure generation in 'building' state.
    pub fn insert_closure_generation(&self, closure_id: &str) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO closure_generations (closure_id) VALUES (?1)",
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
