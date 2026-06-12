//! Tests for `ReferenceResolver::resolve_for_closure()` — scoped resolution
//! that writes to `reference_resolutions` table without mutating the global
//! `references.resolved_*` columns.

use std::sync::Arc;

use db::Store;
use resolution::ReferenceResolver;
use types::enums::{
    Language, ParseStatus, ReferenceKind, SymbolKind, Visibility,
};
use types::ids::{FileId, ReferenceId, SymbolId};
use types::structs::{FileFacts, FileInfo, ReferenceUse, SymbolDef, TextRange};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn test_store() -> Arc<Store> {
    let store = Store::open_in_memory().unwrap();
    store.init_schema().unwrap();
    Arc::new(store)
}

fn default_range() -> TextRange {
    TextRange {
        start_byte: 0,
        end_byte: 10,
        start_line: 1,
        start_column: 0,
        end_line: 1,
        end_column: 10,
    }
}

fn make_symbol(
    file_id: FileId,
    name: &str,
    qualified: &str,
    kind: SymbolKind,
    language: Language,
    visibility: Option<Visibility>,
) -> SymbolDef {
    let range = default_range();
    let id = SymbolId::generate(&file_id, language.as_str(), qualified, kind.as_str(), None);
    SymbolDef {
        id,
        kind,
        name: name.to_string(),
        qualified_name: qualified.to_string(),
        symbol_path: qualified.split('.').map(String::from).collect(),
        file_id,
        language,
        range,
        name_range: range,
        signature: None,
        visibility,
        exported: false,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".to_string(),
    }
}

fn make_reference(
    file_id: FileId,
    name: &str,
    source_symbol: Option<SymbolId>,
    kind: ReferenceKind,
) -> ReferenceUse {
    let text = name.to_string();
    let range = default_range();
    let id = ReferenceId::generate(&file_id, source_symbol.as_ref(), 0, 10, &text, kind);
    ReferenceUse {
        id,
        file_id,
        source_symbol,
        scope_id: None,
        kind,
        text: text.clone(),
        name: text,
        receiver: None,
        arity: None,
        range,
        binding_id: None,
        resolved: None,
    }
}

fn make_file_facts(
    file_id: FileId,
    path: &str,
    language: Language,
    symbols: Vec<SymbolDef>,
    references: Vec<ReferenceUse>,
) -> FileFacts {
    FileFacts {
        file: FileInfo {
            file_id,
            path: path.to_string(),
            language,
            content_hash: "test_hash".to_string(),
            status: ParseStatus::Success,
        },
        symbols,
        references,
        ..Default::default()
    }
}

fn insert_closure_generation(store: &Store, closure_id: &str) {
    store.insert_closure_generation(closure_id).unwrap();
}

// ── Test 1: resolve_for_closure writes to reference_resolutions, NOT references.resolved_* ──

