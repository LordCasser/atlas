//! DataFlow builder — per-function dataflow graph construction.
//!
//! The DataFlowBuilder creates [`DataNode`]s and [`DataFlowEdge`]s from tree-sitter
//! AST captures.  This forms the basis for intraprocedural dataflow analysis.
//!
//! # Architecture
//!
//! 1. Runs the adapter's `dataflow_builder_query()` to find assignments, returns,
//!    call arguments, member accesses, and literals.
//! 2. Creates [`DataNode`]s for each capture (Parameter, Local, Field, Return,
//!    CallArg, Literal, Expr).
//! 3. Creates [`DataFlowEdge`]s connecting related nodes (Assign, Read, Write,
//!    FieldLoad, FieldStore, ArgToCall, ReturnValue).
//!
//! # Edge-building rules
//!
//! - **Assign**: position-based value → target (Expr nodes between consecutive
//!   Local/Parameter targets).
//! - **FieldLoad**: name-based base → field (looks up the base of an access path
//!   among known locals/params).
//! - **ArgToCall**: callsite-grouped call_arg → call_target (intra-procedural).
//!   CallArg and CallTarget nodes from the same `call_expression` share a
//!   `callsite_id` (set during extraction by walking the AST).  Falls back to
//!   "most recent preceding target" heuristic when `callsite_id` is not set.
//!   (Note: `ArgToParam` is reserved for inter-procedural caller→callee edges.)
//! - **ReturnValue**: contained-node → Return (intra-procedural).  Nodes whose
//!   range falls fully inside a Return node's range (e.g. `CallTarget` in
//!   `return foo()`) are linked to the Return.
//!   (Note: `ReturnToCall` is reserved for inter-procedural callee→caller edges.)
//!
//! # Invariants
//!
//! - Source/Target of [`DataFlowEdge`] are **always** [`DataNodeId`], never
//!   [`SymbolId`].  This is the fundamental invariant that separates the dataflow
//!   graph from the symbol-level graph.
//! - DataNode IDs are deterministic (blake3).
//! - DataFlowEdge IDs are deterministic (blake3(source + target + kind)).
//! - Each DataNode has exactly one function_id (or None for top-level).
//! - Per-function dataflow only (interprocedural deferred to analysis layer).

use std::collections::HashMap;

use types::CallsiteId;
use types::ScopeDef;
use types::bindings::BindingDef;
use types::dataflow::{DataFlowEdge, DataNode};
use types::enums::{DataFlowKind, DataNodeKind, SymbolKind};
use types::ids::{BindingId, DataFlowEdgeId, DataNodeId, ScopeId, SymbolId};
use types::structs::{SymbolDef, TextRange};

use super::frontend::{Capture, DataflowSpec};
use crate::extraction_ctx::ExtractionCtx;

/// Key for mapping tree-sitter capture positions to DataNodeIds.
/// (start_byte, end_byte, DataNodeKind) uniquely identifies a data node
/// within a file.
type NodePosKey = (u32, u32, types::enums::DataNodeKind);

/// Result of dataflow builder extraction.
#[derive(Debug, Clone, Default)]
pub struct DataFlowResult {
    /// All data nodes created.
    pub nodes: Vec<DataNode>,
    /// All dataflow edges created.
    pub edges: Vec<DataFlowEdge>,
}

/// Builds per-function dataflow graphs from tree-sitter AST captures.
pub struct DataFlowBuilder;

impl DataFlowBuilder {
    /// Extract data nodes and dataflow edges from the given AST.
    ///
    /// This runs `dataflow_builder_query()` and creates DataNodes for each
    /// capture.  Intra-function dataflow edges are created by matching
    /// assignment targets to values, return expressions to return nodes,
    /// and call argument expressions to call argument nodes.
    pub(crate) fn extract(
        dataflow_spec: &dyn DataflowSpec,
        ctx: &ExtractionCtx<'_>,
        bindings: &[BindingDef],
        scopes: &[ScopeDef],
        symbols: &[SymbolDef],
    ) -> anyhow::Result<DataFlowResult> {
        let query_src = dataflow_spec.dataflow_builder_query();
        if query_src.trim().is_empty() {
            return Ok(DataFlowResult::default());
        }

        // Collect captures
        let captures = super::query_helpers::collect_captures(
            ctx.ts_lang,
            query_src,
            ctx.root,
            ctx.source_bytes(),
            "dataflow",
        )
        .map_err(|failure| {
            use crate::error::ExtractionFailure;
            let filled = ExtractionFailure {
                file_path: ctx.file_path.to_string_lossy().to_string(),
                language: ctx.language,
                ..failure
            };
            anyhow::Error::new(filled)
        })?;

        let mut nodes: Vec<DataNode> = Vec::new();
        let mut edges: Vec<DataFlowEdge> = Vec::new();

        // Create data nodes from captures, collecting a position→id lookup
        // for later AST-driven edge creation.
        let nctx = ctx.normalize_ctx();
        let mut node_pos_map: HashMap<NodePosKey, DataNodeId> = HashMap::new();
        for (name, node) in captures {
            let start_byte = node.start_byte() as u32;
            let end_byte = node.end_byte() as u32;
            let capture = Capture { name, node };
            let (dn_opt, de_opt) = dataflow_spec.normalize(nctx, capture);
            if let Some(ref dn) = dn_opt {
                let key = (start_byte, end_byte, dn.kind);
                node_pos_map.insert(key, dn.id);
                nodes.push(dn.clone());
            }
            if let Some(de) = de_opt {
                edges.push(de);
            }
        }

    // Post-process: resolve bindings to nodes
    resolve_bindings_to_nodes(&mut nodes, bindings, scopes);

    // Resolve function_ids BEFORE building edges so that
    // FieldLoad, Assign, and containment edges have correct function_id
    // for scope-aware matching.
    resolve_dataflow_function_ids(&mut nodes, symbols);

    // Post-process: create dataflow edges from AST structure
    build_dataflow_edges(
        &nodes,
        bindings,
        ctx,
        &node_pos_map,
        &mut edges,
    );

    // Language-specific edge building (e.g., destructuring, tuple unpacking)
    dataflow_spec.build_language_edges(&nodes, bindings, scopes, &mut edges)?;

        Ok(DataFlowResult { nodes, edges })
    }

