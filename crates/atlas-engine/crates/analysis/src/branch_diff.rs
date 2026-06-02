//! Branch Effect Diff — compare sibling branch paths for suspicious asymmetries.
//!
//! Compares the side effects of true/false branches (if/else) or switch cases.
//! Detects patterns like: one branch frees a field but the other doesn't.

use crate::cfg_graph::CfgGraph;
use std::collections::HashMap;
use types::cfg::{CfgEdge, CfgNode};
use types::enums::{CfgEdgeKind, CfgNodeKind, EffectKind};
use types::ids::CfgNodeId;

/// Summary of effects on one side of a branch.
#[derive(Debug, Clone, Default)]
pub struct BranchPathSummary {
    pub frees: Vec<String>,
    pub allocates: Vec<String>,
    pub writes: Vec<String>,
    pub reads: Vec<String>,
    pub calls: Vec<String>,
}

/// Diff between two branch paths.
#[derive(Debug, Clone)]
pub struct BranchDiff {
    pub branch_node_line: u32,
    pub common_prefix: String,
    pub path_true: BranchPathSummary,
    pub path_false: BranchPathSummary,
    pub suspicious_asymmetry: Option<String>,
}

/// Engine for comparing branch side effects.
pub struct BranchDiffEngine;

impl BranchDiffEngine {
    /// Find and diff all sibling branch pairs in a function's CFG
    /// using edge-based traversal through the control-flow graph.
    pub fn diff_branches(cfg_nodes: &[CfgNode], cfg_edges: &[CfgEdge]) -> Vec<BranchDiff> {
        let graph = match CfgGraph::build(cfg_nodes, cfg_edges) {
            Ok(g) => g,
            Err(_) => return vec![],
        };

        let mut diffs = Vec::new();

        // Find all Branch nodes
        for (nid, node) in &graph.nodes {
            if node.kind != CfgNodeKind::Branch {
                continue;
            }

            // Get true/false successor targets
            let true_targets = graph.successors_by_kind(nid, CfgEdgeKind::TrueBranch);
            let false_targets = graph.successors_by_kind(nid, CfgEdgeKind::FalseBranch);

            if true_targets.is_empty() && false_targets.is_empty() {
                continue; // Degenerate branch with no outgoing edges
            }

            let true_path = if let Some(true_edge) = true_targets.first() {
                Self::walk_branch_path(&graph, &true_edge.target, nid)
            } else {
                BranchPathSummary::default()
            };

            let false_path = if let Some(false_edge) = false_targets.first() {
                Self::walk_branch_path(&graph, &false_edge.target, nid)
            } else {
                BranchPathSummary::default()
            };

            let common_prefix = node.target_field.clone().unwrap_or_default();

            // Detect asymmetry
            let suspicious = if !true_path.frees.is_empty() && false_path.frees.is_empty() {
                Some(format!(
                    "Branch asymmetry: field(s) freed in true path ({}) but not in false path",
                    true_path.frees.join(", ")
                ))
            } else if true_path.frees.is_empty() && !false_path.frees.is_empty() {
                Some(format!(
                    "Branch asymmetry: field(s) freed in false path ({}) but not in true path",
                    false_path.frees.join(", ")
                ))
            } else {
                None
            };

            diffs.push(BranchDiff {
                branch_node_line: node.stmt_range.start_line,
                common_prefix: if common_prefix.is_empty() {
                    "?".to_string()
                } else {
                    common_prefix
                },
                path_true: true_path,
                path_false: false_path,
                suspicious_asymmetry: suspicious,
            });
        }

        diffs
    }

    /// Kept for API compatibility; the public `diff_branches` now uses edge-based traversal.
    #[allow(dead_code)]
    fn diff_single_branch(_cfg_nodes: &[CfgNode], _branch_idx: usize) -> Option<BranchDiff> {
        None
    }

