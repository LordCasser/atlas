//! Domain rules CRUD — language-agnostic rule storage for analysis.
//!
//! Supports language-specific rule kinds (free_fn, alloc_fn, owned_pattern, cleanup_fn, etc.)
//! with extensible meta and status management.

use rusqlite::params;

use super::Store;

/// A domain rule row from the database.
#[derive(Debug, Clone)]
pub struct DomainRuleRow {
    pub id: String,
    pub language: String,
    pub rule_kind: String,
    pub pattern: String,
    pub pattern_kind: String,
    pub meta: Option<String>,
    pub meta_version: i32,
    pub source: String,
    pub status: String,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
}

fn row_to_domain_rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<DomainRuleRow> {
    Ok(DomainRuleRow {
        id: row.get(0)?,
        language: row.get(1)?,
        rule_kind: row.get(2)?,
        pattern: row.get(3)?,
        pattern_kind: row.get(4)?,
        meta: row.get(5)?,
        meta_version: row.get(6)?,
        source: row.get(7)?,
        status: row.get(8)?,
        confidence: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

impl Store {
    /// Insert or replace a domain rule.
    pub fn upsert_domain_rule(
        &self,
        language: &str,
        rule_kind: &str,
        pattern: &str,
        pattern_kind: &str,
        source: &str,
        status: &str,
        confidence: f64,
        meta: Option<&str>,
    ) -> anyhow::Result<String> {
        let id = format!("{}_{}_{}", language, rule_kind, pattern);
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO domain_rules (id, language, rule_kind, pattern, pattern_kind, meta, meta_version, source, status, confidence, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9, datetime('now'))",
            params![
                id,
                language,
                rule_kind,
                pattern,
                pattern_kind,
                meta,
                source,
                status,
                confidence,
            ],
        )?;
        Ok(id)
    }

    /// List all domain rules, with optional filters.
    pub fn list_domain_rules(
        &self,
        language: Option<&str>,
        status: Option<&str>,
    ) -> anyhow::Result<Vec<DomainRuleRow>> {
        let conn = self.lock_read();
        let select_sql = "SELECT id, language, rule_kind, pattern, pattern_kind, meta, meta_version, source, status, confidence, created_at, updated_at FROM domain_rules";

        match (language, status) {
            (Some(lang), Some(st)) => {
                let mut stmt = conn.prepare(&format!(
                    "{} WHERE language = ?1 AND status = ?2 ORDER BY rule_kind, pattern",
                    select_sql
                ))?;
                let rows = stmt.query_map(params![lang, st], row_to_domain_rule)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
            (Some(lang), None) => {
                let mut stmt = conn.prepare(&format!(
                    "{} WHERE language = ?1 ORDER BY rule_kind, pattern",
                    select_sql
                ))?;
                let rows = stmt.query_map(params![lang], row_to_domain_rule)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
            (None, Some(st)) => {
                let mut stmt = conn.prepare(&format!(
                    "{} WHERE status = ?1 ORDER BY language, rule_kind, pattern",
                    select_sql
                ))?;
                let rows = stmt.query_map(params![st], row_to_domain_rule)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
            (None, None) => {
                let mut stmt = conn.prepare(&format!(
                    "{} ORDER BY language, source, rule_kind, pattern",
                    select_sql
                ))?;
                let rows = stmt.query_map([], row_to_domain_rule)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
        }
    }

    /// Delete a domain rule by id.
    pub fn delete_domain_rule(&self, id: &str) -> anyhow::Result<bool> {
        let conn = self.lock();
        let count = conn.execute("DELETE FROM domain_rules WHERE id = ?1", params![id])?;
        Ok(count > 0)
    }

    /// Get domain rules of a specific kind, optionally filtered by language.
    pub fn get_domain_rules_by_kind(
        &self,
        rule_kind: &str,
        language: Option<&str>,
    ) -> anyhow::Result<Vec<DomainRuleRow>> {
        let conn = self.lock_read();
        let select_sql = "SELECT id, language, rule_kind, pattern, pattern_kind, meta, meta_version, source, status, confidence, created_at, updated_at FROM domain_rules WHERE rule_kind = ?1";
        match language {
            Some(lang) => {
                let mut stmt = conn.prepare(&format!(
                    "{} AND language = ?2 ORDER BY source",
                    select_sql
                ))?;
                let rows = stmt.query_map(params![rule_kind, lang], row_to_domain_rule)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
            None => {
                let mut stmt = conn.prepare(&format!("{} ORDER BY source", select_sql))?;
                let rows = stmt.query_map(params![rule_kind], row_to_domain_rule)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    fn test_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
    }

    #[test]
    fn test_upsert_and_list_domain_rules() {
        let store = test_store();
        let id = store
            .upsert_domain_rule("c", "free_fn", "my_free", "exact", "user", "enabled", 1.0, None)
            .unwrap();
        assert!(!id.is_empty());

        let rules = store.list_domain_rules(None, None).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_kind, "free_fn");
        assert_eq!(rules[0].pattern, "my_free");
        assert_eq!(rules[0].source, "user");
    }

    #[test]
    fn test_list_domain_rules_filter_by_language() {
        let store = test_store();
        store
            .upsert_domain_rule("c", "free_fn", "c_free", "exact", "user", "enabled", 1.0, None)
            .unwrap();
        store
            .upsert_domain_rule(
                "rust",
                "alloc_fn",
                "rust_alloc",
                "exact",
                "builtin",
                "enabled",
                1.0,
                None,
            )
            .unwrap();

        let c_rules = store.list_domain_rules(Some("c"), None).unwrap();
        assert_eq!(c_rules.len(), 1);
        assert_eq!(c_rules[0].pattern, "c_free");

        let rust_rules = store.list_domain_rules(Some("rust"), None).unwrap();
        assert_eq!(rust_rules.len(), 1);
        assert_eq!(rust_rules[0].pattern, "rust_alloc");
    }

    #[test]
    fn test_list_domain_rules_filter_by_status() {
        let store = test_store();
        store
            .upsert_domain_rule(
                "c", "free_fn", "enabled_fn", "exact", "user", "enabled", 1.0, None,
            )
            .unwrap();
        store
            .upsert_domain_rule(
                "c", "free_fn", "disabled_fn", "exact", "user", "disabled", 1.0, None,
            )
            .unwrap();

        let enabled = store.list_domain_rules(None, Some("enabled")).unwrap();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].pattern, "enabled_fn");
    }

    #[test]
    fn test_delete_domain_rule() {
        let store = test_store();
        let id = store
            .upsert_domain_rule(
                "c", "free_fn", "to_delete", "exact", "user", "enabled", 1.0, None,
            )
            .unwrap();
        assert!(store.delete_domain_rule(&id).unwrap());
        assert!(!store.delete_domain_rule(&id).unwrap()); // already deleted
        assert!(store.list_domain_rules(None, None).unwrap().is_empty());
    }

    #[test]
    fn test_get_domain_rules_by_kind() {
        let store = test_store();
        store
            .upsert_domain_rule("c", "free_fn", "f1", "exact", "user", "enabled", 1.0, None)
            .unwrap();
        store
            .upsert_domain_rule("c", "alloc_fn", "a1", "exact", "user", "enabled", 1.0, None)
            .unwrap();

        let free_rules = store.get_domain_rules_by_kind("free_fn", Some("c")).unwrap();
        assert_eq!(free_rules.len(), 1);

        let alloc_rules = store.get_domain_rules_by_kind("alloc_fn", Some("c")).unwrap();
        assert_eq!(alloc_rules.len(), 1);

        let none = store
            .get_domain_rules_by_kind("nonexistent", None)
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn test_upsert_idempotent() {
        let store = test_store();
        let id1 = store
            .upsert_domain_rule("c", "free_fn", "func", "exact", "user", "enabled", 0.9, None)
            .unwrap();
        let id2 = store
            .upsert_domain_rule("c", "free_fn", "func", "exact", "user", "enabled", 1.0, None)
            .unwrap();
        assert_eq!(id1, id2);
        let rules = store.list_domain_rules(None, None).unwrap();
        assert_eq!(rules.len(), 1, "Upsert should replace, not duplicate");
        assert!((rules[0].confidence - 1.0).abs() < 0.01);
    }
}