    /// Resolve use-def edges across statements within each function.
    ///
    /// After the initial extraction creates intra-statement edges (Assign for
    /// target↔value, FieldLoad for base↔field), this second pass creates
    /// edges from variable definitions to later uses of the same name.
    ///
    /// Key heuristic: nodes with the same `function_id` and name (case-
    /// insensitive) are grouped.  The first Local/Parameter in byte order
    /// is treated as a definition; all later Expr/CallArg/Field nodes are
    /// treated as uses. This enables basic cross-statement dataflow tracking
    /// propagation (e.g. `const x = source; sink(x)`).
    ///
    /// This is a conservative heuristic — it may connect unrelated
    /// occurrences in nested scopes.  Shadowing and SSA-style precision
    /// require BindingGraph (P3-deferred).
    pub fn resolve_use_def(data_nodes: &[DataNode]) -> Vec<DataFlowEdge> {
        resolve_use_def(data_nodes)
    }
}

/// Resolve `binding_id` on data nodes using scope-chain-aware lookup.
///
/// For each DataNode with a `name`, finds the innermost scope containing its
/// range, then walks the scope chain upward looking for a `BindingDef` with
/// a matching name.  This properly handles variable shadowing: same-named
/// vars in nested scopes receive distinct binding_ids.
///
/// Falls back to a flat name-based lookup when the node's range is not
/// contained by any scope.
fn resolve_bindings_to_nodes(nodes: &mut [DataNode], bindings: &[BindingDef], scopes: &[ScopeDef]) {
    // Build scope → bindings map (bindings indexed by their scope_id)
    let mut scope_bindings: HashMap<ScopeId, Vec<&BindingDef>> = HashMap::new();
    for binding in bindings {
        scope_bindings
            .entry(binding.scope_id)
            .or_default()
            .push(binding);
    }

    // Build parent map from the scope tree
    let parent_map: HashMap<ScopeId, Option<ScopeId>> =
        scopes.iter().map(|s| (s.id, s.parent_id)).collect();

    // Flat fallback: for nodes not contained by any scope, find the
    // binding whose range most closely precedes or contains the node's
    // start byte (for the same name).  This replaces the former HashMap
    // which had "last writer wins" collision for same-named bindings.
    //
    // We iterate bindings sorted by range and pick the best match;
    // a binding whose range contains the node is preferred over one
    // whose range precedes it.
    for node in nodes.iter_mut() {
        let name = match &node.name {
            Some(n) => n,
            None => continue,
        };

        // Find the innermost scope containing this DataNode's range
        let containing_scope = innermost_scope_by_range(scopes, node.range);

        let binding_id = match containing_scope {
            Some(scope_id) => {
                // Walk the scope chain upward looking for a binding with
                // the matching name
                let mut found: Option<&BindingId> = None;
                let mut current = Some(scope_id);
                while let Some(sid) = current {
                    if let Some(bindings_in_scope) = scope_bindings.get(&sid) {
                        if let Some(b) = bindings_in_scope
                            .iter()
                            .find(|b| b.name.as_str() == name.as_str())
                        {
                            found = Some(&b.id);
                            break;
                        }
                    }
                    current = parent_map.get(&sid).and_then(|&maybe_parent| maybe_parent);
                }
                if found.is_some() {
                    found
                } else {
                    // Fallback: find closest binding by range for this name
                    closest_binding_by_range(bindings, name, node.range)
                }
            }
            None => closest_binding_by_range(bindings, name, node.range),
        };

        if let Some(&bid) = binding_id {
            node.binding_id = Some(bid);
        }
    }
}

/// Find the innermost scope that fully contains the given range.
fn innermost_scope_by_range(scopes: &[ScopeDef], range: TextRange) -> Option<ScopeId> {
    scopes
        .iter()
        .filter(|s| s.range.start_byte <= range.start_byte && s.range.end_byte >= range.end_byte)
        .min_by_key(|s| s.range.byte_len())
        .map(|s| s.id)
}

/// Find the closest binding with the given name to the given DataNode range.
///
/// Prefers a binding whose range contains the node's range (same scope).
/// Falls back to the closest preceding binding (by byte distance from the
/// node's start byte to the binding's end byte).
fn closest_binding_by_range<'a>(
    bindings: &'a [BindingDef],
    name: &str,
    node_range: TextRange,
) -> Option<&'a BindingId> {
    let candidates: Vec<&BindingDef> = bindings
        .iter()
        .filter(|b| b.name.as_str() == name)
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Prefer a binding whose range fully contains the node's range.
    if let Some(containing) = candidates.iter().find(|b| {
        b.range.start_byte <= node_range.start_byte && b.range.end_byte >= node_range.end_byte
    }) {
        return Some(&containing.id);
    }

    // Fallback: closest preceding binding by byte distance.
    candidates
        .iter()
        .filter(|b| b.range.end_byte <= node_range.start_byte)
        .min_by_key(|b| node_range.start_byte.saturating_sub(b.range.end_byte))
        .map(|b| &b.id)
        .or_else(|| {
            // If no preceding binding, closest following binding.
            candidates
                .iter()
                .filter(|b| b.range.start_byte >= node_range.end_byte)
                .min_by_key(|b| b.range.start_byte.saturating_sub(node_range.end_byte))
                .map(|b| &b.id)
        })
}

