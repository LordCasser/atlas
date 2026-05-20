//! DataFlow builder — per-function dataflow graph construction.
//!
//! The DataFlowBuilder creates [`DataNode`]s and [`DataFlowEdge`]s from tree-sitter
//! AST captures.  This forms the basis for taint analysis.
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
//! # Invariants
//!
//! - Source/Target of [`DataFlowEdge`] are **always** [`DataNodeId`], never
//!   [`SymbolId`].  This is the fundamental invariant that separates the dataflow
//!   graph from the symbol-level graph.
//! - DataNode IDs are deterministic (blake3).
//! - DataFlowEdge IDs are deterministic (blake3(source + target + kind)).
//! - Each DataNode has exactly one function_id (or None for top-level).
//! - Per-function dataflow only (interprocedural deferred to taint analysis layer).

use std::collections::HashMap;
use std::path::Path;

use crate::types::bindings::BindingDef;
use crate::types::dataflow::{DataFlowEdge, DataNode};
use crate::types::enums::{DataFlowKind, DataNodeKind};
use crate::types::ids::{BindingId, DataFlowEdgeId, DataNodeId, FileId, SymbolId};
use crate::types::ScopeDef;

use super::languages::LanguageAdapter;

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
    pub fn extract(
        adapter: &dyn LanguageAdapter,
        ts_lang: &tree_sitter::Language,
        root: tree_sitter::Node,
        source: &str,
        source_bytes: &[u8],
        file_id: FileId,
        file_path: &Path,
        bindings: &[BindingDef],
        _scopes: &[ScopeDef],
    ) -> anyhow::Result<DataFlowResult> {
        let query_src = adapter.dataflow_builder_query();
        if query_src.trim().is_empty() {
            return Ok(DataFlowResult::default());
        }

        // Collect captures
        let captures = super::extract::collect_captures(ts_lang, query_src, root, source_bytes)?;

        let mut nodes: Vec<DataNode> = Vec::new();
        let mut edges: Vec<DataFlowEdge> = Vec::new();

        // Create data nodes from captures
        for (name, node) in captures {
            let (dn_opt, de_opt) =
                adapter.normalize_dataflow_builder(&name, node, source, file_id, file_path);
            if let Some(dn) = dn_opt {
                nodes.push(dn);
            }
            if let Some(de) = de_opt {
                edges.push(de);
            }
        }

        // Post-process: resolve bindings to nodes
        resolve_bindings_to_nodes(&mut nodes, bindings);

        // Post-process: create dataflow edges from assignments
        build_dataflow_edges(&nodes, &bindings, file_id, &mut edges);

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
    /// treated as uses.  This enables basic cross-statement taint
    /// propagation (e.g. `const x = source; sink(x)`).
    ///
    /// This is a conservative heuristic — it may connect unrelated
    /// occurrences in nested scopes.  Shadowing and SSA-style precision
    /// require BindingGraph (P3-deferred).
    pub fn resolve_use_def(data_nodes: &[DataNode]) -> Vec<DataFlowEdge> {
        resolve_use_def(data_nodes)
    }
}

