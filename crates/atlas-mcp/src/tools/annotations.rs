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

use super::{ToolRouter, get_str};

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

        // Resolve field symbol
        let field_id = match self.resolve_qname(field_qname) {
            Ok(id) => id,
            Err(e) => return (json!({"error": e}).to_string(), true),
        };

        // Resolve target symbol
        let target_id = match self.resolve_qname(target_qname) {
            Ok(id) => id,
            Err(e) => return (json!({"error": e}).to_string(), true),
        };

        // ── Language compatibility check ────────────────────────────
        // Function-pointer dispatch annotations are only meaningful for
        // C and C++. Other languages use dynamic dispatch (virtual tables,
        // reflection, prototype chains) that the engine detects through
        // static analysis.
        let field_sym = self.store.find_symbol_by_id(&field_id).ok().flatten();
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
        let target_sym = self.store.find_symbol_by_id(&target_id).ok().flatten();
        if let Some(sym) = &target_sym {
            if !matches!(sym.kind, atlas_engine::SymbolKind::Function | atlas_engine::SymbolKind::Method) {
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

        match self.store.upsert_fp_annotation(&annotation) {
            Ok(()) => {
                // Materialize the edge immediately
                if let Err(e) = atlas_engine::materialize_annotations(&self.store) {
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
        match self.store.get_all_fp_annotations() {
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
                    let qname = self.store.find_symbol_by_id(&id).ok().flatten()
                        .map(|s| s.qualified_name);
                    if let Some(qn) = qname {
                        symbol_map.insert(id, qn);
                    }
                }

                let items: Vec<serde_json::Value> = annotations
                    .iter()
                    .map(|a| {
                        let source_qname = symbol_map.get(&a.source_symbol)
                            .cloned()
                            .unwrap_or_else(|| a.field_name.clone());
                        let target_qname = symbol_map.get(&a.target_symbol)
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
    pub(crate) fn handle_delete_fp_annotation(
        &self,
        args: &serde_json::Value,
    ) -> (String, bool) {
        let annotation_id = get_str(args, "annotation_id");
        let field_qname = get_str(args, "field_qname");

        let (deleted, deleted_annotation_id) = if !annotation_id.is_empty() {
            (
                self.store
                    .delete_fp_annotation(annotation_id)
                    .map_err(|e| format!("Failed to delete annotation: {}", e)),
                annotation_id.to_string(),
            )
        } else if !field_qname.is_empty() {
            let field_id = match self.resolve_qname(field_qname) {
                Ok(id) => id,
                Err(e) => return (json!({"error": e}).to_string(), true),
            };
            // Extract field_name from qname
            let field_name = field_qname
                .rsplit(&['.', ':'][..])
                .find(|s| !s.is_empty())
                .unwrap_or(field_qname);
            // Look up annotation to get its ID for the response
            let annotation_id = self
                .store
                .find_fp_annotation_by_field(&field_id, field_name)
                .map_err(|e| format!("Lookup error: {}", e));
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
                self.store
                    .delete_fp_annotation(&ann_id)
                    .map_err(|e| format!("Failed to delete annotation: {}", e)),
                ann_id,
            )
        } else {
            return (
                json!({"error": "Either annotation_id or field_qname is required. Example: delete_fp_annotation(annotation_id='fpa:abc123:do_it') or delete_fp_annotation(field_qname='Curl_handler.do_it')"}).to_string(),
                true,
            );
        };

        match deleted {
            Ok(true) => (
                json!({"status": "deleted", "annotation_id": deleted_annotation_id})
                    .to_string(),
                false,
            ),
            Ok(false) => (
                json!({"status": "not_found", "message": "No matching annotation found"})
                    .to_string(),
                true,
            ),
            Err(e) => (json!({"error": e}).to_string(), true),
        }
    }
}
