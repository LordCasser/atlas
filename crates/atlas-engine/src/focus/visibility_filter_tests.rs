//! Tests for language-specific visibility filters.

use super::visibility_filter::*;
use types::enums::{Language, SymbolKind, Visibility};
use types::ids::FileId;
use types::structs::SymbolDef;

// ── Helpers ────────────────────────────────────────────────────────────

fn sample_file_id(name: &str) -> FileId {
    FileId::generate(name)
}

fn make_symbol(
    file_id: FileId,
    name: &str,
    qualified: &str,
    kind: SymbolKind,
    language: Language,
    visibility: Option<Visibility>,
    exported: bool,
) -> SymbolDef {
    let id =
        types::ids::SymbolId::generate(&file_id, language.as_str(), qualified, kind.as_str(), None);
    SymbolDef {
        id,
        kind,
        name: name.to_string(),
        qualified_name: qualified.to_string(),
        symbol_path: qualified.split('.').map(String::from).collect(),
        file_id,
        language,
        range: types::structs::TextRange::default(),
        name_range: types::structs::TextRange::default(),
        signature: None,
        visibility,
        exported,
        static_: false,
        async_: false,
        container: None,
        scope_id: None,
        package_name: None,
        namespace_path: vec![],
        layer: "structural".to_string(),
    }
}

fn default_context(from_file: FileId) -> VisibilityContext {
    VisibilityContext {
        from_file,
        from_crate_root: None,
        target_crate_root: None,
    }
}

// ── C visibility ───────────────────────────────────────────────────────

#[test]
fn test_c_static_not_visible() {
    let fid = sample_file_id("src/a.c");
    let other_fid = sample_file_id("src/b.c");
    let sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::C,
        Some(Visibility::Private),
        false,
    );
    let filter = CVisibilityFilter;
    assert!(!filter.is_visible(&sym, other_fid, &default_context(other_fid)));
}

#[test]
fn test_c_public_visible() {
    let fid = sample_file_id("src/a.c");
    let other_fid = sample_file_id("src/b.c");
    let sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::C,
        Some(Visibility::Public),
        false,
    );
    let filter = CVisibilityFilter;
    assert!(filter.is_visible(&sym, other_fid, &default_context(other_fid)));
}

// ── Rust visibility ────────────────────────────────────────────────────

#[test]
fn test_rust_public_visible() {
    let fid = sample_file_id("src/a.rs");
    let other_fid = sample_file_id("src/b.rs");
    let sym = make_symbol(
        fid,
        "Helper",
        "Helper",
        SymbolKind::Function,
        Language::Rust,
        Some(Visibility::Public),
        false,
    );
    let filter = RustVisibilityFilter;
    assert!(filter.is_visible(&sym, other_fid, &default_context(other_fid)));
}

#[test]
fn test_rust_private_cross_file_not_visible() {
    let fid = sample_file_id("src/a.rs");
    let other_fid = sample_file_id("src/b.rs");
    let sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::Rust,
        Some(Visibility::Private),
        false,
    );
    let filter = RustVisibilityFilter;
    assert!(!filter.is_visible(&sym, other_fid, &default_context(other_fid)));
}

#[test]
fn test_rust_private_same_file_visible() {
    let fid = sample_file_id("src/a.rs");
    let sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::Rust,
        Some(Visibility::Private),
        false,
    );
    let filter = RustVisibilityFilter;
    assert!(filter.is_visible(&sym, fid, &default_context(fid)));
}

// ── TypeScript visibility ──────────────────────────────────────────────

#[test]
fn test_typescript_exported_visible() {
    let fid = sample_file_id("src/a.ts");
    let other_fid = sample_file_id("src/b.ts");
    let sym = make_symbol(
        fid,
        "Helper",
        "Helper",
        SymbolKind::Function,
        Language::TypeScript,
        Some(Visibility::Public),
        true,
    );
    let filter = TypeScriptVisibilityFilter;
    assert!(filter.is_visible(&sym, other_fid, &default_context(other_fid)));
}

#[test]
fn test_typescript_non_exported_not_visible() {
    let fid = sample_file_id("src/a.ts");
    let other_fid = sample_file_id("src/b.ts");
    let sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::TypeScript,
        Some(Visibility::Private),
        false,
    );
    let filter = TypeScriptVisibilityFilter;
    assert!(!filter.is_visible(&sym, other_fid, &default_context(other_fid)));
}

// ── Python visibility ──────────────────────────────────────────────────

#[test]
fn test_python_all_visible() {
    let fid = sample_file_id("src/a.py");
    let other_fid = sample_file_id("src/b.py");
    let sym = make_symbol(
        fid,
        "_private_helper",
        "_private_helper",
        SymbolKind::Function,
        Language::Python,
        None,
        false,
    );
    let filter = PythonVisibilityFilter;
    assert!(filter.is_visible(&sym, other_fid, &default_context(other_fid)));
}

