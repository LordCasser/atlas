//! CallsiteExtractorSpec — per-language callsite extraction interface.
//!
//! Moves the language-specific AST walking for callsite detection out of
//! `extract.rs` and into each language adapter.  The core pipeline calls
//! `frontend.callsites.extract_callsite()` instead of using a hardcoded
//! `find_call_expression_ancestor()` with all languages' node kinds.
//!
//! ## Adding a new language
//! 1. Implement `CallsiteExtractorSpec` for the language
//! 2. Return it from the language's `LanguageFrontend.callsites` slot
//! 3. The core pipeline will use it automatically

use atlas_types::TextRange;

// ---------------------------------------------------------------------------
// CallKind
// ---------------------------------------------------------------------------

/// Classification of a call expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// `f(args)` — plain function call.
    FunctionCall,
    /// `obj.method(args)` — method call on a receiver.
    MethodCall,
    /// `new C(args)` — constructor instantiation.
    ConstructorCall,
    /// `macro!(args)` — macro invocation.
    MacroCall,
    /// Could not determine the specific kind.
    Unknown,
}

// ---------------------------------------------------------------------------
// CallsiteParts
// ---------------------------------------------------------------------------

/// Structured result from language-specific callsite extraction.
///
/// Replaces the old pattern of walking the tree from a reference node and
/// hoping to find a `call_expression` ancestor with a hardcoded kind list.
pub struct CallsiteParts {
    /// Range of the entire call expression (including arguments).
    /// e.g. `inner(doubled)` → covers the whole expression.
    pub call_range: TextRange,
    /// Range of the callee identifier/token.
    /// e.g. `inner(doubled)` → just `inner`.
    pub callee_range: TextRange,
    /// Range of the receiver object (for method calls), if any.
    /// e.g. `obj.method()` → range of `obj`.
    pub receiver_range: Option<TextRange>,
    /// Text of the receiver object, if any.
    pub receiver_text: Option<String>,
    /// Ranges of each argument expression.
    pub argument_ranges: Vec<TextRange>,
    /// Whether this is a constructor call (`new X()`).
    pub is_constructor: bool,
    /// Classification of the call.
    pub call_kind: CallKind,
}

// ---------------------------------------------------------------------------
// CallsiteExtractorSpec trait
// ---------------------------------------------------------------------------

/// Per-language callsite extraction.
///
/// Each language adapter implements this to walk its own AST and extract
/// structured callsite information from a call reference node.
///
/// The `extract_callsite` method receives the tree root, the reference node
/// (which covers the callee name), and the source text.  It walks the tree
/// using language-specific node kind knowledge and returns `CallsiteParts`
/// if a valid call expression is found.
pub trait CallsiteExtractorSpec: Send + Sync {
    /// Extract structured callsite information from a call reference.
    ///
    /// - `root` — the tree-sitter root node
    /// - `ref_start_byte` / `ref_end_byte` — byte range of the callee
    ///   reference node (from the reference's `TextRange`)
    /// - `source` — the full source text
    ///
    /// Returns `None` if no valid call expression ancestor is found.
    fn extract_callsite(
        &self,
        root: tree_sitter::Node,
        ref_start_byte: usize,
        ref_end_byte: usize,
        source: &str,
    ) -> Option<CallsiteParts>;
}

// ---------------------------------------------------------------------------
// Generic implementation (used as fallback / default)
// ---------------------------------------------------------------------------

/// Generic callsite extractor that knows the call-expression node kinds
/// for all languages.  This is the *bridge* implementation that allows
/// the old `find_call_expression_ancestor()` logic to be reused while
/// adapters are being migrated one at a time.
pub struct GenericCallsiteExtractor {
    /// Tree-sitter node kinds that represent call expressions.
    call_kinds: &'static [&'static str],
    /// Tree-sitter node kinds that represent constructor/new expressions.
    constructor_kinds: &'static [&'static str],
    /// Node kinds that represent method calls (have a receiver).
    method_kinds: &'static [&'static str],
    /// Node kinds that mark statement/declaration boundaries (stop walking).
    boundary_kinds: &'static [&'static str],
}

impl GenericCallsiteExtractor {
    /// Create a generic callsite extractor with the given node kind sets.
    pub fn new(
        call_kinds: &'static [&'static str],
        constructor_kinds: &'static [&'static str],
        method_kinds: &'static [&'static str],
        boundary_kinds: &'static [&'static str],
    ) -> Self {
        Self {
            call_kinds,
            constructor_kinds,
            method_kinds,
            boundary_kinds,
        }
    }

    /// Walk up the tree from `node` to find the nearest ancestor whose kind
    /// represents a call expression.
    fn find_call_expression_ancestor<'a>(
        &self,
        mut node: tree_sitter::Node<'a>,
    ) -> Option<tree_sitter::Node<'a>> {
        loop {
            let kind = node.kind();
            if self.call_kinds.contains(&kind)
                || self.constructor_kinds.contains(&kind)
                || self.method_kinds.contains(&kind)
            {
                return Some(node);
            }
            if self.boundary_kinds.contains(&kind) {
                // Stop at boundary — do NOT cross into an outer scope.
                // The caller (extract_file) will use the reference range
                // as a conservative fallback.
                return None;
            }
            node = node.parent()?;
        }
    }
}

