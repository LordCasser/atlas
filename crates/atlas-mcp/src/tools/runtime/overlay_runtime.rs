//! Overlay runtime — user annotation management with generation tracking.
//!
//! # Responsibilities
//! - Centralized mutation for fp_dispatches and domain_rules
//! - Centralized generation counters via RuntimeInvalidation
//! - All mutation methods bump generation on success
//!
//! # Public API
//! - `upsert_fp_annotation()` / `delete_fp_annotation()`: FP dispatch CRUD
//! - `upsert_domain_rule()` / `delete_domain_rule()`: domain rule CRUD
//! - `current_generation()`: read overlay generation counter (for cache comparison)
//!
//! # Usage pattern
//! ```ignore
//! let ok = self.active.overlay_runtime.upsert_fp_annotation(&annotation)?;
//! ```
//!
//! # Dependencies
//! - `atlas_engine::Store` (mutation path)
//! - `super::invalidation::RuntimeInvalidation`

use std::sync::Arc;
use std::sync::atomic::Ordering;

use atlas_engine::{FpAnnotation, Store};

use super::invalidation::RuntimeInvalidation;

/// Manages user-defined annotations that modify graph topology
/// (function pointer dispatches) and analysis semantics (domain rules).
///
/// In Phase 4, this provides immediate materialization and
/// cache invalidation for overlay changes.
pub struct OverlayRuntime {
    pub store: Arc<Store>,
    /// Shared invalidation counters — centralized generation tracking.
    pub invalidation: Arc<RuntimeInvalidation>,
}

impl OverlayRuntime {
    pub fn new(store: Arc<Store>, invalidation: Arc<RuntimeInvalidation>) -> Self {
        Self {
            store,
            invalidation,
        }
    }

    /// Increment the overlay generation counter.
    /// Returns the new generation value.
    /// Used primarily by tests; mutation methods bump counters directly via
    /// `RuntimeInvalidation` for fine-grained ordering control.
    #[allow(dead_code)]
    pub fn increment_generation(&self) -> u64 {
        self.invalidation
            .overlay_generation
            .fetch_add(1, Ordering::Relaxed)
            + 1
    }

    /// Get the current overlay generation (for cache comparison).
    /// Currently exercised only in tests; reserved for future cache invalidation logic.
    #[allow(dead_code)]
    pub fn current_generation(&self) -> u64 {
        self.invalidation.overlay_generation.load(Ordering::Relaxed)
    }