/// AST‑driven assignment edge creation.
///
/// Walks the tree-sitter AST looking for `variable_declarator` and
/// `assignment_expression` (TS/JS) or `assignment` (Python) nodes.  For
/// each, looks up the DataNodeIds of the left‑hand target and right‑hand
/// value by their byte range + kind, and creates an Assign edge between
/// them.
///
/// This replaces the former position‑based heuristic (sort‑by‑start_byte
/// then gap‑fill).
fn walk_for_assign_edges(
    node: tree_sitter::Node,
    _source: &str,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    _all_nodes: &[DataNode],
    edges: &mut Vec<DataFlowEdge>,
) {
    let kind = node.kind();

    // variable_declarator: name (Local) ← value (Expr)
    if kind == "variable_declarator" {
        if let (Some(name_node), Some(value_node)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
        ) {
            let name_kind = name_node.kind();
            if name_kind == "object_pattern" || name_kind == "array_pattern"
                || name_kind == "pattern_list" || name_kind == "tuple_pattern"
                || name_kind == "list_pattern" {
                // Destructuring: each binding gets edge from the initializer Expr
                // Walk the pattern to find all identifier bindings and create
                // Assign edges: initializer_Expr → each_destructured_Local
                let value_key = (
                    value_node.start_byte() as u32,
                    value_node.end_byte() as u32,
                    DataNodeKind::Expr,
                );
                if let Some(&source_id) = pos_map.get(&value_key) {
                    let mut bindings: Vec<tree_sitter::Node> = Vec::new();
                    collect_pattern_bindings(name_node, &mut bindings);
                    for binding_node in &bindings {
                        let bind_key = (
                            binding_node.start_byte() as u32,
                            binding_node.end_byte() as u32,
                            DataNodeKind::Local,
                        );
                        if let Some(&target_id) = pos_map.get(&bind_key) {
                            let edge_id = DataFlowEdgeId::generate(
                                &source_id, &target_id, DataFlowKind::Assign.as_str(),
                            );
                            edges.push(DataFlowEdge::new(
                                edge_id, source_id, target_id,
                                DataFlowKind::Assign,
                                ts_node_range(binding_node), 0.85,
                            ));
                        }
                    }
                }
            } else {
                // Simple variable declarator: name (Local) ← value (Expr)
                let name_key = (
                    name_node.start_byte() as u32,
                    name_node.end_byte() as u32,
                    DataNodeKind::Local,
                );
                let value_key = (
                    value_node.start_byte() as u32,
                    value_node.end_byte() as u32,
                    DataNodeKind::Expr,
                );
                if let (Some(&target_id), Some(&source_id)) =
                    (pos_map.get(&name_key), pos_map.get(&value_key))
                {
                    let edge_id =
                        DataFlowEdgeId::generate(&source_id, &target_id, DataFlowKind::Assign.as_str());
                    edges.push(DataFlowEdge::new(
                        edge_id, source_id, target_id,
                        DataFlowKind::Assign,
                        ts_node_range(&name_node), 0.95,
                    ));
                }
            }
        }
    }

    // ── assignment_expression / assignment: left ← right ──────────────
    if kind == "assignment_expression" || kind == "assignment" {
        if let (Some(left_node), Some(right_node)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) {
            let left_start = left_node.start_byte() as u32;
            let left_end = left_node.end_byte() as u32;
            let (target_id, edge_kind) = if let Some(&id) = pos_map.get(&(left_start, left_end, DataNodeKind::Field)) {
                (id, DataFlowKind::FieldStore)
            } else if let Some(&id) = pos_map.get(&(left_start, left_end, DataNodeKind::Local)) {
                (id, DataFlowKind::Assign)
            } else { return; };
            let right_key = (right_node.start_byte() as u32, right_node.end_byte() as u32, DataNodeKind::Expr);
            if let Some(&source_id) = pos_map.get(&right_key) {
                let eid = DataFlowEdgeId::generate(&source_id, &target_id, edge_kind.as_str());
                edges.push(DataFlowEdge::new(eid, source_id, target_id, edge_kind, ts_node_range(&left_node), 0.90));
            }
        }
    }

    // ── Go: short_var_declaration (x := expr) ──────────────────────────
    // left/right are expression_list containers; look inside for actual nodes
    if kind == "short_var_declaration" {
        if let (Some(left_list), Some(right_list)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) {
            create_assign_edges_from_expression_lists(left_list, right_list, pos_map, edges);
        }
    }

    // ── Go: assignment_statement (x = expr) ────────────────────────────
    if kind == "assignment_statement" {
        if let (Some(left_list), Some(right_list)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) {
            create_assign_edges_from_expression_lists(left_list, right_list, pos_map, edges);
        }
    }

    // ── Go: var_spec (var x = expr) ────────────────────────────────────
    if kind == "var_spec" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name_key = (
                name_node.start_byte() as u32,
                name_node.end_byte() as u32,
                DataNodeKind::Local,
            );
            // Value is inside expression_list
            if let Some(val_list) = node.child_by_field_name("value") {
                for i in 0..val_list.child_count() {
                    if let Some(val_node) = val_list.child(i) {
                        if val_node.is_named() {
                            let value_key = (
                                val_node.start_byte() as u32,
                                val_node.end_byte() as u32,
                                DataNodeKind::Expr,
                            );
                            if let (Some(&target_id), Some(&source_id)) =
                                (pos_map.get(&name_key), pos_map.get(&value_key))
                            {
                                let edge_id = DataFlowEdgeId::generate(
                                    &source_id, &target_id, DataFlowKind::Assign.as_str(),
                                );
                                edges.push(DataFlowEdge::new(
                                    edge_id, source_id, target_id, DataFlowKind::Assign,
                                    ts_node_range(&name_node), 0.90,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    // ── C/C++: init_declarator (int x = expr) ──────────────────────────
    if kind == "init_declarator" {
        if let (Some(name_node), Some(value_node)) = (
            node.child_by_field_name("declarator"),
            node.child_by_field_name("value"),
        ) {
            // declarator may be an identifier or pointer_declarator wrapping one
            let actual_name = if name_node.kind() == "identifier" {
                Some(name_node)
            } else {
                // pointer_declarator, array_declarator, etc. — find inner identifier
                find_identifier_child(name_node)
            };
            if let Some(id_node) = actual_name {
                let name_key = (id_node.start_byte() as u32, id_node.end_byte() as u32, DataNodeKind::Local);
                let value_key = (value_node.start_byte() as u32, value_node.end_byte() as u32, DataNodeKind::Expr);
                if let (Some(&target_id), Some(&source_id)) = (pos_map.get(&name_key), pos_map.get(&value_key)) {
                    let edge_id = DataFlowEdgeId::generate(&source_id, &target_id, DataFlowKind::Assign.as_str());
                    edges.push(DataFlowEdge::new(edge_id, source_id, target_id, DataFlowKind::Assign, ts_node_range(&id_node), 0.90));
                }
            }
        }
    }

    // ── Rust: let_declaration (let x = expr) ───────────────────────────
    if kind == "let_declaration" {
        if let (Some(pattern_node), Some(value_node)) = (
            node.child_by_field_name("pattern"),
            node.child_by_field_name("value"),
        ) {
            // pattern may be identifier or destructuring pattern
            if pattern_node.kind() == "identifier" {
                let name_key = (pattern_node.start_byte() as u32, pattern_node.end_byte() as u32, DataNodeKind::Local);
                let value_key = (value_node.start_byte() as u32, value_node.end_byte() as u32, DataNodeKind::Expr);
                if let (Some(&target_id), Some(&source_id)) = (pos_map.get(&name_key), pos_map.get(&value_key)) {
                    let edge_id = DataFlowEdgeId::generate(&source_id, &target_id, DataFlowKind::Assign.as_str());
                    edges.push(DataFlowEdge::new(edge_id, source_id, target_id, DataFlowKind::Assign, ts_node_range(&pattern_node), 0.90));
                }
            }
        }
    }

    // ── Kotlin: variable_declaration (val x = expr) ────────────────────
    if kind == "variable_declaration" {
        // In tree-sitter-kotlin, variable_declaration contains simple_identifier + optional expression
        let mut name_node: Option<tree_sitter::Node> = None;
        let mut value_node: Option<tree_sitter::Node> = None;
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.is_named() {
                    if child.kind() == "simple_identifier" && name_node.is_none() {
                        name_node = Some(child);
                    } else if value_node.is_none() {
                        value_node = Some(child);
                    }
                }
            }
        }
        if let (Some(name), Some(value)) = (name_node, value_node) {
            let name_key = (name.start_byte() as u32, name.end_byte() as u32, DataNodeKind::Local);
            let value_key = (value.start_byte() as u32, value.end_byte() as u32, DataNodeKind::Expr);
            if let (Some(&target_id), Some(&source_id)) = (pos_map.get(&name_key), pos_map.get(&value_key)) {
                let edge_id = DataFlowEdgeId::generate(&source_id, &target_id, DataFlowKind::Assign.as_str());
                edges.push(DataFlowEdge::new(edge_id, source_id, target_id, DataFlowKind::Assign, ts_node_range(&name), 0.85));
            }
        }
    }

    // Recurse into children
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            walk_for_assign_edges(child, _source, pos_map, _all_nodes, edges);
        }
    }
}

/// Create Assign edges from Go expression_list pairs.
fn create_assign_edges_from_expression_lists(
    left_list: tree_sitter::Node,
    right_list: tree_sitter::Node,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    let left_nodes: Vec<tree_sitter::Node> = (0..left_list.child_count())
        .filter_map(|i| left_list.child(i))
        .filter(|c| c.is_named())
        .collect();
    let right_nodes: Vec<tree_sitter::Node> = (0..right_list.child_count())
        .filter_map(|i| right_list.child(i))
        .filter(|c| c.is_named())
        .collect();
    for (i, left_node) in left_nodes.iter().enumerate() {
        let target_kind = if left_node.kind().contains("identifier") { DataNodeKind::Local } else { DataNodeKind::Field };
        let left_key = (left_node.start_byte() as u32, left_node.end_byte() as u32, target_kind);
        let right_node = right_nodes.get(i).or(right_nodes.first());
        if let Some(right_node) = right_node {
            let right_key = (right_node.start_byte() as u32, right_node.end_byte() as u32, DataNodeKind::Expr);
            if let (Some(&target_id), Some(&source_id)) = (pos_map.get(&left_key), pos_map.get(&right_key)) {
                let edge_id = DataFlowEdgeId::generate(&source_id, &target_id, DataFlowKind::Assign.as_str());
                edges.push(DataFlowEdge::new(edge_id, source_id, target_id, DataFlowKind::Assign, ts_node_range(left_node), 0.90));
            }
        }
    }
}

/// Find inner identifier inside pointer/array declarator.
fn find_identifier_child(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    if node.kind() == "identifier" { return Some(node); }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if let Some(found) = find_identifier_child(child) { return Some(found); }
        }
    }
    None
}

/// Extract a TextRange from a tree-sitter node.
fn ts_node_range(ts_node: &tree_sitter::Node) -> TextRange {
    TextRange {
        start_byte: ts_node.start_byte() as u32,
        end_byte: ts_node.end_byte() as u32,
        start_line: ts_node.start_position().row as u32,
        start_column: ts_node.start_position().column as u32,
        end_line: ts_node.end_position().row as u32,
        end_column: ts_node.end_position().column as u32,
    }
}

/// Build intra-function dataflow edges between related nodes.
///
/// Assignment edges are AST‑driven: variable_declarator (name/value) and
/// assignment_expression (left/right) provide explicit parent‑child structure.
/// This replaces the former position‑based heuristic (Nth≈Nth target grouping).
///
/// Other edge types (FieldLoad, ArgToCall, containment, ReturnValue) are
/// constrained by function scope and (where available) binding_id to avoid
/// false connections across same-named variables in different scopes.
fn build_dataflow_edges(
    nodes: &[DataNode],
    _bindings: &[BindingDef],
    ctx: &ExtractionCtx<'_>,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    // ── AST‑driven Assign edges ─────────────────────────────────────
    walk_for_assign_edges(ctx.root, ctx.source, pos_map, nodes, edges);

    // ── FieldLoad edges (function‑scoped, binding‑aware) ───────────
    // For each Field data node, find the base variable DataNode within
    // the same function and (when available) with matching binding_id.
    // Falls back to name-only within the same function when binding_id
    // is absent (languages without lexical binder).
    let field_nodes: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Field)
        .collect();

    for field_node in &field_nodes {
        if let Some(ref access_path) = field_node.access_path {
            let base_name = base_name_from_access_path(access_path);

            // Find the best-matching base node within the same function.
            // Prefer binding_id match; fall back to name-only.
            let base_node = field_node.binding_id.and_then(|bid| {
                nodes
                    .iter()
                    .find(|n| {
                        n.function_id == field_node.function_id
                            && n.binding_id == Some(bid)
                            && n.kind != DataNodeKind::Field
                            && n.range.start_byte < field_node.range.start_byte
                    })
            }).or_else(|| {
                // Fallback: same function, same name, Local/Parameter before field
                nodes
                    .iter()
                    .find(|n| {
                        n.function_id == field_node.function_id
                            && n.name.as_deref() == Some(base_name)
                            && (n.kind == DataNodeKind::Local
                                || n.kind == DataNodeKind::Parameter)
                            && n.range.start_byte < field_node.range.start_byte
                    })
            });

            if let Some(base_node) = base_node {
                let edge_id = DataFlowEdgeId::generate(
                    &base_node.id,
                    &field_node.id,
                    DataFlowKind::FieldLoad.as_str(),
                );
                let edge = DataFlowEdge::new(
                    edge_id,
                    base_node.id,
                    field_node.id,
                    DataFlowKind::FieldLoad,
                    field_node.range,
                    0.80,
                );
                edges.push(edge);
            }
        }
    }

    // ── ArgToCall edges ──────────────────────────────────────────────────
    // Group CallArgs and CallTargets by their `callsite_id` (set during
    // extraction by walking up to the enclosing `call_expression`).
    // This correctly handles nested calls like `foo(bar(a), b)` where
    // a simple "most recent preceding target" heuristic would mis-assign
    // `b` to `bar` instead of `foo`.
    //
    // Fallback: when callsite_id is None (languages/adapters that don't
    // set it), use the position-based "most recent preceding target"
    // heuristic as before.

    let call_targets: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::CallTarget)
        .collect();

    let call_args: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::CallArg)
        .collect();

    if !call_targets.is_empty() && !call_args.is_empty() {
        // Build: callsite_id → Vec<CallTarget> for group-based matching
        let mut targets_by_group: HashMap<Option<CallsiteId>, Vec<&DataNode>> = HashMap::new();
        for t in &call_targets {
            targets_by_group.entry(t.callsite_id).or_default().push(t);
        }

        for arg in &call_args {
            // Primary strategy: match by callsite_id group
            if let Some(cid) = arg.callsite_id {
                if let Some(matching_targets) = targets_by_group.get(&Some(cid)) {
                    for target in matching_targets {
                        if target.function_id == arg.function_id {
                            let edge_id = DataFlowEdgeId::generate(
                                &arg.id,
                                &target.id,
                                DataFlowKind::ArgToCall.as_str(),
                            );
                            edges.push(DataFlowEdge::new(
                                edge_id,
                                arg.id,
                                target.id,
                                DataFlowKind::ArgToCall,
                                arg.range,
                                0.75,
                            ));
                        }
                    }
                    continue; // matched by group — skip fallback
                }
            }

            // Fallback: "most recent preceding target" heuristic
            // (for languages that don't set callsite_id)
            let best_target = call_targets
                .iter()
                .filter(|t| t.function_id == arg.function_id)
                .filter(|t| t.range.start_byte < arg.range.start_byte)
                .max_by_key(|t| t.range.start_byte);

            if let Some(target) = best_target {
                let edge_id = DataFlowEdgeId::generate(
                    &arg.id,
                    &target.id,
                    DataFlowKind::ArgToCall.as_str(),
                );
                edges.push(DataFlowEdge::new(
                    edge_id,
                    arg.id,
                    target.id,
                    DataFlowKind::ArgToCall,
                    arg.range,
                    0.75,
                ));
            }
        }
    }

    // ── Sub-expression containment edges ─────────────────────────────────
    // For each Expr node (e.g. `p.x * factor`), find contained Field/Literal/
    // CallTarget nodes and create Read edges from them to the Expr.
    // This enables backward slicers to trace through sub-expressions:
    //   scaledX ← Assign ← p.x*factor(Expr) ← Read ← p.x(Field) ← FieldLoad ← p
    let expr_nodes_for_containment: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Expr || n.kind == DataNodeKind::Return)
        .collect();

    let contained_kinds = [
        DataNodeKind::Field,
        DataNodeKind::Literal,
        DataNodeKind::CallTarget,
        DataNodeKind::VariableUse,
    ];

    for expr in &expr_nodes_for_containment {
        for contained in nodes.iter().filter(|n| {
            n.id != expr.id
                && n.function_id == expr.function_id
                && n.range.start_byte >= expr.range.start_byte
                && n.range.end_byte <= expr.range.end_byte
                && contained_kinds.contains(&n.kind)
        }) {
            let edge_id =
                DataFlowEdgeId::generate(&contained.id, &expr.id, DataFlowKind::Read.as_str());
            edges.push(DataFlowEdge::new(
                edge_id,
                contained.id,
                expr.id,
                DataFlowKind::Read,
                expr.range,
                0.75,
            ));
        }
    }

    // ── ReturnValue edges ────────────────────────────────────────────────
    // Link return-value expression nodes to their enclosing Return nodes.
    // When `return compute()` is captured, the CallTarget `compute` is
    // inside the Return node's range.  Create a dataflow edge so that
    // slicers can follow the value flow from call result to return site.
    let return_nodes: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Return)
        .collect();

    for ret in &return_nodes {
        // Find nodes whose range is contained within the Return's range
        // and are in the same function.  These are the value-producing
        // nodes of the return expression (e.g. CallTarget, Expr, Literal,
        // CallArg, Field).
        for source in nodes.iter().filter(|n| {
            n.id != ret.id
                && n.function_id == ret.function_id
                && n.range.start_byte >= ret.range.start_byte
                && n.range.end_byte <= ret.range.end_byte
                && matches!(
                    n.kind,
                    DataNodeKind::CallTarget
                        | DataNodeKind::Expr
                        | DataNodeKind::Literal
                        | DataNodeKind::CallArg
                        | DataNodeKind::Field
                        | DataNodeKind::VariableUse
                )
        }) {
            let edge_id =
                DataFlowEdgeId::generate(&source.id, &ret.id, DataFlowKind::ReturnValue.as_str());
            edges.push(DataFlowEdge::new(
                edge_id,
                source.id,
                ret.id,
                DataFlowKind::ReturnValue,
                ret.range,
                0.85,
            ));
        }
    }
}

