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
use types::enums::{CfgEdgeKind, CfgNodeKind, EffectKind, Language, SymbolKind};
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
        },
        Language::Rust => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_expression"],
            loop_kinds: &["for_expression", "while_expression", "loop_expression"],
            return_kinds: &["return_expression"],
            throw_kinds: &[], // Rust uses Result, not throw
            stmt_kinds: &[
                "expression_statement",
                "let_declaration",
                "continue_expression",
                "break_expression",
            ],
        },
        Language::Cangjie => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["ifExpression"],
            loop_kinds: &["whileExpression", "forInExpression", "doWhileExpression"],
            return_kinds: &["jumpExpression"], // jumpExpression covers return/break/continue
            throw_kinds: &[],
            stmt_kinds: &["variableDeclaration", "expressionStatement"],
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
        let mut ctx = CfgContext {
            function_id: *function_id,
            nodes: Vec::new(),
            edges: Vec::new(),
            source: source_bytes,
            prev_node_id: None,
            language,
            config,
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
    fn add_node(&mut self, kind: CfgNodeKind, start_byte: u32, stmt_node: Option<&Node>) -> types::ids::CfgNodeId {
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
        let node = CfgNode::new(&self.function_id, kind, range);
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

    /// Infer the effect of a C/C++ CFG node. Returns (effect_kind, target_field).
    /// Uses simple tree-sitter node kind matching — NOT full dataflow analysis.
    fn infer_effect(&self, node: &Node, node_kind: CfgNodeKind) -> (Option<EffectKind>, Option<String>) {
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
                    return (if is_delete { Some(EffectKind::Free) } else { Some(EffectKind::Allocate) }, None);
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

    /// Canonicalize a field path by replacing `->` with `.` for consistency.
    fn canonicalize_field_path(path: &str) -> String {
        path.replace("->", ".")
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
                if parts.is_empty() { None } else { Some(Self::canonicalize_field_path(&parts.join("."))) }
            }
            "subscript_expression" => {
                // array[index] — extract array name
                let mut cursor = node.walk();
                if let Some(first) = node.named_children(&mut cursor).next() {
                    if let Ok(text) = first.utf8_text(self.source) {
                        return Some(Self::canonicalize_field_path(&text.to_string()));
                    }
                }
                None
            }
            "identifier" => {
                if let Ok(text) = node.utf8_text(self.source) {
                    Some(Self::canonicalize_field_path(&text.to_string()))
                } else {
                    None
                }
            }
            _ => {
                // Walk children to find field expressions
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if let Some(path) = self.extract_field_path(&child) {
                        return Some(Self::canonicalize_field_path(&path));
                    }
                }
                None
            }
        }
    }

    fn walk_block(&mut self, block: Node, _block_start: u32) {
        let mut cursor = block.walk();
        let children: Vec<Node> = block.named_children(&mut cursor).collect();

        // Process each statement in the block
        let mut i = 0;
        while i < children.len() {
            let stmt = children[i];
            let kind = stmt.kind();
            let stmt_range = node_text_range(&stmt, self.source);

            if self.config.if_kinds.contains(&kind) {
                i = self.walk_if(&children, i, stmt_range.start_byte);
            } else if self.config.loop_kinds.contains(&kind) {
                i = self.walk_loop(&children, i, stmt_range.start_byte);
            } else if self.config.return_kinds.contains(&kind) {
                self.emit_stmt(CfgNodeKind::Return, stmt_range.start_byte, &stmt);
                i += 1;
            } else if self.config.throw_kinds.contains(&kind) {
                self.emit_stmt(CfgNodeKind::Throw, stmt_range.start_byte, &stmt);
                i += 1;
            } else if self.config.stmt_kinds.contains(&kind) {
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                i += 1;
            } else if self.config.block_kinds.contains(&kind) {
                // Nested block
                self.walk_block(stmt, stmt_range.start_byte);
                i += 1;
            } else if kind == "try_statement" || kind == "switch_statement" {
                // Deferred: treat as single statement
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
        let branch_id = self.add_node(CfgNodeKind::Branch, start_byte, None);
        if let Some(prev) = self.prev_node_id.take() {
            self.add_edge(&prev, &branch_id, CfgEdgeKind::Normal);
        }

        // Annotate branch node with condition effect (C/C++ only)
        if self.is_c_or_cpp() {
            let (effect, target) = self.infer_effect(if_node, CfgNodeKind::Branch);
            if let Some(ref mut last) = self.nodes.last_mut() {
                last.effect_kind = effect;
                last.target_field = target;
            }
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
        let loop_id = self.add_node(CfgNodeKind::Loop, start_byte, None);
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

    /// Emit a statement/return/throw node and connect to previous.
    fn emit_stmt(&mut self, kind: CfgNodeKind, start_byte: u32, stmt_node: &Node) -> types::ids::CfgNodeId {
        let node_id = self.add_node(kind, start_byte, Some(stmt_node));

        // Annotate effect for C/C++ (language check via self.config)
        if self.is_c_or_cpp() {
            let (effect, target) = self.infer_effect(stmt_node, kind);
            if let Some(ref mut last) = self.nodes.last_mut() {
                last.effect_kind = effect;
                last.target_field = target;
            }
        }

        // Link from previous statement
        if let Some(prev) = self.prev_node_id.take() {
            self.add_edge(&prev, &node_id, CfgEdgeKind::Normal);
        }
        if kind != CfgNodeKind::Return && kind != CfgNodeKind::Throw {
            self.prev_node_id = Some(node_id);
        }
        node_id
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

    let cons = if blocks.len() >= 1 {
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
    "function_item", // tree-sitter-rust
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
        let source = "function check(x: number) { if (x > 0) { return 1; } else { return -1; } }";
        let (tree, source_bytes) = parse_ts(source);
        let (func_node, func_id) = find_function(&tree, &source_bytes);

        let result = CfgBuilder::build(Language::TypeScript, &func_id, func_node, &source_bytes);

        let has_branch = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch);
        let has_join = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join);
        assert!(has_branch, "Expected Branch node for if/else");
        assert!(has_join, "Expected Join node for if/else");
    }
}
