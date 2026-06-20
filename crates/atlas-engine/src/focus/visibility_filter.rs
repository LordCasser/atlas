//! Language-specific visibility filters for ClosureReachableSymbols.
//!
//! When building a focus closure, not all symbols in all closure files are
//! reachable. Each language has visibility rules (e.g., C `static` functions
//! are file-local; Rust `private` items are module-local).
//!
//! These filters prevent false-positive resolution matches: a reference in file A
//! should NEVER resolve to a `static` function in file B, even if both files are
//! in the same closure.

use std::collections::HashMap;

use types::enums::{Language, Visibility};
use types::ids::FileId;
use types::structs::SymbolDef;

/// Context for visibility checks — module/crate/package membership.
#[derive(Debug, Clone)]
pub struct VisibilityContext {
    /// The file making the reference.
    pub from_file: FileId,
    /// Known crate/module root for the from_file (language-specific).
    pub from_crate_root: Option<FileId>,
    /// Known crate/module root for the target file.
    pub target_crate_root: Option<FileId>,
}

/// Determines whether a symbol in a closure is reachable (visible) from
/// a reference point, given language-specific visibility rules.
pub trait VisibilityFilter: Send + Sync {
    /// Returns true if `symbol` (in its file) is visible from `from_file`.
    fn is_visible(
        &self,
        symbol: &SymbolDef,
        from_file: FileId,
        context: &VisibilityContext,
    ) -> bool;

    /// The language this filter applies to.
    fn language(&self) -> Language;
}

// ── C / C++ ────────────────────────────────────────────────────────────

pub struct CVisibilityFilter;

impl VisibilityFilter for CVisibilityFilter {
    fn is_visible(
        &self,
        symbol: &SymbolDef,
        _from_file: FileId,
        _context: &VisibilityContext,
    ) -> bool {
        // C: static functions and static file-scope variables are
        // visible ONLY within their translation unit.
        // We approximate "translation unit" as "same file."
        // For now: exclude any static symbol from cross-file visibility.
        //
        // Later refinement: same-file static should be visible.
        // But ClosureReachable is about cross-file, so exclude all static.
        !matches!(symbol.visibility, Some(Visibility::Private))
        // Note: In C, 'static' maps to Visibility::Private in our IR.
    }

    fn language(&self) -> Language {
        Language::C
    }
}

pub struct CppVisibilityFilter;

impl VisibilityFilter for CppVisibilityFilter {
    fn is_visible(
        &self,
        symbol: &SymbolDef,
        _from_file: FileId,
        _context: &VisibilityContext,
    ) -> bool {
        // C++: as C, plus anonymous namespaces (which are file-local).
        // Anonymous namespace symbols also get Visibility::Private in IR.
        !matches!(symbol.visibility, Some(Visibility::Private))
    }

    fn language(&self) -> Language {
        Language::Cpp
    }
}

// ── Rust ───────────────────────────────────────────────────────────────

pub struct RustVisibilityFilter;

impl VisibilityFilter for RustVisibilityFilter {
    fn is_visible(
        &self,
        symbol: &SymbolDef,
        from_file: FileId,
        context: &VisibilityContext,
    ) -> bool {
        match symbol.visibility {
            Some(Visibility::Public) => true,
            Some(Visibility::Private) | None => {
                // Private (or unknown): only visible within the same module.
                // Approximate "same module" as same-file for now.
                symbol.file_id == from_file
            }
            Some(Visibility::Internal) => {
                // pub(crate): visible within same crate.
                crate_match(context.from_crate_root, context.target_crate_root)
            }
            Some(Visibility::Protected) => {
                // Not applicable in Rust; treat as Private.
                symbol.file_id == from_file
            }
            Some(Visibility::Package) => {
                // Not applicable in Rust; treat as Internal.
                crate_match(context.from_crate_root, context.target_crate_root)
            }
        }
    }

    fn language(&self) -> Language {
        Language::Rust
    }
}

// ── TypeScript / JavaScript ────────────────────────────────────────────

pub struct TypeScriptVisibilityFilter;

impl VisibilityFilter for TypeScriptVisibilityFilter {
    fn is_visible(
        &self,
        symbol: &SymbolDef,
        _from_file: FileId,
        _context: &VisibilityContext,
    ) -> bool {
        // TypeScript: only exported symbols are visible outside their module.
        // Non-exported = Visibility::Private in IR.
        // Note: TypeScript `protected` class members (Visibility::Protected)
        // are visible to subclasses regardless of file. For closure analysis
        // we conservatively make them visible to avoid false negatives.
        if symbol.exported {
            return true;
        }
        matches!(
            symbol.visibility,
            Some(Visibility::Public) | Some(Visibility::Protected)
        )
    }

