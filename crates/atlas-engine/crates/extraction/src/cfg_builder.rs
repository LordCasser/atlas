//! CfgBuilder — per-function control-flow graph from tree-sitter AST.
//!
//! # Architecture
//!
//! Walks the tree-sitter AST of a function body and produces:
//! - [`CfgNode`]s: Entry, Statement, Branch, Loop, Return, Throw, Join, Exit
//! - [`CfgEdge`]s: Normal, TrueBranch, FalseBranch, LoopBack
//!
//! # Supported constructs (TypeScript)
//!
//! - Block statements (sequential Normal edges)
//! - if/else (Branch → TrueBranch/FalseBranch → Join)
//! - for/while/do (Loop → body → LoopBack → exit)
//! - return/throw (→ Exit)
//! - ?: ternary (Branch → Join)
//!
//! # NOT supported (deferred)
//! - try/catch/finally
//! - switch/case
//! - async/await
//! - labeled break/continue
//!
//! # Invariants
//!
//! - Every function CFG has exactly one Entry and one Exit node.
//! - All nodes belong to the same `function_id`.
//! - CfgNodeId and CfgEdgeId are deterministic (blake3).

use tree_sitter::Node;
use types::cfg::{CfgEdge, CfgNode};
use types::enums::{CallContext, CfgEdgeKind, CfgNodeKind, EffectKind, Language, SymbolKind};
use types::ids::SymbolId;
use types::structs::{SymbolDef, TextRange};

// ── CfgLanguageConfig ───────────────────────────────────────────────────────

/// Language-specific tree-sitter node kind names used by the CFG builder.
struct CfgLanguageConfig {
    /// Node kinds that represent function body blocks.
    block_kinds: &'static [&'static str],
    /// Node kinds for if/else branches.
    if_kinds: &'static [&'static str],
    /// Node kinds for loops (for, while, do).
    loop_kinds: &'static [&'static str],
    /// Node kinds for return statements.
    return_kinds: &'static [&'static str],
    /// Node kinds for throw/raise statements.
    throw_kinds: &'static [&'static str],
    /// Node kinds for expression/declaration statements.
    stmt_kinds: &'static [&'static str],
    /// Node kinds for switch statements (switch, switch_expression, etc.).
    switch_kinds: &'static [&'static str],
    /// Node kinds for case/default clauses inside a switch body
    /// (case_statement, switch_case, expression_case, etc.).
    case_kinds: &'static [&'static str],
}

/// Return the language-specific CFG configuration for the given language.
fn cfg_config(lang: Language) -> CfgLanguageConfig {
    match lang {
        Language::TypeScript | Language::JavaScript | Language::ArkTS => CfgLanguageConfig {
            block_kinds: &["statement_block"],
            if_kinds: &["if_statement"],
            loop_kinds: &["for_statement", "while_statement", "do_statement"],
            return_kinds: &["return_statement"],
            throw_kinds: &["throw_statement"],
            stmt_kinds: &[
                "expression_statement",
                "variable_declaration",
                "lexical_declaration",
                "continue_statement",
                "break_statement",
                "debugger_statement",
                "empty_statement",
            ],
            switch_kinds: &["switch_statement"],
            case_kinds: &["switch_case", "switch_default"],
        },
        Language::Java => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_statement"],
            loop_kinds: &[
                "for_statement",
                "while_statement",
                "do_statement",
                "enhanced_for_statement",
            ],
            return_kinds: &["return_statement"],
            throw_kinds: &["throw_statement"],
            stmt_kinds: &[
                "expression_statement",
                "local_variable_declaration",
                "continue_statement",
                "break_statement",
            ],
            switch_kinds: &["switch_expression"],
            case_kinds: &["switch_block_statement_group", "switch_rule"],
        },
        Language::Go => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_statement"],
            loop_kinds: &["for_statement"],
            return_kinds: &["return_statement"],
            throw_kinds: &[], // Go has no throw
            stmt_kinds: &[
                "expression_statement",
                "short_var_declaration",
                "var_declaration",
                "continue_statement",
                "break_statement",
            ],
            switch_kinds: &["expression_switch_statement", "type_switch_statement"],
            case_kinds: &["expression_case", "type_case", "default_case"],
        },
        Language::Python => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_statement", "elif_clause", "else_clause"],
            loop_kinds: &["for_statement", "while_statement"],
            return_kinds: &["return_statement"],
            throw_kinds: &["raise_statement"],
            stmt_kinds: &[
                "expression_statement",
                "assignment",
                "continue_statement",
                "break_statement",
            ],
            // Python `match` is pattern-matching (guards, capture bindings),
            // not a C-style switch; deferred like Rust `match_expression`.
            switch_kinds: &[],
            case_kinds: &[],
        },
        Language::C | Language::Cpp => CfgLanguageConfig {
            block_kinds: &["compound_statement"],
            if_kinds: &["if_statement"],
            loop_kinds: &["for_statement", "while_statement", "do_statement"],
            return_kinds: &["return_statement"],
            throw_kinds: &[], // C has no throw; C++ has throw but via exceptions
            stmt_kinds: &[
                "expression_statement",
                "declaration",
                "continue_statement",
                "break_statement",
            ],
            switch_kinds: &["switch_statement"],
            case_kinds: &["case_statement"],
        },
        Language::Rust => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_expression"],
            loop_kinds: &["for_expression", "while_expression", "loop_expression"],
            return_kinds: &["return_expression"],
            throw_kinds: &[], // Rust uses Result, not throw
            stmt_kinds: &["let_declaration", "continue_expression", "break_expression"],
            // Rust `match` is pattern-matching (guards, bindings), deferred —
            // see the `match_expression` arm in walk_stmt_list.
            switch_kinds: &[],
            case_kinds: &[],
        },
        Language::CSharp => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_statement"],
            loop_kinds: &[
                "for_statement",
                "foreach_statement",
                "while_statement",
                "do_statement",
            ],
            return_kinds: &["return_statement"],
            throw_kinds: &["throw_statement"],
            stmt_kinds: &["expression_statement", "local_declaration_statement"],
            switch_kinds: &["switch_statement"],
            case_kinds: &["switch_section"],
        },
        Language::Kotlin => CfgLanguageConfig {
            block_kinds: &["function_body"],
            if_kinds: &["if_expression"],
            loop_kinds: &["for_statement", "while_statement", "do_while_statement"],
            return_kinds: &["jump_expression"],
            throw_kinds: &[],
            stmt_kinds: &[
                "property_declaration",
                "assignment",
                "variable_declaration",
                "call_expression",
            ],
            // Kotlin `when` AST not yet verified for a unified walk; deferred.
            switch_kinds: &[],
            case_kinds: &[],
        },
        Language::Cangjie => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["ifExpression"],
            loop_kinds: &["whileExpression", "forInExpression", "doWhileExpression"],
            return_kinds: &["jumpExpression"], // jumpExpression covers return/break/continue
            throw_kinds: &[],
            stmt_kinds: &["variableDeclaration", "expressionStatement"],
            // Cangjie `match` AST not yet verified for a unified walk; deferred.
            switch_kinds: &[],
            case_kinds: &[],
        },
        Language::Ruby => CfgLanguageConfig {
            block_kinds: &["body_statement"],
            if_kinds: &["if", "unless", "elsif"],
            loop_kinds: &["while", "until", "for"],
            return_kinds: &["return"],
            throw_kinds: &["raise"],
            stmt_kinds: &["call", "assignment", "break", "next"],
            // Ruby `case`/`when` AST not yet verified for a unified walk; deferred.
            switch_kinds: &[],
            case_kinds: &[],
        },
        _ => CfgLanguageConfig {
            // Default: TS/JS config (best-effort for unknown languages)
            block_kinds: &["statement_block"],
            if_kinds: &["if_statement"],
            loop_kinds: &["for_statement", "while_statement", "do_statement"],
            return_kinds: &["return_statement"],
            throw_kinds: &["throw_statement"],
            stmt_kinds: &[
                "expression_statement",
                "variable_declaration",
                "lexical_declaration",
                "continue_statement",
                "break_statement",
                "debugger_statement",
                "empty_statement",
            ],
            switch_kinds: &["switch_statement"],
            case_kinds: &["switch_case", "switch_default"],
        },
    }
}

