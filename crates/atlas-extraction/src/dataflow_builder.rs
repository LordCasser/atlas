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
//!    FieldLoad, FieldStore, ArgToParam, ReturnToCall).
//!
//! # Edge-building rules
//!
//! - **Assign**: position-based value → target (Expr nodes between consecutive
//!   Local/Parameter targets).
//! - **FieldLoad**: name-based base → field (looks up the base of an access path
//!   among known locals/params).
//! - **ArgToParam**: callsite-grouped call_arg → call_target.  CallArg and
//!   CallTarget nodes from the same `call_expression` share a `callsite_id`
//!   (set during extraction by walking the AST).  Falls back to "most recent
//!   preceding target" heuristic when `callsite_id` is not set.
//! - **ReturnToCall**: contained-node → Return.  Nodes whose range falls fully
//!   inside a Return node's range (e.g. `CallTarget` in `return foo()`) are
//!   linked to the Return.
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

use atlas_types::CallsiteId;
use atlas_types::ScopeDef;
use atlas_types::bindings::BindingDef;
use atlas_types::dataflow::{DataFlowEdge, DataNode};
use atlas_types::enums::{DataFlowKind, DataNodeKind, SymbolKind};
use atlas_types::ids::{BindingId, DataFlowEdgeId, DataNodeId, FileId, ScopeId, SymbolId};
use atlas_types::structs::{SymbolDef, TextRange};

use super::frontend::{Capture, DataflowSpec};
use crate::extraction_ctx::ExtractionCtx;

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

        // Create data nodes from captures
        let nctx = ctx.normalize_ctx();
        for (name, node) in captures {
            let capture = Capture { name, node };
            let (dn_opt, de_opt) = dataflow_spec.normalize(nctx, capture);
            if let Some(dn) = dn_opt {
                nodes.push(dn);
            }
            if let Some(de) = de_opt {
                edges.push(de);
            }
        }

        // Post-process: resolve bindings to nodes
        resolve_bindings_to_nodes(&mut nodes, bindings, scopes);

        // Post-process: create dataflow edges from assignments
        build_dataflow_edges(&nodes, bindings, ctx.file_id, &mut edges);

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

    // Flat fallback: name → binding_id (used when no scope contains the node)
    let flat_map: HashMap<&str, &BindingId> =
        bindings.iter().map(|b| (b.name.as_str(), &b.id)).collect();

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
                found.or_else(|| flat_map.get(name.as_str()).copied())
            }
            None => flat_map.get(name.as_str()).copied(),
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

/// Build intra-function dataflow edges between related nodes.
///
/// Currently creates:
/// - FieldLoad: receiver → field for member access chains (when receiver node exists)
/// - Assign: value → target for assignment/variable declaration captures
fn build_dataflow_edges(
    nodes: &[DataNode],
    _bindings: &[BindingDef],
    _file_id: FileId,
    edges: &mut Vec<DataFlowEdge>,
) {
    // Build a lookup: name+kind → DataNodeId for quick matching
    let node_by_name: HashMap<(&str, DataNodeKind), &DataNodeId> = nodes
        .iter()
        .filter_map(|n| n.name.as_ref().map(|name| ((name.as_str(), n.kind), &n.id)))
        .collect();

    // Range-based assignment matching: each assign target groups with
    // value nodes that sit between it and the next assignment target (or
    // the end of the function).  This replaces Nth≈Nth heuristic with
    // position-based grouping.
    //
    // In typical code like `let result = a + b`, "result" is the target
    // and "a", "b" are the values (which come AFTER the target in source
    // order).
    let mut assign_targets: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Local || n.kind == DataNodeKind::Parameter)
        .collect();
    assign_targets.sort_by_key(|n| n.range.start_byte);

    let mut expr_nodes: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Expr)
        .collect();
    expr_nodes.sort_by_key(|n| n.range.start_byte);

    for (idx, target) in assign_targets.iter().enumerate() {
        // Values for this target: those between this target and the next
        // target (or end of function if this is the last target).
        let next_target_start = assign_targets
            .get(idx + 1)
            .map(|t| t.range.start_byte)
            .unwrap_or(u32::MAX);

        for value in &expr_nodes {
            if value.range.start_byte > target.range.start_byte
                && value.range.start_byte < next_target_start
            {
                let edge_id =
                    DataFlowEdgeId::generate(&value.id, &target.id, DataFlowKind::Assign.as_str());
                let edge = DataFlowEdge::new(
                    edge_id,
                    value.id,
                    target.id,
                    DataFlowKind::Assign,
                    target.range,
                    0.9,
                );
                edges.push(edge);
            }
        }
    }

    // Create FieldLoad edges for member access chains
    let field_nodes: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Field)
        .collect();

    for field_node in &field_nodes {
        if let Some(ref access_path) = field_node.access_path {
            // If the base name (first part of access_path) matches a known local/parameter,
            // create a FieldLoad edge from base → field.
            let base_name = access_path.split('.').next().unwrap_or(access_path);
            if let Some(base_id) = node_by_name
                .get(&(base_name, DataNodeKind::Local))
                .or_else(|| node_by_name.get(&(base_name, DataNodeKind::Parameter)))
            {
                let edge_id = DataFlowEdgeId::generate(
                    base_id,
                    &field_node.id,
                    DataFlowKind::FieldLoad.as_str(),
                );
                let edge = DataFlowEdge::new(
                    edge_id,
                    **base_id,
                    field_node.id,
                    DataFlowKind::FieldLoad,
                    field_node.range,
                    0.8,
                );
                edges.push(edge);
            }
        }
    }

    // ── ArgToParam edges ────────────────────────────────────────────────
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
                                DataFlowKind::ArgToParam.as_str(),
                            );
                            edges.push(DataFlowEdge::new(
                                edge_id,
                                arg.id,
                                target.id,
                                DataFlowKind::ArgToParam,
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
                    DataFlowKind::ArgToParam.as_str(),
                );
                edges.push(DataFlowEdge::new(
                    edge_id,
                    arg.id,
                    target.id,
                    DataFlowKind::ArgToParam,
                    arg.range,
                    0.75,
                ));
            }
        }
    }

    // ── ReturnToCall edges ──────────────────────────────────────────────
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
                )
        }) {
            let edge_id =
                DataFlowEdgeId::generate(&source.id, &ret.id, DataFlowKind::ReturnToCall.as_str());
            edges.push(DataFlowEdge::new(
                edge_id,
                source.id,
                ret.id,
                DataFlowKind::ReturnToCall,
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
            // Create edges from def to all later uses of different kinds
            for use_node in group.iter().skip(def_idx + 1) {
                if use_node.id == def_node.id {
                    continue;
                }
                // Only connect to nodes that are Expr, CallArg, Return, or Field uses
                if matches!(
                    use_node.kind,
                    DataNodeKind::Expr
                        | DataNodeKind::CallArg
                        | DataNodeKind::Field
                        | DataNodeKind::Return
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

#[cfg(test)]
mod tests {
    use super::*;
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
            language: atlas_types::Language::TypeScript,
        };

        let result = DataFlowBuilder::extract(dataflow_spec, &ctx, &bindings, &scopes).unwrap();

        // We should have some data nodes (at minimum, the variable declarations and returns)
        assert!(!result.nodes.is_empty(), "Should have data nodes");
    }

    #[test]
    fn test_resolve_use_def_creates_cross_statement_edges() {
        use atlas_types::ids::SymbolId;
        use atlas_types::structs::TextRange;

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
        use atlas_types::ids::{BindingId, ScopeId, SymbolId};
        use atlas_types::structs::TextRange;

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
