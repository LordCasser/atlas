//! Symbol hints store — manifest-level symbol name index (Tier 1 bootstrap).
//!
//! Hints are lightweight, non-definitive lookups derived from manifest extraction.
//! Missing from hints does not mean the symbol doesn't exist — it may be in a
//! file that hasn't been manifest-extracted yet.

use rusqlite::params;

use super::Store;

impl Store {
    /// Insert a single symbol hint.
    pub fn insert_symbol_hint(
        &self,
        name: &str,
        file_id: &[u8],
        kind: &str,
        line: u32,
        confidence: f64,
        source: &str,
    ) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO symbol_hints
                (name, file_id, kind, line, confidence, source, freshness)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
            params![name, file_id, kind, line, confidence, source],
        )?;
        Ok(())
    }

    /// Batch-insert symbol hints. Returns the number inserted.
    pub fn insert_symbol_hints_batch(&self, hints: &[SymbolHint]) -> anyhow::Result<usize> {
        let conn = self.lock();
        let mut count = 0;
        for hint in hints {
            conn.execute(
                "INSERT OR REPLACE INTO symbol_hints
                    (name, file_id, kind, line, confidence, source, freshness)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
                params![
                    hint.name,
                    hint.file_id,
                    hint.kind,
                    hint.line,
                    hint.confidence,
                    hint.source,
                ],
            )?;
            count += 1;
        }
        Ok(count)
    }

    /// Query hints for a symbol name (case-insensitive match).
    /// Returns up to 20 results, ordered by confidence descending.
    pub fn query_symbol_hints(&self, name: &str) -> anyhow::Result<Vec<SymbolHint>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT name, file_id, kind, line, confidence, source, freshness
             FROM symbol_hints WHERE lower(name) = lower(?1)
             ORDER BY confidence DESC LIMIT 20",
        )?;
        let rows = stmt
            .query_map(params![name], |row| {
                Ok(SymbolHint {
                    name: row.get(0)?,
                    file_id: row.get(1)?,
                    kind: row.get(2)?,
                    line: row.get(3)?,
                    confidence: row.get(4)?,
                    source: row.get(5)?,
                    freshness: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Check whether any hints exist for the given symbol name.
    pub fn has_symbol_hints(&self, name: &str) -> anyhow::Result<bool> {
        let conn = self.lock_read();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM symbol_hints WHERE lower(name) = lower(?1)",
            params![name],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }
}

/// A row from the symbol_hints table.
#[derive(Debug, Clone)]
pub struct SymbolHint {
    pub name: String,
    pub file_id: Vec<u8>,
    pub kind: String,
    pub line: u32,
    pub confidence: f64,
    pub source: String,
    pub freshness: String,
}