// ── CfgBuilder ──────────────────────────────────────────────────────────────

/// Builds per-function control-flow graphs from a tree-sitter AST.
pub struct CfgBuilder;

/// Result of CFG construction for a single function.
#[derive(Debug, Default)]
pub struct CfgResult {
    pub nodes: Vec<CfgNode>,
    pub edges: Vec<CfgEdge>,
}

/// Context for building a CFG for one function.
struct CfgContext<'a> {
    function_id: SymbolId,
    nodes: Vec<CfgNode>,
    edges: Vec<CfgEdge>,
    source: &'a [u8],
    prev_node_id: Option<types::ids::CfgNodeId>,
    language: Language,
    config: CfgLanguageConfig,
    /// Pending call-site context for the next emitted statement node
    /// (Python with, Go go/defer, etc.).
    pending_call_context: CallContext,
    /// Persistent scope-level call context (applies to ALL nodes until reset).
    /// Used for React cleanup arrow bodies where every statement
    /// shares the same context.  When `pending_call_context` is `None`,
    /// `add_node` falls back to this value.
    scope_call_context: CallContext,
}

impl CfgBuilder {
    /// Build CFG for a function node.
    ///
    /// Scans the function body for statements and produces CFG nodes/edges.
    pub fn build(
        language: Language,
        function_id: &SymbolId,
        function_node: Node,
        source_bytes: &[u8],
    ) -> CfgResult {
        let config = cfg_config(language);

        // Detect React cleanup arrow: an arrow_function that is the direct
        // child of a return_statement (e.g., `return () => cleanup()`).
        // When true, all CFG nodes inside this function inherit
        // ReactEffectCleanup context so compose_effects marks their
        // Free effects as Deferred.
        let is_cleanup_return = function_node
            .parent()
            .map(|p| p.kind() == "return_statement")
            .unwrap_or(false);

        let mut ctx = CfgContext {
            function_id: *function_id,
            nodes: Vec::new(),
            edges: Vec::new(),
            source: source_bytes,
            prev_node_id: None,
            language,
            config,
            pending_call_context: CallContext::None,
            scope_call_context: if is_cleanup_return {
                CallContext::ReactEffectCleanup
            } else {
                CallContext::None
            },
        };

        // 1. Create Entry node
        let entry_id = ctx.add_node(CfgNodeKind::Entry, 0, None);
        ctx.prev_node_id = Some(entry_id);

        // 2. Find the statement block
        let body = find_function_body(function_node, ctx.config.block_kinds);

        // 3. Walk the body
        if let Some(body) = body {
            let body_range = node_text_range(&body, source_bytes);
            ctx.walk_block(body, body_range.start_byte);
        }

        // 4. If no body found, create a single Statement node
        if ctx.prev_node_id.is_some() && ctx.nodes.len() == 1 {
            let fn_range = node_text_range(&function_node, source_bytes);
            ctx.add_node(CfgNodeKind::Statement, fn_range.start_byte, None);
        }

        // 5. Create Exit node and connect last node to exit
        let last = ctx.prev_node_id;
        let exit_id = ctx.add_node(CfgNodeKind::Exit, 0, None);
        if let Some(last_id) = last {
            ctx.add_edge(&last_id, &exit_id, CfgEdgeKind::Normal);
        }

        CfgResult {
            nodes: ctx.nodes,
            edges: ctx.edges,
        }
    }
}

/// Check whether a function name (case-insensitive) is a heap-free function.
fn is_free_function_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        "free"
            | "delete"
            | "operator delete"
            | "operator delete[]"
            | "std::free"
            | "safefree"
            | "curl_safefree"
    )
}

/// Check whether a function name (case-insensitive) is a heap-allocator function.
fn is_alloc_function_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        "malloc"
            | "calloc"
            | "realloc"
            | "new"
            | "operator new"
            | "aprintf"
            | "asprintf"
            | "strdup"
            | "strndup"
    )
}

