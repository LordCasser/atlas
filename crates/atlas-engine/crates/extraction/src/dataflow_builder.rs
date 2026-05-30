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

use std::collections::{HashMap, HashSet};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct NodePosKey {
    pub start_byte: u32,
    pub end_byte: u32,
    pub kind: types::enums::DataNodeKind,
}

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
    /// Run the dataflow query, normalize captures, build edges.
    ///
    /// When `capture_byte_ranges` is provided, only captures whose byte span
    /// falls inside at least one of the given `(start, end)` ranges are
    /// kept.  This is used by lazy dataflow loading to avoid building
    /// dataflow for the entire file when only a window of functions is needed.
    pub(crate) fn extract(
        dataflow_spec: &dyn DataflowSpec,
        ctx: &ExtractionCtx<'_>,
        bindings: &[BindingDef],
        scopes: &[ScopeDef],
        symbols: &[SymbolDef],
        capture_byte_ranges: Option<&[(u32, u32)]>,
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

        // Lazy dataflow: filter captures to requested byte ranges.
        let captures: Vec<_> = if let Some(ranges) = capture_byte_ranges {
            if ranges.is_empty() {
                return Ok(DataFlowResult::default());
            }
            captures
                .into_iter()
                .filter(|(_, node)| {
                    let s = node.start_byte() as u32;
                    let e = node.end_byte() as u32;
                    ranges.iter().any(|&(rs, re)| s >= rs && e <= re)
                })
                .collect()
        } else {
            captures
        };

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
                let key = NodePosKey {
                    start_byte,
                    end_byte,
                    kind: dn.kind,
                };
                node_pos_map.insert(key, dn.id);
                nodes.push(dn.clone());
            }
            if let Some(de) = de_opt {
                edges.push(de);
            }
        }

        // ── Deduplicate DataNodes ─────────────────────────────────────────
        // When multiple captures produce nodes at the same (range, kind),
        // node_pos_map tracks the last-write-wins DataNodeId.  Remove any
        // pushed node whose ID does not match the winning entry so that
        // duplicate nodes don't inflate O(N²) edge building.
        if nodes.len() > 1 {
            let keep_ids: HashSet<DataNodeId> = node_pos_map.values().copied().collect();
            let before = nodes.len();
            nodes.retain(|n| keep_ids.contains(&n.id));
            let after = nodes.len();
            if before != after {
                tracing::debug!(
                    file_id = ?ctx.file_id,
                    before,
                    after,
                    removed = before - after,
                    "DataNode dedup",
                );
            }
        }

        // Post-process: resolve bindings to nodes
        resolve_bindings_to_nodes(&mut nodes, bindings, scopes);

        // Resolve function_ids BEFORE building edges so that
        // FieldLoad, Assign, and containment edges have correct function_id
        // for scope-aware matching.
        resolve_dataflow_function_ids(&mut nodes, symbols);

        // Post-process: create dataflow edges from AST structure
        build_dataflow_edges(&nodes, bindings, ctx, &node_pos_map, &mut edges);

        // Language-specific edge building (e.g., destructuring, tuple unpacking)
        dataflow_spec.build_language_edges(
            ctx,
            &node_pos_map,
            &nodes,
            bindings,
            scopes,
            &mut edges,
        )?;

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
        // Field nodes represent property access (e.g. `obj.prop` has
        // name == "prop"). The property name is NOT a variable binding,
        // so skip binding resolution for Field nodes to avoid falsely
        // linking a FieldLoad edge to a same-named local variable.
        if node.kind == DataNodeKind::Field {
            continue;
        }

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
    source: &str,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    _all_nodes: &[DataNode],
    edges: &mut Vec<DataFlowEdge>,
    is_csharp: bool,
) {
    let kind = node.kind();

    // variable_declarator: name (Local) ← value (Expr)
    // tree-sitter-csharp ≥ 0.23 uses a flat structure: `name` field +
    // `expression` child (no wrapper field like `value` or
    // `equals_value_clause`).  Other languages may provide `value` directly.
    if kind == "variable_declarator" {
        let name_node_opt = node.child_by_field_name("name");
        let raw_value = node
            .child_by_field_name("value")
            .or_else(|| node.child_by_field_name("equals_value_clause"))
            .or_else(|| {
                // C# (ts-csharp >= 0.23): no field for the initializer —
                // fall back to the last named child that isn't the name.
                if !is_csharp {
                    return None;
                }
                (0..node.child_count())
                    .rev()
                    .filter_map(|i| node.child(i as u32))
                    .find(|c| c.is_named())
                    .filter(|c| name_node_opt.map_or(true, |n| c.id() != n.id()))
            });
        // C#: equals_value_clause (if present in older grammars) wraps `=
        // expr` — unwrap it to the actual expression so byte-range lookup
        // matches the Expr data node.
        let value_node = raw_value.and_then(|v| {
            if v.kind() == "equals_value_clause" {
                v.named_child(0).or(Some(v))
            } else {
                Some(v)
            }
        });
        if let (Some(name_node), Some(value_node)) = (name_node_opt, value_node) {
            let name_kind = name_node.kind();
            if name_kind == "object_pattern"
                || name_kind == "array_pattern"
                || name_kind == "pattern_list"
                || name_kind == "tuple_pattern"
                || name_kind == "list_pattern"
            {
                // Destructuring: each binding gets edge from the initializer Expr
                // Walk the pattern to find all identifier bindings and create
                // Assign edges: initializer_Expr → each_destructured_Local
                let value_key = NodePosKey {
                    start_byte: value_node.start_byte() as u32,
                    end_byte: value_node.end_byte() as u32,
                    kind: DataNodeKind::Expr,
                };
                if let Some(&source_id) = pos_map.get(&value_key) {
                    let mut bindings: Vec<tree_sitter::Node> = Vec::new();
                    collect_pattern_bindings(name_node, &mut bindings);
                    for binding_node in &bindings {
                        let bind_key = NodePosKey {
                            start_byte: binding_node.start_byte() as u32,
                            end_byte: binding_node.end_byte() as u32,
                            kind: DataNodeKind::Local,
                        };
                        if let Some(&target_id) = pos_map.get(&bind_key) {
                            let edge_id = DataFlowEdgeId::generate(
                                &source_id,
                                &target_id,
                                DataFlowKind::Assign.as_str(),
                            );
                            edges.push(DataFlowEdge::new(
                                edge_id,
                                source_id,
                                target_id,
                                DataFlowKind::Assign,
                                ts_node_range(binding_node),
                                0.85,
                            ));
                        }
                    }
                }
            } else {
                // Simple variable declarator: name (Local) ← value (Expr)
                let name_key = NodePosKey {
                    start_byte: name_node.start_byte() as u32,
                    end_byte: name_node.end_byte() as u32,
                    kind: DataNodeKind::Local,
                };
                let value_key = NodePosKey {
                    start_byte: value_node.start_byte() as u32,
                    end_byte: value_node.end_byte() as u32,
                    kind: DataNodeKind::Expr,
                };
                if let (Some(&target_id), Some(&source_id)) =
                    (pos_map.get(&name_key), pos_map.get(&value_key))
                {
                    let edge_id = DataFlowEdgeId::generate(
                        &source_id,
                        &target_id,
                        DataFlowKind::Assign.as_str(),
                    );
                    edges.push(DataFlowEdge::new(
                        edge_id,
                        source_id,
                        target_id,
                        DataFlowKind::Assign,
                        ts_node_range(&name_node),
                        0.95,
                    ));
                }
            }
        }
    }

    // ── Kotlin: property_declaration (val/var x = expr) ──────────────
    // In tree-sitter-kotlin v0.3.5+, `property_declaration` wraps
    // `variable_declaration` (name+type) and optional `= expr`.
    // The simple_identifier is nested inside variable_declaration;
    // the expression is a direct unnamed child of property_declaration.
    if kind == "property_declaration" {
        let name_node = node.child_by_field_name("declarator").or_else(|| {
            // Look for simple_identifier inside variable_declaration
            (0..node.child_count())
                .filter_map(|i| node.child(i as u32))
                .find(|c| c.kind() == "variable_declaration")
                .and_then(|vd| {
                    (0..vd.child_count())
                        .filter_map(|i| vd.child(i as u32))
                        .find(|c| c.kind() == "simple_identifier")
                })
        });
        let expr_node = node.child_by_field_name("expr").or_else(|| {
            // Fallback: find expression after '='
            let mut seen_eq = false;
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if seen_eq && child.is_named() {
                        return Some(child);
                    }
                    if child.kind() == "=" {
                        seen_eq = true;
                    }
                }
            }
            None
        });
        if let (Some(name_node), Some(expr_node)) = (name_node, expr_node) {
            let name_key = NodePosKey {
                start_byte: name_node.start_byte() as u32,
                end_byte: name_node.end_byte() as u32,
                kind: DataNodeKind::Local,
            };
            let value_key = NodePosKey {
                start_byte: expr_node.start_byte() as u32,
                end_byte: expr_node.end_byte() as u32,
                kind: DataNodeKind::Expr,
            };
            if let (Some(&target_id), Some(&source_id)) =
                (pos_map.get(&name_key), pos_map.get(&value_key))
            {
                let edge_id =
                    DataFlowEdgeId::generate(&source_id, &target_id, DataFlowKind::Assign.as_str());
                edges.push(DataFlowEdge::new(
                    edge_id,
                    source_id,
                    target_id,
                    DataFlowKind::Assign,
                    ts_node_range(&name_node),
                    0.95,
                ));
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
            // Try simple (non‑destructuring) target first
            if let Some(&id) = pos_map.get(&NodePosKey {
                start_byte: left_start,
                end_byte: left_end,
                kind: DataNodeKind::Field,
            }) {
                let right_key = NodePosKey {
                    start_byte: right_node.start_byte() as u32,
                    end_byte: right_node.end_byte() as u32,
                    kind: DataNodeKind::Expr,
                };
                if let Some(&source_id) = pos_map.get(&right_key) {
                    let eid = DataFlowEdgeId::generate(
                        &source_id,
                        &id,
                        DataFlowKind::FieldStore.as_str(),
                    );
                    edges.push(DataFlowEdge::new(
                        eid,
                        source_id,
                        id,
                        DataFlowKind::FieldStore,
                        ts_node_range(&left_node),
                        0.90,
                    ));
                }
            } else if let Some(&id) = pos_map.get(&NodePosKey {
                start_byte: left_start,
                end_byte: left_end,
                kind: DataNodeKind::Local,
            }) {
                let right_key = NodePosKey {
                    start_byte: right_node.start_byte() as u32,
                    end_byte: right_node.end_byte() as u32,
                    kind: DataNodeKind::Expr,
                };
                if let Some(&source_id) = pos_map.get(&right_key) {
                    let eid =
                        DataFlowEdgeId::generate(&source_id, &id, DataFlowKind::Assign.as_str());
                    edges.push(DataFlowEdge::new(
                        eid,
                        source_id,
                        id,
                        DataFlowKind::Assign,
                        ts_node_range(&left_node),
                        0.90,
                    ));
                }
            }
            // Destructuring assignment: a, b = expr
            // The left node is pattern_list / tuple_pattern / list_pattern.
            // Create Assign edges from the RHS expression to each identifier target.
            else if matches!(
                left_node.kind(),
                "pattern_list" | "tuple_pattern" | "list_pattern"
            ) {
                let right_key = NodePosKey {
                    start_byte: right_node.start_byte() as u32,
                    end_byte: right_node.end_byte() as u32,
                    kind: DataNodeKind::Expr,
                };
                if let Some(&source_id) = pos_map.get(&right_key) {
                    for i in 0..left_node.child_count() {
                        if let Some(child) = left_node.child(i as u32) {
                            if child.is_named() && child.kind() == "identifier" {
                                let child_key = NodePosKey {
                                    start_byte: child.start_byte() as u32,
                                    end_byte: child.end_byte() as u32,
                                    kind: DataNodeKind::Local,
                                };
                                if let Some(&target_id) = pos_map.get(&child_key) {
                                    let eid = DataFlowEdgeId::generate(
                                        &source_id,
                                        &target_id,
                                        DataFlowKind::Assign.as_str(),
                                    );
                                    edges.push(DataFlowEdge::new(
                                        eid,
                                        source_id,
                                        target_id,
                                        DataFlowKind::Assign,
                                        ts_node_range(&child),
                                        0.90,
                                    ));
                                }
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
                let name_key = NodePosKey {
                    start_byte: id_node.start_byte() as u32,
                    end_byte: id_node.end_byte() as u32,
                    kind: DataNodeKind::Local,
                };
                let value_key = NodePosKey {
                    start_byte: value_node.start_byte() as u32,
                    end_byte: value_node.end_byte() as u32,
                    kind: DataNodeKind::Expr,
                };
                if let (Some(&target_id), Some(&source_id)) =
                    (pos_map.get(&name_key), pos_map.get(&value_key))
                {
                    let edge_id = DataFlowEdgeId::generate(
                        &source_id,
                        &target_id,
                        DataFlowKind::Assign.as_str(),
                    );
                    edges.push(DataFlowEdge::new(
                        edge_id,
                        source_id,
                        target_id,
                        DataFlowKind::Assign,
                        ts_node_range(&id_node),
                        0.90,
                    ));
                }
            }
        }
    }

    // Recurse into children
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            walk_for_assign_edges(child, source, pos_map, _all_nodes, edges, is_csharp);
        }
    }
}

/// Create Assign edges from Go expression_list pairs.
#[allow(dead_code)] // used by Go language adapter (feature-gated)
pub(crate) fn create_assign_edges_from_expression_lists(
    left_list: tree_sitter::Node,
    right_list: tree_sitter::Node,
    pos_map: &HashMap<NodePosKey, DataNodeId>,
    edges: &mut Vec<DataFlowEdge>,
) {
    let left_nodes: Vec<tree_sitter::Node> = (0..left_list.child_count())
        .filter_map(|i| left_list.child(i as u32))
        .filter(|c| c.is_named())
        .collect();
    let right_nodes: Vec<tree_sitter::Node> = (0..right_list.child_count())
        .filter_map(|i| right_list.child(i as u32))
        .filter(|c| c.is_named())
        .collect();
    for (i, left_node) in left_nodes.iter().enumerate() {
        let (target_kind, edge_kind) = if left_node.kind().contains("identifier") {
            (DataNodeKind::Local, DataFlowKind::Assign)
        } else {
            (DataNodeKind::Field, DataFlowKind::FieldStore)
        };
        let left_key = NodePosKey {
            start_byte: left_node.start_byte() as u32,
            end_byte: left_node.end_byte() as u32,
            kind: target_kind,
        };
        let right_node = right_nodes.get(i).or(right_nodes.first());
        if let Some(right_node) = right_node {
            let right_key = NodePosKey {
                start_byte: right_node.start_byte() as u32,
                end_byte: right_node.end_byte() as u32,
                kind: DataNodeKind::Expr,
            };
            if let (Some(&target_id), Some(&source_id)) =
                (pos_map.get(&left_key), pos_map.get(&right_key))
            {
                let edge_id = DataFlowEdgeId::generate(&source_id, &target_id, edge_kind.as_str());
                edges.push(DataFlowEdge::new(
                    edge_id,
                    source_id,
                    target_id,
                    edge_kind,
                    ts_node_range(left_node),
                    0.90,
                ));
            }
        }
    }
}

/// Find inner identifier inside pointer/array declarator.
fn find_identifier_child(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if let Some(found) = find_identifier_child(child) {
                return Some(found);
            }
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
    let is_csharp = matches!(ctx.language, types::enums::Language::CSharp);
    walk_for_assign_edges(ctx.root, ctx.source, pos_map, nodes, edges, is_csharp);

    // ── FieldLoad edges (function‑scoped, access‑path‑aware) ───────
    // For each Field data node, find the base data node within the same
    // function.  Strategy (in order):
    //
    //   1.  Match a parent Field node whose access_path is the parent
    //       path of the current field (e.g.  req.body.name → req.body).
    //       This handles chained access like a.b.c.d correctly.
    //   2.  Fall back to name-based matching for Local / Parameter /
    //       Receiver nodes whose name equals base_name_from_access_path.
    //
    //  Note: binding_id is intentionally NOT used for Field nodes
    //  because Field nodes carry the property name (not the base
    //  variable name) and could falsely match a same‑named local.
    let field_nodes: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Field)
        .collect();

    for field_node in &field_nodes {
        if let Some(ref access_path) = field_node.access_path {
            // 1. Try parent access-path match against another Field node
            let base_node = parent_access_path(access_path)
                .and_then(|parent_path| {
                    nodes.iter().find(|n| {
                        n.function_id == field_node.function_id
                            && n.access_path.as_deref() == Some(parent_path)
                            && n.range.start_byte < field_node.range.start_byte
                    })
                })
                .or_else(|| {
                    // 2. Fallback: name-based match for root access
                    let base_name = base_name_from_access_path(access_path);
                    nodes.iter().find(|n| {
                        n.function_id == field_node.function_id
                            && n.name.as_deref() == Some(base_name)
                            && (n.kind == DataNodeKind::Local
                                || n.kind == DataNodeKind::Parameter
                                || n.kind == DataNodeKind::Receiver
                                || n.kind == DataNodeKind::Global)
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
                let edge_id =
                    DataFlowEdgeId::generate(&arg.id, &target.id, DataFlowKind::ArgToCall.as_str());
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
/// **Field nodes are excluded** from use-def resolution: field dataflow
/// is expressed through access_path / FieldLoad edges rather than
/// name-based grouping.  This prevents false edges when a property name
/// (e.g. "name" in `req.body.name`) accidentally matches a same‑named
/// local variable or parameter.
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
                // Connect to nodes that are Expr, CallArg, Return, or
                // Local uses (Local for multi-assignment chains like
                // x = a; x = b; return x).
                // Field nodes intentionally excluded — field dataflow uses
                // access_path / FieldLoad edges, not name-based use-def.
                if matches!(
                    use_node.kind,
                    DataNodeKind::VariableUse
                        | DataNodeKind::Expr
                        | DataNodeKind::CallArg
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
    // Field nodes express dataflow through access_path / FieldLoad edges.
    // Excluding them from name-based use-def prevents false edges where
    // a property name (e.g. "name" in req.body.name) matches a same-named
    // local variable or parameter.
    if node.kind == DataNodeKind::Field {
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
/// Recognised separators (multi‑char tokens checked before single‑char):
/// - Dot:           `obj.field`       → `obj`
/// - Arrow:         `ptr->field`      → `ptr`
/// - Optional chain:`obj?.field`      → `obj`
/// - Static/method: `Class::method`   → `Class`
/// - Bracket/index: `arr[i]`, `hash[:k]`, `$_GET["k"]` → `arr` / `hash` / `$_GET`
///
/// Single `-`, `:` and `?` are NOT treated as separators to avoid false
/// splits on names that contain them or on unrelated operators.
fn base_name_from_access_path(raw: &str) -> &str {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // Dot: obj.field
            b'.' => {
                if i > 0 {
                    return &raw[..i];
                }
                i += 1;
            }
            // Arrow: ptr->field
            b'-' if bytes.get(i + 1) == Some(&b'>') => {
                if i > 0 {
                    return &raw[..i];
                }
                i += 2;
            }
            // Optional chaining: obj?.field
            b'?' if bytes.get(i + 1) == Some(&b'.') => {
                if i > 0 {
                    return &raw[..i];
                }
                i += 1; // skip '?'; '.' will be handled next iteration
            }
            // Static/method resolution: Class::method
            b':' if bytes.get(i + 1) == Some(&b':') => {
                if i > 0 {
                    return &raw[..i];
                }
                i += 2;
            }
            // Bracket/index: arr[i]
            b'[' => {
                if i > 0 {
                    return &raw[..i];
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    raw
}

/// Given an access path like `req.body.name`, return the parent access path
/// (`req.body`) by stripping the last component. Returns `None` when the path
/// has no separator (it is already a root name).
///
/// Supports `.`, `->`, `[`, `?.`, and `::` separators.
fn parent_access_path(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        match bytes[i] {
            // Dot — but check for optional chain ?. first
            b'.' => {
                if i > 0 && bytes[i - 1] == b'?' {
                    return Some(&raw[..i - 1]); // cut before '?'
                }
                return Some(&raw[..i]);
            }
            // Bracket/index
            b'[' => return Some(&raw[..i]),
            // Arrow: ->
            b'>' if i > 0 && bytes[i - 1] == b'-' => return Some(&raw[..i - 1]),
            // Static/method resolution: ::
            b':' if i > 0 && bytes[i - 1] == b':' => return Some(&raw[..i - 1]),
            _ => {}
        }
    }
    None
}

/// Recursively collect all identifier binding nodes from a destructuring pattern
/// (object_pattern, array_pattern, tuple_pattern, list_pattern, pair_pattern, rest_pattern).
pub(crate) fn collect_pattern_bindings<'a>(
    pattern_node: tree_sitter::Node<'a>,
    out: &mut Vec<tree_sitter::Node<'a>>,
) {
    match pattern_node.kind() {
        "identifier" => {
            out.push(pattern_node);
        }
        "shorthand_property_identifier_pattern" => {
            out.push(pattern_node);
        }
        "pair_pattern" => {
            if let Some(value_node) = pattern_node.child_by_field_name("value") {
                collect_pattern_bindings(value_node, out);
            }
        }
        // Python destructuring: a, b = pair  /  (a, b) = pair
        "pattern_list" | "tuple_pattern" | "list_pattern" => {
            for i in 0..pattern_node.child_count() {
                if let Some(child) = pattern_node.child(i as u32) {
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
                if let Some(child) = pattern_node.child(i as u32) {
                    collect_pattern_bindings(child, out);
                }
            }
        }
        _ => {
            for i in 0..pattern_node.child_count() {
                if let Some(child) = pattern_node.child(i as u32) {
                    collect_pattern_bindings(child, out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use types::ids::FileId;

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
        let symbols: Vec<SymbolDef> = vec![];
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

        let result =
            DataFlowBuilder::extract(dataflow_spec, &ctx, &bindings, &scopes, &symbols, None)
                .unwrap();

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

    // ── access path helper tests ──────────────────────────────────────

    #[test]
    fn test_base_name_from_access_path() {
        // Dot notation
        assert_eq!(base_name_from_access_path("req.body.name"), "req");
        assert_eq!(base_name_from_access_path("obj.field"), "obj");
        assert_eq!(base_name_from_access_path("x"), "x"); // no separator

        // Arrow
        assert_eq!(base_name_from_access_path("ptr->field"), "ptr");
        assert_eq!(base_name_from_access_path("ptr->field->nested"), "ptr");

        // Optional chaining
        assert_eq!(base_name_from_access_path("obj?.field"), "obj");
        assert_eq!(base_name_from_access_path("obj?.field?.nested"), "obj");

        // Static/method resolution
        assert_eq!(base_name_from_access_path("Class::method"), "Class");
        assert_eq!(base_name_from_access_path("NS::Class::method"), "NS");

        // Bracket/index
        assert_eq!(base_name_from_access_path("arr[0]"), "arr");
        assert_eq!(base_name_from_access_path("arr[0].field"), "arr");
        assert_eq!(base_name_from_access_path("$_GET[\"name\"]"), "$_GET");
        assert_eq!(base_name_from_access_path("hash[:key]"), "hash");

        // Single ? : - should NOT be treated as separators on their own
        // (the function requires a following . for ? and following : / > for : / -)
        assert_eq!(base_name_from_access_path("no-sep-name"), "no-sep-name");
        assert_eq!(base_name_from_access_path("no:sep:name"), "no:sep:name");
        assert_eq!(base_name_from_access_path("no?sep"), "no?sep");
    }

    #[test]
    fn test_parent_access_path() {
        // Dot chain
        assert_eq!(parent_access_path("req.body.name"), Some("req.body"));
        assert_eq!(parent_access_path("req.body"), Some("req"));
        assert_eq!(parent_access_path("req"), None);

        // Arrow chain
        assert_eq!(parent_access_path("ptr->field->nested"), Some("ptr->field"));
        assert_eq!(parent_access_path("ptr->field"), Some("ptr"));

        // Optional chaining
        assert_eq!(parent_access_path("obj?.field?.nested"), Some("obj?.field"));
        assert_eq!(parent_access_path("obj?.field"), Some("obj"));

        // Static resolution
        assert_eq!(parent_access_path("NS::Class::method"), Some("NS::Class"));
        assert_eq!(parent_access_path("Class::method"), Some("Class"));

        // Mixed separators
        assert_eq!(parent_access_path("obj.field->ptr"), Some("obj.field"));
        assert_eq!(parent_access_path("arr[0].field"), Some("arr[0]"));
        assert_eq!(parent_access_path("arr[0]->field"), Some("arr[0]"));
    }
}
