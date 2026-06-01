//! Thin store wrapper — delegates to db::Store for rule persistence.

/// Re-export db's DomainRuleRow for convenience.
pub use db::DomainRuleRow;

/// A thin wrapper around [`db::Store`] for domain rule operations.
///
/// This struct exists for consumers that want a focused interface
/// without depending on db directly. It delegates all operations.
pub struct GenericRuleStore<'a> {
    store: &'a db::Store,
}

impl<'a> GenericRuleStore<'a> {
    /// Create a new store wrapper.
    pub fn new(store: &'a db::Store) -> Self {
        Self { store }
    }

    /// Get a reference to the underlying store.
    pub fn inner(&self) -> &'a db::Store {
        self.store
    }

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
        self.store.upsert_domain_rule(
            language,
            rule_kind,
            pattern,
            pattern_kind,
            source,
            status,
            confidence,
            meta,
        )
    }

    /// List domain rules with optional filters.
    pub fn list_domain_rules(
        &self,
        language: Option<&str>,
        status: Option<&str>,
    ) -> anyhow::Result<Vec<DomainRuleRow>> {
        self.store.list_domain_rules(language, status)
    }

    /// Delete a domain rule by id.
    pub fn delete_domain_rule(&self, id: &str) -> anyhow::Result<bool> {
        self.store.delete_domain_rule(id)
    }

    /// Get rules of a specific kind, optionally filtered by language.
    pub fn get_domain_rules_by_kind(
        &self,
        rule_kind: &str,
        language: Option<&str>,
    ) -> anyhow::Result<Vec<DomainRuleRow>> {
        self.store.get_domain_rules_by_kind(rule_kind, language)
    }
}