/// For each data node that has a binding_id placeholder, try to match it
/// to an actual binding definition by name.
fn resolve_bindings_to_nodes(nodes: &mut [DataNode], bindings: &[BindingDef]) {
    // Build a lookup: name → binding_id (within a scope)
    let binding_by_name: HashMap<&str, &BindingId> = bindings
        .iter()
        .map(|b| (b.name.as_str(), &b.id))
        .collect();

    for node in nodes.iter_mut() {
        if let Some(ref name) = node.name {
            if let Some(binding_id) = binding_by_name.get(name.as_str()) {
                node.binding_id = Some(**binding_id);
            }
        }
    }
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
        .filter_map(|n| {
            n.name.as_ref().map(|name| ((name.as_str(), n.kind), &n.id))
        })
        .collect();

    // For each pair of related captures (target+value), create an Assign edge.
    let assign_targets: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Local || n.kind == DataNodeKind::Parameter)
        .collect();

    let expr_nodes: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Expr)
        .collect();

    // Simple heuristic: pair each assign target with the next expr node
    let pair_count = assign_targets.len().min(expr_nodes.len());
    for i in 0..pair_count {
        let target = assign_targets[i];
        let value = expr_nodes[i];
        let edge_id = DataFlowEdgeId::generate(
            &value.id,
            &target.id,
            DataFlowKind::Assign.as_str(),
        );
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
            if let Some(base_id) = node_by_name.get(&(base_name, DataNodeKind::Local))
                .or_else(|| node_by_name.get(&(base_name, DataNodeKind::Parameter)))
            {
                let edge_id = DataFlowEdgeId::generate(
                    *base_id,
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

    // Create ArgToParam edges: call_arg → call_target
    // Associates each call argument with its call target (callee).
    // Logic: within each function, associate each CallArg with the most recent
    // preceding CallTarget in byte position.
    let call_targets: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::CallTarget)
        .collect();

    let call_args: Vec<&DataNode> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::CallArg)
        .collect();

    if !call_targets.is_empty() && !call_args.is_empty() {
        for arg in &call_args {
            let best_target = call_targets.iter()
                .filter(|t| t.function_id == arg.function_id)
                .filter(|t| t.range.start_byte < arg.range.start_byte)
                .max_by_key(|t| t.range.start_byte);

            if let Some(target) = best_target {
                let edge_id = DataFlowEdgeId::generate(
                    &arg.id,
                    &target.id,
                    DataFlowKind::ArgToParam.as_str(),
                );
                let edge = DataFlowEdge::new(
                    edge_id,
                    arg.id,
                    target.id,
                    DataFlowKind::ArgToParam,
                    arg.range,
                    0.75,
                );
                edges.push(edge);
            }
        }
    }
}

/// Standalone use-def resolution: creates edges from variable definitions
/// to later uses of the same name within each function scope.
fn resolve_use_def(data_nodes: &[DataNode]) -> Vec<DataFlowEdge> {
    let mut edges = Vec::new();

    // Group nodes by (function_id, lowercase name)
    let mut groups: HashMap<(Option<SymbolId>, String), Vec<&DataNode>> = HashMap::new();
    for node in data_nodes {
        if let Some(ref name) = node.name {
            let key = (node.function_id, name.to_lowercase());
            groups.entry(key).or_default().push(node);
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
            .filter(|(_, n)| {
                n.kind == DataNodeKind::Local || n.kind == DataNodeKind::Parameter
            })
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
                if matches!(use_node.kind,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(feature = "typescript")]
    #[test]
    fn test_dataflow_builder_creates_nodes() {
        use crate::extraction::languages::typescript::TypeScriptAdapter;
        use tree_sitter::Parser;

        let source = "function handler(req: any) {\n  const name = req.body.name;\n  return name;\n}";
        let file_id = FileId::generate("test.ts");
        let adapter = TypeScriptAdapter;
        let ts_lang = adapter.tree_sitter_language();

        let mut parser = Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source.as_bytes(), None).unwrap();
        let root = tree.root_node();

        let bindings: Vec<BindingDef> = vec![];
        let scopes: Vec<ScopeDef> = vec![];

        let result = DataFlowBuilder::extract(
            &adapter, &ts_lang, root, source, source.as_bytes(),
            file_id, &PathBuf::from("test.ts"), &bindings, &scopes,
        ).unwrap();

        // We should have some data nodes (at minimum, the variable declarations and returns)
        assert!(!result.nodes.is_empty(), "Should have data nodes");
    }

    #[test]
    fn test_resolve_use_def_creates_cross_statement_edges() {
        use crate::types::ids::SymbolId;
        use crate::types::structs::TextRange;

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
            range: TextRange { start_byte: 10, end_byte: 11, start_line: 0, start_column: 0, end_line: 0, end_column: 0 },
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
            range: TextRange { start_byte: 40, end_byte: 41, start_line: 0, start_column: 0, end_line: 0, end_column: 0 },
        };

        let nodes = vec![def, use1];
        let edges = resolve_use_def(&nodes);

        assert!(!edges.is_empty(), "Should create use-def edge");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, DataFlowKind::Assign);
    }
}
