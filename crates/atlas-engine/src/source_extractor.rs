//! Source Extractor — extracts symbol source code using tree-sitter AST
//! boundaries instead of relying on pre-computed `TextRange` metadata.
//!
//! # Motivation
//!
//! The `SymbolDef.range` stored in the database comes from the *capture node*
//! (usually the identifier token), not the enclosing definition node.  While
//! step 7z in `extract.rs` expands function ranges to their scope boundaries
//! during structural extraction, manifest-only symbols retain narrow ranges,
//! and even structural ranges may differ from the actual tree-sitter
//! definition node.
//!
//! `SourceExtractor` side-steps these issues by re-parsing the file with
//! tree-sitter and walking the CST from the symbol's byte position upward to
//! find the enclosing definition node — ensuring the extracted source is
//! always the exact, complete function/class/struct body.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use db::Store;
use types::{Language, SymbolDef, SymbolId, SymbolKind};

use extraction::create_frontend;

// ─── Thread-local parser (avoids re-allocating per call) ──────────────────

thread_local! {
    static TL_PARSER: RefCell<Option<tree_sitter::Parser>> = const { RefCell::new(None) };
}

// ─── SourceExtractor ──────────────────────────────────────────────────────

/// Extracts precise source code for symbols using tree-sitter AST boundaries.
///
/// # Usage
///
/// ```ignore
/// let extractor = SourceExtractor::new(store, project_root);
/// let source: Option<String> = extractor.extract_source(&symbol_id);
/// ```
#[derive(Clone)]
pub struct SourceExtractor {
    store: Arc<Store>,
    project_root: PathBuf,
}

impl SourceExtractor {
    pub fn new(store: Arc<Store>, project_root: PathBuf) -> Self {
        Self {
            store,
            project_root,
        }
    }

    /// Extract the exact source code for a symbol.
    ///
    /// # Strategy
    ///
    /// 1. Resolve symbol metadata and file path from the store.
    /// 2. Read the source file from disk.
    /// 3. Parse the file with tree-sitter to obtain the concrete syntax tree.
    /// 4. Navigate from the symbol's byte range upward through the CST to
    ///    find the enclosing definition node.
    /// 5. Extract the source text using that node's exact byte range.
    ///
    /// Falls back to `TextRange`-based line extraction if tree-sitter
    /// parsing is unavailable (grammar not compiled in, parse failure, or
    /// the enclosing definition node cannot be found).
    ///
    /// Returns `None` only when the file cannot be read, the path escapes
    /// the project root (security), or the symbol's range is invalid.
    pub fn extract_source(&self, symbol_id: &SymbolId) -> Option<String> {
        let sym = self.store.find_symbol_by_id(symbol_id).ok()??;
        let file_info = self.store.get_file(&sym.file_id).ok().flatten()?;
        let lang = file_info.language;
        let full_path = self.project_root.join(&file_info.path);
        let canonical = full_path.canonicalize().ok()?;

        // Security: ensure path is within project root.
        let canonical_root = self.project_root.canonicalize().ok()?;
        if !canonical.starts_with(&canonical_root) {
            return None;
        }

        let source = std::fs::read_to_string(&canonical).ok()?;

        // Primary path: tree-sitter AST-based extraction.
        if let Some(result) = self.extract_via_ast(&sym, &source, lang) {
            return Some(result);
        }

        // Fallback: TextRange-based extraction.
        self.extract_via_range(&sym, &source)
    }

    // ── Private helpers ─────────────────────────────────────────────────

    /// AST-based extraction using tree-sitter.
    fn extract_via_ast(&self, sym: &SymbolDef, source: &str, lang: Language) -> Option<String> {
        let frontend = create_frontend(lang)?;
        let ts_lang = frontend.parser.tree_sitter_language();

        // Acquire a thread-local parser.
        // Use a scope guard to ensure the parser is always returned to the cache,
        // even on early returns (parse failure, missing node, etc.).
        let mut parser = TL_PARSER.with(|cell| {
            cell.borrow_mut()
                .take()
                .unwrap_or_else(tree_sitter::Parser::new)
        });

        // Run extraction; the parser is returned to cache on every path.
        let result = (|| -> Option<String> {
            parser.set_language(&ts_lang).ok()?;
            let tree = parser.parse(source.as_bytes(), None)?;
            let root = tree.root_node();

            // Find the CST node at the symbol's byte position.
            let start_byte = sym.range.start_byte as usize;
            let end_byte = sym.range.end_byte as usize;
            let node = root.descendant_for_byte_range(start_byte, end_byte)?;

            // Walk up to find the enclosing definition node.
            let def_node = find_enclosing_definition(node, sym.kind, lang)?;

            // Extract the exact source text using the definition node's byte range.
            let def_start = def_node.start_byte() as usize;
            let def_end = def_node.end_byte() as usize;
            if def_start >= source.len() || def_end > source.len() || def_start >= def_end {
                return None;
            }
            Some(source[def_start..def_end].to_string())
        })();

        // Always return parser to thread-local cache.
        TL_PARSER.with(|cell| *cell.borrow_mut() = Some(parser));

        result
    }

