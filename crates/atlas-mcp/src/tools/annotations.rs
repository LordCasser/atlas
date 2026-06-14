//! Function-pointer dispatch annotation tools.
//!
//! These tools let users declare runtime dispatch mappings between
//! function-pointer fields and their concrete target functions, enabling
//! path queries to traverse indirect calls.
//!
//! **Supported languages**: C and C++ only. Other languages use dynamic
//! dispatch mechanisms (virtual tables, reflection) that Atlas detects
//! automatically through static analysis — no manual annotation needed.

use atlas_engine::{FpAnnotation, Language};

use super::{MAX_ANNOTATION_QNAME_LENGTH, ToolRouter, get_str};
use crate::tools::symbol_selector::{
    SymbolInput, SymbolResolution, SymbolResolutionPolicy,
};

use serde_json::json;

impl ToolRouter {
    /// Handle `annotate_fp_dispatch` — declare a function-pointer dispatch mapping.
    ///
    /// Example: `annotate_fp_dispatch("Curl_handler.do_it", "Curl_http")`
    /// declares that when `do_it` field of `Curl_handler` struct is called,
    /// the concrete target is `Curl_http`.
    pub(crate) fn handle_annotate_fp_dispatch(
        &mut self,
        args: &serde_json::Value,
    ) -> (String, bool) {
        let field_qname = get_str(args, "field_qname");
        let target_qname = get_str(args, "target_qname");

        if field_qname.is_empty() || target_qname.is_empty() {
            return (
                r#"{"error":"field_qname and target_qname are required. Example: annotate_fp_dispatch('Curl_handler.do_it', 'Curl_http')"}"#
                    .to_string(),
                true,
            );
        }

        if field_qname.len() > MAX_ANNOTATION_QNAME_LENGTH {
            return (
                json!({"error": format!(
                    "field_qname exceeds maximum length of {} characters",
                    MAX_ANNOTATION_QNAME_LENGTH
                )})
                .to_string(),
                true,
            );
        }
        if target_qname.len() > MAX_ANNOTATION_QNAME_LENGTH {
            return (
                json!({"error": format!(
                    "target_qname exceeds maximum length of {} characters",
                    MAX_ANNOTATION_QNAME_LENGTH
                )})
                .to_string(),
                true,
            );
        }

        // Resolve field symbol
        let field_id = match self.resolve_symbol_input(
            &SymbolInput::Name(field_qname.to_string()),
            SymbolResolutionPolicy::BestEffortSingle,
        ) {
            Ok(SymbolResolution::Single { symbol_id, .. }) => symbol_id,
            Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                let candidates_str: Vec<String> = candidates
                    .iter()
                    .take(5)
                    .map(|c| format!("{}::{} [{}]", c.file_path, c.line, c.kind))
                    .collect();
                return (
                    json!({
                        "error": format!(
                            "Field symbol '{}' is ambiguous ({} matches: {}). Use a SymbolSelector object from search results (symbol_ref field).",
                            field_qname, candidates.len(), candidates_str.join(", ")
                        )
                    })
                    .to_string(),
                    true,
                );
            }
            Ok(SymbolResolution::NotFound { .. }) => {
                let mut err = format!("Symbol not found: {field_qname}");
                err.push_str(self.active().store_query_runtime.not_indexed_guidance());
                return (json!({"error": err}).to_string(), true);
            }
            Err(e) => return (json!({"error": e}).to_string(), true),
        };