/// Standalone use-def resolution: creates edges from variable definitions
/// to later uses of the same name within each function scope.
///
/// Grouping strategy: when a DataNode has a `binding_id`, it groups by
/// `(function_id, binding_id)` — different lexical bindings (even with
/// the same name in nested scopes) produce distinct groups, preventing
/// false def-use connections across shadow boundaries.
///
/// When `binding_id` is not set, falls back to grouping by
/// `(function_id, name)` as a conservative heuristic.
///
/// Standalone use-def resolution: creates edges from variable definitions
/// to later uses of the same name within each function scope.
///
/// Grouping strategy: when a DataNode has a `binding_id`, it groups by
/// `(function_id, binding_id)` — different lexical bindings (even with
/// the same name in nested scopes) produce distinct groups, preventing
/// false def-use connections across shadow boundaries.
///
/// When `binding_id` is not set, falls back to grouping by
/// `(function_id, name)` as a conservative heuristic.
///
/// Edge creation: for backward‑slice provenance tracing, each definition
/// connects to ALL subsequent uses within its group.  This preserves the
/// full assignment chain (x = a; x = b; x = c; return x) that a backward
/// slice must traverse.  The binding_id grouping prevents cross‑scope
/// false connections for languages with a lexical binder.
fn resolve_use_def(data_nodes: &[DataNode]) -> Vec<DataFlowEdge> {
    let mut edges = Vec::new();

    // Group nodes by (function_id, binding_id?, name)
    // - binding_id takes priority (scope-aware: same-named vars in
    //   different scopes get different binding_ids)
    // - name is the fallback when binding_id is None
    let mut groups: HashMap<UseDefKey, Vec<&DataNode>> = HashMap::new();
    for node in data_nodes {
        let key = use_def_key(node);
        if let Some(k) = key {
            groups.entry(k).or_default().push(node);
        }
    }

    for (_key, mut group) in groups {
        if group.len() < 2 {
            continue;
        }

        // Sort by byte position
        group.sort_by_key(|n| n.range.start_byte);

        // Find definition nodes (first Local/Parameter in byte order)
        let def_indices: Vec<usize> = group
            .iter()
            .enumerate()
            .filter(|(_, n)| n.kind == DataNodeKind::Local || n.kind == DataNodeKind::Parameter)
            .map(|(i, _)| i)
            .collect();

        for &def_idx in &def_indices {
            let def_node = group[def_idx];
            // Create edges from def to all later uses within the same group.
            // For backward‑slice trace this gives the full provenance chain;
            // binding_id grouping prevents cross‑scope false connections.
            for use_node in group.iter().skip(def_idx + 1) {
                if use_node.id == def_node.id {
                    continue;
                }
                // Connect to nodes that are Expr, CallArg, Return, Field, or
                // Local uses (Local for multi-assignment chains like
                // x = a; x = b; return x).
                if matches!(
                    use_node.kind,
                    DataNodeKind::VariableUse
                        | DataNodeKind::Expr
                        | DataNodeKind::CallArg
                        | DataNodeKind::Field
                        | DataNodeKind::Return
                        | DataNodeKind::Local
                ) {
                    let edge_id = DataFlowEdgeId::generate(
                        &def_node.id,
                        &use_node.id,
                        DataFlowKind::Assign.as_str(),
                    );
                    edges.push(DataFlowEdge::new(
                        edge_id,
                        def_node.id,
                        use_node.id,
                        DataFlowKind::Assign,
                        use_node.range,
                        0.85,
                    ));
                }
            }
        }
    }

    edges
}

