//! Symbol CRUD: insert, search (FTS5, LIKE, exact name), bulk queries.

use rusqlite::params;
use types::*;

use super::Store;
use super::files::{normalize_scope, scope_child_bounds};
use crate::store_fts::sanitize_fts5_query;
use crate::store_rows::row_to_symbol;
use crate::store_writers::write_symbols;

impl Store {
    /// Batch-insert symbols inside a transaction.
    pub fn insert_symbols(&self, symbols: &[SymbolDef]) -> anyhow::Result<()> {
        if symbols.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| {
            write_symbols(tx, symbols, "structural")?;
            Ok(())
        })
    }

    /// Find all symbols in a file.
    pub fn find_symbols_by_file(&self, file_id: &FileId) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json, layer
             FROM symbols WHERE file_id = ?1 ORDER BY qualified_name",
        )?;
        let rows = stmt.query_map(params![file_id], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find symbols across multiple files in a single query.
    ///
    /// Used by delta graph refresh to load only the symbols for files
    /// affected by lazy structural extraction, avoiding a full scan.
    pub fn find_symbols_by_files(&self, file_ids: &[FileId]) -> anyhow::Result<Vec<SymbolDef>> {
        if file_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock_read();
        let placeholders: Vec<String> = file_ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json, layer
             FROM symbols WHERE file_id IN ({}) ORDER BY qualified_name",
            placeholders.join(",")
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = file_ids
            .iter()
            .map(|f| f as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), row_to_symbol)?;
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
        let conn = self.lock_read();
        let safe_query = sanitize_fts5_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }
        // Append * for prefix matching (matches "User" → "UserManager")
        let match_query = format!("{safe_query}*");
        let sql = if kind_filter.is_some() {
            format!(
                r#"SELECT s.symbol_id, s.file_id, s.kind, s.name, s.qualified_name,
                          s.symbol_path_json, s.language,
                          s.range_start_byte, s.range_end_byte, s.range_start_line,
                          s.range_start_column, s.range_end_line, s.range_end_column,
                          s.name_start_byte, s.name_end_byte, s.name_start_line,
                          s.name_start_column, s.name_end_line, s.name_end_column,
                          s.signature, s.visibility, s.exported, s.static_, s.async_,
                          s.container_id, s.scope_id, s.package_name, s.namespace_path_json, s.layer
                   FROM symbols s
                   JOIN symbols_fts fts ON s.rowid = fts.rowid
                   WHERE symbols_fts MATCH ?1 AND s.kind = ?2
                   ORDER BY rank
                   LIMIT {limit}"#
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
                          s.container_id, s.scope_id, s.package_name, s.namespace_path_json, s.layer
                   FROM symbols s
                   JOIN symbols_fts fts ON s.rowid = fts.rowid
                   WHERE symbols_fts MATCH ?1
                   ORDER BY rank
                   LIMIT {limit}"#
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

    /// FTS5 search restricted to a project-relative directory or file scope.
    pub fn search_symbols_in_scope_with_limit(
        &self,
        query: &str,
        scope: &str,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> anyhow::Result<Vec<SymbolDef>> {
        let scope = normalize_scope(scope);
        if scope.is_empty() {
            return Ok(Vec::new());
        }
        let safe_query = sanitize_fts5_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }
        let match_query = format!("{safe_query}*");
        let (scope_lower, scope_upper) = scope_child_bounds(&scope);
        let conn = self.lock_read();
        let sql = if kind_filter.is_some() {
            format!(
                r#"SELECT s.symbol_id, s.file_id, s.kind, s.name, s.qualified_name,
                          s.symbol_path_json, s.language,
                          s.range_start_byte, s.range_end_byte, s.range_start_line,
                          s.range_start_column, s.range_end_line, s.range_end_column,
                          s.name_start_byte, s.name_end_byte, s.name_start_line,
                          s.name_start_column, s.name_end_line, s.name_end_column,
                          s.signature, s.visibility, s.exported, s.static_, s.async_,
                          s.container_id, s.scope_id, s.package_name, s.namespace_path_json, s.layer
                   FROM symbols s
                   JOIN symbols_fts fts ON s.rowid = fts.rowid
                   JOIN files f ON f.file_id = s.file_id
                   WHERE symbols_fts MATCH ?1
                     AND (f.path = ?2 OR (f.path >= ?3 AND f.path < ?4))
                     AND s.kind = ?5
                   ORDER BY rank
                   LIMIT {limit}"#
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
                          s.container_id, s.scope_id, s.package_name, s.namespace_path_json, s.layer
                   FROM symbols s
                   JOIN symbols_fts fts ON s.rowid = fts.rowid
                   JOIN files f ON f.file_id = s.file_id
                   WHERE symbols_fts MATCH ?1
                     AND (f.path = ?2 OR (f.path >= ?3 AND f.path < ?4))
                   ORDER BY rank
                   LIMIT {limit}"#
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = if let Some(kind) = kind_filter {
            stmt.query_map(
                params![match_query, scope, scope_lower, scope_upper, kind.as_str()],
                row_to_symbol,
            )?
        } else {
            stmt.query_map(
                params![match_query, scope, scope_lower, scope_upper],
                row_to_symbol,
            )?
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Exact-name symbol lookup restricted to a project-relative directory or file scope.
    ///
    /// This is the fastest scoped search path for navigation-style queries such
    /// as Linux function names. It uses `idx_symbols_name` before falling back
    /// to broader FTS/LIKE search layers.
    pub fn find_symbols_by_name_in_scope(
        &self,
        name: &str,
        scope: &str,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> anyhow::Result<Vec<SymbolDef>> {
        let scope = normalize_scope(scope);
        if scope.is_empty() || name.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let (scope_lower, scope_upper) = scope_child_bounds(&scope);
        let conn = self.lock_read();
        let sql = if kind_filter.is_some() {
            format!(
                r#"SELECT s.symbol_id, s.file_id, s.kind, s.name, s.qualified_name,
                          s.symbol_path_json, s.language,
                          s.range_start_byte, s.range_end_byte, s.range_start_line,
                          s.range_start_column, s.range_end_line, s.range_end_column,
                          s.name_start_byte, s.name_end_byte, s.name_start_line,
                          s.name_start_column, s.name_end_line, s.name_end_column,
                          s.signature, s.visibility, s.exported, s.static_, s.async_,
                          s.container_id, s.scope_id, s.package_name, s.namespace_path_json, s.layer
                   FROM symbols s
                   JOIN files f ON f.file_id = s.file_id
                   WHERE s.name = ?1
                     AND (f.path = ?2 OR (f.path >= ?3 AND f.path < ?4))
                     AND s.kind = ?5
                   ORDER BY s.qualified_name
                   LIMIT {limit}"#
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
                          s.container_id, s.scope_id, s.package_name, s.namespace_path_json, s.layer
                   FROM symbols s
                   JOIN files f ON f.file_id = s.file_id
                   WHERE s.name = ?1
                     AND (f.path = ?2 OR (f.path >= ?3 AND f.path < ?4))
                   ORDER BY s.qualified_name
                   LIMIT {limit}"#
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = if let Some(kind) = kind_filter {
            stmt.query_map(
                params![name, scope, scope_lower, scope_upper, kind.as_str()],
                row_to_symbol,
            )?
        } else {
            stmt.query_map(
                params![name, scope, scope_lower, scope_upper],
                row_to_symbol,
            )?
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
        let conn = self.lock_read();
        let like_pattern = format!("%{}%", pattern.replace(['%', '_'], ""));
        if like_pattern.len() <= 2 {
            // Just "%%"
            return Ok(Vec::new());
        }

        // Build WHERE clause dynamically based on filters
        let mut where_clauses = vec!["(s.name LIKE ?1 OR s.qualified_name LIKE ?2)".to_string()];
        let mut param_idx = 3; // ?1 and ?2 are LIKE patterns

        if language.is_some() {
            where_clauses.push(format!("s.language = ?{param_idx}"));
            param_idx += 1;
        }
        if kind_filter.is_some() {
            where_clauses.push(format!("s.kind = ?{param_idx}"));
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
                      s.container_id, s.scope_id, s.package_name, s.namespace_path_json, s.layer
               FROM symbols s
               WHERE {where_sql}
               LIMIT {limit}"#
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

    /// LIKE-based symbol search restricted to a project-relative scope.
    pub fn search_symbols_by_name_like_in_scope(
        &self,
        pattern: &str,
        scope: &str,
        language: Option<&Language>,
        limit: usize,
        kind_filter: Option<&SymbolKind>,
    ) -> anyhow::Result<Vec<SymbolDef>> {
        let scope = normalize_scope(scope);
        if scope.is_empty() {
            return Ok(Vec::new());
        }
        let like_pattern = format!("%{}%", pattern.replace(['%', '_'], ""));
        if like_pattern.len() <= 2 {
            return Ok(Vec::new());
        }
        let (scope_lower, scope_upper) = scope_child_bounds(&scope);

        let mut where_clauses = vec![
            "(s.name LIKE ?1 OR s.qualified_name LIKE ?2)".to_string(),
            "(f.path = ?3 OR (f.path >= ?4 AND f.path < ?5))".to_string(),
        ];
        let mut param_idx = 6;

        if language.is_some() {
            where_clauses.push(format!("s.language = ?{param_idx}"));
            param_idx += 1;
        }
        if kind_filter.is_some() {
            where_clauses.push(format!("s.kind = ?{param_idx}"));
        }

        let sql = format!(
            r#"SELECT s.symbol_id, s.file_id, s.kind, s.name, s.qualified_name,
                      s.symbol_path_json, s.language,
                      s.range_start_byte, s.range_end_byte, s.range_start_line,
                      s.range_start_column, s.range_end_line, s.range_end_column,
                      s.name_start_byte, s.name_end_byte, s.name_start_line,
                      s.name_start_column, s.name_end_line, s.name_end_column,
                      s.signature, s.visibility, s.exported, s.static_, s.async_,
                      s.container_id, s.scope_id, s.package_name, s.namespace_path_json, s.layer
               FROM symbols s
               JOIN files f ON f.file_id = s.file_id
               WHERE {}
               LIMIT {}"#,
            where_clauses.join(" AND "),
            limit
        );

        let conn = self.lock_read();
        let mut stmt = conn.prepare(&sql)?;
        let lang_str = language.map(|l| l.as_str().to_string()).unwrap_or_default();
        let kind_str = kind_filter
            .map(|k| k.as_str().to_string())
            .unwrap_or_default();

        let rows = match (language.is_some(), kind_filter.is_some()) {
            (true, true) => stmt.query_map(
                params![
                    like_pattern,
                    like_pattern,
                    scope,
                    scope_lower,
                    scope_upper,
                    lang_str,
                    kind_str
                ],
                row_to_symbol,
            )?,
            (true, false) => stmt.query_map(
                params![
                    like_pattern,
                    like_pattern,
                    scope,
                    scope_lower,
                    scope_upper,
                    lang_str
                ],
                row_to_symbol,
            )?,
            (false, true) => stmt.query_map(
                params![
                    like_pattern,
                    like_pattern,
                    scope,
                    scope_lower,
                    scope_upper,
                    kind_str
                ],
                row_to_symbol,
            )?,
            (false, false) => stmt.query_map(
                params![like_pattern, like_pattern, scope, scope_lower, scope_upper],
                row_to_symbol,
            )?,
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ── Counts ──────────────────────────────────────────────────────────────

    /// Look up a single symbol's kind by its ID.
    ///
    /// Returns `None` if the symbol does not exist.  Used by TypeGraph
    /// strategy to determine whether a resolved target is a type definition.
    pub fn get_symbol_kind(&self, symbol_id: &SymbolId) -> anyhow::Result<Option<SymbolKind>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare("SELECT kind FROM symbols WHERE symbol_id = ?1")?;
        let mut rows = stmt.query_map(params![symbol_id], |row| {
            let kind_str: String = row.get(0)?;
            SymbolKind::from_str(&kind_str).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid SymbolKind in DB: {kind_str}"),
                    )),
                )
            })
        })?;
        match rows.next() {
            Some(Ok(kind)) => Ok(Some(kind)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Total number of symbols in the database.
    pub fn count_symbols(&self) -> anyhow::Result<usize> {
        let conn = self.lock_read();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    // ── Bulk queries ────────────────────────────────────────────────────────

    /// Find symbols by qualified name (exact match, index lookup).
    pub fn find_symbols_by_qname(&self, qname: &str) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json, layer
             FROM symbols WHERE qualified_name = ?1",
        )?;
        let rows = stmt.query_map(params![qname], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Load ALL symbols (for GraphSnapshot construction).
    /// Uses a separate read connection to avoid blocking writes via the mutex.
    pub fn get_all_symbols(&self) -> anyhow::Result<Vec<SymbolDef>> {
        let guard = self.lock_read();
        let conn: &rusqlite::Connection = &guard;
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json, layer
             FROM symbols",
        )?;
        let rows = stmt.query_map([], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find symbols by exact `name` match (uses index on `symbols.name`).
    ///
    /// Faster than `search_symbols_by_name_like` for exact-match lookups,
    /// avoiding FTS5 overhead and LIKE scans.
    pub fn find_symbols_by_name(&self, name: &str) -> anyhow::Result<Vec<SymbolDef>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT symbol_id, file_id, kind, name, qualified_name, symbol_path_json,
                    language,
                    range_start_byte, range_end_byte, range_start_line, range_start_column,
                    range_end_line, range_end_column,
                    name_start_byte, name_end_byte, name_start_line, name_start_column,
                    name_end_line, name_end_column,
                    signature, visibility, exported, static_, async_,
                    container_id, scope_id, package_name, namespace_path_json, layer
             FROM symbols WHERE name = ?1 ORDER BY qualified_name",
        )?;
        let rows = stmt.query_map(params![name], row_to_symbol)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