impl CfgContext<'_> {
    fn add_node(
        &mut self,
        kind: CfgNodeKind,
        start_byte: u32,
        stmt_node: Option<&Node>,
    ) -> types::ids::CfgNodeId {
        let range = if let Some(node) = stmt_node {
            let r = node.range();
            TextRange {
                start_byte: r.start_byte as u32,
                end_byte: r.end_byte as u32,
                start_line: r.start_point.row as u32,
                start_column: r.start_point.column as u32,
                end_line: r.end_point.row as u32,
                end_column: r.end_point.column as u32,
            }
        } else {
            TextRange {
                start_byte,
                end_byte: start_byte,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            }
        };
        let mut node = CfgNode::new(&self.function_id, kind, range);
        node.call_context = if self.pending_call_context != CallContext::None {
            let ctx = self.pending_call_context;
            self.pending_call_context = CallContext::None;
            ctx
        } else {
            self.scope_call_context
        };
        let id = node.id;
        self.nodes.push(node);
        id
    }

    fn add_edge(
        &mut self,
        source: &types::ids::CfgNodeId,
        target: &types::ids::CfgNodeId,
        kind: CfgEdgeKind,
    ) {
        self.edges.push(CfgEdge::new(source, target, kind));
    }

    fn is_c_or_cpp(&self) -> bool {
        matches!(self.language, Language::C | Language::Cpp)
    }

    fn is_ruby(&self) -> bool {
        matches!(self.language, Language::Ruby)
    }

    fn is_kotlin(&self) -> bool {
        matches!(self.language, Language::Kotlin)
    }

    /// Check whether a `call` node has a `block` or `do_block` child.
    fn has_block_child(&self, call_node: &Node) -> bool {
        let mut cursor = call_node.walk();
        call_node
            .named_children(&mut cursor)
            .any(|c| c.kind() == "do_block" || c.kind() == "block")
    }

    /// Check whether a `call_expression` node has a `lambda_literal` child
    /// (indicating a trailing lambda like `.use { ... }`).
    /// Search recursively for a `lambda_literal` descendant of the given node.
    /// In Kotlin tree-sitter, trailing lambdas are nested inside `call_suffix`,
    /// not as direct children of `call_expression`.
    #[allow(clippy::only_used_in_recursion)]
    fn find_lambda_literal<'a>(&self, node: &Node<'a>) -> Option<Node<'a>> {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "lambda_literal" {
                return Some(child);
            }
            if child.child_count() > 0 {
                if let Some(found) = self.find_lambda_literal(&child) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn has_lambda_child(&self, call_node: &Node) -> bool {
        self.find_lambda_literal(call_node).is_some()
    }

    /// Check whether a Kotlin `call_expression`'s callee name ends with `.use`.
    /// Scans named children (skipping `call_suffix` children) and checks if any
    /// child's text ends with `.use`. This avoids matching `list.map { }`,
    /// `run { }`, etc.
    fn is_kotlin_use_call(&self, call_node: &Node) -> bool {
        let mut cursor = call_node.walk();
        for child in call_node.named_children(&mut cursor) {
            if child.kind() == "call_suffix" {
                continue;
            }
            if let Ok(text) = child.utf8_text(self.source) {
                if text.ends_with(".use") {
                    return true;
                }
            }
        }
        false
    }

    /// Check whether a Ruby `call` node's callee name matches a known
    /// resource-managing method (e.g. `File.open`, `Dir.chdir`).
    /// Filters out non-resource block calls like `[1,2,3].map { }`,
    /// `5.times { }`, `users.each { }`.
    fn is_ruby_resource_block_call(&self, call_node: &Node) -> bool {
        if let Some(name) = self.extract_callee_name(call_node) {
            matches!(
                name.as_str(),
                "File.open"
                    | "File.new"
                    | "IO.open"
                    | "IO.new"
                    | "open"
                    | "Tempfile.create"
                    | "Dir.chdir"
                    | "Dir.open"
                    | "Dir.new"
                    | "TCPServer.new"
                    | "UDPSocket.new"
            )
        } else {
            // Fallback: scan node text for known patterns
            if let Ok(text) = call_node.utf8_text(self.source) {
                text.starts_with("File.open")
                    || text.starts_with("File.new")
                    || text.starts_with("IO.open")
                    || text.starts_with("IO.new")
                    || text.starts_with("open(")
            } else {
                false
            }
        }
    }

    /// Infer the effect of a C/C++ CFG node. Returns (effect_kind, target_field).
    /// Uses simple tree-sitter node kind matching — NOT full dataflow analysis.
    fn infer_effect(
        &self,
        node: &Node,
        node_kind: CfgNodeKind,
    ) -> (Option<EffectKind>, Option<String>) {
        let kind = node.kind();

        // Branch conditions
        if node_kind == CfgNodeKind::Branch {
            let target = self.extract_field_path(node);
            return (Some(EffectKind::Condition), target);
        }

        // Return statements
        if node_kind == CfgNodeKind::Return || kind == "return_statement" {
            let target = self.extract_field_path(node);
            return (Some(EffectKind::Return), target);
        }

        // Walk children to determine effect
        let mut cursor = node.walk();
        let children: Vec<tree_sitter::Node> = node.named_children(&mut cursor).collect();

        for child in &children {
            let ck = child.kind();
            match ck {
                "call_expression" => {
                    // Function call — check if it's free/alloc
                    let func_name = self.extract_callee_name(child);
                    let func_str = func_name.as_deref().unwrap_or("");
                    if is_free_function_name(func_str) {
                        let target = self.extract_first_arg_field(child);
                        return (Some(EffectKind::Free), target);
                    } else if is_alloc_function_name(func_str) {
                        return (Some(EffectKind::Allocate), None);
                    }
                    // Unrecognized call — still extract the first arg field so
                    // lifecycle can apply domain rules (e.g. atlas_annotate
                    // free_fn=SuperFree).  Without a target, (EffectKind::Call,
                    // false) can never match the field and domain rules are
                    // unreachable.
                    let target = self.extract_first_arg_field(child);
                    return (Some(EffectKind::Call), target);
                }
                "assignment_expression" => {
                    // Check LHS for field write AND RHS for alloc call
                    let target = self.extract_lhs_field(child);
                    if self.has_alloc_call(child) {
                        return (Some(EffectKind::Allocate), target);
                    }
                    return (Some(EffectKind::Assign), target);
                }
                "new_expression" | "delete_expression" => {
                    let is_delete = ck.starts_with("delete");
                    return (
                        if is_delete {
                            Some(EffectKind::Free)
                        } else {
                            Some(EffectKind::Allocate)
                        },
                        None,
                    );
                }
                "field_expression" | "subscript_expression" => {
                    let target = self.extract_field_path(child);
                    // Field access in non-assignment context → Read
                    return (Some(EffectKind::Read), target);
                }
                "goto_statement" => {
                    return (Some(EffectKind::Goto), None);
                }
                _ => {}
            }
        }

        // Default: if we can find field access anywhere, treat as Read
        for child in &children {
            if child.kind() == "field_expression" || child.kind() == "member_expression" {
                let target = self.extract_field_path(child);
                return (Some(EffectKind::Read), target);
            }
        }

        (None, None)
    }

    /// Extract callee function name from a call_expression node.
    fn extract_callee_name(&self, call_node: &Node) -> Option<String> {
        let mut cursor = call_node.walk();
        for child in call_node.named_children(&mut cursor) {
            if child.kind() == "identifier" || child.kind() == "field_expression" {
                if let Ok(text) = child.utf8_text(self.source) {
                    return Some(text.to_string());
                }
            }
        }
        None
    }

    /// Extract the left-hand-side field path from an assignment_expression.
    fn extract_lhs_field(&self, assign_node: &Node) -> Option<String> {
        let mut cursor = assign_node.walk();
        let children: Vec<tree_sitter::Node> = assign_node.named_children(&mut cursor).collect();
        if let Some(first) = children.first() {
            return self.extract_field_path(first);
        }
        None
    }

    /// Extract the field path of the first argument of a call expression.
    ///
    /// For `free(data->state.aptr)`, returns `"data.state.aptr"`.
    fn extract_first_arg_field(&self, call_node: &Node) -> Option<String> {
        let mut cursor = call_node.walk();
        let children: Vec<tree_sitter::Node> = call_node.named_children(&mut cursor).collect();
        // Skip the callee (function name), find the argument_list/arguments node
        for child in &children {
            if child.kind() == "argument_list" || child.kind() == "arguments" {
                let mut ac = child.walk();
                let args: Vec<tree_sitter::Node> = child.named_children(&mut ac).collect();
                if let Some(first) = args.first() {
                    return self.extract_field_path(first);
                }
            }
        }
        None
    }

    /// Recursively check whether any descendant of `node` is a call_expression
    /// whose callee name is a known allocator function.
    fn has_alloc_call(&self, node: &Node) -> bool {
        if node.kind() == "call_expression" {
            if let Some(name) = self.extract_callee_name(node) {
                if is_alloc_function_name(&name) {
                    return true;
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if self.has_alloc_call(&child) {
                return true;
            }
        }
        false
    }

    /// Walk a tree-sitter node to build a dot-separated field access path.
    /// E.g., for `data->state.aptr.cookiehost` returns "data.state.aptr.cookiehost"
    fn extract_field_path(&self, node: &Node) -> Option<String> {
        let kind = node.kind();
        match kind {
            "field_expression" | "member_expression" | "pointer_expression" => {
                let mut cursor = node.walk();
                let children: Vec<tree_sitter::Node> = node.named_children(&mut cursor).collect();
                let mut parts = Vec::new();
                for child in &children {
                    let ck = child.kind();
                    match ck {
                        "field_identifier" | "property_identifier" | "identifier" => {
                            if let Ok(text) = child.utf8_text(self.source) {
                                parts.push(text.to_string());
                            }
                        }
                        "field_expression" | "member_expression" | "subscript_expression" => {
                            if let Some(sub) = self.extract_field_path(child) {
                                parts.push(sub);
                            }
                        }
                        _ => {}
                    }
                }
                if parts.is_empty() {
                    None
                } else {
                    Some(types::structs::canonicalize_field_path(&parts.join(".")))
                }
            }
            "subscript_expression" => {
                // array[index] — extract array name
                let mut cursor = node.walk();
                if let Some(first) = node.named_children(&mut cursor).next() {
                    if let Ok(text) = first.utf8_text(self.source) {
                        return Some(types::structs::canonicalize_field_path(text));
                    }
                }
                None
            }
            "identifier" => {
                if let Ok(text) = node.utf8_text(self.source) {
                    Some(types::structs::canonicalize_field_path(text))
                } else {
                    None
                }
            }
            _ => {
                // Walk children to find field expressions
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if let Some(path) = self.extract_field_path(&child) {
                        return Some(types::structs::canonicalize_field_path(&path));
                    }
                }
                None
            }
        }
    }

    fn walk_block(&mut self, block: Node, _block_start: u32) {
        let mut cursor = block.walk();
        let children: Vec<Node> = block.named_children(&mut cursor).collect();
        self.walk_stmt_list(&children);
    }

    /// Walk a flat list of statement nodes using the per-statement dispatch.
    ///
    /// Shared by [`Self::walk_block`] (block bodies) and [`Self::walk_switch`]
    /// (switch-case bodies) so that control-flow constructs nested inside a
    /// `switch` case (if/loop/nested switch/etc.) are handled identically to
    /// top-level block statements.
    fn walk_stmt_list(&mut self, children: &[Node]) {
        // Process each statement in the block
        let mut i = 0;
        while i < children.len() {
            let stmt = children[i];
            let kind = stmt.kind();
            let stmt_range = node_text_range(&stmt, self.source);

            if self.config.if_kinds.contains(&kind) {
                i = self.walk_if(children, i, stmt_range.start_byte);
            } else if self.config.loop_kinds.contains(&kind) {
                i = self.walk_loop(children, i, stmt_range.start_byte);
            } else if self.config.return_kinds.contains(&kind) {
                self.emit_stmt(CfgNodeKind::Return, stmt_range.start_byte, &stmt);
                // React cleanup return: `return () => { ... }` or `return () => expr`
                // Walk the arrow body with ReactEffectCleanup scope context, so
                // frees inside the cleanup callback get Deferred consumption style.
                let mut return_cursor = stmt.walk();
                for child in stmt.named_children(&mut return_cursor) {
                    if child.kind() == "arrow_function" {
                        self.walk_react_cleanup_arrow(&child, stmt_range.start_byte);
                        break;
                    }
                }
                i += 1;
            } else if self.config.throw_kinds.contains(&kind) {
                self.emit_stmt(CfgNodeKind::Throw, stmt_range.start_byte, &stmt);
                i += 1;
            } else if self.is_ruby()
                && kind == "call"
                && self.has_block_child(&stmt)
                && self.is_ruby_resource_block_call(&stmt)
            {
                // Ruby block-managed resource: File.open(...) { |f| ... }
                // Set RubyBlock context, walk the call (Alloc), walk the block body,
                // and emit BlockExit for scope-exit auto-free.
                self.pending_call_context = CallContext::RubyBlock;
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);

                // Walk the block body (find do_block/block child → body_statement)
                let mut child_cursor = stmt.walk();
                for child in stmt.named_children(&mut child_cursor) {
                    if child.kind() == "do_block" || child.kind() == "block" {
                        if let Some(body) = find_function_body(child, self.config.block_kinds) {
                            self.walk_block(body, stmt_range.start_byte);
                        } else {
                            // Fallback: walk the block node directly
                            self.walk_block(child, stmt_range.start_byte);
                        }
                        break;
                    }
                }

                // Emit BlockExit node (zero-length range at end of block call)
                let block_exit_id =
                    self.add_node(CfgNodeKind::BlockExit, stmt.end_byte() as u32, None);
                if let Some(last_id) = self.prev_node_id.take() {
                    self.add_edge(&last_id, &block_exit_id, CfgEdgeKind::Normal);
                }
                self.prev_node_id = Some(block_exit_id);

                i += 1;
                continue;
            } else if self.is_kotlin()
                && kind == "call_expression"
                && self.has_lambda_child(&stmt)
                && self.is_kotlin_use_call(&stmt)
            {
                // Kotlin `.use {}` block-managed resource: File(...).use { ... }
                // Set KotlinUse context, walk the call (Alloc), walk the lambda body,
                // and emit BlockExit for scope-exit auto-free.
                self.pending_call_context = CallContext::KotlinUse;
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);

                // Walk the lambda body (recursively find lambda_literal → function_body/statements)
                if let Some(lambda) = self.find_lambda_literal(&stmt) {
                    if let Some(body) = find_function_body(lambda, self.config.block_kinds) {
                        self.walk_block(body, stmt_range.start_byte);
                    } else {
                        // Fallback: walk the lambda node directly
                        self.walk_block(lambda, stmt_range.start_byte);
                    }
                }

                // Emit BlockExit node (zero-length range at end of `.use {}` block)
                let block_exit_id =
                    self.add_node(CfgNodeKind::BlockExit, stmt.end_byte() as u32, None);
                if let Some(last_id) = self.prev_node_id.take() {
                    self.add_edge(&last_id, &block_exit_id, CfgEdgeKind::Normal);
                }
                self.prev_node_id = Some(block_exit_id);

                i += 1;
                continue;
            } else if self.config.stmt_kinds.contains(&kind) {
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                i += 1;
            } else if self.config.block_kinds.contains(&kind) {
                // Nested block
                self.walk_block(stmt, stmt_range.start_byte);
                i += 1;
            } else if kind == "statement_list" {
                // Go block wrapper: recurse through statement_list body
                self.walk_block(stmt, stmt_range.start_byte);
                i += 1;
            } else if self.is_kotlin() && kind == "statements" {
                // Kotlin block wrapper: function_body contains a `statements`
                // group that wraps the actual call_expression/property_declaration
                // children. Recurse through to process them.
                self.walk_block(stmt, stmt_range.start_byte);
                i += 1;
            } else if kind == "expression_statement" {
                // Rust wrapper: check if inner expression is if/loop
                let mut child_cursor = stmt.walk();
                let inner: Vec<Node> = stmt.named_children(&mut child_cursor).collect();
                let dispatched = if let Some(first) = inner.first() {
                    if self.config.if_kinds.contains(&first.kind()) {
                        self.walk_if_node(*first, stmt_range.start_byte);
                        true
                    } else if self.config.loop_kinds.contains(&first.kind()) {
                        self.walk_loop_node(*first, stmt_range.start_byte);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !dispatched {
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                }
                i += 1;
            } else if kind == "try_with_resources_statement" {
                // Java try-with-resources: set context, walk resource specs, walk body, emit BlockExit
                self.pending_call_context = CallContext::JavaTryWith;

                let mut child_cursor = stmt.walk();
                let named: Vec<Node> = stmt.named_children(&mut child_cursor).collect();

                // Walk resource_specification children (e.g., new FileInputStream("file"))
                // The 'resources' child contains 'resource' nodes with 'variable_declarator'
                for gc in &named {
                    let kind = gc.kind();
                    if kind == "resources" {
                        // resources is a container; walk its named children (each 'resource')
                        let mut rc = gc.walk();
                        for res in gc.named_children(&mut rc) {
                            if res.kind() == "resource" {
                                // A resource has a variable_declarator; emit the whole resource as Statement
                                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &res);
                            }
                        }
                    }
                }

                // Walk the body block
                for gc in &named {
                    if gc.kind() == "block" {
                        self.walk_block(*gc, stmt_range.start_byte);
                    }
                }

                // Emit BlockExit node (zero-length range at end of try-with-resources)
                let block_exit_id =
                    self.add_node(CfgNodeKind::BlockExit, stmt.end_byte() as u32, None);
                if let Some(last_id) = self.prev_node_id.take() {
                    self.add_edge(&last_id, &block_exit_id, CfgEdgeKind::Normal);
                }
                self.prev_node_id = Some(block_exit_id);

                i += 1;
                continue;
            } else if self.config.switch_kinds.contains(&kind) {
                // Switch/case: model each case as an independent sibling path
                // from a Branch (dispatch) node into a Join. Fall-through is
                // NOT modeled (Phase 1). See `walk_switch`.
                i = self.walk_switch(children, i, stmt_range.start_byte);
            } else if kind == "try_statement" || kind == "switch_statement" {
                // Deferred: treat as single statement.
                // (A `switch_statement` only reaches here when the active
                // language config has no `switch_kinds` entry — e.g. a
                // language whose switch AST is not yet supported by
                // `walk_switch`. try_statement is always deferred.)
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                i += 1;
            } else if kind == "preproc_if" || kind == "preproc_def" {
                // C/C++ preprocessor directives
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                i += 1;
            } else if kind == "match_expression" {
                // Rust match — complex control flow, deferred
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                i += 1;
            } else if kind == "using_statement" {
                // C# using: set context, walk resource declaration, walk body, emit BlockExit
                self.pending_call_context = CallContext::CSharpUsing;

                let mut child_cursor = stmt.walk();
                let named: Vec<Node> = stmt.named_children(&mut child_cursor).collect();

                // Walk resource declaration nodes (skip "using" keyword, walk named children for resource)
                for gc in &named {
                    let gc_kind = gc.kind();
                    if gc_kind == "variable_declaration"
                        || gc_kind == "local_declaration_statement"
                        || gc_kind == "object_creation_expression"
                    {
                        self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, gc);
                    }
                }

                // Walk the body block
                for gc in &named {
                    if gc.kind() == "block" {
                        self.walk_block(*gc, stmt_range.start_byte);
                    }
                }

                // Emit BlockExit node (zero-length range at end of using statement)
                let block_exit_id =
                    self.add_node(CfgNodeKind::BlockExit, stmt.end_byte() as u32, None);
                if let Some(last_id) = self.prev_node_id.take() {
                    self.add_edge(&last_id, &block_exit_id, CfgEdgeKind::Normal);
                }
                self.prev_node_id = Some(block_exit_id);

                i += 1;
                continue;
            } else if kind == "with_statement" {
                // Python with: set context, walk allocation clause, walk body, emit BlockExit
                self.pending_call_context = CallContext::PythonWith;

                let mut child_cursor = stmt.walk();
                let named: Vec<Node> = stmt.named_children(&mut child_cursor).collect();

                // Walk with_clause children (e.g., open("file"))
                for gc in &named {
                    if gc.kind() == "with_clause" {
                        self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, gc);
                    }
                }

                // Walk the block body
                for gc in &named {
                    if gc.kind() == "block" {
                        self.walk_block(*gc, stmt_range.start_byte);
                    }
                }

                // Emit BlockExit node (zero-length range at end of with statement)
                let block_exit_id =
                    self.add_node(CfgNodeKind::BlockExit, stmt.end_byte() as u32, None);
                if let Some(last_id) = self.prev_node_id.take() {
                    self.add_edge(&last_id, &block_exit_id, CfgEdgeKind::Normal);
                }
                self.prev_node_id = Some(block_exit_id);

                i += 1;
                continue;
            } else if kind == "go_statement" || kind == "defer_statement" {
                // Go goroutine/defer: set call context, process inner expression
                self.pending_call_context = if kind == "go_statement" {
                    CallContext::GoGoroutine
                } else {
                    CallContext::GoDefer
                };
                if let Some(inner) = self.find_first_expression(&stmt) {
                    self.process_go_defer_inner(&inner, stmt_range.start_byte);
                } else {
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                }
                i += 1;
                continue;
            } else {
                // Unknown constructs → treat as statement
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                i += 1;
            }
        }
    }

    /// Handle if/else: Branch → TrueBranch → cons → Join ← FalseBranch ← alt → Join
    /// Returns the index after the if_statement.
    fn walk_if(&mut self, children: &[Node], idx: usize, start_byte: u32) -> usize {
        let if_node = &children[idx];

        // 1. Create Branch node, connect from previous
        let branch_id = self.add_node(CfgNodeKind::Branch, start_byte, Some(if_node));
        if let Some(prev) = self.prev_node_id.take() {
            self.add_edge(&prev, &branch_id, CfgEdgeKind::Normal);
        }

        // Annotate branch node with condition effect (C/C++ only)
        if self.is_c_or_cpp() {
            let (_effect, _target) = self.infer_effect(if_node, CfgNodeKind::Branch);
        }

        // 2. Find consequence and alternative branches
        let (cons_node, alt_node) = find_if_branches(*if_node, self.config.block_kinds);

        // 3. Walk consequence body
        let cons_end = if let Some(cons) = cons_node {
            let saved_edge_count = self.edges.len();
            self.prev_node_id = Some(branch_id);
            self.walk_branch_body(cons);
            // Fix first edge: Branch→first node of consequence to TrueBranch
            if self.edges.len() > saved_edge_count {
                self.edges[saved_edge_count].kind = CfgEdgeKind::TrueBranch;
            }
            self.prev_node_id.take()
        } else {
            None
        };

        // 4. Walk alternative body (if present)
        let alt_end = if let Some(alt) = alt_node {
            let saved_edge_count = self.edges.len();
            self.prev_node_id = Some(branch_id);
            self.walk_branch_body(alt);
            // Fix first edge: Branch→first node of alternative to FalseBranch
            if self.edges.len() > saved_edge_count {
                self.edges[saved_edge_count].kind = CfgEdgeKind::FalseBranch;
            }
            self.prev_node_id.take()
        } else {
            None
        };

        // 5. Create Join node and connect tails
        let join_id = self.add_node(CfgNodeKind::Join, start_byte + 1, None);

        // Connect consequence tail → Join (if branch didn't end with return/throw)
        if let Some(ref last) = cons_end {
            if *last != branch_id {
                self.add_edge(last, &join_id, CfgEdgeKind::Normal);
            }
        }
        // Connect alternative tail → Join
        if let Some(ref last) = alt_end {
            if *last != branch_id {
                self.add_edge(last, &join_id, CfgEdgeKind::Normal);
            }
        }
        // If no else clause, Branch → Join via FalseBranch
        if alt_node.is_none() {
            self.add_edge(&branch_id, &join_id, CfgEdgeKind::FalseBranch);
        }

        self.prev_node_id = Some(join_id);
        idx + 1
    }

    /// Handle switch/case: Branch (dispatch) → CaseBranch → case body → Join.
    ///
    /// # Phase 1 model (conservative approximation)
    ///
    /// Each `case`/`default` clause becomes an **independent sibling path** from
    /// the dispatch Branch node into a shared Join, mirroring the if/else shape
    /// so [`super::super`]'s `BranchDiffEngine` can compare cases as siblings.
    /// The first edge from the Branch into every case body is retagged to
    /// [`CfgEdgeKind::CaseBranch`].
    ///
    /// **Fall-through is NOT modeled.** In C-family languages a case without a
    /// terminating `break` falls through to the next case at runtime; here every
    /// case tail connects only to the Join. This is a deliberate over-connection
    /// to Join (never a spurious edge between cases), keeping the CFG a safe
    /// under-approximation of inter-case flow. See `docs/roadmap.md` §8.2.
    ///
    /// Returns the index after the switch statement.
    fn walk_switch(&mut self, children: &[Node], idx: usize, start_byte: u32) -> usize {
        let switch_node = &children[idx];

        // 1. Create Branch (dispatch) node, connect from previous.
        let branch_id = self.add_node(CfgNodeKind::Branch, start_byte, Some(switch_node));
        if let Some(prev) = self.prev_node_id.take() {
            self.add_edge(&prev, &branch_id, CfgEdgeKind::Normal);
        }

        // 2. Find the case/default clauses. Go keeps cases as direct children of
        //    the switch node; C/Java/TS/C# nest them under a body container.
        let case_clauses = self.find_switch_cases(*switch_node);

        // 3. Walk each case body as an independent path from the Branch.
        let mut case_tails: Vec<types::ids::CfgNodeId> = Vec::new();
        for clause in &case_clauses {
            // Statement nodes belonging to this case clause (skip the case
            // label / pattern nodes, which are not executable statements).
            let body_stmts = self.case_body_statements(clause);
            if body_stmts.is_empty() {
                // Empty case (e.g. C fall-through label `case 1:` with no body,
                // or `default:` with nothing). No node is emitted; the dispatch
                // simply reaches Join for this arm below.
                continue;
            }

            let saved_edge_count = self.edges.len();
            self.prev_node_id = Some(branch_id);
            self.walk_stmt_list(&body_stmts);
            // Retag the first edge (Branch → first node of this case body) to
            // CaseBranch, matching how walk_if tags TrueBranch/FalseBranch.
            if self.edges.len() > saved_edge_count {
                self.edges[saved_edge_count].kind = CfgEdgeKind::CaseBranch;
            }
            if let Some(tail) = self.prev_node_id.take() {
                case_tails.push(tail);
            }
        }

        // 4. Create Join node; connect each case tail → Join. Cases ending in
        //    return/throw leave `prev_node_id` cleared, so they never enqueue a
        //    tail here (matching walk_if semantics).
        let join_id = self.add_node(CfgNodeKind::Join, start_byte + 1, None);
        for tail in &case_tails {
            if *tail != branch_id {
                self.add_edge(tail, &join_id, CfgEdgeKind::Normal);
            }
        }
        // The dispatch can always skip straight to Join when no case matches
        // (or a case is empty), so connect Branch → Join directly. This keeps
        // Join reachable and mirrors the implicit false-edge in walk_if.
        self.add_edge(&branch_id, &join_id, CfgEdgeKind::CaseBranch);

        self.prev_node_id = Some(join_id);
        idx + 1
    }

    /// Collect the case/default clause nodes of a switch statement.
    ///
    /// Handles both layouts observed across grammars:
    /// - Nested: `switch → body-container → case_clauses` (C/C++, Java, TS/JS, C#).
    /// - Flat: `switch → case_clauses` as direct children (Go).
    fn find_switch_cases<'a>(&self, switch_node: Node<'a>) -> Vec<Node<'a>> {
        // First, look for case clauses directly under the switch node (Go).
        let mut direct: Vec<Node> = Vec::new();
        let mut cursor = switch_node.walk();
        for child in switch_node.named_children(&mut cursor) {
            if self.config.case_kinds.contains(&child.kind()) {
                direct.push(child);
            }
        }
        if !direct.is_empty() {
            return direct;
        }

        // Otherwise, descend into the switch body container and collect the
        // case clauses nested inside it (C/Java/TS/C#).
        let mut cases: Vec<Node> = Vec::new();
        let mut cursor = switch_node.walk();
        for child in switch_node.named_children(&mut cursor) {
            // The body container is any child that itself contains case clauses.
            let mut inner_cursor = child.walk();
            let inner_cases: Vec<Node> = child
                .named_children(&mut inner_cursor)
                .filter(|c| self.config.case_kinds.contains(&c.kind()))
                .collect();
            if !inner_cases.is_empty() {
                cases.extend(inner_cases);
            }
        }
        cases
    }

    /// Return the executable statement nodes inside a case/default clause,
    /// skipping the case label / pattern children.
    ///
    /// Grammars differ in how case bodies are structured:
    /// - C/C++/Java/C#: statements are direct children of the case node, after
    ///   the label/pattern (e.g. `case_statement`'s `value`, Java `switch_label`).
    /// - Go: the case wraps its body in a `statement_list` child.
    /// - TS/JS: statements sit in the case node's `body` fields.
    ///
    /// The shared `walk_stmt_list` dispatch already recurses through
    /// `statement_list` wrappers, so Go's container is returned as-is.
    ///
    /// Case labels/patterns/values are identified in two complementary ways so
    /// the walk stays general across grammars:
    /// - by tree-sitter field name (`value`/`type`/`alias`/`pattern`/`guard`/
    ///   `condition`) — used by C/C++, TS/JS, Go;
    /// - by node kind (`switch_label`, `*_pattern`, `when_clause`, …) — used by
    ///   Java and C# whose label nodes carry no field name.
    fn case_body_statements<'a>(&self, clause: &Node<'a>) -> Vec<Node<'a>> {
        let mut cursor = clause.walk();
        let mut stmts = Vec::new();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.is_named() {
                    let field = cursor.field_name().unwrap_or("");
                    if !is_case_label_field(field) && !is_case_label_kind(child.kind()) {
                        stmts.push(child);
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        stmts
    }

    /// Walk a single branch body (consequence or alternative).
    /// If the node is a block, walk its children; otherwise emit as statement.
    fn walk_branch_body(&mut self, node: Node) {
        if self.config.block_kinds.contains(&node.kind()) {
            let range = node_text_range(&node, self.source);
            self.walk_block(node, range.start_byte);
        } else {
            // Single-statement body (e.g., `if (x) return 1;`)
            let range = node_text_range(&node, self.source);
            // Determine node kind from the statement type
            let kind = if self.config.return_kinds.contains(&node.kind()) {
                CfgNodeKind::Return
            } else if self.config.throw_kinds.contains(&node.kind()) {
                CfgNodeKind::Throw
            } else {
                CfgNodeKind::Statement
            };
            self.emit_stmt(kind, range.start_byte, &node);
        }
    }

    /// Handle for/while/do: Loop → body → LoopBack → exit (Join)
    fn walk_loop(&mut self, children: &[Node], idx: usize, start_byte: u32) -> usize {
        let loop_node = &children[idx];

        // 1. Create Loop node, connect from previous
        let loop_id = self.add_node(CfgNodeKind::Loop, start_byte, Some(loop_node));
        if let Some(prev) = self.prev_node_id.take() {
            self.add_edge(&prev, &loop_id, CfgEdgeKind::Normal);
        }

        // 2. Find and walk the loop body
        let body = find_loop_body(*loop_node, self.config.block_kinds);

        let body_last = if let Some(body) = body {
            self.prev_node_id = Some(loop_id);

            if self.config.block_kinds.contains(&body.kind()) {
                let body_range = node_text_range(&body, self.source);
                self.walk_block(body, body_range.start_byte);
            } else {
                // Single-statement body
                let body_range = node_text_range(&body, self.source);
                // Determine node kind
                let kind = if self.config.return_kinds.contains(&body.kind()) {
                    CfgNodeKind::Return
                } else if self.config.throw_kinds.contains(&body.kind()) {
                    CfgNodeKind::Throw
                } else {
                    CfgNodeKind::Statement
                };
                self.emit_stmt(kind, body_range.start_byte, &body);
            }

            self.prev_node_id.take()
        } else {
            None
        };

        // 3. LoopBack edge: last body node → Loop (if body didn't end with return/throw)
        if let Some(ref last) = body_last {
            self.add_edge(last, &loop_id, CfgEdgeKind::LoopBack);
        }

        // 4. Exit edge: Loop → Join (post-loop)
        let join_id = self.add_node(CfgNodeKind::Join, start_byte + 1, None);
        self.add_edge(&loop_id, &join_id, CfgEdgeKind::Normal);

        self.prev_node_id = Some(join_id);
        idx + 1
    }

    /// Node-based wrapper for `walk_if`: collects children and delegates.
    fn walk_if_node(&mut self, node: Node, start_byte: u32) {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        self.walk_if(&children, 0, start_byte);
    }

    /// Node-based wrapper for `walk_loop`: collects children and delegates.
    fn walk_loop_node(&mut self, node: Node, start_byte: u32) {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        self.walk_loop(&children, 0, start_byte);
    }

    /// Emit a statement/return/throw node and connect to previous.
    fn emit_stmt(
        &mut self,
        kind: CfgNodeKind,
        start_byte: u32,
        stmt_node: &Node,
    ) -> types::ids::CfgNodeId {
        let node_id = self.add_node(kind, start_byte, Some(stmt_node));

        // (callee_name extraction removed — superseded by DataFlow-based analysis)

        // (effect annotation removed — superseded by EffectComposer DataFlow analysis)

        // Link from previous statement
        if let Some(prev) = self.prev_node_id.take() {
            self.add_edge(&prev, &node_id, CfgEdgeKind::Normal);
        }
        if kind != CfgNodeKind::Return && kind != CfgNodeKind::Throw {
            self.prev_node_id = Some(node_id);
        }
        node_id
    }

    /// Find the first expression child inside a `go_statement` or `defer_statement` node.
    fn find_first_expression<'a>(&self, node: &Node<'a>) -> Option<Node<'a>> {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let kind = child.kind();
            if kind == "call_expression" || kind == "func_literal" || kind == "expression_statement"
            {
                return Some(child);
            }
        }
        None
    }

    /// Process the inner expression of a `go_statement`/`defer_statement`.
    fn process_go_defer_inner(&mut self, inner: &Node, start_byte: u32) {
        let mut inner_cursor = inner.walk();
        if inner.kind() == "expression_statement" {
            // Unwrap expression_statement wrapper (some tree-sitter grammars)
            if let Some(expr) = inner.named_children(&mut inner_cursor).next() {
                self.emit_stmt(CfgNodeKind::Statement, start_byte, &expr);
                return;
            }
        }
        if inner.kind() == "call_expression" {
            // Find the callee (function name) and emit as statement
            let callee = inner
                .named_children(&mut inner_cursor)
                .next()
                .unwrap_or(*inner);
            self.emit_stmt(CfgNodeKind::Statement, start_byte, &callee);
        } else {
            self.emit_stmt(CfgNodeKind::Statement, start_byte, inner);
        }
    }

    /// Walk the body of a React cleanup arrow function with
    /// `ReactEffectCleanup` scope context.  All CFG nodes generated inside
    /// the body will inherit this context so that `compose_effects` can mark
    /// their `Free` effects as `Deferred`.
    ///
    /// Arrow bodies are walked disconnected from the parent CFG chain —
    /// `prev_node_id` is saved/restored so the cleanup nodes form their own
    /// isolated subgraph.
    fn walk_react_cleanup_arrow(&mut self, arrow_fn: &Node, start_byte: u32) {
        let saved_prev = self.prev_node_id;
        let saved_scope = self.scope_call_context;
        self.scope_call_context = CallContext::ReactEffectCleanup;

        // Find the arrow function body (skip formal_parameters).
        let body = find_function_body(*arrow_fn, self.config.block_kinds);
        if let Some(body_node) = body {
            if self.config.block_kinds.contains(&body_node.kind()) {
                // Block body `{ ... }` — walk each statement.
                // Start with no prev_node_id so these nodes are
                // disconnected from the parent chain (they sit after a
                // Return, which terminates the parent path).
                self.prev_node_id = None;
                self.walk_block(body_node, start_byte);
            } else {
                // Expression body `expr` — emit as single statement.
                self.prev_node_id = None;
                self.emit_stmt(CfgNodeKind::Statement, start_byte, &body_node);
            }
        }

        self.scope_call_context = saved_scope;
        self.prev_node_id = saved_prev;
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// For an if-like node, find the consequence and alternative branch bodies.
///
/// In tree-sitter, the else branch is wrapped in an `else_clause` node in some
/// languages (TS/JS/Rust) or may be a direct child in others (Java/Go/C).
/// This helper handles both cases.
fn find_if_branches<'a>(
    node: Node<'a>,
    block_kinds: &[&str],
) -> (Option<Node<'a>>, Option<Node<'a>>) {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();

    // Strategy 1: find direct block_kind children
    let blocks: Vec<Node> = children
        .iter()
        .filter(|c| block_kinds.contains(&c.kind()))
        .copied()
        .collect();

    let cons = if !blocks.is_empty() {
        Some(blocks[0])
    } else if children.len() >= 2 {
        // Fallback: index-based (children[0]=condition, children[1]=consequence)
        Some(children[1])
    } else {
        None
    };

    // Find alternative: look for else_clause wrapper first, then direct block_kind
    let mut alt = None;
    if blocks.len() >= 2 {
        alt = Some(blocks[1]);
    } else {
        // Search for wrapper nodes that contain the alternative body
        for child in &children {
            match child.kind() {
                "else_clause" | "else" => {
                    let mut sub_cursor = child.walk();
                    let sub_children: Vec<Node> = child.named_children(&mut sub_cursor).collect();
                    // Prefer a block_kind body, fall back to first child
                    for sub in &sub_children {
                        if block_kinds.contains(&sub.kind()) {
                            alt = Some(*sub);
                            break;
                        }
                    }
                    if alt.is_none() {
                        alt = sub_children.first().copied();
                    }
                    break;
                }
                _ => {}
            }
        }
        // Fallback: if still no alternative, try children[2] direct
        if alt.is_none() && children.len() > 2 {
            alt = children.get(2).copied();
        }
    }

    (cons, alt)
}