// ---------------------------------------------------------------------------
// use-def grouping
// ---------------------------------------------------------------------------

/// Key for grouping data nodes in use-def resolution.
///
/// When `binding_id` is set, nodes with the same binding (i.e., same variable
/// in the same scope) are grouped together.  When `binding_id` is None, we
/// fall back to name-based grouping (conservative heuristic).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct UseDefKey {
    function_id: Option<SymbolId>,
    /// Binding-based grouping (scope-aware, preferred).
    binding_id: Option<BindingId>,
    /// Name-based grouping (fallback).
    name: Option<String>,
}

fn use_def_key(node: &DataNode) -> Option<UseDefKey> {
    let name = node.name.clone();
    if node.binding_id.is_none() && name.is_none() {
        return None;
    }
    Some(UseDefKey {
        function_id: node.function_id,
        binding_id: node.binding_id,
        name,
    })
}

/// Resolve DataNode function_ids by matching each node to its enclosing
/// function symbol.
///
/// For each DataNode with `function_id: None`, finds the function symbol
/// whose range contains the node's start position, and sets the id.
pub(crate) fn resolve_dataflow_function_ids(nodes: &mut [DataNode], symbols: &[SymbolDef]) {
    // Build (start_byte, end_byte, symbol_id) for all function symbols
    let function_ranges: Vec<(u32, u32, SymbolId)> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .map(|s| (s.range.start_byte, s.range.end_byte, s.id))
        .collect();

    if function_ranges.is_empty() {
        return;
    }

    for node in nodes.iter_mut() {
        if node.function_id.is_some() {
            continue;
        }
        // Find the innermost function that contains this node's start position
        let pos = node.range.start_byte;
        let mut best: Option<(u32, u32, SymbolId)> = None;
        for (start, end, id) in &function_ranges {
            if pos >= *start && pos <= *end {
                match best {
                    Some((bs, be, _)) if (*end - *start) < (be - bs) => {
                        best = Some((*start, *end, *id));
                    }
                    None => best = Some((*start, *end, *id)),
                    _ => {}
                }
            }
        }
        if let Some((_, _, id)) = best {
            node.function_id = Some(id);
        }
    }
}

