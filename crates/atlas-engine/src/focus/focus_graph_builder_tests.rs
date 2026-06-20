//! Tests for FocusGraphBuilder — closure-based graph edge construction.

use std::sync::Arc;

use db::Store;
use types::enums::{EdgeKind, Language, ParseStatus, ReferenceKind, SymbolKind};
use types::ids::{FileId, ReferenceId};
use types::structs::TextRange;
use types::{FileFacts, FileInfo, ReferenceUse, SymbolDef, SymbolId};

use super::focus_graph_builder::FocusGraphBuilder;

// ── Test helpers ─────────────────────────────────────────────────────────────

fn test_store() -> Arc<Store> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    store
}

fn test_range() -> TextRange {
    TextRange {
        start_byte: 0,
        end_byte: 10,
        start_line: 1,
        start_column: 1,
        end_line: 1,
        end_column: 11,
    }
}

fn test_file_id() -> FileId {
    FileId::generate("src/test.ts")
}

fn test_symbol(file_id: FileId, name: &str, kind: SymbolKind) -> SymbolDef {
    let range = test_range();
    let id = SymbolId::generate(&file_id, "typescript", name, kind.as_str(), None);
    SymbolDef {
        id,
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        symbol_path: vec![name.to_string()],
        file_id,
        language: Language::TypeScript,
        range,
        name_range: range,
        signature: None,
        visibility: None,
        exported: true,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".to_string(),
    }
}

fn insert_closures(store: &Store, closure_ids: &[&str]) {
    for cid in closure_ids {
        store.insert_closure_generation(cid).unwrap();
    }
}

fn insert_symbols(store: &Store, syms: &[SymbolDef]) {
    let file_id = test_file_id();
    let file_info = FileInfo {
        file_id,
        path: "src/test.ts".into(),
        language: Language::TypeScript,
        content_hash: "abc".into(),
        status: ParseStatus::Success,
    };
    store
        .insert_file_facts(&FileFacts {
            file: file_info,
            symbols: syms.to_vec(),
            ..Default::default()
        })
        .unwrap();
}

/// Insert a reference and return its ReferenceId.
fn insert_reference(
    store: &Store,
    source_symbol: SymbolId,
    name: &str,
    kind: ReferenceKind,
) -> ReferenceId {
    let file_id = test_file_id();
    let range = test_range();
    let ref_id = ReferenceId::generate(
        &file_id,
        Some(&source_symbol),
        range.start_byte,
        range.end_byte,
        name,
        kind,
    );
    let reference = ReferenceUse {
        id: ref_id,
        file_id,
        source_symbol: Some(source_symbol),
        scope_id: None,
        kind,
        text: name.to_string(),
        name: name.to_string(),
        receiver: None,
        arity: if kind == ReferenceKind::Call {
            Some(0)
        } else {
            None
        },
        range,
        binding_id: None,
        resolved: None,
    };
    store.insert_references(&[reference]).unwrap();
    ref_id
}

/// Insert a reference resolution row and make it visible.
fn insert_visible_resolution(
    store: &Store,
    closure_id: &str,
    generation: i64,
    ref_id: &ReferenceId,
    target_sym_id: &SymbolId,
    coverage_tier: &str,
    semantic_confidence: &str,
    resolution_strategy: &str,
) {
    store
        .insert_reference_resolution(
            ref_id.as_bytes(),
            closure_id,
            generation,
            "closure_reachable",
            Some(target_sym_id.as_bytes()),
            coverage_tier,
            semantic_confidence,
            resolution_strategy,
            Some("test"),
        )
        .unwrap();
    store
        .make_resolutions_visible(closure_id, generation)
        .unwrap();
}

/// Assert that a canonical edge exists in symbol_edges with the given kind.
fn assert_canonical_edge_exists(
    store: &Store,
    source: &SymbolId,
    target: &SymbolId,
    kind: EdgeKind,
) {
    let edge = store
        .find_edge_by_source_target_kind(source, target, &kind)
        .unwrap();
    assert!(
        edge.is_some(),
        "expected canonical edge {source:?} -> {target:?} ({kind:?}), but none found"
    );
}

/// Assert no canonical edge exists.
fn assert_no_canonical_edge(store: &Store, source: &SymbolId, target: &SymbolId, kind: EdgeKind) {
    let edge = store
        .find_edge_by_source_target_kind(source, target, &kind)
        .unwrap();
    assert!(
        edge.is_none(),
        "expected NO canonical edge {source:?} -> {target:?} ({kind:?}), but one was found"
    );
}

// ── T1: Canonical edges from Certain confidence ─────────────────────────────