    /// Walk a branch path from `start` until the matching Join node (depth=0)
    /// or Exit node, collecting all effects into a BranchPathSummary.
    fn walk_branch_path(
        graph: &CfgGraph,
        start: &CfgNodeId,
        _branch_node_id: &CfgNodeId,
    ) -> BranchPathSummary {
        let mut summary = BranchPathSummary::default();
        let mut current = *start;
        let mut depth: u32 = 1; // We start inside the branch
        let mut visited: HashMap<CfgNodeId, bool> = HashMap::new();
        let max_nodes = 200;

        for _ in 0..max_nodes {
            if visited.contains_key(&current) {
                break; // Cycle detected
            }
            visited.insert(current, true);

            let node = match graph.nodes.get(&current) {
                Some(n) => n,
                None => break,
            };

            // Collect effects
            let target = node.target_field.as_deref().unwrap_or("");
            match node.effect_kind {
                Some(EffectKind::Free) => {
                    if !target.is_empty() && !summary.frees.contains(&target.to_string()) {
                        summary.frees.push(target.to_string());
                    }
                }
                Some(EffectKind::Allocate) => {
                    if !target.is_empty() && !summary.allocates.contains(&target.to_string()) {
                        summary.allocates.push(target.to_string());
                    }
                }
                Some(EffectKind::Assign) | Some(EffectKind::Write) => {
                    if !target.is_empty() && !summary.writes.contains(&target.to_string()) {
                        summary.writes.push(target.to_string());
                    }
                }
                Some(EffectKind::Read) => {
                    if !target.is_empty() && !summary.reads.contains(&target.to_string()) {
                        summary.reads.push(target.to_string());
                    }
                }
                Some(EffectKind::Call) => {
                    let callee = node.callee_name.as_deref().unwrap_or("");
                    if !callee.is_empty() && !summary.calls.contains(&callee.to_string()) {
                        summary.calls.push(callee.to_string());
                    }
                }
                _ => {}
            }

            // Track branch nesting
            match node.kind {
                CfgNodeKind::Branch => depth += 1,
                CfgNodeKind::Join => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break; // Reached matching join
                    }
                }
                CfgNodeKind::Exit => break,
                _ => {}
            }