#[test]
fn test_resolve_for_closure_writes_to_reference_resolutions_not_references() {
    let store = test_store();

    let lib_id = FileId::generate("lib.ts");
    let greet_sym = make_symbol(
        lib_id,
        "greet",
        "greet",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    let lib_facts =
        make_file_facts(lib_id, "lib.ts", Language::TypeScript, vec![greet_sym.clone()], vec![]);
    store.insert_file_facts(&lib_facts).unwrap();

    let main_id = FileId::generate("main.ts");
    let main_sym = make_symbol(
        main_id,
        "main",
        "main",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    let ref_to_greet = make_reference(main_id, "greet", Some(main_sym.id), ReferenceKind::Call);
    let main_facts = make_file_facts(
        main_id,
        "main.ts",
        Language::TypeScript,
        vec![main_sym],
        vec![ref_to_greet.clone()],
    );
    store.insert_file_facts(&main_facts).unwrap();

    insert_closure_generation(&store, "cl_test1");

    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved, _stats) = resolver
        .resolve_for_closure("cl_test1", 1, &[lib_id, main_id], None)
        .expect("resolve_for_closure failed");

    // (a) Reference was resolved
    assert!(!resolved.is_empty(), "expected at least one resolved reference");

    // (a) Rows exist in reference_resolutions table
    let count = store.count_reference_resolutions("cl_test1", 1).unwrap();
    assert!(count > 0, "expected rows in reference_resolutions table");

    // (b) references.resolved_symbol_id is still NULL
    let refs_in_db = store
        .find_references_by_file(&main_id)
        .unwrap();
    let greet_ref = refs_in_db
        .iter()
        .find(|r| r.name == "greet")
        .expect("greet reference not found");
    assert!(
        greet_ref.resolved.is_none(),
        "references.resolved should remain None — scoped resolution must not modify references table"
    );
}

// ── Test 2: resolve_for_closure inserts staged (is_visible=0) rows ───────────

#[test]
fn test_resolve_for_closure_staged_by_default() {
    let store = test_store();

    let lib_id = FileId::generate("lib.ts");
    let greet_sym = make_symbol(
        lib_id,
        "greet",
        "greet",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    store
        .insert_file_facts(&make_file_facts(
            lib_id,
            "lib.ts",
            Language::TypeScript,
            vec![greet_sym],
            vec![],
        ))
        .unwrap();

    let main_id = FileId::generate("main.ts");
    let main_sym = make_symbol(
        main_id,
        "main",
        "main",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    let ref_to_greet = make_reference(main_id, "greet", Some(main_sym.id), ReferenceKind::Call);
    store
        .insert_file_facts(&make_file_facts(
            main_id,
            "main.ts",
            Language::TypeScript,
            vec![main_sym],
            vec![ref_to_greet.clone()],
        ))
        .unwrap();

    insert_closure_generation(&store, "cl_staged");

    let mut resolver = ReferenceResolver::new(store.clone());
    resolver
        .resolve_for_closure("cl_staged", 1, &[lib_id, main_id], None)
        .expect("resolve_for_closure failed");

    // Count includes staged rows
    let count = store.count_reference_resolutions("cl_staged", 1).unwrap();
    assert!(count > 0, "expected staged rows in reference_resolutions");

    // Staged rows are NOT visible
    let visible = store
        .get_visible_resolution(ref_to_greet.id.as_bytes(), "cl_staged")
        .unwrap();
    assert!(
        visible.is_empty(),
        "staged resolutions should have is_visible=0"
    );
}

// ── Test 3: make_resolutions_visible flips staged to visible ─────────────────

#[test]
fn test_make_resolutions_visible() {
    let store = test_store();

    let lib_id = FileId::generate("lib.ts");
    let greet_sym = make_symbol(
        lib_id,
        "greet",
        "greet",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    store
        .insert_file_facts(&make_file_facts(
            lib_id,
            "lib.ts",
            Language::TypeScript,
            vec![greet_sym],
            vec![],
        ))
        .unwrap();

    let main_id = FileId::generate("main.ts");
    let main_sym = make_symbol(
        main_id,
        "main",
        "main",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    let ref_to_greet = make_reference(main_id, "greet", Some(main_sym.id), ReferenceKind::Call);
    store
        .insert_file_facts(&make_file_facts(
            main_id,
            "main.ts",
            Language::TypeScript,
            vec![main_sym],
            vec![ref_to_greet.clone()],
        ))
        .unwrap();

    insert_closure_generation(&store, "cl_visible");

    let mut resolver = ReferenceResolver::new(store.clone());
    resolver
        .resolve_for_closure("cl_visible", 1, &[lib_id, main_id], None)
        .expect("resolve_for_closure failed");

    // Verify staged
    assert!(
        store
            .get_visible_resolution(ref_to_greet.id.as_bytes(), "cl_visible")
            .unwrap()
            .is_empty(),
        "should be staged initially"
    );

    // Make visible
    let updated = store
        .make_resolutions_visible("cl_visible", 1)
        .unwrap();
    assert!(updated > 0, "expected rows to become visible");

    // Now visible
    let visible = store
        .get_visible_resolution(&ref_to_greet.id.as_bytes().to_vec(), "cl_visible")
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert!(visible[0].is_visible);
}

// ── Test 4: resolve_for_closure with multiple files ──────────────────────────

#[test]
fn test_resolve_for_closure_multiple_files() {
    let store = test_store();

    // File A: defines helper()
    let file_a_id = FileId::generate("file_a.ts");
    let helper_sym = make_symbol(
        file_a_id,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    store
        .insert_file_facts(&make_file_facts(
            file_a_id,
            "file_a.ts",
            Language::TypeScript,
            vec![helper_sym],
            vec![],
        ))
        .unwrap();

    // File B: defines worker() and calls helper()
    let file_b_id = FileId::generate("file_b.ts");
    let worker_sym = make_symbol(
        file_b_id,
        "worker",
        "worker",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    let ref_b_helper =
        make_reference(file_b_id, "helper", Some(worker_sym.id), ReferenceKind::Call);
    store
        .insert_file_facts(&make_file_facts(
            file_b_id,
            "file_b.ts",
            Language::TypeScript,
            vec![worker_sym],
            vec![ref_b_helper.clone()],
        ))
        .unwrap();

    // File C: defines main() and calls worker()
    let file_c_id = FileId::generate("file_c.ts");
    let main_sym = make_symbol(
        file_c_id,
        "main",
        "main",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    let ref_c_worker =
        make_reference(file_c_id, "worker", Some(main_sym.id), ReferenceKind::Call);
    store
        .insert_file_facts(&make_file_facts(
            file_c_id,
            "file_c.ts",
            Language::TypeScript,
            vec![main_sym],
            vec![ref_c_worker.clone()],
        ))
        .unwrap();

    insert_closure_generation(&store, "cl_multi");

    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved, _stats) = resolver
        .resolve_for_closure("cl_multi", 1, &[file_a_id, file_b_id, file_c_id], None)
        .expect("resolve_for_closure failed");

    // Both cross-file references should resolve
    assert!(
        resolved.len() >= 2,
        "expected at least 2 resolved references, got {}",
        resolved.len()
    );

    let count = store.count_reference_resolutions("cl_multi", 1).unwrap();
    assert_eq!(
        count as usize, resolved.len(),
        "reference_resolutions row count should match resolved pairs"
    );
}

// ── Test 5: Empty closure returns no results ─────────────────────────────────

#[test]
fn test_resolve_for_closure_empty_closure() {
    let store = test_store();
    insert_closure_generation(&store, "cl_empty");

    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved, _stats) = resolver
        .resolve_for_closure("cl_empty", 1, &[], None)
        .expect("resolve_for_closure failed");

    assert!(resolved.is_empty(), "empty closure should produce no resolved pairs");

    let count = store.count_reference_resolutions("cl_empty", 1).unwrap();
    assert_eq!(count, 0, "empty closure should insert 0 rows");
}

// ── Test 6: Different closures have independent rows ─────────────────────────

#[test]
fn test_resolve_for_closure_different_closures_independent() {
    let store = test_store();

    let lib_id = FileId::generate("lib.ts");
    let greet_sym = make_symbol(
        lib_id,
        "greet",
        "greet",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    store
        .insert_file_facts(&make_file_facts(
            lib_id,
            "lib.ts",
            Language::TypeScript,
            vec![greet_sym],
            vec![],
        ))
        .unwrap();

    let main_id = FileId::generate("main.ts");
    let main_sym = make_symbol(
        main_id,
        "main",
        "main",
        SymbolKind::Function,
        Language::TypeScript,
        None,
    );
    let ref_to_greet = make_reference(main_id, "greet", Some(main_sym.id), ReferenceKind::Call);
    store
        .insert_file_facts(&make_file_facts(
            main_id,
            "main.ts",
            Language::TypeScript,
            vec![main_sym],
            vec![ref_to_greet],
        ))
        .unwrap();

    insert_closure_generation(&store, "cl_A");
    insert_closure_generation(&store, "cl_B");

    let mut resolver = ReferenceResolver::new(store.clone());

    // Closure A
    resolver
        .resolve_for_closure("cl_A", 1, &[lib_id, main_id], None)
        .unwrap();
    let count_a = store.count_reference_resolutions("cl_A", 1).unwrap();

    // Closure B on same files with different generation
    resolver
        .resolve_for_closure("cl_B", 5, &[lib_id, main_id], None)
        .unwrap();
    let count_b = store.count_reference_resolutions("cl_B", 5).unwrap();

    assert!(count_a > 0, "closure A should have rows");
    assert!(count_b > 0, "closure B should have rows");

    // Different closures should have independent rows
    assert_ne!(
        store.count_reference_resolutions("cl_A", 5).unwrap_or(0),
        count_b,
        "closure B generation should not affect closure A"
    );
}

// ── Test 7: C visibility filter excludes static functions ────────────────────

#[test]
fn test_visibility_filter_c_static() {
    let store = test_store();

    let file_a_id = FileId::generate("file_a.c");
    // Static helper — not visible from other files
    let static_helper = make_symbol(
        file_a_id,
        "do_stuff",
        "do_stuff",
        SymbolKind::Function,
        Language::C,
        Some(Visibility::Private), // C static → Private
    );
    // Public API — visible everywhere
    let public_api = make_symbol(
        file_a_id,
        "public_api",
        "public_api",
        SymbolKind::Function,
        Language::C,
        Some(Visibility::Public),
    );
    store
        .insert_file_facts(&make_file_facts(
            file_a_id,
            "file_a.c",
            Language::C,
            vec![static_helper.clone(), public_api.clone()],
            vec![],
        ))
        .unwrap();

    // File B references both functions
    let file_b_id = FileId::generate("file_b.c");
    let caller_sym = make_symbol(
        file_b_id,
        "caller",
        "caller",
        SymbolKind::Function,
        Language::C,
        None,
    );
    let ref_static = make_reference(file_b_id, "do_stuff", Some(caller_sym.id), ReferenceKind::Call);
    let ref_public =
        make_reference(file_b_id, "public_api", Some(caller_sym.id), ReferenceKind::Call);
    store
        .insert_file_facts(&make_file_facts(
            file_b_id,
            "file_b.c",
            Language::C,
            vec![caller_sym],
            vec![ref_static.clone(), ref_public.clone()],
        ))
        .unwrap();

    insert_closure_generation(&store, "cl_c_vis");

    // C visibility filter: exclude Private (static) symbols
    let visibility_filter: &dyn Fn(&SymbolDef, FileId) -> bool =
        &|sym: &SymbolDef, _from_file: FileId| -> bool {
            sym.visibility != Some(Visibility::Private)
        };

    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved, _stats) = resolver
        .resolve_for_closure(
            "cl_c_vis",
            1,
            &[file_a_id, file_b_id],
            Some(visibility_filter),
        )
        .expect("resolve_for_closure failed");

    // public_api should be resolved
    let public_resolved = resolved
        .iter()
        .any(|(r, t)| r.name == "public_api" && t.symbol_id == public_api.id);
    assert!(
        public_resolved,
        "public_api should be resolved (Visibility::Public)"
    );

    // do_stuff should NOT be resolved (Visibility::Private = C static)
    let static_resolved = resolved.iter().any(|(r, _)| r.name == "do_stuff");
    assert!(
        !static_resolved,
        "do_stuff (static) should NOT be resolved from file_b.c (visibility filter)"
    );
}

// ── Test 8: Rust visibility filter excludes private cross-file ───────────────

#[test]
fn test_visibility_filter_rust_private() {
    let store = test_store();

    let file_a_id = FileId::generate("crate_a/src/lib.rs");
    // Private function in crate A — not visible from crate B
    let private_fn = make_symbol(
        file_a_id,
        "internal_helper",
        "internal_helper",
        SymbolKind::Function,
        Language::Rust,
        Some(Visibility::Private),
    );
    // Public function in crate A
    let public_fn = make_symbol(
        file_a_id,
        "public_fn",
        "public_fn",
        SymbolKind::Function,
        Language::Rust,
        Some(Visibility::Public),
    );
    store
        .insert_file_facts(&make_file_facts(
            file_a_id,
            "crate_a/src/lib.rs",
            Language::Rust,
            vec![private_fn.clone(), public_fn.clone()],
            vec![],
        ))
        .unwrap();

    // File B in a different crate
    let file_b_id = FileId::generate("crate_b/src/main.rs");
    let main_sym = make_symbol(
        file_b_id,
        "main",
        "main",
        SymbolKind::Function,
        Language::Rust,
        None,
    );
    let ref_private =
        make_reference(file_b_id, "internal_helper", Some(main_sym.id), ReferenceKind::Call);
    let ref_public =
        make_reference(file_b_id, "public_fn", Some(main_sym.id), ReferenceKind::Call);
    store
        .insert_file_facts(&make_file_facts(
            file_b_id,
            "crate_b/src/main.rs",
            Language::Rust,
            vec![main_sym],
            vec![ref_private.clone(), ref_public.clone()],
        ))
        .unwrap();

    insert_closure_generation(&store, "cl_rust_vis");

    // Rust visibility filter: private symbols only visible within the same file
    let visibility_filter: &dyn Fn(&SymbolDef, FileId) -> bool =
        &|sym: &SymbolDef, from_file: FileId| -> bool {
            match sym.visibility {
                Some(Visibility::Public) => true,
                Some(Visibility::Private) | None => sym.file_id == from_file,
                _ => true, // Internal/Protected/Package visible for MVP
            }
        };

    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved, _stats) = resolver
        .resolve_for_closure(
            "cl_rust_vis",
            1,
            &[file_a_id, file_b_id],
            Some(visibility_filter),
        )
        .expect("resolve_for_closure failed");

    // public_fn should be resolved
    let public_resolved = resolved
        .iter()
        .any(|(r, t)| r.name == "public_fn" && t.symbol_id == public_fn.id);
    assert!(
        public_resolved,
        "public_fn should be resolved (Visibility::Public)"
    );

    // internal_helper should NOT be resolved (private in different file)
    let private_resolved = resolved
        .iter()
        .any(|(r, _)| r.name == "internal_helper");
    assert!(
        !private_resolved,
        "internal_helper (private) should NOT be resolved from crate_b (visibility filter)"
    );
}