#[test]
fn test_build_for_closure_creates_canonical_edges() {
    let store = test_store();
    let file_id = test_file_id();

    let caller = test_symbol(file_id, "caller", SymbolKind::Function);
    let target = test_symbol(file_id, "targetFunc", SymbolKind::Function);
    insert_symbols(&store, &[caller.clone(), target.clone()]);
    insert_closures(&store, &["cl_certain"]);

    let ref_id = insert_reference(&store, caller.id, "targetFunc", ReferenceKind::Call);

    insert_visible_resolution(
        &store,
        "cl_certain",
        1,
        &ref_id,
        &target.id,
        "closure_complete",
        "certain",
        "closure_reachable",
    );

    let builder = FocusGraphBuilder::new(store.clone());
    let result = builder.build_for_closure("cl_certain", 1).unwrap();

    assert!(result.stats.edges_built > 0, "should build edges");
    assert_eq!(
        result.stats.edges_built, result.stats.edges_written,
        "canonical edges should be written"
    );
    assert_canonical_edge_exists(&store, &caller.id, &target.id, EdgeKind::Calls);
    let edge = store
        .find_edge_by_source_target_kind(&caller.id, &target.id, &EdgeKind::Calls)
        .unwrap()
        .unwrap();
    assert_eq!(edge.location, Some(test_range()));
    assert_eq!(result.candidate_count, 0, "no candidates for Certain");
}

#[test]
fn test_new_closure_replaces_superseded_focus_target() {
    let store = test_store();
    let file_id = test_file_id();
    let caller = test_symbol(file_id, "caller", SymbolKind::Function);
    let old_target = test_symbol(file_id, "oldTarget", SymbolKind::Function);
    let new_target = test_symbol(file_id, "newTarget", SymbolKind::Function);
    insert_symbols(
        &store,
        &[caller.clone(), old_target.clone(), new_target.clone()],
    );
    insert_closures(&store, &["cl_old", "cl_new"]);
    let ref_id = insert_reference(&store, caller.id, "target", ReferenceKind::Call);

    insert_visible_resolution(
        &store,
        "cl_old",
        1,
        &ref_id,
        &old_target.id,
        "boundary",
        "certain",
        "name_only",
    );
    let builder = FocusGraphBuilder::new(store.clone());
    builder.build_for_closure("cl_old", 1).unwrap();
    assert_canonical_edge_exists(&store, &caller.id, &old_target.id, EdgeKind::Calls);

    insert_visible_resolution(
        &store,
        "cl_new",
        1,
        &ref_id,
        &new_target.id,
        "boundary",
        "certain",
        "name_only",
    );
    builder.build_for_closure("cl_new", 1).unwrap();

    assert_no_canonical_edge(&store, &caller.id, &old_target.id, EdgeKind::Calls);
    assert_canonical_edge_exists(&store, &caller.id, &new_target.id, EdgeKind::Calls);
}

// ── T2: Medium confidence → candidate edges ─────────────────────────────────

#[test]
fn test_build_for_closure_candidate_edges() {
    let store = test_store();
    let file_id = test_file_id();

    let caller = test_symbol(file_id, "caller", SymbolKind::Function);
    let target = test_symbol(file_id, "targetFunc", SymbolKind::Function);
    insert_symbols(&store, &[caller.clone(), target.clone()]);
    insert_closures(&store, &["cl_med"]);

    let ref_id = insert_reference(&store, caller.id, "targetFunc", ReferenceKind::Call);

    insert_visible_resolution(
        &store,
        "cl_med",
        1,
        &ref_id,
        &target.id,
        "closure_complete",
        "medium",
        "closure_reachable",
    );

    let builder = FocusGraphBuilder::new(store.clone());
    let result = builder.build_for_closure("cl_med", 1).unwrap();

    assert!(result.stats.edges_built > 0, "should build candidate edges");
    assert_eq!(
        result.stats.edges_written, 0,
        "no canonical edges for Medium"
    );
    assert!(result.candidate_count > 0, "candidate edges expected");
    assert_no_canonical_edge(&store, &caller.id, &target.id, EdgeKind::Calls);
}

// ── T3: Low confidence → candidate, not canonical ───────────────────────────

#[test]
fn test_build_for_closure_low_confidence_not_persisted() {
    let store = test_store();
    let file_id = test_file_id();

    let caller = test_symbol(file_id, "caller", SymbolKind::Function);
    let target = test_symbol(file_id, "targetFunc", SymbolKind::Function);
    insert_symbols(&store, &[caller.clone(), target.clone()]);
    insert_closures(&store, &["cl_low"]);

    let ref_id = insert_reference(&store, caller.id, "targetFunc", ReferenceKind::Call);

    insert_visible_resolution(
        &store,
        "cl_low",
        1,
        &ref_id,
        &target.id,
        "closure_complete",
        "low",
        "closure_reachable",
    );

    let builder = FocusGraphBuilder::new(store.clone());
    let result = builder.build_for_closure("cl_low", 1).unwrap();

    // No canonical edges for Low confidence
    assert_eq!(result.stats.edges_written, 0, "no canonical edges for Low");
    // Low confidence goes to candidates via classify_new → KeepAsCandidates
    assert!(result.candidate_count > 0, "Low → candidate edge");
}

