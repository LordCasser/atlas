//! Materialize function-pointer dispatch annotations as symbol edges.
//!
//! Annotations live in `function_pointer_annotations` and survive re-index.
//! They are materialized into `symbol_edges` so the graph engine (path queries,
//! callers, callees) can traverse them without any code changes.
//!
//! Each annotation produces two kinds of edges:
//! 1. **Direct edge**: `field_symbol → target_function` (Calls).
//!    Makes `callees(field)` and `callers(target)` work for annotation symbols.
//! 2. **Caller bridge edges**: for every callsite that resolves to the field,
//!    an edge `calling_function → target_function` (Calls).
//!    This ensures BFS path traversal can hop from a real caller through the
//!    annotation to the target without needing an intermediate field-symbol hop.
//!    (The graph builder may not create `function → field` edges for indirect
//!    function-pointer calls, so the field-only edge is not traversable by BFS.)
//!
//! All edges carry provenance `UserAnnotation`, confidence from the annotation,
//! and metadata `{"annotation_id": "…", "field_name": "…"}` for traceability.

use db::Store;
use types::ids::EdgeId;
use types::{EdgeKind, Provenance, RawEdge};

/// Materialize all function-pointer annotations as `symbol_edges` rows.
///
/// Uses `INSERT OR REPLACE` so repeated calls are idempotent.
/// Returns the number of edges materialized.
pub fn materialize_annotations(store: &Store) -> anyhow::Result<usize> {
    // 1. Clean stale edges from previous materializations so that
    //    deleted / overwritten annotations don't leave orphaned edges.
    store.delete_edges_by_provenance(Provenance::UserAnnotation.as_str())?;

    // 2. Re-materialize current annotations.
    let annotations = store.get_all_fp_annotations()?;
    if annotations.is_empty() {
        return Ok(0);
    }

    let mut edges = Vec::with_capacity(annotations.len());
    for ann in &annotations {
        let metadata = serde_json::json!({
            "annotation_id": &ann.annotation_id,
            "field_name": &ann.field_name,
        })
        .to_string();

        // ── Direct edge: field → target (for direct callee/caller queries) ──
        let edge_id = EdgeId::generate(
            &ann.source_symbol,
            &ann.target_symbol,
            "calls",
            None,
            "user_annotation",
        );
        let mut edge = RawEdge::new(
            edge_id,
            ann.source_symbol,
            ann.target_symbol,
            EdgeKind::Calls,
            ann.confidence,
            Provenance::UserAnnotation,
        );
        edge.metadata = Some(metadata.clone());
        edges.push(edge);

        // ── Caller bridge edges: for each callsite resolving to the field,
        //     create calling_function → target (enables BFS path traversal) ──
        if let Ok(refs) = store.find_references_by_symbol(&ann.source_symbol) {
            for r in &refs {
                let caller = match r.source_symbol {
                    Some(sid) if sid != ann.source_symbol => sid,
                    _ => continue,
                };
                let bridge_id = EdgeId::generate(
                    &caller,
                    &ann.target_symbol,
                    "calls",
                    None,
                    "user_annotation",
                );
                let mut bridge_edge = RawEdge::new(
                    bridge_id,
                    caller,
                    ann.target_symbol,
                    EdgeKind::Calls,
                    ann.confidence,
                    Provenance::UserAnnotation,
                );
                bridge_edge.metadata = Some(metadata.clone());
                edges.push(bridge_edge);
            }
        }
    }

    store.batch_insert_edges(&edges)?;
    Ok(edges.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::ids::{FileId, ReferenceId};
    use types::{
        Confidence, FileInfo, Language, ParseStatus, ReferenceUse, SymbolDef, SymbolKind, TextRange,
    };

    fn setup_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
    }

    fn insert_sym(
        store: &Store,
        file_id: FileId,
        name: &str,
        qname: &str,
        kind: SymbolKind,
    ) -> types::SymbolId {
        let range = TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };
        let id = types::SymbolId::generate(&file_id, "c", qname, kind.as_str(), None);
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
        use types::FileFacts;
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

    fn annotation_id(source: &types::SymbolId, field_name: &str) -> String {
        let hex = blake3::hash(source.as_bytes()).to_hex();
        format!("fpa:{}:{}", &hex[..16], field_name)
    }

    #[test]
    fn test_materialize_creates_edge() {
        let store = setup_store();
        let fa = FileId::generate("src/field.c");
        let fb = FileId::generate("src/target.c");

        let field = insert_sym(&store, fa, "do_it", "Curl_handler.do_it", SymbolKind::Field);
        let target = insert_sym(&store, fb, "Curl_http", "Curl_http", SymbolKind::Function);

        let ann = types::FpAnnotation {
            annotation_id: annotation_id(&field, "do_it"),
            source_symbol: field,
            field_name: "do_it".into(),
            target_symbol: target,
            confidence: Confidence::new(1.0),
        };
        store.upsert_fp_annotation(&ann).unwrap();

        let count = materialize_annotations(&store).unwrap();
        assert_eq!(count, 1);

        // Verify the edge exists
        let edges = store.find_edges_by_source(&field).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, target);
        assert_eq!(edges[0].kind, EdgeKind::Calls);
        assert_eq!(edges[0].provenance, Provenance::UserAnnotation);
        assert!(edges[0].metadata.is_some());
    }

    #[test]
    fn test_materialize_idempotent() {
        let store = setup_store();
        let fa = FileId::generate("src/hand.c");
        let fb = FileId::generate("src/fn1.c");

        let field = insert_sym(&store, fa, "handler", "S.handler", SymbolKind::Field);
        let target = insert_sym(&store, fb, "fn1", "fn1", SymbolKind::Function);

        let ann = types::FpAnnotation {
            annotation_id: annotation_id(&field, "handler"),
            source_symbol: field,
            field_name: "handler".into(),
            target_symbol: target,
            confidence: Confidence::new(1.0),
        };
        store.upsert_fp_annotation(&ann).unwrap();

        materialize_annotations(&store).unwrap();
        materialize_annotations(&store).unwrap(); // second call

        let edges = store.find_edges_by_source(&field).unwrap();
        assert_eq!(edges.len(), 1); // INSERT OR REPLACE, not duplicate
    }

    #[test]
    fn test_materialize_cleans_stale_edges() {
        let store = setup_store();
        let fa = FileId::generate("src/f.c");
        let fb = FileId::generate("src/fn_a.c");
        let fc = FileId::generate("src/fn_b.c");

        let field = insert_sym(&store, fa, "handler", "S.handler", SymbolKind::Field);
        let target_a = insert_sym(&store, fb, "fn_a", "fn_a", SymbolKind::Function);
        let target_b = insert_sym(&store, fc, "fn_b", "fn_b", SymbolKind::Function);

        // Step 1: annotate S.handler → fn_a
        let ann_a = types::FpAnnotation {
            annotation_id: annotation_id(&field, "handler"),
            source_symbol: field,
            field_name: "handler".into(),
            target_symbol: target_a,
            confidence: Confidence::new(1.0),
        };
        store.upsert_fp_annotation(&ann_a).unwrap();
        materialize_annotations(&store).unwrap();
        assert_eq!(store.find_edges_by_source(&field).unwrap().len(), 1);

        // Step 2: overwrite annotation with S.handler → fn_b
        let ann_b = types::FpAnnotation {
            annotation_id: annotation_id(&field, "handler"),
            source_symbol: field,
            field_name: "handler".into(),
            target_symbol: target_b,
            confidence: Confidence::new(1.0),
        };
        store.upsert_fp_annotation(&ann_b).unwrap();
        materialize_annotations(&store).unwrap();

        // After re-materialization, only the new edge should exist
        let edges = store.find_edges_by_source(&field).unwrap();
        assert_eq!(
            edges.len(),
            1,
            "stale edge from fn_a should have been cleaned up"
        );
        assert_eq!(edges[0].target, target_b);
    }

    #[test]
    fn test_materialize_empty() {
        let store = setup_store();
        let count = materialize_annotations(&store).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_materialize_creates_bridge_edges() {
        let store = setup_store();
        let fa = FileId::generate("src/field.c");
        let fb = FileId::generate("src/target.c");
        let fc = FileId::generate("src/caller.c");

        let field = insert_sym(&store, fa, "do_it", "Handler.do_it", SymbolKind::Field);
        let target = insert_sym(&store, fb, "Curl_http", "Curl_http", SymbolKind::Function);
        let caller = insert_sym(&store, fc, "do_request", "do_request", SymbolKind::Function);

        // Insert a reference that resolves to the field — the calling function
        // (caller) contains a reference that resolves to the field symbol.
        use types::{ReferenceKind, ResolutionStrategy, ResolvedTarget};
        let ref_id = ReferenceId::generate(
            &fc,
            Some(&caller),
            0,
            6,
            "do_it",
            ReferenceKind::FieldAccess,
        );
        let resolved = ResolvedTarget {
            symbol_id: field,
            confidence: types::Confidence::new(1.0),
            strategy: ResolutionStrategy::ExactMatch,
            provenance: Provenance::TreeSitter,
        };
        let text_range = TextRange {
            start_byte: 0,
            end_byte: 6,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 7,
        };
        let r = ReferenceUse {
            id: ref_id,
            file_id: fc,
            source_symbol: Some(caller),
            scope_id: None,
            kind: ReferenceKind::FieldAccess,
            text: "do_it".into(),
            name: "do_it".into(),
            receiver: None,
            arity: Some(0),
            range: text_range,
            binding_id: None,
            resolved: Some(resolved),
        };
        store.insert_references(&[r]).unwrap();

        // Create annotation: field → target
        let ann = types::FpAnnotation {
            annotation_id: annotation_id(&field, "do_it"),
            source_symbol: field,
            field_name: "do_it".into(),
            target_symbol: target,
            confidence: Confidence::new(1.0),
        };
        store.upsert_fp_annotation(&ann).unwrap();

        let count = materialize_annotations(&store).unwrap();
        // 1 direct edge (field→target) + 1 bridge edge (caller→target)
        assert_eq!(count, 2);

        // Verify direct edge exists
        let field_edges = store.find_edges_by_source(&field).unwrap();
        assert_eq!(field_edges.len(), 1);
        assert_eq!(field_edges[0].target, target);

        // Verify bridge edge exists
        let caller_edges = store.find_edges_by_source(&caller).unwrap();
        assert_eq!(caller_edges.len(), 1);
        assert_eq!(caller_edges[0].target, target);
        assert_eq!(caller_edges[0].provenance, Provenance::UserAnnotation);
    }
}
