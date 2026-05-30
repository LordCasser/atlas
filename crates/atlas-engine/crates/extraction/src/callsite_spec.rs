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

use types::TextRange;

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
/// of the call node that are in an "arguments", "argument_list",
/// "value_arguments", "callSuffix", or "call_suffix" child.
///
/// For Kotlin's `call_suffix` → `value_arguments` → `value_argument` → `expr`
/// nesting, the innermost named child of each `value_argument` is returned so
/// that the range matches the DataNode's capture range.
fn extract_argument_ranges(call_node: &tree_sitter::Node, _source: &str) -> Vec<TextRange> {
    let mut args = Vec::new();

    let arg_container_kinds: &[&str] = &[
        "arguments",
        "argument_list",
        "parenthesized_list",
        "value_arguments", // Kotlin (when direct child of call node)
        "callSuffix",      // Cangjie
        "call_suffix",     // Kotlin (tree-sitter-kotlin wraps args)
    ];

    // Look for an argument container child
    let mut cursor = call_node.walk();
    for child in call_node.children(&mut cursor) {
        let kind = child.kind();
        if arg_container_kinds.contains(&kind) {
            match kind {
                "callSuffix" => {
                    // Cangjie: argument expressions are direct named children
                    // of callSuffix (unnamed children are '(' and ')').
                    let mut arg_cursor = child.walk();
                    for arg_child in child.children(&mut arg_cursor) {
                        if arg_child.is_named() {
                            args.push(crate::languages::node_range(arg_child));
                        }
                    }
                }
                "call_suffix" => {
                    // Kotlin: call_suffix contains value_arguments.
                    // Look for value_arguments inside call_suffix.
                    let mut suffix_cursor = child.walk();
                    for inner in child.children(&mut suffix_cursor) {
                        if inner.kind() == "value_arguments" {
                            extract_value_arguments(&inner, &mut args);
                            break;
                        }
                    }
                }
                "value_arguments" => {
                    extract_value_arguments(&child, &mut args);
                }
                _ => {
                    // Walk the argument children inside the container
                    let mut arg_cursor = child.walk();
                    for arg_child in child.children(&mut arg_cursor) {
                        let arg_kind = arg_child.kind();
                        // Skip punctuation (commas, parens)
                        if arg_kind == ","
                            || arg_kind == "("
                            || arg_kind == ")"
                            || arg_kind.is_empty()
                        {
                            continue;
                        }
                        args.push(crate::languages::node_range(arg_child));
                    }
                }
            }
            break;
        }
    }

    args
}

/// Extract argument expression ranges from a Kotlin `value_arguments` node.
///
/// `value_arguments` contains `value_argument` wrappers; the DataNode capture
/// is on the expression inside `value_argument`, so we extract the innermost
/// named child of each `value_argument`.
fn extract_value_arguments(va_node: &tree_sitter::Node, args: &mut Vec<TextRange>) {
    let mut arg_cursor = va_node.walk();
    for outer in va_node.children(&mut arg_cursor) {
        if !outer.is_named() {
            continue;
        }
        // Each named child is a value_argument; use the
        // first named child's range (the expression).
        let mut inner_cursor = outer.walk();
        let expr = outer
            .named_children(&mut inner_cursor)
            .next()
            .unwrap_or(outer);
        args.push(crate::languages::node_range(expr));
    }
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

/// Go callsite extractor.
pub fn go_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &["call_expression"],
        &[], // Go uses `make`/`new` builtins, not grammar-level constructors
        &[], // method calls use selector_expression inside call_expression
        &[
            "block",
            "function_declaration",
            "method_declaration",
            "source_file",
        ],
    )
}

/// C# callsite extractor.
pub fn csharp_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &["invocation_expression"],
        &["object_creation_expression"],
        &[],
        &[
            "block",
            "method_declaration",
            "constructor_declaration",
            "class_declaration",
            "compilation_unit",
        ],
    )
}

/// PHP callsite extractor.
pub fn php_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &[
            "function_call_expression",
            "member_call_expression",
            "scoped_call_expression",
        ],
        &["object_creation_expression"],
        &[],
        &[
            "compound_statement",
            "function_definition",
            "method_declaration",
            "class_declaration",
            "program",
        ],
    )
}

/// Kotlin callsite extractor.
pub fn kotlin_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &["call_expression"],
        &[], // Kotlin uses regular function calls for object construction
        &[], // method calls use navigation_expression inside call_expression
        &[
            "block",
            "function_declaration",
            "class_declaration",
            "source_file",
        ],
    )
}

/// Ruby callsite extractor.
pub fn ruby_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &["call"],
        &[], // Ruby has no grammar-level constructor
        &[], // all calls use the `call` node
        &[
            "block",
            "do_block",
            "method",
            "singleton_method",
            "class",
            "module",
            "program",
        ],
    )
}

/// Rust callsite extractor.
pub fn rust_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        &["call_expression", "macro_invocation"],
        &[], // Rust has no grammar-level constructor
        &[],
        &["block", "function_item", "source_file"],
    )
}
pub fn cangjie_callsite_extractor() -> GenericCallsiteExtractor {
    GenericCallsiteExtractor::new(
        // Cangjie uses postfixExpression + callSuffix for function/method calls
        // (not invocation_expression).  Only ReferenceKind::Call references
        // enter the callsite pipeline, so matching postfixExpression is safe
        // even though it also covers index/member access — those have different
        // reference kinds (FieldAccess) and won't be processed.
        &["postfixExpression"],
        &[],
        &[],
        &[
            "block",
            "functionDefinition",
            "classDefinition",
            "translationUnit",
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
/// This is the canonical factory used by the per-language frontend factories.
/// It lives in `callsite_spec.rs` (not `extract.rs`) so that both `frontend`
/// and `extract` depend on the same lower-level spec layer.
pub fn create_extractor(lang: types::enums::Language) -> Box<dyn CallsiteExtractorSpec> {
    match lang {
        #[cfg(feature = "typescript")]
        types::enums::Language::TypeScript
        | types::enums::Language::JavaScript
        | types::enums::Language::ArkTS => Box::new(ts_callsite_extractor()),
        #[cfg(feature = "python")]
        types::enums::Language::Python => Box::new(python_callsite_extractor()),
        #[cfg(feature = "java")]
        types::enums::Language::Java => Box::new(java_callsite_extractor()),
        #[cfg(feature = "c")]
        types::enums::Language::C => Box::new(c_callsite_extractor()),
        #[cfg(feature = "cpp")]
        types::enums::Language::Cpp => Box::new(c_callsite_extractor()),
        #[cfg(feature = "cangjie")]
        types::enums::Language::Cangjie => Box::new(cangjie_callsite_extractor()),
        #[cfg(feature = "go")]
        types::enums::Language::Go => Box::new(go_callsite_extractor()),
        #[cfg(feature = "csharp")]
        types::enums::Language::CSharp => Box::new(csharp_callsite_extractor()),
        #[cfg(feature = "rust")]
        types::enums::Language::Rust => Box::new(rust_callsite_extractor()),
        #[cfg(feature = "php")]
        types::enums::Language::Php => Box::new(php_callsite_extractor()),
        #[cfg(feature = "ruby")]
        types::enums::Language::Ruby => Box::new(ruby_callsite_extractor()),
        #[cfg(feature = "kotlin")]
        types::enums::Language::Kotlin => Box::new(kotlin_callsite_extractor()),
        #[allow(unreachable_patterns)]
        _ => Box::new(GenericCallsiteExtractor::new(
            &["call_expression"],
            &[],
            &[],
            &["statement_block", "function_declaration", "program"],
        )),
    }
}