// ── T4: Certain edges are immutable ─────────────────────────────────────────

#[test]
fn test_build_for_closure_certain_edges_immutable() {
    let store = test_store();
    let file_id = test_file_id();

    let caller = test_symbol(file_id, "caller", SymbolKind::Function);
    let target = test_symbol(file_id, "targetFunc", SymbolKind::Function);
    insert_symbols(&store, &[caller.clone(), target.clone()]);
    insert_closures(&store, &["cl_certain", "cl_medium"]);

    let ref_id = insert_reference(&store, caller.id, "targetFunc", ReferenceKind::Call);

    // First: build with Certain → creates canonical edge
    insert_visible_resolution(
        &store,
        "cl_certain",
        1,
        &ref_id,
        &target.id,
        "closure_complete",
        "certain",
        "closure_reachable",
    );

    let builder = FocusGraphBuilder::new(store.clone());
    builder.build_for_closure("cl_certain", 1).unwrap();
    assert_canonical_edge_exists(&store, &caller.id, &target.id, EdgeKind::Calls);

    // Second: Medium for SAME reference
    insert_visible_resolution(
        &store,
        "cl_medium",
        2,
        &ref_id,
        &target.id,
        "closure_complete",
        "medium",
        "closure_reachable",
    );

    let result = builder.build_for_closure("cl_medium", 2).unwrap();

    // Certain edge should still be there
    assert_canonical_edge_exists(&store, &caller.id, &target.id, EdgeKind::Calls);
    // Medium incoming is KEPT (skipped) because Certain is immutable.
    // It is NOT written as candidate — it's simply dropped.
    assert_eq!(
        result.stats.edges_built, 0,
        "Medium should be skipped when Certain exists"
    );
    assert_eq!(result.candidate_count, 0, "no candidate for skipped Medium");
}

// ── T5: Empty resolutions → no edges ────────────────────────────────────────

#[test]
fn test_build_for_closure_empty_resolutions() {
    let store = test_store();
    insert_closures(&store, &["cl_empty"]);

    let builder = FocusGraphBuilder::new(store.clone());
    let result = builder.build_for_closure("cl_empty", 1).unwrap();

    assert_eq!(result.stats.edges_built, 0);
    assert_eq!(result.stats.edges_written, 0);
    assert_eq!(result.candidate_count, 0);
}

// ── T6: Staged (is_visible=0) resolutions are ignored ───────────────────────

#[test]
fn test_build_for_closure_staged_not_visible() {
    let store = test_store();
    let file_id = test_file_id();

    let caller = test_symbol(file_id, "caller", SymbolKind::Function);
    let target = test_symbol(file_id, "targetFunc", SymbolKind::Function);
    insert_symbols(&store, &[caller.clone(), target.clone()]);
    insert_closures(&store, &["cl_staged"]);

    let ref_id = insert_reference(&store, caller.id, "targetFunc", ReferenceKind::Call);

    // Insert staged but do NOT make visible
    store
        .insert_reference_resolution(
            ref_id.as_bytes(),
            "cl_staged",
            1,
            "closure_reachable",
            Some(target.id.as_bytes()),
            "closure_complete",
            "certain",
            "closure_reachable",
            Some("test"),
        )
        .unwrap();

    let builder = FocusGraphBuilder::new(store.clone());
    let result = builder.build_for_closure("cl_staged", 1).unwrap();

    assert_eq!(result.stats.edges_built, 0, "staged should be invisible");
}

// ── T7: Call reference → Calls edge kind ────────────────────────────────────

#[test]
fn test_build_for_closure_edge_kind_calls() {
    let store = test_store();
    let file_id = test_file_id();

    let caller = test_symbol(file_id, "caller", SymbolKind::Function);
    let target = test_symbol(file_id, "targetFunc", SymbolKind::Function);
    insert_symbols(&store, &[caller.clone(), target.clone()]);
    insert_closures(&store, &["cl_kind"]);

    let ref_id = insert_reference(&store, caller.id, "targetFunc", ReferenceKind::Call);

    insert_visible_resolution(
        &store,
        "cl_kind",
        1,
        &ref_id,
        &target.id,
        "closure_complete",
        "certain",
        "closure_reachable",
    );

    let builder = FocusGraphBuilder::new(store.clone());
    builder.build_for_closure("cl_kind", 1).unwrap();

    assert_canonical_edge_exists(&store, &caller.id, &target.id, EdgeKind::Calls);
    assert_no_canonical_edge(&store, &caller.id, &target.id, EdgeKind::References);
}

