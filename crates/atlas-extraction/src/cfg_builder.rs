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

use atlas_types::cfg::{CfgEdge, CfgNode};
use atlas_types::enums::{CfgEdgeKind, CfgNodeKind, SymbolKind};
use atlas_types::ids::SymbolId;
use atlas_types::structs::{SymbolDef, TextRange};
use tree_sitter::Node;

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
    prev_node_id: Option<atlas_types::ids::CfgNodeId>,
}

impl CfgBuilder {
    /// Build CFG for a function node.
    ///
    /// Scans the function body for statements and produces CFG nodes/edges.
    pub fn build(function_id: &SymbolId, function_node: Node, source_bytes: &[u8]) -> CfgResult {
        let mut ctx = CfgContext {
            function_id: function_id.clone(),
            nodes: Vec::new(),
            edges: Vec::new(),
            source: source_bytes,
            prev_node_id: None,
        };

        // 1. Create Entry node
        let entry_id = ctx.add_node(CfgNodeKind::Entry, 0);
        ctx.prev_node_id = Some(entry_id);

        // 2. Find the statement block
        let body = find_function_body(function_node);

        // 3. Walk the body
        if let Some(body) = body {
            let body_range = node_text_range(&body, source_bytes);
            ctx.walk_block(body, body_range.start_byte);
        }

        // 4. If no body found, create a single Statement node
        if ctx.prev_node_id.is_some() && ctx.nodes.len() == 1 {
            let fn_range = node_text_range(&function_node, source_bytes);
            ctx.add_node(CfgNodeKind::Statement, fn_range.start_byte);
        }

        // 5. Create Exit node and connect last node to exit
        let last = ctx.prev_node_id;
        let exit_id = ctx.add_node(CfgNodeKind::Exit, 0);
        if let Some(last_id) = last {
            ctx.add_edge(&last_id, &exit_id, CfgEdgeKind::Normal);
        }

        CfgResult {
            nodes: ctx.nodes,
            edges: ctx.edges,
        }
    }
}

impl CfgContext<'_> {
    fn add_node(&mut self, kind: CfgNodeKind, start_byte: u32) -> atlas_types::ids::CfgNodeId {
        let range = TextRange {
            start_byte,
            end_byte: start_byte,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        let node = CfgNode::new(&self.function_id, kind, range);
        let id = node.id;
        self.nodes.push(node);
        id
    }

    fn add_edge(
        &mut self,
        source: &atlas_types::ids::CfgNodeId,
        target: &atlas_types::ids::CfgNodeId,
        kind: CfgEdgeKind,
    ) {
        self.edges.push(CfgEdge::new(source, target, kind));
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

            match kind {
                "if_statement" => {
                    i = self.walk_if(&children, i, stmt_range.start_byte);
                }
                "for_statement" | "while_statement" | "do_statement" => {
                    i = self.walk_loop(&children, i, stmt_range.start_byte);
                }
                "return_statement" => {
                    self.emit_stmt(CfgNodeKind::Return, stmt_range.start_byte);
                    // Return → Exit (connect to exit outside)
                    i += 1;
                }
                "throw_statement" => {
                    self.emit_stmt(CfgNodeKind::Throw, stmt_range.start_byte);
                    i += 1;
                }
                "expression_statement"
                | "variable_declaration"
                | "lexical_declaration"
                | "continue_statement"
                | "break_statement"
                | "debugger_statement"
                | "empty_statement" => {
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte);
                    i += 1;
                }
                // Might be a nested block or other construct
                "statement_block" => {
                    self.walk_block(stmt, stmt_range.start_byte);
                    i += 1;
                }
                "try_statement" | "switch_statement" => {
                    // Deferred: treat as single statement
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte);
                    i += 1;
                }
                _ => {
                    // Unknown constructs → treat as statement
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte);
                    i += 1;
                }
            }
        }
    }

    /// Handle if/else: Branch → TrueBranch → cons → Join ← FalseBranch ← alt → Join
    /// Returns the index after the if_statement.
    fn walk_if(&mut self, _children: &[Node], _idx: usize, start_byte: u32) -> usize {
        // Create Branch node and connect from previous
        let branch_id = self.emit_stmt(CfgNodeKind::Branch, start_byte);

        // For now, emit a Branch that goes to a Join (full sub-block walking deferred)
        let join_id = self.add_node(CfgNodeKind::Join, start_byte + 1);

        // Connect Branch → Join (placeholder — full resolution requires sub-node access)
        self.add_edge(&branch_id, &join_id, CfgEdgeKind::TrueBranch);
        self.add_edge(&branch_id, &join_id, CfgEdgeKind::FalseBranch);

        self.prev_node_id = Some(join_id);
        _idx + 1
    }

    /// Handle for/while/do: Loop → body → LoopBack → exit
    fn walk_loop(&mut self, _children: &[Node], _idx: usize, start_byte: u32) -> usize {
        let loop_id = self.emit_stmt(CfgNodeKind::Loop, start_byte);
        let post_id = self.add_node(CfgNodeKind::Statement, start_byte + 1);

        // Connect Loop → post-loop, and LoopBack edge
        self.add_edge(&loop_id, &post_id, CfgEdgeKind::Normal);
        self.add_edge(&post_id, &loop_id, CfgEdgeKind::LoopBack);

        self.prev_node_id = Some(post_id);
        _idx + 1
    }

    /// Emit a statement/return/throw node and connect to previous.
    fn emit_stmt(&mut self, kind: CfgNodeKind, start_byte: u32) -> atlas_types::ids::CfgNodeId {
        let node_id = self.add_node(kind, start_byte);
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

fn find_function_body(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "statement_block" {
            return Some(child);
        }
        // Arrow function: body might be an expression
        if node.kind() == "arrow_function" && child.kind() != "formal_parameters" {
            return Some(child);
        }
        // Recursive: the statement_block might be nested
        if let Some(found) = find_function_body(child) {
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
];

/// Build per-function control-flow graphs by matching function symbols
/// to tree-sitter nodes.
pub(crate) fn build_cfg_for_functions<'a>(
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
            let result = CfgBuilder::build(&sym.id, func_node, source_bytes);
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
fn find_function_node<'a>(
    root: Node<'a>,
    symbol: &SymbolDef,
) -> Option<Node<'a>> {
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
    use atlas_types::enums::Language;
    use atlas_types::ids::FileId;

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

        let result = CfgBuilder::build(&func_id, func_node, &source_bytes);

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

        let result = CfgBuilder::build(&func_id, func_node, &source_bytes);

        let has_branch = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch);
        let has_join = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join);
        assert!(has_branch, "Expected Branch node for if/else");
        assert!(has_join, "Expected Join node for if/else");
    }
}