// ── Go visibility ──────────────────────────────────────────────────────

#[test]
fn test_go_unexported_not_visible() {
    let fid = sample_file_id("src/a.go");
    let other_fid = sample_file_id("src/b.go");
    let sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::Go,
        Some(Visibility::Private),
        false,
    );
    let filter = GoVisibilityFilter;
    assert!(!filter.is_visible(&sym, other_fid, &default_context(other_fid)));
}

// ── Registry ───────────────────────────────────────────────────────────

#[test]
fn test_registry_get_returns_filter() {
    let registry = VisibilityFilterRegistry::new();
    let filter = registry.get(Language::Rust);
    assert_eq!(filter.language(), Language::Rust);
}

// ── Registry is_visible ───────────────────────────────────────────────

#[test]
fn test_registry_is_visible() {
    let registry = VisibilityFilterRegistry::new();
    let fid = sample_file_id("src/a.rs");
    let other_fid = sample_file_id("src/b.rs");

    // Public Rust symbol in file A should be visible from file B
    let pub_sym = make_symbol(
        fid,
        "PubFn",
        "PubFn",
        SymbolKind::Function,
        Language::Rust,
        Some(Visibility::Public),
        false,
    );
    assert!(registry.is_visible(&pub_sym, other_fid, &default_context(other_fid)));

    // Private Rust symbol should NOT be visible from different file
    let priv_sym = make_symbol(
        fid,
        "priv_fn",
        "priv_fn",
        SymbolKind::Function,
        Language::Rust,
        Some(Visibility::Private),
        false,
    );
    assert!(!registry.is_visible(&priv_sym, other_fid, &default_context(other_fid)));
}

// ── C: static inline ──────────────────────────────────────────────────

#[test]
fn test_c_static_inline_not_visible() {
    // C static inline functions map to Visibility::Private + static_:true.
    // They should be as invisible cross-file as regular static functions.
    let fid = sample_file_id("src/a.c");
    let other_fid = sample_file_id("src/b.c");
    let sym = make_symbol(
        fid,
        "inline_helper",
        "inline_helper",
        SymbolKind::Function,
        Language::C,
        Some(Visibility::Private),
        false,
    );
    // Set static_ flag true for the inline case.
    let mut sym = sym;
    sym.static_ = true;
    let filter = CVisibilityFilter;
    assert!(
        !filter.is_visible(&sym, other_fid, &default_context(other_fid)),
        "C static inline function should NOT be visible from another file"
    );
}

// ── C: same-file static ───────────────────────────────────────────────

#[test]
fn test_c_same_file_static_blocked() {
    // NOTE: Current C filter implementation blocks ALL static symbols,
    // even within the same file. This is by design for ClosureReachable
    // which operates at cross-file granularity.
    let fid = sample_file_id("src/a.c");
    let sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::C,
        Some(Visibility::Private),
        false,
    );
    let filter = CVisibilityFilter;
    assert!(
        !filter.is_visible(&sym, fid, &default_context(fid)),
        "C static function is blocked even from same file (cross-file focus design)"
    );
}

// ── C++ visibility ────────────────────────────────────────────────────

#[test]
fn test_cpp_static_not_visible() {
    let fid = sample_file_id("src/a.cpp");
    let other_fid = sample_file_id("src/b.cpp");
    let sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::Cpp,
        Some(Visibility::Private),
        false,
    );
    let filter = CppVisibilityFilter;
    assert!(
        !filter.is_visible(&sym, other_fid, &default_context(other_fid)),
        "C++ static (private) function should NOT be visible from another file"
    );
}

#[test]
fn test_cpp_public_visible() {
    let fid = sample_file_id("src/a.cpp");
    let other_fid = sample_file_id("src/b.cpp");
    let sym = make_symbol(
        fid,
        "Helper",
        "Helper",
        SymbolKind::Function,
        Language::Cpp,
        Some(Visibility::Public),
        false,
    );
    let filter = CppVisibilityFilter;
    assert!(
        filter.is_visible(&sym, other_fid, &default_context(other_fid)),
        "C++ public function should be visible from another file"
    );
}

// ── Rust: pub(crate) ──────────────────────────────────────────────────

#[test]
fn test_rust_pub_crate_same_crate_visible() {
    let dir = sample_file_id("src");
    let fid = sample_file_id("src/a.rs");
    let other_fid = sample_file_id("src/b.rs");
    let sym = make_symbol(
        fid,
        "helper",
        "my_crate::helper",
        SymbolKind::Function,
        Language::Rust,
        Some(Visibility::Internal),
        false,
    );
    let filter = RustVisibilityFilter;
    let context = VisibilityContext {
        from_file: other_fid,
        from_crate_root: Some(dir),
        target_crate_root: Some(dir),
    };
    assert!(
        filter.is_visible(&sym, other_fid, &context),
        "pub(crate) function should be visible within the same crate"
    );
}