// ── T8: Multiple closures independent ───────────────────────────────────────

#[test]
fn test_build_for_closure_multiple_closures_independent() {
    let store = test_store();
    let file_id = test_file_id();

    let caller = test_symbol(file_id, "caller", SymbolKind::Function);
    let target = test_symbol(file_id, "targetFunc", SymbolKind::Function);
    insert_symbols(&store, &[caller.clone(), target.clone()]);
    insert_closures(&store, &["cl_a", "cl_b"]);

    let ref_id = insert_reference(&store, caller.id, "targetFunc", ReferenceKind::Call);

    insert_visible_resolution(
        &store,
        "cl_a",
        1,
        &ref_id,
        &target.id,
        "closure_complete",
        "certain",
        "closure_a",
    );

    let builder = FocusGraphBuilder::new(store.clone());
    builder.build_for_closure("cl_a", 1).unwrap();
    assert_canonical_edge_exists(&store, &caller.id, &target.id, EdgeKind::Calls);

    // Build cl_b (its incoming Certain edge conflicts with existing Certain → Keep)
    let result_b = builder.build_for_closure("cl_b", 1).unwrap();
    // Existing edge from cl_a should still exist
    assert_canonical_edge_exists(&store, &caller.id, &target.id, EdgeKind::Calls);
    assert!(result_b.stats.warnings.is_empty());
}

// ── T9: Usage reference → References edge kind ──────────────────────────────

#[test]
fn test_build_for_closure_edge_kind_references() {
    let store = test_store();
    let file_id = test_file_id();

    let caller = test_symbol(file_id, "main", SymbolKind::Function);
    let target = test_symbol(file_id, "SomeClass", SymbolKind::Class);
    insert_symbols(&store, &[caller.clone(), target.clone()]);
    insert_closures(&store, &["cl_ref"]);

    let ref_id = insert_reference(&store, caller.id, "SomeClass", ReferenceKind::Usage);

    insert_visible_resolution(
        &store,
        "cl_ref",
        1,
        &ref_id,
        &target.id,
        "closure_complete",
        "certain",
        "closure_reachable",
    );

    let builder = FocusGraphBuilder::new(store.clone());
    builder.build_for_closure("cl_ref", 1).unwrap();
    assert_canonical_edge_exists(&store, &caller.id, &target.id, EdgeKind::References);
}

// ── T10: Call reference → Class target → Instantiates edge ──────────────────

#[test]
fn test_build_for_closure_edge_kind_instantiates() {
    let store = test_store();
    let file_id = test_file_id();

    let caller = test_symbol(file_id, "factory", SymbolKind::Function);
    let target = test_symbol(file_id, "MyClass", SymbolKind::Class);
    insert_symbols(&store, &[caller.clone(), target.clone()]);
    insert_closures(&store, &["cl_inst"]);

    let ref_id = insert_reference(&store, caller.id, "MyClass", ReferenceKind::Call);

    insert_visible_resolution(
        &store,
        "cl_inst",
        1,
        &ref_id,
        &target.id,
        "closure_complete",
        "certain",
        "closure_reachable",
    );

    let builder = FocusGraphBuilder::new(store.clone());
    builder.build_for_closure("cl_inst", 1).unwrap();
    assert_canonical_edge_exists(&store, &caller.id, &target.id, EdgeKind::Instantiates);
}

// ── T11: Pipeline consistency — resolution crate writes snake_case ──────────

#[test]
fn test_build_for_closure_resolution_pipeline_consistency() {
    // This test verifies that when the resolution crate writes
    // coverage_tier info and the graph builder reads it, the
    // case matches correctly.
    let store = test_store();
    let file_id = test_file_id();

    let caller = test_symbol(file_id, "caller", SymbolKind::Function);
    let target = test_symbol(file_id, "targetFunc", SymbolKind::Function);
    insert_symbols(&store, &[caller.clone(), target.clone()]);
    insert_closures(&store, &["cl_pipe"]);

    let ref_id = insert_reference(&store, caller.id, "targetFunc", ReferenceKind::Call);

    // Simulate what the resolution crate writes — must be snake_case
    insert_visible_resolution(
        &store,
        "cl_pipe",
        1,
        &ref_id,
        &target.id,
        "closure_complete", // <-- must be snake_case (what resolution writes)
        "certain",
        "closure_reachable",
    );

    let builder = FocusGraphBuilder::new(store.clone());
    let result = builder.build_for_closure("cl_pipe", 1).unwrap();

    assert!(result.stats.edges_built > 0, "should build edges");
    assert_eq!(
        result.stats.edges_built, result.stats.edges_written,
        "ClosureComplete+Certain should produce canonical edges"
    );
}