        // Resolve target symbol
        let target_id = match self.resolve_symbol_input(
            &SymbolInput::Name(target_qname.to_string()),
            SymbolResolutionPolicy::BestEffortSingle,
        ) {
            Ok(SymbolResolution::Single { symbol_id, .. }) => symbol_id,
            Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                let candidates_str: Vec<String> = candidates
                    .iter()
                    .take(5)
                    .map(|c| format!("{}::{} [{}]", c.file_path, c.line, c.kind))
                    .collect();
                return (
                    json!({
                        "error": format!(
                            "Target symbol '{}' is ambiguous ({} matches: {}). Use a SymbolSelector object from search results (symbol_ref field).",
                            target_qname, candidates.len(), candidates_str.join(", ")
                        )
                    })
                    .to_string(),
                    true,
                );
            }
            Ok(SymbolResolution::NotFound { .. }) => {
                let mut err = format!("Symbol not found: {target_qname}");
                err.push_str(self.active().store_query_runtime.not_indexed_guidance());
                return (json!({"error": err}).to_string(), true);
            }
            Err(e) => return (json!({"error": e}).to_string(), true),
        };

        // ── Language compatibility check ────────────────────────────
        // Function-pointer dispatch annotations are only meaningful for
        // C and C++. Other languages use dynamic dispatch (virtual tables,
        // reflection, prototype chains) that the engine detects through
        // static analysis.
        let field_sym = self.active().store.find_symbol_by_id(&field_id).ok().flatten();
        if let Some(sym) = &field_sym {
            if !matches!(sym.language, Language::C | Language::Cpp) {
                let lang_name = sym.language.as_str();
                let kind_name = sym.kind.as_str();
                return (
                    json!({
                        "error": format!(
                            "Function-pointer dispatch annotations are only applicable to C and C++. \
                             Symbol '{}' is a '{}' in {} — {lang} uses dynamic dispatch \
                             (virtual tables, reflection, or prototype chains) rather than \
                             function-pointer tables. Atlas detects these through static analysis \
                             automatically; no manual annotation is needed.",
                            field_qname, kind_name, lang_name,
                            lang = match sym.language {
                                Language::TypeScript | Language::JavaScript => "JavaScript/TypeScript",
                                Language::Python => "Python",
                                Language::Java => "Java",
                                Language::Rust => "Rust",
                                Language::CSharp => "C#",
                                Language::Go => "Go",
                                Language::Ruby => "Ruby",
                                Language::Php => "PHP",
                                Language::Kotlin => "Kotlin",
                                Language::ArkTS => "ArkTS",
                                Language::Cangjie => "Cangjie",
                                _ => "this language",
                            }
                        )
                    })
                    .to_string(),
                    true,
                );
            }

            // Also check that the source is actually a field
            if sym.kind != atlas_engine::SymbolKind::Field {
                return (
                    json!({
                        "error": format!(
                            "Expected a Field symbol for the function pointer, but '{}' is a '{}'. \
                             Function-pointer dispatch annotations require a struct field \
                             (function pointer), not a {}. Example: annotate_fp_dispatch('MyStruct.handler', 'target_fn')",
                            field_qname, sym.kind.as_str(), sym.kind.as_str()
                        )
                    })
                    .to_string(),
                    true,
                );
            }
        }

        // ── Target must be a Function or Method ────────────────────
        let target_sym = self.active().store.find_symbol_by_id(&target_id).ok().flatten();
        if let Some(sym) = &target_sym {
            if !matches!(
                sym.kind,
                atlas_engine::SymbolKind::Function | atlas_engine::SymbolKind::Method
            ) {
                return (
                    json!({
                        "error": format!(
                            "Expected a Function or Method symbol as the target, but '{}' is a '{}'. \
                             Function-pointer dispatch annotations map a struct field to a concrete \
                             function implementation.",
                            target_qname, sym.kind.as_str()
                        )
                    })
                    .to_string(),
                    true,
                );
            }
        }

        // Extract field_name from qualified name (last segment after dot/::)
        let field_name = field_qname
            .rsplit(&['.', ':'][..])
            .find(|s| !s.is_empty())
            .unwrap_or(field_qname);

        // Build annotation ID
        let hex = blake3::hash(field_id.as_bytes()).to_hex();
        let annotation_id = format!("fpa:{}:{}", &hex[..16], field_name);

        let confidence = atlas_engine::Confidence::new(
            args.get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0) as f32,
        );

        let annotation = FpAnnotation {
            annotation_id: annotation_id.clone(),
            source_symbol: field_id,
            field_name: field_name.to_string(),
            target_symbol: target_id,
            confidence,
        };

        match self.active().overlay_runtime.upsert_fp_annotation(&annotation) {
            Ok(()) => {
                // Materialize the edge immediately
                if let Err(e) = atlas_engine::materialize_annotations(&self.active().store) {
                    return (
                        json!({
                            "error": format!("Annotation stored but edge materialization failed: {}", e),
                            "annotation_id": &annotation_id,
                            "field_qname": field_qname,
                            "target_qname": target_qname,
                        })
                        .to_string(),
                        true,
                    );
                }
                // Refresh the graph so subsequent graph-backed queries
                // immediately see the new annotation edges (best-effort;
                // a no-op if the graph hasn't been initialized yet).
                let _ = self.force_refresh_graph();
                (
                    json!({
                        "annotation_id": &annotation_id,
                        "field_qname": field_qname,
                        "field_name": field_name,
                        "target_qname": target_qname,
                        "confidence": confidence,
                        "status": "created"
                    })
                    .to_string(),
                    false,
                )
            }
            Err(e) => (
                json!({"error": format!("Failed to store annotation: {}", e)}).to_string(),
                true,
            ),
        }
    }

    /// Handle `list_fp_annotations` — list all dispatch annotations.
    pub(crate) fn handle_list_fp_annotations(&self) -> (String, bool) {
        match self.active().store.get_all_fp_annotations() {
            Ok(annotations) => {
                // Batch-lookup all source + target symbols to avoid N+1 queries.
                let mut symbol_ids = std::collections::HashSet::new();
                for a in &annotations {
                    symbol_ids.insert(a.source_symbol);
                    symbol_ids.insert(a.target_symbol);
                }
                let mut symbol_map: std::collections::HashMap<atlas_engine::SymbolId, String> =
                    std::collections::HashMap::new();
                for id in symbol_ids {
                    let qname = self
                        .active().store
                        .find_symbol_by_id(&id)
                        .ok()
                        .flatten()
                        .map(|s| s.qualified_name);
                    if let Some(qn) = qname {
                        symbol_map.insert(id, qn);
                    }
                }

                let items: Vec<serde_json::Value> = annotations
                    .iter()
                    .map(|a| {
                        let source_qname = symbol_map
                            .get(&a.source_symbol)
                            .cloned()
                            .unwrap_or_else(|| a.field_name.clone());
                        let target_qname = symbol_map
                            .get(&a.target_symbol)
                            .cloned()
                            .unwrap_or_else(|| "?".to_string());

                        json!({
                            "annotation_id": &a.annotation_id,
                            "source_qname": source_qname,
                            "field_name": &a.field_name,
                            "target_qname": target_qname,
                            "confidence": a.confidence,
                        })
                    })
                    .collect();

                (
                    json!({
                        "count": annotations.len(),
                        "annotations": items,
                    })
                    .to_string(),
                    false,
                )
            }
            Err(e) => (
                json!({"error": format!("Failed to list annotations: {}", e)}).to_string(),
                true,
            ),
        }
    }

    /// Handle `delete_fp_annotation` — delete a dispatch annotation.
    pub(crate) fn handle_delete_fp_annotation(&mut self, args: &serde_json::Value) -> (String, bool) {
        let annotation_id = get_str(args, "annotation_id");
        let field_qname = get_str(args, "field_qname");

        let (deleted, deleted_annotation_id) = if !annotation_id.is_empty() {
            (
                self.active()
                    .overlay_runtime
                    .delete_fp_annotation(annotation_id)
                    .map_err(|e| format!("Failed to delete annotation: {e}")),
                annotation_id.to_string(),
            )
        } else if !field_qname.is_empty() {
            let field_id = match self.resolve_symbol_input(
                &SymbolInput::Name(field_qname.to_string()),
                SymbolResolutionPolicy::BestEffortSingle,
            ) {
                Ok(SymbolResolution::Single { symbol_id, .. }) => symbol_id,
                Ok(SymbolResolution::Ambiguous { candidates, .. }) => {
                    let candidates_str: Vec<String> = candidates
                        .iter()
                        .take(5)
                        .map(|c| format!("{}::{} [{}]", c.file_path, c.line, c.kind))
                        .collect();
                    return (
                        json!({
                            "error": format!(
                                "Field symbol '{}' is ambiguous ({} matches: {}). Use a SymbolSelector object from search results (symbol_ref field).",
                                field_qname, candidates.len(), candidates_str.join(", ")
                            )
                        })
                        .to_string(),
                        true,
                    );
                }
                Ok(SymbolResolution::NotFound { .. }) => {
                    let mut err = format!("Symbol not found: {field_qname}");
                    err.push_str(self.active().store_query_runtime.not_indexed_guidance());
                    return (json!({"error": err}).to_string(), true);
                }
                Err(e) => return (json!({"error": e}).to_string(), true),
            };
            // Extract field_name from qname
            let field_name = field_qname
                .rsplit(&['.', ':'][..])
                .find(|s| !s.is_empty())
                .unwrap_or(field_qname);
            // Look up annotation to get its ID for the response
            let annotation_id = self
                .active().store
                .find_fp_annotation_by_field(&field_id, field_name)
                .map_err(|e| format!("Lookup error: {e}"));
            let ann_id = match annotation_id {
                Ok(Some(ref a)) => a.annotation_id.clone(),
                Ok(None) => {
                    return (
                        json!({"status": "not_found", "message": format!("No annotation found for field '{}'", field_qname)})
                            .to_string(),
                        true,
                    )
                }
                Err(e) => return (json!({"error": e}).to_string(), true),
            };
            (
                self.active()
                    .overlay_runtime
                    .delete_fp_annotation(&ann_id)
                    .map_err(|e| format!("Failed to delete annotation: {e}")),
                ann_id,
            )
        } else {
            return (
                json!({"error": "Either annotation_id or field_qname is required. Example: delete_fp_annotation(annotation_id='fpa:abc123:do_it') or delete_fp_annotation(field_qname='Curl_handler.do_it')"}).to_string(),
                true,
            );
        };

        match deleted {
            Ok(true) => {
                // Clean up stale materialized edges
                if let Err(e) = atlas_engine::materialize_annotations(&self.active().store) {
                    return (
                        json!({"error": format!("Annotation deleted but edge cleanup failed: {e}"), "annotation_id": deleted_annotation_id}).to_string(),
                        true,
                    );
                }
                // Force graph refresh so the in-memory snapshot reflects the removal
                if let Err(e) = self.force_refresh_graph() {
                    return (
                        json!({"error": format!("Failed to rebuild graph after annotation removal: {e}"), "annotation_id": deleted_annotation_id}).to_string(),
                        true,
                    );
                }
                (
                    json!({"status": "deleted", "annotation_id": deleted_annotation_id}).to_string(),
                    false,
                )
            }
            Ok(false) => (
                json!({"status": "not_found", "message": "No matching annotation found"})
                    .to_string(),
                true,
            ),
            Err(e) => (json!({"error": e}).to_string(), true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::{
        Confidence, FileFacts, FileId, FileInfo, FpAnnotation, Language, ParseStatus, Store,
        SymbolDef, SymbolId, SymbolKind, TextRange,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_store() -> Arc<Store> {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        Arc::new(store)
    }

    fn insert_sym(
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

    fn annotation_id(source: &SymbolId, field_name: &str) -> String {
        let hex = blake3::hash(source.as_bytes()).to_hex();
        format!("fpa:{}:{}", &hex[..16], field_name)
    }

    #[test]
    fn delete_fp_annotation_cleans_materialized_edges() {
        let store = test_store();
        let fa = FileId::generate("src/field.c");
        let fb = FileId::generate("src/target.c");

        let field = insert_sym(&store, fa, "do_it", "Curl_handler.do_it", SymbolKind::Field);
        let target = insert_sym(&store, fb, "Curl_http", "Curl_http", SymbolKind::Function);

        // Add annotation and materialize edges
        let ann_id = annotation_id(&field, "do_it");
        let ann = FpAnnotation {
            annotation_id: ann_id.clone(),
            source_symbol: field,
            field_name: "do_it".into(),
            target_symbol: target,
            confidence: Confidence::new(1.0),
        };
        store.upsert_fp_annotation(&ann).unwrap();
        let count = atlas_engine::materialize_annotations(&store).unwrap();
        assert_eq!(count, 1, "materialize should create 1 edge");
        let edges_before = store.find_edges_by_source(&field).unwrap();
        assert!(!edges_before.is_empty(), "edge should exist before delete");

        // Delete via ToolRouter's handle_delete_fp_annotation
        let mut router = ToolRouter::new_empty(store.clone(), PathBuf::from("/tmp"));
        let args = json!({"annotation_id": ann_id});
        let (result, is_error) = router.handle_delete_fp_annotation(&args);
        assert!(!is_error, "delete should succeed: {result}");

        // Verify annotation is gone
        let remaining = store.get_all_fp_annotations().unwrap();
        assert!(remaining.is_empty(), "annotation should be deleted from store");

        // Verify edges are cleaned up
        let edges_after = store.find_edges_by_source(&field).unwrap();
        assert!(
            edges_after.is_empty(),
            "materialized edges should be cleaned up after delete, found {} edges",
            edges_after.len()
        );
    }
}