/// Find the body block/statement of a loop node (for/while/do).
///
/// Looks for a child matching `block_kinds` first; if none found, returns
/// the last named child (single-statement body like `while (x) doSomething();`).
fn find_loop_body<'a>(node: Node<'a>, block_kinds: &[&str]) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();

    // Prefer block_kind children (statement_block, block, compound_statement)
    for child in &children {
        if block_kinds.contains(&child.kind()) {
            return Some(*child);
        }
    }

    // Fallback: last named child (for single-statement body)
    children.last().copied()
}

fn find_function_body<'a>(node: Node<'a>, block_kinds: &[&str]) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if block_kinds.contains(&child.kind()) {
            return Some(child);
        }
        // Arrow function: body might be an expression
        if node.kind() == "arrow_function" && child.kind() != "formal_parameters" {
            return Some(child);
        }
        // Recursive: the block might be nested
        if let Some(found) = find_function_body(child, block_kinds) {
            return Some(found);
        }
    }
    None
}

fn node_text_range(node: &Node, _source: &[u8]) -> TextRange {
    TextRange {
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
        start_line: node.start_position().row as u32,
        start_column: node.start_position().column as u32,
        end_line: node.end_position().row as u32,
        end_column: node.end_position().column as u32,
    }
}

/// Whether a tree-sitter field name identifies a case label/value/guard child
/// rather than an executable statement.  The same field name is used across
/// grammars for the same structural role:
///
/// | Field        | Languages                        |
/// |--------------|----------------------------------|
/// | `value`      | C/C++, TS/JS, Go (expr_switch)   |
/// | `type`       | Go (type_switch)                 |
/// | `alias`      | Go (type_switch variable bind)   |
/// | `pattern`    | C# (constant_pattern, etc.)      |
/// | `guard`      | C# (case_guard)                  |
/// | `condition`  | C++ (condition_clause, if/switch) |
pub fn is_case_label_field(field: &str) -> bool {
    matches!(
        field,
        "value" | "type" | "alias" | "pattern" | "guard" | "condition"
    )
}