/// Extract the base variable name from an access path string.
///
/// Handles common field access syntax across languages:
/// - Dot: `obj.field` → `obj`
/// - Arrow: `ptr->field` → `ptr`
/// - Bracket/Index: `arr[i]`, `params[:name]`, `$_GET["name"]` → `arr` / `params` / `$_GET`
/// - Static: `Class::method` → `Class`
/// - Optional: `obj?.field` → `obj`
fn base_name_from_access_path(raw: &str) -> &str {
    // Find the first separator character
    for (i, c) in raw.char_indices() {
        match c {
            '.' | '-' | '[' | ':' | '?' => {
                if i > 0 {
                    return &raw[..i];
                }
            }
            _ => {}
        }
        // Handle "->" as two chars
        if c == '-' && raw.as_bytes().get(i + 1) == Some(&b'>') {
            if i > 0 {
                return &raw[..i];
            }
        }
    }
    raw
}

/// Recursively collect all identifier binding nodes from a destructuring pattern
/// (object_pattern, array_pattern, tuple_pattern, list_pattern, pair_pattern, rest_pattern).
pub(crate) fn collect_pattern_bindings<'a>(pattern_node: tree_sitter::Node<'a>, out: &mut Vec<tree_sitter::Node<'a>>) {
    match pattern_node.kind() {
        "identifier" => { out.push(pattern_node); }
        "shorthand_property_identifier_pattern" => { out.push(pattern_node); }
        "pair_pattern" => {
            if let Some(value_node) = pattern_node.child_by_field_name("value") {
                collect_pattern_bindings(value_node, out);
            }
        }
        // Python destructuring: a, b = pair  /  (a, b) = pair
        "pattern_list" | "tuple_pattern" | "list_pattern" => {
            for i in 0..pattern_node.child_count() {
                if let Some(child) = pattern_node.child(i) {
                    if child.kind() == "identifier" {
                        out.push(child);
                    } else {
                        collect_pattern_bindings(child, out);
                    }
                }
            }
        }
        "object_pattern" | "array_pattern" | "rest_pattern" => {
            for i in 0..pattern_node.child_count() {
                if let Some(child) = pattern_node.child(i) {
                    collect_pattern_bindings(child, out);
                }
            }
        }
        _ => {
            for i in 0..pattern_node.child_count() {
                if let Some(child) = pattern_node.child(i) {
                    collect_pattern_bindings(child, out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::ids::FileId;
    use std::path::PathBuf;

    #[cfg(feature = "typescript")]
    #[test]
    fn test_dataflow_builder_creates_nodes() {
        use crate::frontend::ParserSpec;
        use crate::languages::typescript::TypeScriptFrontendSpec;
        use tree_sitter::Parser;

        let source =
            "function handler(req: any) {\n  const name = req.body.name;\n  return name;\n}";
        let file_id = FileId::generate("test.ts");
        let spec = TypeScriptFrontendSpec;
        let ts_lang = spec.tree_sitter_language();
        let dataflow_spec: &dyn DataflowSpec = &spec;

        let mut parser = Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        let root = tree.root_node();

        let bindings: Vec<BindingDef> = vec![];
        let scopes: Vec<ScopeDef> = vec![];
        let file_path = PathBuf::from("test.ts");

        let ctx = ExtractionCtx {
            ts_lang: &ts_lang,
            root,
            source,
            file_id,
            file_path: &file_path,
            language: types::Language::TypeScript,
        };

        let result = DataFlowBuilder::extract(dataflow_spec, &ctx, &bindings, &scopes).unwrap();

        // We should have some data nodes (at minimum, the variable declarations and returns)
        assert!(!result.nodes.is_empty(), "Should have data nodes");
    }

    #[test]
    fn test_resolve_use_def_creates_cross_statement_edges() {
        use types::ids::SymbolId;
        use types::structs::TextRange;

        let file_id = FileId::generate("t.ts");
        let fid = SymbolId::generate(&file_id, "typescript", "f", "function", None);

        let def = DataNode {
            id: DataNodeId::generate(&file_id, Some(&fid), "local", Some("x"), None, 10),
            file_id,
            function_id: Some(fid),
            kind: DataNodeKind::Local,
            binding_id: None,
            callsite_id: None,
            name: Some("x".into()),
            access_path: None,
            arg_index: None,
            range: TextRange {
                start_byte: 10,
                end_byte: 11,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
        };
        let use1 = DataNode {
            id: DataNodeId::generate(&file_id, Some(&fid), "expr", Some("x"), None, 40),
            file_id,
            function_id: Some(fid),
            kind: DataNodeKind::Expr,
            binding_id: None,
            callsite_id: None,
            name: Some("x".into()),
            access_path: None,
            arg_index: None,
            range: TextRange {
                start_byte: 40,
                end_byte: 41,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
        };

        let nodes = vec![def, use1];
        let edges = resolve_use_def(&nodes);

        assert!(!edges.is_empty(), "Should create use-def edge");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, DataFlowKind::Assign);
    }

    #[test]
    fn test_resolve_use_def_respects_shadowing() {
        // Same-named "x" in different scopes (different binding_ids) should
        // NOT be connected by use-def — they are different variables.
        use types::ids::{BindingId, ScopeId, SymbolId};
        use types::structs::TextRange;

        let file_id = FileId::generate("t.ts");
        let fid = SymbolId::generate(&file_id, "typescript", "f", "function", None);
        let outer_scope = ScopeId::generate(&file_id, None, "function", 0);
        let inner_scope = ScopeId::generate(&file_id, Some(&outer_scope), "block", 50);
        let outer_binding = BindingId::generate(&file_id, &outer_scope, "local", "x", 10);
        let inner_binding = BindingId::generate(&file_id, &inner_scope, "local", "x", 60);

        // Outer x definition and use (same scope → should pair)
        let outer_def = DataNode {
            id: DataNodeId::generate(&file_id, Some(&fid), "local", Some("x"), None, 10),
            file_id,
            function_id: Some(fid),
            kind: DataNodeKind::Local,
            binding_id: Some(outer_binding),
            callsite_id: None,
            name: Some("x".into()),
            access_path: None,
            arg_index: None,
            range: TextRange {
                start_byte: 10,
                end_byte: 11,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
        };
        let outer_use = DataNode {
            id: DataNodeId::generate(&file_id, Some(&fid), "expr", Some("x"), None, 40),
            file_id,
            function_id: Some(fid),
            kind: DataNodeKind::Expr,
            binding_id: Some(outer_binding), // same binding → should connect
            callsite_id: None,
            name: Some("x".into()),
            access_path: None,
            arg_index: None,
            range: TextRange {
                start_byte: 40,
                end_byte: 41,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
        };
        // Inner x — different binding_id, should NOT connect to outer
        let inner_def = DataNode {
            id: DataNodeId::generate(&file_id, Some(&fid), "local", Some("x"), None, 60),
            file_id,
            function_id: Some(fid),
            kind: DataNodeKind::Local,
            binding_id: Some(inner_binding),
            callsite_id: None,
            name: Some("x".into()),
            access_path: None,
            arg_index: None,
            range: TextRange {
                start_byte: 60,
                end_byte: 61,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
        };
        let inner_use = DataNode {
            id: DataNodeId::generate(&file_id, Some(&fid), "expr", Some("x"), None, 80),
            file_id,
            function_id: Some(fid),
            kind: DataNodeKind::Expr,
            binding_id: Some(inner_binding), // same binding as inner_def
            callsite_id: None,
            name: Some("x".into()),
            access_path: None,
            arg_index: None,
            range: TextRange {
                start_byte: 80,
                end_byte: 81,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
        };

        let outer_def_id = outer_def.id;
        let outer_use_id = outer_use.id;
        let inner_def_id = inner_def.id;
        let inner_use_id = inner_use.id;

        let nodes = vec![outer_def, inner_def, outer_use, inner_use];
        let edges = resolve_use_def(&nodes);

        // We expect edges: outer_def→outer_use (1 edge) and inner_def→inner_use (1 edge)
        // but NOT outer_def→inner_use or inner_def→outer_use.
        assert_eq!(
            edges.len(),
            2,
            "Should have 2 edges (outer→outer, inner→inner)"
        );

        // Verify outer→outer edge exists
        let outer_edge = edges
            .iter()
            .find(|e| e.source == outer_def_id && e.target == outer_use_id);
        assert!(
            outer_edge.is_some(),
            "Should connect outer def to outer use"
        );

        // Verify inner→inner edge exists
        let inner_edge = edges
            .iter()
            .find(|e| e.source == inner_def_id && e.target == inner_use_id);
        assert!(
            inner_edge.is_some(),
            "Should connect inner def to inner use"
        );

        // Verify NO cross-edge: outer_def→inner_use or inner_def→outer_use
        let cross1 = edges
            .iter()
            .find(|e| e.source == outer_def_id && e.target == inner_use_id);
        assert!(
            cross1.is_none(),
            "Should NOT connect outer def to inner use (different scopes)"
        );
        let cross2 = edges
            .iter()
            .find(|e| e.source == inner_def_id && e.target == outer_use_id);
        assert!(
            cross2.is_none(),
            "Should NOT connect inner def to outer use (different scopes)"
        );
    }
}