    fn language(&self) -> Language {
        Language::TypeScript
    }
}

// ── Python ─────────────────────────────────────────────────────────────

pub struct PythonVisibilityFilter;

impl VisibilityFilter for PythonVisibilityFilter {
    fn is_visible(
        &self,
        _symbol: &SymbolDef,
        _from_file: FileId,
        _context: &VisibilityContext,
    ) -> bool {
        // Python: all module-level names are public.
        // _private by convention is still importable.
        // We make everything visible — Python doesn't enforce visibility.
        true
    }

    fn language(&self) -> Language {
        Language::Python
    }
}

// ── Go ──────────────────────────────────────────────────────────────────

pub struct GoVisibilityFilter;

impl VisibilityFilter for GoVisibilityFilter {
    fn is_visible(
        &self,
        symbol: &SymbolDef,
        _from_file: FileId,
        _context: &VisibilityContext,
    ) -> bool {
        // Go: exported (Capitalized) identifiers are Public.
        // unexported (lowercase) identifiers are Private.
        // Non-exported = invisible across packages.
        symbol.visibility == Some(Visibility::Public)
    }

    fn language(&self) -> Language {
        Language::Go
    }
}

// ── Default (permissive) ───────────────────────────────────────────────

/// Default filter: everything is visible. Used for languages without
/// specialized visibility rules.
pub struct PermissiveVisibilityFilter {
    lang: Language,
}

impl PermissiveVisibilityFilter {
    pub fn new(language: Language) -> Self {
        PermissiveVisibilityFilter { lang: language }
    }
}

impl VisibilityFilter for PermissiveVisibilityFilter {
    fn is_visible(
        &self,
        _symbol: &SymbolDef,
        _from_file: FileId,
        _context: &VisibilityContext,
    ) -> bool {
        true
    }

    fn language(&self) -> Language {
        self.lang
    }
}

// ── Registry ───────────────────────────────────────────────────────────

/// Registry that maps languages to their visibility filters.
pub struct VisibilityFilterRegistry {
    filters: HashMap<Language, Box<dyn VisibilityFilter>>,
}

impl VisibilityFilterRegistry {
    /// Create a registry with all known language filters.
    /// Languages without specialized filters get a permissive default.
    pub fn new() -> Self {
        let mut filters: HashMap<Language, Box<dyn VisibilityFilter>> = HashMap::new();

        // Specialized language filters
        filters.insert(Language::C, Box::new(CVisibilityFilter));
        filters.insert(Language::Cpp, Box::new(CppVisibilityFilter));
        filters.insert(Language::Rust, Box::new(RustVisibilityFilter));
        filters.insert(Language::TypeScript, Box::new(TypeScriptVisibilityFilter));
        filters.insert(Language::JavaScript, Box::new(TypeScriptVisibilityFilter));
        filters.insert(Language::Python, Box::new(PythonVisibilityFilter));
        filters.insert(Language::Go, Box::new(GoVisibilityFilter));

        // Permissive fallback for all remaining languages
        for lang in Language::all() {
            filters
                .entry(lang)
                .or_insert_with(|| Box::new(PermissiveVisibilityFilter::new(lang)));
        }

        Self { filters }
    }

    /// Get the filter for a language. Falls back to permissive default if the
    /// language is not registered (e.g., when Language enum is extended without
    /// adding a corresponding entry to LanguageExt::all()).
    pub fn get(&self, language: Language) -> &dyn VisibilityFilter {
        self.filters
            .get(&language)
            .map(|b| b.as_ref())
            .unwrap_or_else(|| {
                tracing::warn!(
                    "VisibilityFilterRegistry: language {:?} not registered, using permissive fallback",
                    language
                );
                self.filters.get(&Language::TypeScript)
                    .map(|b| b.as_ref())
                    .expect("TypeScript filter must always be registered")
            })
    }

    /// Check if a symbol is visible, given the language and context.
    pub fn is_visible(
        &self,
        symbol: &SymbolDef,
        from_file: FileId,
        context: &VisibilityContext,
    ) -> bool {
        let filter = self.get(symbol.language);
        filter.is_visible(symbol, from_file, context)
    }
}

impl Default for VisibilityFilterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn crate_match(a: Option<FileId>, b: Option<FileId>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Extension trait to list all Language variants for registry pre-population.
trait LanguageExt {
    fn all() -> Vec<Language>;
}

impl LanguageExt for Language {
    fn all() -> Vec<Language> {
        vec![
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::ArkTS,
            Language::Cangjie,
            Language::Go,
            Language::CSharp,
            Language::Rust,
            Language::Php,
            Language::Ruby,
            Language::Kotlin,
        ]
    }
}