/// Whether a tree-sitter node kind represents a case label/pattern/guard that
/// should be skipped when extracting executable statements from a case clause.
///
/// This covers languages whose case labels are unnamed children (no field name
/// assigned by the grammar), notably Java and C#.
pub fn is_case_label_kind(kind: &str) -> bool {
    // Java (`switch_label`), C# (`case_switch_label`, `default_switch_label`)
    if kind.ends_with("_label") || kind == "switch_label" {
        return true;
    }
    // C# patterns (`constant_pattern`, `relational_pattern`, `when_clause`, …)
    if kind.ends_with("_pattern") || kind == "when_clause" {
        return true;
    }
    false
}

// ── CFG extraction helpers (used by extract.rs) ─────────────────────────

/// Function node kinds that CfgBuilder handles across languages.
const FUNCTION_NODE_KINDS: &[&str] = &[
    "function_declaration",
    "method_definition",
    "arrow_function",
    "generator_function_declaration",
    "generator_function",
    "function_definition",
    "async_function_definition",
    "method_declaration",
    "constructor_declaration",
    "function_item",    // tree-sitter-rust
    "method",           // tree-sitter-ruby
    "singleton_method", // tree-sitter-ruby
];

/// Build per-function control-flow graphs by matching function symbols
/// to tree-sitter nodes.
pub(crate) fn build_cfg_for_functions<'a>(
    language: Language,
    root: Node<'a>,
    symbols: &[SymbolDef],
    source_bytes: &[u8],
) -> anyhow::Result<CfgResult> {
    let function_symbols: Vec<&SymbolDef> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();

    let mut all_nodes = Vec::new();
    let mut all_edges = Vec::new();

    for sym in &function_symbols {
        if let Some(func_node) = find_function_node(root, sym) {
            let result = CfgBuilder::build(language, &sym.id, func_node, source_bytes);
            all_nodes.extend(result.nodes);
            all_edges.extend(result.edges);
        }
    }

    Ok(CfgResult {
        nodes: all_nodes,
        edges: all_edges,
    })
}