impl CallsiteExtractorSpec for GenericCallsiteExtractor {
    fn extract_callsite(
        &self,
        root: tree_sitter::Node,
        ref_start_byte: usize,
        ref_end_byte: usize,
        source: &str,
    ) -> Option<CallsiteParts> {
        let node = root.descendant_for_byte_range(ref_start_byte, ref_end_byte)?;
        let call_node = self.find_call_expression_ancestor(node)?;

        let call_range = crate::languages::node_range(call_node);
        let callee_range = crate::languages::node_range(node);

        let is_constructor = self.constructor_kinds.contains(&call_node.kind());
        let is_method = self.method_kinds.contains(&call_node.kind());

        // Try to extract receiver for method calls
        let (receiver_range, receiver_text) = if is_method {
            // First child is typically the object expression
            if let Some(obj) = call_node.child(0) {
                let range = crate::languages::node_range(obj);
                let text = obj.utf8_text(source.as_bytes()).ok().map(|s| s.to_string());
                (Some(range), text)
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        // Extract argument ranges: walk children of the call node
        // to find argument/parenthesized nodes
        let argument_ranges = extract_argument_ranges(&call_node, source);

        let call_kind = if is_constructor {
            CallKind::ConstructorCall
        } else if is_method {
            CallKind::MethodCall
        } else if self.call_kinds.contains(&call_node.kind()) {
            CallKind::FunctionCall
        } else {
            CallKind::Unknown
        };

        Some(CallsiteParts {
            call_range,
            callee_range,
            receiver_range,
            receiver_text,
            argument_ranges,
            is_constructor,
            call_kind,
        })
    }
}

/// Extract argument ranges from a call expression node.
///
/// This is a generic best-effort implementation that looks for children
/// of the call node that are in an "arguments" or "argument_list" child.
fn extract_argument_ranges(call_node: &tree_sitter::Node, _source: &str) -> Vec<TextRange> {
    let mut args = Vec::new();

    // Look for an "arguments" or "argument_list" child
    let mut cursor = call_node.walk();
    for child in call_node.children(&mut cursor) {
        let kind = child.kind();
        if kind == "arguments" || kind == "argument_list" || kind == "parenthesized_list" {
            // Walk the argument children
            let mut arg_cursor = child.walk();
            for arg_child in child.children(&mut arg_cursor) {
                let arg_kind = arg_child.kind();
                // Skip punctuation (commas, parens)
                if arg_kind == ","
                    || arg_kind == "("
                    || arg_kind == ")"
                    || arg_kind == ")"
                    || arg_kind.is_empty()
                {
                    continue;
                }
                args.push(crate::languages::node_range(arg_child));
            }
            break;
        }
    }

    args
}

// ---------------------------------------------------------------------------
// Language-specific constructors
// ---------------------------------------------------------------------------

/// TypeScript / JavaScript / ArkTS callsite extractor.
pub fn ts_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &["call_expression"],
        &["new_expression"],
        &[], // method calls are also `call_expression` in TS (with member_expression callee)
        &[
            "statement_block",
            "function_declaration",
            "method_definition",
            "class_declaration",
            "program",
            "module",
        ],
    )
}

/// Python callsite extractor.
pub fn python_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &["call"],
        &[],
        &[],
        &["block", "function_definition", "class_definition", "module"],
    )
}

/// C / C++ callsite extractor.
pub fn c_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &["call_expression"],
        &[],
        &[],
        &[
            "compound_statement",
            "function_definition",
            "translation_unit",
        ],
    )
}

/// Java callsite extractor.
pub fn java_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &["method_invocation"],
        &["object_creation_expression", "class_creation_expression"],
        &["method_invocation"], // Java method invocations always have a receiver
        &[
            "block",
            "method_declaration",
            "class_declaration",
            "program",
        ],
    )
}

/// Cangjie callsite extractor.
pub fn cangjie_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &["invocation_expression"],
        &["class_creation_expression"],
        &[],
        &[
            "block",
            "function_declaration",
            "class_declaration",
            "program",
        ],
    )
}

// ---------------------------------------------------------------------------
// Factory: create a CallsiteExtractorSpec for a given language
// ---------------------------------------------------------------------------

/// Create a `CallsiteExtractorSpec` for the given language.
///
/// Always returns a spec — uses a no-op fallback (`GenericCallsiteExtractor`)
/// for unknown languages instead of returning `None`.
///
/// This is the canonical factory used by `LanguageFrontend::from_adapter()`.
/// It lives in `callsite_spec.rs` (not `extract.rs`) so that both `frontend`
/// and `extract` depend on the same lower-level spec layer.
pub fn create_extractor(lang: atlas_types::enums::Language) -> Box<dyn CallsiteExtractorSpec> {
    match lang {
        #[cfg(feature = "typescript")]
        atlas_types::enums::Language::TypeScript
        | atlas_types::enums::Language::JavaScript
        | atlas_types::enums::Language::ArkTS => Box::new(ts_callsite_extractor()),
        #[cfg(feature = "python")]
        atlas_types::enums::Language::Python => Box::new(python_callsite_extractor()),
        #[cfg(feature = "java")]
        atlas_types::enums::Language::Java => Box::new(java_callsite_extractor()),
        #[cfg(feature = "c")]
        atlas_types::enums::Language::C => Box::new(c_callsite_extractor()),
        #[cfg(feature = "cpp")]
        atlas_types::enums::Language::Cpp => Box::new(c_callsite_extractor()),
        #[cfg(feature = "cangjie")]
        atlas_types::enums::Language::Cangjie => Box::new(cangjie_callsite_extractor()),
        #[allow(unreachable_patterns)]
        _ => Box::new(GenericCallsiteExtractor::new(
            &["call_expression"],
            &[],
            &[],
            &["statement_block", "function_declaration", "program"],
        )),
    }
}