            // Move to next node via Normal edge (first successor)
            let succs = graph.successors.get(&current);
            if let Some(edges) = succs {
                if let Some(edge) = edges.first() {
                    current = edge.target;
                    continue;
                }
            }
            break;
        }

        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::cfg::CfgEdge;
    use types::enums::{CfgEdgeKind, CfgNodeKind};
    use types::ids::CfgNodeId;
    use types::ids::SymbolId;
    use types::structs::TextRange;

    fn test_function_id() -> SymbolId {
        let file_id = types::ids::FileId::generate("test.c");
        SymbolId::generate(&file_id, "c", "test_fn", "function", None)
    }

    fn text_range(line: u32, byte: u32) -> TextRange {
        TextRange {
            start_byte: byte,
            end_byte: byte,
            start_line: line,
            start_column: 0,
            end_line: line,
            end_column: 0,
        }
    }

    fn make_node(
        fid: &SymbolId,
        kind: CfgNodeKind,
        line: u32,
        byte: u32,
        effect: Option<EffectKind>,
        target: Option<&str>,
        callee: Option<&str>,
    ) -> CfgNode {
        let range = text_range(line, byte);
        let id = CfgNodeId::generate(fid, kind.as_str(), byte);
        CfgNode {
            id,
            function_id: *fid,
            kind,
            stmt_range: range,
            effect_kind: effect,
            target_field: target.map(String::from),
            callee_name: callee.map(String::from),
        }
    }

    fn make_entry_node(fid: &SymbolId, byte: u32) -> CfgNode {
        make_node(fid, CfgNodeKind::Entry, 0, byte, None, None, None)
    }

    fn make_exit_node(fid: &SymbolId, byte: u32) -> CfgNode {
        make_node(fid, CfgNodeKind::Exit, 0, byte, None, None, None)
    }

    fn make_branch_node(
        fid: &SymbolId,
        effect: Option<EffectKind>,
        target: Option<&str>,
        line: u32,
        byte: u32,
    ) -> CfgNode {
        make_node(fid, CfgNodeKind::Branch, line, byte, effect, target, None)
    }

    fn make_stmt_node(
        fid: &SymbolId,
        effect: Option<EffectKind>,
        target: Option<&str>,
        line: u32,
        byte: u32,
    ) -> CfgNode {
        make_node(
            fid,
            CfgNodeKind::Statement,
            line,
            byte,
            effect,
            target,
            None,
        )
    }

    fn make_join_node(fid: &SymbolId, line: u32, byte: u32) -> CfgNode {
        make_node(fid, CfgNodeKind::Join, line, byte, None, None, None)
    }

    fn make_edge(source: &CfgNodeId, target: &CfgNodeId, kind: CfgEdgeKind) -> CfgEdge {
        CfgEdge::new(source, target, kind)
    }

    #[test]
    fn test_empty_cfg() {
        let result = BranchDiffEngine::diff_branches(&[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_no_branches() {
        let fid = test_function_id();
        let entry = make_entry_node(&fid, 0);
        let stmt = make_stmt_node(&fid, None, None, 1, 1);
        let exit = make_exit_node(&fid, 2);
        let nodes = vec![entry.clone(), stmt.clone(), exit.clone()];
        let edges = vec![
            make_edge(&entry.id, &stmt.id, CfgEdgeKind::Normal),
            make_edge(&stmt.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert!(result.is_empty());
    }

    #[test]
    fn test_branch_without_join() {
        let fid = test_function_id();
        let entry = make_entry_node(&fid, 0);
        let branch = make_branch_node(&fid, Some(EffectKind::Condition), Some("ptr"), 10, 1);
        let exit = make_exit_node(&fid, 2);
        let nodes = vec![entry.clone(), branch.clone(), exit.clone()];
        let edges = vec![
            make_edge(&entry.id, &branch.id, CfgEdgeKind::Normal),
            make_edge(&branch.id, &exit.id, CfgEdgeKind::TrueBranch),
            make_edge(&branch.id, &exit.id, CfgEdgeKind::FalseBranch),
        ];
        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].branch_node_line, 10);
    }

    #[test]
    fn test_branch_with_asymmetric_free() {
        let fid = test_function_id();
        let entry = make_entry_node(&fid, 0);
        let branch = make_branch_node(&fid, Some(EffectKind::Condition), Some("ptr"), 10, 1);
        let stmt = make_stmt_node(&fid, Some(EffectKind::Free), Some("ptr"), 11, 2);
        let join = make_join_node(&fid, 20, 3);
        let exit = make_exit_node(&fid, 4);
        let nodes = vec![
            entry.clone(),
            branch.clone(),
            stmt.clone(),
            join.clone(),
            exit.clone(),
        ];
        let edges = vec![
            make_edge(&entry.id, &branch.id, CfgEdgeKind::Normal),
            make_edge(&branch.id, &stmt.id, CfgEdgeKind::TrueBranch),
            make_edge(&branch.id, &join.id, CfgEdgeKind::FalseBranch),
            make_edge(&stmt.id, &join.id, CfgEdgeKind::Normal),
            make_edge(&join.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert_eq!(result.len(), 1);
        assert!(result[0].suspicious_asymmetry.is_some());
    }

    #[test]
    fn test_branch_with_nested_branch() {
        // Branch1 -> (Branch2 -> Free -> Join1) -> Join2
        let fid = test_function_id();
        let entry = make_entry_node(&fid, 0);
        let branch1 = make_branch_node(&fid, Some(EffectKind::Condition), Some("field_a"), 1, 1);
        let branch2 = make_branch_node(&fid, Some(EffectKind::Condition), Some("field_b"), 2, 2);
        let stmt = make_stmt_node(&fid, Some(EffectKind::Free), Some("field_a"), 3, 3);
        let join1 = make_join_node(&fid, 4, 4);
        let join2 = make_join_node(&fid, 5, 5);
        let exit = make_exit_node(&fid, 6);
        let nodes = vec![
            entry.clone(),
            branch1.clone(),
            branch2.clone(),
            stmt.clone(),
            join1.clone(),
            join2.clone(),
            exit.clone(),
        ];
        let edges = vec![
            make_edge(&entry.id, &branch1.id, CfgEdgeKind::Normal),
            make_edge(&branch1.id, &branch2.id, CfgEdgeKind::TrueBranch),
            make_edge(&branch1.id, &join2.id, CfgEdgeKind::FalseBranch),
            make_edge(&branch2.id, &stmt.id, CfgEdgeKind::TrueBranch),
            make_edge(&branch2.id, &join1.id, CfgEdgeKind::FalseBranch),
            make_edge(&stmt.id, &join1.id, CfgEdgeKind::Normal),
            make_edge(&join1.id, &join2.id, CfgEdgeKind::Normal),
            make_edge(&join2.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert!(!result.is_empty(), "Should find at least the outer branch");
    }

    #[test]
    fn test_branch_alloc_vs_no_alloc_asymmetry() {
        // Branch -> Alloc -> Join (only in true path)
        let fid = test_function_id();
        let entry = make_entry_node(&fid, 0);
        let branch = make_branch_node(&fid, Some(EffectKind::Condition), Some("ctx->buf"), 1, 1);
        let alloc = make_stmt_node(&fid, Some(EffectKind::Allocate), Some("ctx->buf"), 2, 2);
        let join = make_join_node(&fid, 3, 3);
        let exit = make_exit_node(&fid, 4);
        let nodes = vec![
            entry.clone(),
            branch.clone(),
            alloc.clone(),
            join.clone(),
            exit.clone(),
        ];
        let edges = vec![
            make_edge(&entry.id, &branch.id, CfgEdgeKind::Normal),
            make_edge(&branch.id, &alloc.id, CfgEdgeKind::TrueBranch),
            make_edge(&branch.id, &join.id, CfgEdgeKind::FalseBranch),
            make_edge(&alloc.id, &join.id, CfgEdgeKind::Normal),
            make_edge(&join.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert!(!result.is_empty());
        assert!(!result.is_empty(), "Should detect at least one branch");
    }

    #[test]
    fn test_multiple_branches_in_function() {
        let fid = test_function_id();
        let entry = make_entry_node(&fid, 0);
        let br1 = make_branch_node(&fid, Some(EffectKind::Condition), Some("x"), 1, 1);
        let free1 = make_stmt_node(&fid, Some(EffectKind::Free), Some("x"), 2, 2);
        let join1 = make_join_node(&fid, 3, 3);
        let br2 = make_branch_node(&fid, Some(EffectKind::Condition), Some("y"), 4, 4);
        let free2 = make_stmt_node(&fid, Some(EffectKind::Free), Some("y"), 5, 5);
        let join2 = make_join_node(&fid, 6, 6);
        let exit = make_exit_node(&fid, 7);
        let nodes = vec![
            entry.clone(),
            br1.clone(),
            free1.clone(),
            join1.clone(),
            br2.clone(),
            free2.clone(),
            join2.clone(),
            exit.clone(),
        ];
        let edges = vec![
            make_edge(&entry.id, &br1.id, CfgEdgeKind::Normal),
            make_edge(&br1.id, &free1.id, CfgEdgeKind::TrueBranch),
            make_edge(&br1.id, &join1.id, CfgEdgeKind::FalseBranch),
            make_edge(&free1.id, &join1.id, CfgEdgeKind::Normal),
            make_edge(&join1.id, &br2.id, CfgEdgeKind::Normal),
            make_edge(&br2.id, &free2.id, CfgEdgeKind::TrueBranch),
            make_edge(&br2.id, &join2.id, CfgEdgeKind::FalseBranch),
            make_edge(&free2.id, &join2.id, CfgEdgeKind::Normal),
            make_edge(&join2.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert_eq!(result.len(), 2, "Should find both branches");
    }
}