/// Walk up from the symbol's name position to find the enclosing function node.
fn find_function_node<'a>(root: Node<'a>, symbol: &SymbolDef) -> Option<Node<'a>> {
    let pos = symbol.name_range.start_byte as usize;
    let mut node = root.descendant_for_byte_range(pos, pos)?;
    // Walk up parent chain to find the enclosing function node
    loop {
        if FUNCTION_NODE_KINDS.contains(&node.kind()) {
            return Some(node);
        }
        node = node.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::create_frontend;
    use types::enums::Language;
    use types::ids::FileId;

    fn parse_ts(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let source_bytes = source.as_bytes().to_vec();
        let mut parser = tree_sitter::Parser::new();
        let frontend = create_frontend(Language::TypeScript).unwrap();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .unwrap();
        let tree = parser.parse(&source_bytes, None).unwrap();
        (tree, source_bytes)
    }

    fn find_function<'a>(tree: &'a tree_sitter::Tree, source: &[u8]) -> (Node<'a>, SymbolId) {
        let root = tree.root_node();
        let file_id = FileId::generate("test.ts");
        find_function_recursive(root, &file_id, source).expect("no function found")
    }

    fn find_function_recursive<'a>(
        node: Node<'a>,
        file_id: &FileId,
        source: &[u8],
    ) -> Option<(Node<'a>, SymbolId)> {
        if node.kind() == "function_declaration"
            || node.kind() == "method_definition"
            || node.kind() == "arrow_function"
        {
            let name = node
                .named_child(0)
                .and_then(|c| {
                    if c.kind() == "identifier" {
                        c.utf8_text(source).ok()
                    } else {
                        None
                    }
                })
                .unwrap_or("anon");
            let fid = SymbolId::generate(file_id, "typescript", name, "function", None);
            return Some((node, fid));
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(found) = find_function_recursive(child, file_id, source) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_cfg_builder_simple_function() {
        let source = "function hello() { const x = 1; return x; }";
        let (tree, source_bytes) = parse_ts(source);
        let (func_node, func_id) = find_function(&tree, &source_bytes);

        let result = CfgBuilder::build(Language::TypeScript, &func_id, func_node, &source_bytes);

        assert!(
            result.nodes.len() >= 3,
            "Expected at least Entry + Statement + Exit"
        );
        assert!(result.edges.len() >= 2, "Expected at least 2 edges");

        // Check that there's an Entry, at least one Statement/Return, and Exit
        let has_entry = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Entry);
        let has_exit = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Exit);
        assert!(has_entry);
        assert!(has_exit);
    }

    #[test]
    fn test_cfg_builder_if_else() {
        let source = r#"function check(x: number) {
  if (x > 0) { return 1; } else { return -1; }
}"#;
        let (tree, source_bytes) = parse_ts(source);
        let (func_node, func_id) = find_function(&tree, &source_bytes);

        let result = CfgBuilder::build(Language::TypeScript, &func_id, func_node, &source_bytes);

        let has_branch = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch);
        let has_join = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join);
        assert!(has_branch, "Expected Branch node for if/else");
        assert!(has_join, "Expected Join node for if/else");
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .unwrap();
        assert_eq!(branch.stmt_range.start_line, 1);
    }

    fn parse_lang(lang: Language, source: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let source_bytes = source.as_bytes().to_vec();
        let mut parser = tree_sitter::Parser::new();
        let frontend = create_frontend(lang).unwrap();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .unwrap();
        let tree = parser.parse(&source_bytes, None).unwrap();
        (tree, source_bytes)
    }

    /// Build a CFG for the first `function_definition` found in the tree (C/C++).
    fn build_cfg_for_first_fn(lang: Language, source: &str) -> super::CfgResult {
        let (tree, source_bytes) = parse_lang(lang, source);
        let root = tree.root_node();
        let file_id = FileId::generate("test.c");
        let mut cursor = root.walk();
        let (func_node, fid) = root
            .named_children(&mut cursor)
            .filter(|n| {
                n.kind() == "function_definition"
                    || n.kind() == "function_declaration"
                    || n.kind() == "method_declaration"
            })
            .find_map(|n| {
                let name = n
                    .named_child(0)
                    .and_then(|c| {
                        if c.kind() == "identifier" {
                            c.utf8_text(&source_bytes).ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or("anon");
                let fid = SymbolId::generate(&file_id, "", name, "function", None);
                Some((n, fid))
            })
            .expect("no function definition found");
        CfgBuilder::build(lang, &fid, func_node, &source_bytes)
    }

    /// TS/JS: find function by name.
    fn build_cfg_for_fn_ts(source: &str) -> super::CfgResult {
        let (tree, source_bytes) = parse_ts(source);
        let (func_node, func_id) = find_function(&tree, &source_bytes);
        CfgBuilder::build(Language::TypeScript, &func_id, func_node, &source_bytes)
    }

    /// Build a CFG for a Java method by wrapping it in a minimal class.
    fn build_cfg_for_java_method(method_src: &str) -> super::CfgResult {
        let source = format!("class T{{ {method_src} }}");
        let (tree, source_bytes) = parse_lang(Language::Java, &source);
        let root = tree.root_node();
        let file_id = FileId::generate("test.java");
        // Recursively find the first method_declaration.
        fn find_method<'a>(node: Node<'a>) -> Option<Node<'a>> {
            if node.kind() == "method_declaration" {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(found) = find_method(child) {
                    return Some(found);
                }
            }
            None
        }
        let func_node = find_method(root).expect("no method found");
        let name = func_node
            .named_child(1)
            .and_then(|c| {
                if c.kind() == "identifier" {
                    c.utf8_text(&source_bytes).ok()
                } else {
                    None
                }
            })
            .unwrap_or("anon");
        let fid = SymbolId::generate(&file_id, "", name, "method", None);
        CfgBuilder::build(Language::Java, &fid, func_node, &source_bytes)
    }

    // ── Switch CFG tests ──────────────────────────────────────────

    /// A TypeScript switch with 3 cases + default should produce:
    ///   Branch + 4 CaseBranch edges + Join
    #[test]
    fn test_switch_cfg_ts() {
        let result = build_cfg_for_fn_ts(
            "function f(x: number) {
               switch (x) {
                 case 1: a(); break;
                 case 2: b(); break;
                 case 3: c(); break;
                 default: d();
               }
             }",
        );
        let has_branch = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch);
        let has_join = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join);
        assert!(has_branch, "Expected Branch node for switch");
        assert!(has_join, "Expected Join node for switch");
        let branch = result
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Branch)
            .unwrap();
        // Find CaseBranch edges out of the Branch node
        let cb_count = result
            .edges
            .iter()
            .filter(|e| e.source == branch.id && e.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert!(
            cb_count >= 4,
            "Expected >= 4 CaseBranch edges (3 case + 1 default + 1 no-match skip), got {cb_count}"
        );
        // Ensure each case body was connected to Join
        let join = result
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Join)
            .unwrap();
        let normal_to_join = result
            .edges
            .iter()
            .filter(|e| e.target == join.id && e.kind == CfgEdgeKind::Normal)
            .count();
        assert!(
            normal_to_join >= 3,
            "Expected >= 3 Normal edges into Join (3 cases that don't return), got {normal_to_join}"
        );
    }

    /// C switch with 3 cases + default.
    #[test]
    fn test_switch_cfg_c() {
        let result = build_cfg_for_first_fn(
            Language::C,
            "void f(int x) {
               switch (x) {
                 case 1: free(a); break;
                 case 2: g(); break;
                 case 3: h(); break;
                 default: i();
               }
             }",
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Expected Branch for C switch"
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Expected Join for C switch"
        );
        let branch = result
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Branch)
            .unwrap();
        let cb_count = result
            .edges
            .iter()
            .filter(|e| e.source == branch.id && e.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert!(
            cb_count >= 4,
            "Expected >= 4 CaseBranch edges for C switch (3 case + default + no-match)"
        );
    }

    /// Java switch_expression with 3 cases.
    #[test]
    fn test_switch_cfg_java() {
        let result = build_cfg_for_java_method(
            "void f(int x) {
               switch (x) {
                 case 1: a(); break;
                 case 2: b(); break;
                 case 3: c(); break;
                 default: d();
               }
             }",
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Expected Branch for Java switch"
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Expected Join for Java switch"
        );
        let branch = result
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Branch)
            .unwrap();
        let cb_count = result
            .edges
            .iter()
            .filter(|e| e.source == branch.id && e.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert!(
            cb_count >= 4,
            "Expected >= 4 CaseBranch edges for Java switch, got {cb_count}"
        );
    }

    /// Go switch with 2 cases + default.
    #[test]
    fn test_switch_cfg_go() {
        let result = build_cfg_for_first_fn(
            Language::Go,
            "func f(x int) {
               switch x {
               case 1: a()
               case 2: b()
               default: c()
               }
             }",
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Expected Branch for Go switch"
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Expected Join for Go switch"
        );
        let branch = result
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Branch)
            .unwrap();
        let cb_count = result
            .edges
            .iter()
            .filter(|e| e.source == branch.id && e.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert!(
            cb_count >= 3,
            "Expected >= 3 CaseBranch edges for Go switch (2 case + default + no-match), got {cb_count}"
        );
    }

    /// try_statement is STILL deferred — remains a single Statement node.
    #[test]
    fn test_try_statement_still_deferred() {
        let result = build_cfg_for_fn_ts(
            "function f() {
               try { risky(); } catch(e) { handle(); }
             }",
        );
        let has_branch = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch);
        let has_join = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join);
        assert!(!has_branch, "try_statement should NOT create a Branch");
        assert!(!has_join, "try_statement should NOT create a Join");
        let stmt_count = result
            .nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Statement)
            .count();
        assert!(
            stmt_count == 1,
            "Expected exactly 1 Statement node for deferred try, got {stmt_count}"
        );
    }
}