    /// Upsert a function-pointer annotation, materializing edges via the store.
    /// Bumps both overlay_generation and graph_generation (fp_dispatches affect
    /// graph topology).
    pub fn upsert_fp_annotation(&self, annotation: &FpAnnotation) -> anyhow::Result<()> {
        self.store.upsert_fp_annotation(annotation)?;
        self.invalidation
            .overlay_generation
            .fetch_add(1, Ordering::Relaxed);
        self.invalidation
            .graph_generation
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Delete a function-pointer annotation by ID.
    /// Bumps both overlay_generation and graph_generation only if an annotation
    /// was actually deleted.
    pub fn delete_fp_annotation(&self, annotation_id: &str) -> anyhow::Result<bool> {
        let deleted = self.store.delete_fp_annotation(annotation_id)?;
        if deleted {
            self.invalidation
                .overlay_generation
                .fetch_add(1, Ordering::Relaxed);
            self.invalidation
                .graph_generation
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(deleted)
    }

    /// Upsert a domain rule.
    /// Bumps overlay_generation and analysis_config_generation on success.
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
        let rule_id = self.store.upsert_domain_rule(
            language,
            rule_kind,
            pattern,
            pattern_kind,
            source,
            status,
            confidence,
            meta,
        )?;
        self.invalidation
            .overlay_generation
            .fetch_add(1, Ordering::Relaxed);
        self.invalidation
            .analysis_config_generation
            .fetch_add(1, Ordering::Relaxed);
        Ok(rule_id)
    }

    /// Delete a domain rule.
    /// Bumps overlay_generation and analysis_config_generation only if a rule
    /// was actually deleted.
    pub fn delete_domain_rule(&self, rule_id: &str) -> anyhow::Result<bool> {
        let deleted = self.store.delete_domain_rule(rule_id)?;
        if deleted {
            self.invalidation
                .overlay_generation
                .fetch_add(1, Ordering::Relaxed);
            self.invalidation
                .analysis_config_generation
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::{
        Confidence, FileFacts, FileId, FileInfo, FpAnnotation, Language, ParseStatus, Store,
        SymbolDef, SymbolId, SymbolKind, TextRange,
    };
    use std::sync::Arc;

    fn create_test_overlay_runtime() -> OverlayRuntime {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let invalidation = Arc::new(RuntimeInvalidation::new());
        OverlayRuntime::new(store, invalidation)
    }

    fn insert_symbol(
        store: &Store,
        file_id: FileId,
        name: &str,
        qname: &str,
        kind: SymbolKind,
    ) -> SymbolId {
        let range = TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };
        let id = SymbolId::generate(&file_id, "c", qname, kind.as_str(), None);
        let sym = SymbolDef {
            id,
            kind,
            name: name.to_string(),
            qualified_name: qname.to_string(),
            symbol_path: vec![name.to_string()],
            file_id,
            language: Language::C,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        };
        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: format!("src/{name}.c"),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![sym],
            ..Default::default()
        };
        store.insert_file_facts(&facts).unwrap();
        id
    }

    fn test_annotation_id(source: &SymbolId, field_name: &str) -> String {
        let hex = blake3::hash(source.as_bytes()).to_hex();
        format!("fpa:{}:{}", &hex[..16], field_name)
    }

    // ── Basic generation tests ──────────────────────────────────────────────

    #[test]
    fn generation_starts_at_one() {
        let or = create_test_overlay_runtime();
        assert_eq!(or.current_generation(), 1);
    }

    #[test]
    fn increment_generation_returns_new_value() {
        let or = create_test_overlay_runtime();
        assert_eq!(or.increment_generation(), 2);
        assert_eq!(or.increment_generation(), 3);
        assert_eq!(or.current_generation(), 3);
    }

    // ── FP annotation mutation tests ─────────────────────────────────────────

    #[test]
    fn upsert_fp_annotation_increments_generation() {
        let or = create_test_overlay_runtime();
        let file_a = FileId::generate("src/field.c");
        let file_b = FileId::generate("src/target.c");

        let source = insert_symbol(
            &or.store,
            file_a,
            "do_it",
            "Curl_handler.do_it",
            SymbolKind::Field,
        );
        let target = insert_symbol(
            &or.store,
            file_b,
            "Curl_http",
            "Curl_http",
            SymbolKind::Function,
        );

        let ann = FpAnnotation {
            annotation_id: test_annotation_id(&source, "do_it"),
            source_symbol: source,
            field_name: "do_it".into(),
            target_symbol: target,
            confidence: Confidence::new(1.0),
        };

        assert_eq!(or.current_generation(), 1);
        or.upsert_fp_annotation(&ann).unwrap();
        assert!(or.current_generation() > 1);
    }

    #[test]
    fn delete_fp_annotation_returns_false_for_nonexistent() {
        let or = create_test_overlay_runtime();
        let result = or.delete_fp_annotation("nonexistent_id");
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    // ── Domain rule mutation tests ──────────────────────────────────────────

    #[test]
    fn upsert_domain_rule_increments_generation() {
        let or = create_test_overlay_runtime();
        let result = or.upsert_domain_rule(
            "c",
            "free_fn",
            "test_free",
            "exact",
            "user",
            "enabled",
            1.0,
            None,
        );
        assert!(result.is_ok());
        assert!(or.current_generation() > 1);
    }

    #[test]
    fn delete_domain_rule_returns_false_for_nonexistent() {
        let or = create_test_overlay_runtime();
        let result = or.delete_domain_rule("nonexistent_id");
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }
}
