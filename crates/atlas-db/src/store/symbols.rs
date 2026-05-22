//! Symbol CRUD: insert, search (FTS5, LIKE, exact name), bulk queries.

use atlas_types::*;
use rusqlite::params;

use super::Store;
use crate::store_fts::sanitize_fts5_query;
use crate::store_rows::row_to_symbol;
use crate::store_writers::write_symbols;

impl Store {
    /// Batch-insert symbols inside a transaction.
    pub fn insert_symbols(&self, symbols: &[SymbolDef]) -> anyhow::Result<()> {
        if symbols.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| write_symbols(tx, symbols))
    }

    /// Find all symbols in a file.
    pub fn find_symbols_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json
             FROM symbols WHERE file_id = ?1 ORDER BY qualified_name",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// FTS5 search by name (default limit 50).
    pub fn search_symbols(&self, query: &str) -> anyhow::Result<Vec<SymbolDef>> {
        self.search_symbols_with_limit(query, 50, None)
    }

    /// FTS5 search by name with custom limit and optional kind filter.
    ///
    /// When `kind_filter` is provided, the filter is applied at the SQL level
    /// (`WHERE s.kind = ?`) so that the search pipeline's fallback logic
    /// (FTS5 → LIKE → Levenshtein) correctly triggers when a stage returns
    /// zero results after filtering.
    pub fn search_symbols_with_limit(
        &self,
        query: &str,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock();
        let safe_query = sanitize_fts5_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }
        // Append * for prefix matching (matches "User" → "UserManager")
        let match_query = format!("{}*", safe_query);
        let sql = if kind_filter.is_some() {
            format!(
                r#"SELECT s.symbol_id, s.file_id, s.kind, s.name, s.qualified_name,
                          s.symbol_path_json, s.language,
                          s.range_start_byte, s.range_end_byte, s.range_start_line,
                          s.range_start_column, s.range_end_line, s.range_end_column,
                          s.name_start_byte, s.name_end_byte, s.name_start_line,
                          s.name_start_column, s.name_end_line, s.name_end_column,
                          s.signature, s.visibility, s.exported, s.static_, s.async_,
                          s.container_id, s.scope_id, s.package_name, s.namespace_path_json
                   FROM symbols s
                   JOIN symbols_fts fts ON s.rowid = fts.rowid
                   WHERE symbols_fts MATCH ?1 AND s.kind = ?2
                   ORDER BY rank
                   LIMIT {}"#,
                limit
            )
        } else {
            format!(
                r#"SELECT s.symbol_id, s.file_id, s.kind, s.name, s.qualified_name,
                          s.symbol_path_json, s.language,
                          s.range_start_byte, s.range_end_byte, s.range_start_line,
                          s.range_start_column, s.range_end_line, s.range_end_column,
                          s.name_start_byte, s.name_end_byte, s.name_start_line,
                          s.name_start_column, s.name_end_line, s.name_end_column,
                          s.signature, s.visibility, s.exported, s.static_, s.async_,
                          s.container_id, s.scope_id, s.package_name, s.namespace_path_json
                   FROM symbols s
                   JOIN symbols_fts fts ON s.rowid = fts.rowid
                   WHERE symbols_fts MATCH ?1
                   ORDER BY rank
                   LIMIT {}"#,
                limit
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = if let Some(kind) = kind_filter {
            let kind_str = kind.as_str();
            stmt.query_map(params![match_query, kind_str], row_to_symbol)?
        } else {
            stmt.query_map(params![match_query], row_to_symbol)?
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// LIKE-based name search (fallback when FTS5 returns nothing).
    ///
    /// Optional `kind_filter` is applied at the SQL level so that the
    /// search pipeline's fallback logic works correctly with kind filters.
    pub fn search_symbols_by_name_like(
        &self,
        pattern: &str,
        language: Option<&Language>,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock();
        let like_pattern = format!("%{}%", pattern.replace('%', "").replace('_', ""));
        if like_pattern.len() <= 2 {
            // Just "%%"
            return Ok(Vec::new());
        }

        // Build WHERE clause dynamically based on filters
        let mut where_clauses = vec!["(s.name LIKE ?1 OR s.qualified_name LIKE ?2)".to_string()];
        let mut param_idx = 3; // ?1 and ?2 are LIKE patterns

        if language.is_some() {
            where_clauses.push(format!("s.language = ?{}", param_idx));
            param_idx += 1;
        }
        if kind_filter.is_some() {
            where_clauses.push(format!("s.kind = ?{}", param_idx));
        }

        let where_sql = where_clauses.join(" AND ");

        let sql = format!(
            r#"SELECT s.symbol_id, s.file_id, s.kind, s.name, s.qualified_name,
                      s.symbol_path_json, s.language,
                      s.range_start_byte, s.range_end_byte, s.range_start_line,
                      s.range_start_column, s.range_end_line, s.range_end_column,
                      s.name_start_byte, s.name_end_byte, s.name_start_line,
                      s.name_start_column, s.name_end_line, s.name_end_column,
                      s.signature, s.visibility, s.exported, s.static_, s.async_,
                      s.container_id, s.scope_id, s.package_name, s.namespace_path_json
               FROM symbols s
               WHERE {}
               LIMIT {}"#,
            where_sql, limit
        );

        let mut stmt = conn.prepare(&sql)?;

        // Build params dynamically
        let lang_str = language.map(|l| l.as_str().to_string()).unwrap_or_default();
        let kind_str = kind_filter
            .map(|k| k.as_str().to_string())
            .unwrap_or_default();

        let rows = match (language.is_some(), kind_filter.is_some()) {
            (true, true) => stmt.query_map(
                params![like_pattern, like_pattern, lang_str, kind_str],
                row_to_symbol,
            )?,
            (true, false) => {
                stmt.query_map(params![like_pattern, like_pattern, lang_str], row_to_symbol)?
            }
            (false, true) => {
                stmt.query_map(params![like_pattern, like_pattern, kind_str], row_to_symbol)?
            }
            (false, false) => stmt.query_map(params![like_pattern, like_pattern], row_to_symbol)?,
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ── Counts ──────────────────────────────────────────────────────────────

    /// Total number of symbols in the database.
    pub fn count_symbols(&self) -> anyhow::Result<usize> {
        let conn = self.lock();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    // ── Bulk queries ────────────────────────────────────────────────────────

    /// Find symbols by qualified name (exact match, index lookup).
    pub fn find_symbols_by_qname(&self, qname: &str) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json
             FROM symbols WHERE qualified_name = ?1",
        )?;
        let rows = stmt.query_map(params![qname], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Load ALL symbols (for GraphSnapshot construction).
    /// Uses a separate read connection to avoid blocking writes via the mutex.
    pub fn get_all_symbols(&self) -> anyhow::Result<Vec<SymbolDef>> {
        let guard = self.lock();
        let conn: &rusqlite::Connection = &guard;
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json
             FROM symbols",
        )?;
        let rows = stmt.query_map([], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Alias for `get_all_symbols` (P4: GlobalSymbolIndex construction).
    pub fn load_all_symbols(&self) -> anyhow::Result<Vec<SymbolDef>> {
        self.get_all_symbols()
    }

    /// Find symbols by exact `name` match (uses index on `symbols.name`).
    ///
    /// Faster than `search_symbols_by_name_like` for exact-match lookups,
    /// avoiding FTS5 overhead and LIKE scans.
    pub fn find_symbols_by_name(&self, name: &str) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json
             FROM symbols WHERE name = ?1 ORDER BY qualified_name",
        )?;
        let rows = stmt.query_map(params![name], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
