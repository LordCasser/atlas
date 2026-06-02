//! Domain rules CRUD — language-agnostic rule storage for analysis.
//!
//! Supports language-specific rule kinds (free_fn, alloc_fn, owned_pattern, cleanup_fn, etc.)
//! with extensible meta and status management.

use rusqlite::params;

use super::Store;

/// Allowed status values for domain rules.
const VALID_STATUSES: &[&str] = &["candidate", "enabled", "disabled", "rejected", "deprecated"];

/// Generate a deterministic, collision-free domain rule ID.
///
/// Uses blake3 with explicit `\xff` delimiters so that fields containing
/// underscores or other printable characters cannot collide.
fn rule_id(language: &str, rule_kind: &str, pattern_kind: &str, pattern: &str) -> String {
    let mut buf = Vec::with_capacity(
        language.len() + 1 + rule_kind.len() + 1 + pattern_kind.len() + 1 + pattern.len(),
    );
    buf.extend_from_slice(language.as_bytes());
    buf.push(0xff);
    buf.extend_from_slice(rule_kind.as_bytes());
    buf.push(0xff);
    buf.extend_from_slice(pattern_kind.as_bytes());
    buf.push(0xff);
    buf.extend_from_slice(pattern.as_bytes());
    hex::encode(blake3::hash(&buf).as_bytes())
}

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
    ///
    /// Panics (in debug via `debug_assert!`) or returns an error if required
    /// fields are empty or `status` is not a recognised value.
    #[allow(clippy::too_many_arguments)]
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
        // Input validation
        anyhow::ensure!(!language.is_empty(), "domain rule language must not be empty");
        anyhow::ensure!(!rule_kind.is_empty(), "domain rule rule_kind must not be empty");
        anyhow::ensure!(!pattern.is_empty(), "domain rule pattern must not be empty");
        anyhow::ensure!(
            VALID_STATUSES.contains(&status),
            "domain rule status '{status}' is not valid; allowed: {VALID_STATUSES:?}"
        );
        let id = rule_id(language, rule_kind, pattern_kind, pattern);
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
                    "{select_sql} WHERE language = ?1 AND status = ?2 ORDER BY rule_kind, pattern"
                ))?;
                let rows = stmt.query_map(params![lang, st], row_to_domain_rule)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
            (Some(lang), None) => {
                let mut stmt = conn.prepare(&format!(
                    "{select_sql} WHERE language = ?1 ORDER BY rule_kind, pattern"
                ))?;
                let rows = stmt.query_map(params![lang], row_to_domain_rule)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
            (None, Some(st)) => {
                let mut stmt = conn.prepare(&format!(
                    "{select_sql} WHERE status = ?1 ORDER BY language, rule_kind, pattern"
                ))?;
                let rows = stmt.query_map(params![st], row_to_domain_rule)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
            (None, None) => {
                let mut stmt = conn.prepare(&format!(
                    "{select_sql} ORDER BY language, source, rule_kind, pattern"
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
                    "{select_sql} AND language = ?2 ORDER BY source"
                ))?;
                let rows = stmt.query_map(params![rule_kind, lang], row_to_domain_rule)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
            None => {
                let mut stmt = conn.prepare(&format!("{select_sql} ORDER BY source"))?;
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

    #[test]
    fn test_rule_id_no_collision_on_ambiguous_delimiter() {
        // Regression: the old format!("{}_{}_{}", lang, kind, pattern) would
        // collide when fields themselves contained underscores:
        //   ("c", "free", "fn_malloc")       → "c_free_fn_malloc"
        //   ("c", "free_fn", "malloc")       → "c_free_fn_malloc"  (COLLISION)
        // The blake3-based ID must produce *different* IDs for these inputs.
        let store = test_store();
        let id_a = store
            .upsert_domain_rule("c", "free", "fn_malloc", "exact", "user", "enabled", 1.0, None)
            .unwrap();
        let id_b = store
            .upsert_domain_rule("c", "free_fn", "malloc", "exact", "user", "enabled", 1.0, None)
            .unwrap();
        assert_ne!(
            id_a, id_b,
            "Collision!  ('c','free','fn_malloc') and ('c','free_fn','malloc') must produce different IDs"
        );
        // Both rules must be present (2 rows, not 1 overriding the other).
        let rules = store.list_domain_rules(None, None).unwrap();
        assert_eq!(
            rules.len(),
            2,
            "Expected 2 distinct rules, got {} — collision caused silent overwrite",
            rules.len()
        );
    }

    #[test]
    fn test_upsert_rejects_empty_language() {
        let store = test_store();
        let err = store
            .upsert_domain_rule("", "free_fn", "p", "exact", "user", "enabled", 1.0, None)
            .unwrap_err();
        assert!(err.to_string().contains("language"), "should reject empty language");
    }

    #[test]
    fn test_upsert_rejects_invalid_status() {
        let store = test_store();
        let err = store
            .upsert_domain_rule("c", "free_fn", "p", "exact", "user", "bogus_status", 1.0, None)
            .unwrap_err();
        assert!(
            err.to_string().contains("status"),
            "should reject invalid status: {err}"
        );
    }
}