    /// Fallback: line-based extraction from `TextRange`.
    fn extract_via_range(&self, sym: &SymbolDef, source: &str) -> Option<String> {
        let all_lines: Vec<&str> = source.lines().collect();
        let start = sym.range.start_line as usize;
        let end = (sym.range.end_line as usize + 1).min(all_lines.len());
        if start >= all_lines.len() {
            return None;
        }
        Some(all_lines[start..end].join("\n"))
    }
}

// ─── CST navigation helpers ───────────────────────────────────────────────

/// Walk up the CST from `node` to find the enclosing definition node
/// for the given `SymbolKind` and `Language`.
fn find_enclosing_definition(
    node: tree_sitter::Node,
    kind: SymbolKind,
    lang: Language,
) -> Option<tree_sitter::Node> {
    let target_kinds = enclosing_definition_kinds(kind, lang);
    if target_kinds.is_empty() {
        return None;
    }

    let mut current = node;
    loop {
        if target_kinds.contains(&current.kind()) {
            return Some(current);
        }
        current = current.parent()?;
    }
}

/// Map `(SymbolKind, Language)` to the tree-sitter node kind names that
/// represent a complete definition of that symbol kind.
///
/// These node kinds correspond to the CST nodes whose byte ranges cover the
/// entire function/class/struct body — exactly what we want to extract.
fn enclosing_definition_kinds(kind: SymbolKind, lang: Language) -> &'static [&'static str] {
    use Language::*;
    use SymbolKind::*;

    match (kind, lang) {
        // ── Functions / Methods / Constructors ──
        (Function | Method | Constructor, C) => &["function_definition"],
        (Function | Method | Constructor, Cpp) => &["function_definition", "template_declaration"],
        (Function | Method | Constructor, Python) => &["function_definition", "lambda"],
        (Function | Method | Constructor, TypeScript | JavaScript | ArkTS) => &[
            "function_declaration",
            "function_expression",
            "arrow_function",
            "method_definition",
        ],
        (Function | Method | Constructor, Java) => {
            &["method_declaration", "constructor_declaration"]
        }
        (Function | Method | Constructor, Go) => &["function_declaration", "method_declaration"],
        (Function | Method | Constructor, Rust) => &["function_item"],
        (Function | Method | Constructor, CSharp) => {
            &["method_declaration", "local_function_statement"]
        }
        (Function | Method | Constructor, Php) => &["function_definition", "method_declaration"],
        (Function | Method | Constructor, Ruby) => &["method", "singleton_method"],
        (Function | Method | Constructor, Kotlin) => &["function_declaration"],
        (Function | Method | Constructor, Cangjie) => &["functionDefinition"],

        // ── Classes ──
        (Class, Cangjie) => &["classDefinition"],
        (Class, Cpp) => &["class_specifier"],
        (Class, Python) => &["class_definition"],
        (Class, TypeScript | JavaScript | ArkTS) => &["class_declaration", "class_expression"],
        (Class, Java) => &["class_declaration"],
        (Class, CSharp) => &["class_declaration"],
        (Class, Php) => &["class_declaration"],
        (Class, Ruby) => &["class"],
        (Class, Kotlin) => &["class_declaration"],

        // ── Structs ──
        (Struct, C | Cpp) => &["struct_specifier"],
        (Struct, Go) => &["type_declaration"],
        (Struct, Rust) => &["struct_item"],

        // ── Interfaces / Traits ──
        (Interface, Cangjie) => &["interfaceDefinition"],
        (Interface, TypeScript | JavaScript | ArkTS) => &["interface_declaration"],
        (Interface, Java) => &["interface_declaration"],
        (Interface, CSharp) => &["interface_declaration"],
        (Interface, Go) => &["type_declaration"],
        (Interface, Php) => &["interface_declaration"],
        (Interface, Kotlin) => &["interface_declaration"],
        (Trait, Rust) => &["trait_item"],
        (Trait, Php) => &["trait_declaration"],

        // ── Enums ──
        (Enum, Cangjie) => &["enumDefinition"],
        (Enum, TypeScript | JavaScript | ArkTS) => &["enum_declaration"],
        (Enum, Java) => &["enum_declaration"],
        (Enum, CSharp) => &["enum_declaration"],
        (Enum, Rust) => &["enum_item"],
        (Enum, C | Cpp) => &["enum_specifier"],
        (Enum, Kotlin) => &["enum_class"],

        // ── Type aliases ──
        (TypeAlias, TypeScript | JavaScript | ArkTS) => &["type_alias_declaration"],
        (TypeAlias, Go) => &["type_declaration"],
        (TypeAlias, Rust) => &["type_item"],
        (TypeAlias, C | Cpp) => &["type_definition"],
        (TypeAlias, Kotlin) => &["type_alias"],

        // ── Modules / Namespaces / Packages ──
        (Module | Namespace, TypeScript | JavaScript | ArkTS) => {
            &["module", "namespace_declaration"]
        }
        (Package, Java) => &["package_declaration"],
        (Module, Rust) => &["mod_item"],

        // ── No known enclosing node for these kinds ──
        _ => &[],
    }
}