#[test]
fn test_rust_pub_crate_different_crate_not_visible() {
    let crate_a = sample_file_id("crate_a");
    let crate_b = sample_file_id("crate_b");
    let fid = sample_file_id("crate_a/src/lib.rs");
    let other_fid = sample_file_id("crate_b/src/main.rs");
    let sym = make_symbol(
        fid,
        "helper",
        "my_crate::helper",
        SymbolKind::Function,
        Language::Rust,
        Some(Visibility::Internal),
        false,
    );
    let filter = RustVisibilityFilter;
    let context = VisibilityContext {
        from_file: other_fid,
        from_crate_root: Some(crate_b),
        target_crate_root: Some(crate_a),
    };
    assert!(
        !filter.is_visible(&sym, other_fid, &context),
        "pub(crate) function should NOT be visible from a different crate"
    );
}

// ── Go: exported ──────────────────────────────────────────────────────

#[test]
fn test_go_exported_visible() {
    let fid = sample_file_id("src/a.go");
    let other_fid = sample_file_id("src/b.go");
    let sym = make_symbol(
        fid,
        "Helper",
        "Helper",
        SymbolKind::Function,
        Language::Go,
        Some(Visibility::Public),
        false,
    );
    let filter = GoVisibilityFilter;
    assert!(
        filter.is_visible(&sym, other_fid, &default_context(other_fid)),
        "Go exported (public) function should be visible from another file"
    );
}

// ── TypeScript: protected ─────────────────────────────────────────────

#[test]
fn test_typescript_protected_visible() {
    // NOTE: Current TypeScript filter treats Protected as VISIBLE.
    // This is a conservative policy: TypeScript `protected` class members
    // are visible to subclasses regardless of file. The filter allows them
    // to avoid false negatives in closure analysis.
    let fid = sample_file_id("src/a.ts");
    let other_fid = sample_file_id("src/b.ts");
    let sym = make_symbol(
        fid,
        "Helper",
        "Helper",
        SymbolKind::Function,
        Language::TypeScript,
        Some(Visibility::Protected),
        false,
    );
    let filter = TypeScriptVisibilityFilter;
    assert!(
        filter.is_visible(&sym, other_fid, &default_context(other_fid)),
        "TypeScript protected symbol is visible (conservative policy)"
    );
}

// ── Registry: permissive fallback ─────────────────────────────────────

#[test]
fn test_registry_fallback_permissive() {
    let registry = VisibilityFilterRegistry::new();
    let filter = registry.get(Language::Kotlin);
    assert_eq!(filter.language(), Language::Kotlin);

    let fid = sample_file_id("src/a.kt");
    let other_fid = sample_file_id("src/b.kt");
    let sym = make_symbol(
        fid,
        "privateHelper",
        "privateHelper",
        SymbolKind::Function,
        Language::Kotlin,
        Some(Visibility::Private),
        false,
    );
    // Permissive filter should make everything visible
    assert!(
        filter.is_visible(&sym, other_fid, &default_context(other_fid)),
        "Kotlin (fallback permissive) filter should make all symbols visible"
    );
}

// ── Null visibility handling ──────────────────────────────────────────

#[test]
fn test_null_visibility_handling() {
    // When SymbolDef.visibility is None, behavior varies by filter.
    let fid = sample_file_id("src/a.c");
    let other_fid = sample_file_id("src/b.c");

    // C: None visibility → NOT Private → IS visible (permissive for unknown)
    let c_sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::C,
        None,
        false,
    );
    let c_filter = CVisibilityFilter;
    assert!(
        c_filter.is_visible(&c_sym, other_fid, &default_context(other_fid)),
        "C filter: None visibility → visible (not matched as Private)"
    );

    // Rust: None visibility treated same as Private → same-file visible, cross-file not
    let rust_sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::Rust,
        None,
        false,
    );
    let rust_filter = RustVisibilityFilter;
    assert!(
        rust_filter.is_visible(&rust_sym, fid, &default_context(fid)),
        "Rust filter: None visibility + same file → visible (Private-like)"
    );
    assert!(
        !rust_filter.is_visible(&rust_sym, other_fid, &default_context(other_fid)),
        "Rust filter: None visibility + cross-file → NOT visible (Private-like)"
    );

    // Python: permissive → always visible
    let py_sym = make_symbol(
        fid,
        "helper",
        "helper",
        SymbolKind::Function,
        Language::Python,
        None,
        false,
    );
    let py_filter = PythonVisibilityFilter;
    assert!(
        py_filter.is_visible(&py_sym, other_fid, &default_context(other_fid)),
        "Python filter: None visibility → always visible (permissive)"
    );
}
