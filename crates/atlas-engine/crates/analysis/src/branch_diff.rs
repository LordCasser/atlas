//! Branch Effect Diff — compare sibling branch paths for suspicious asymmetries.
//!
//! Compares the side effects of true/false branches (if/else) or switch cases.
//! Detects patterns like: one branch frees a field but the other doesn't.

use crate::cfg_graph::CfgGraph;
use std::collections::{HashSet, VecDeque};
use types::cfg::{CfgEdge, CfgNode};
use types::enums::{CfgEdgeKind, CfgNodeKind};
use types::ids::CfgNodeId;

use super::effect_composer::EffectComposition;

/// Summary of effects on one side of a branch.
#[derive(Debug, Clone, Default)]
pub struct BranchPathSummary {
    pub frees: Vec<String>,
    pub allocates: Vec<String>,
    pub writes: Vec<String>,
    pub reads: Vec<String>,
    pub calls: Vec<String>,
}

impl BranchPathSummary {
    /// Merge another summary's effects into this one, deduplicating entries.
    pub fn merge_from(&mut self, other: &Self) {
        for f in &other.frees {
            if !self.frees.contains(f) {
                self.frees.push(f.clone());
            }
        }
        for a in &other.allocates {
            if !self.allocates.contains(a) {
                self.allocates.push(a.clone());
            }
        }
        for w in &other.writes {
            if !self.writes.contains(w) {
                self.writes.push(w.clone());
            }
        }
        for r in &other.reads {
            if !self.reads.contains(r) {
                self.reads.push(r.clone());
            }
        }
        for c in &other.calls {
            if !self.calls.contains(c) {
                self.calls.push(c.clone());
            }
        }
    }
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

        for (nid, node) in &graph.nodes {
            if node.kind != CfgNodeKind::Branch {
                continue;
            }

            let true_targets = graph.successors_by_kind(nid, CfgEdgeKind::TrueBranch);
            let false_targets = graph.successors_by_kind(nid, CfgEdgeKind::FalseBranch);

            if true_targets.is_empty() && false_targets.is_empty() {
                continue;
            }

            let true_path = {
                let mut merged = BranchPathSummary::default();
                for edge in true_targets {
                    let path = Self::walk_branch_path(&graph, &edge.target);
                    merged.merge_from(&path);
                }
                merged
            };

            let false_path = {
                let mut merged = BranchPathSummary::default();
                for edge in false_targets {
                    let path = Self::walk_branch_path(&graph, &edge.target);
                    merged.merge_from(&path);
                }
                merged
            };

            let common_prefix = String::new();

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
            } else if !true_path.allocates.is_empty() && false_path.allocates.is_empty() {
                Some(format!(
                    "Branch asymmetry: field(s) allocated in true path ({}) but not in false path",
                    true_path.allocates.join(", ")
                ))
            } else if true_path.allocates.is_empty() && !false_path.allocates.is_empty() {
                Some(format!(
                    "Branch asymmetry: field(s) allocated in false path ({}) but not in true path",
                    false_path.allocates.join(", ")
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

    /// Phase 2: Diff branches using semantic effects from EffectComposer.
    ///
    /// Converts structured `BranchDiffIssue` results into legacy `BranchDiff` format
    /// for backward-compatible consumption by MCP tools and CLI.
    pub fn diff_branches_semantic(
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        composition: &EffectComposition,
    ) -> Vec<BranchDiff> {
        let graph = match CfgGraph::build(cfg_nodes, cfg_edges) {
            Ok(g) => g,
            Err(_) => return vec![],
        };

        let issues = super::branch_diff_semantic::analyze_branch_semantic(&graph, composition);

        // Convert structured issues → legacy BranchDiff format
        issues
            .into_iter()
            .map(|issue| {
                // Determine node line from the branch node in the graph
                let node_line = graph
                    .nodes
                    .get(&issue.branch_node_id)
                    .map(|n| n.stmt_range.start_line)
                    .unwrap_or(0);

                let mut path_true = BranchPathSummary::default();
                let mut path_false = BranchPathSummary::default();

                // Map field effects to summary
                if issue.true_side.has_free {
                    path_true.frees.push(issue.field.clone());
                }
                if issue.true_side.has_alloc {
                    path_true.allocates.push(issue.field.clone());
                }
                if issue.true_side.has_write {
                    path_true.writes.push(issue.field.clone());
                }
                if issue.false_side.has_free {
                    path_false.frees.push(issue.field.clone());
                }
                if issue.false_side.has_alloc {
                    path_false.allocates.push(issue.field.clone());
                }
                if issue.false_side.has_write {
                    path_false.writes.push(issue.field.clone());
                }

                BranchDiff {
                    branch_node_line: node_line,
                    common_prefix: issue.field.clone(),
                    path_true,
                    path_false,
                    suspicious_asymmetry: Some(issue.description),
                }
            })
            .collect()
    }

    /// Walk a branch path from `start` until the matching Join node (depth=0)
    /// or Exit node, collecting all effects into a BranchPathSummary.
    fn walk_branch_path(
        graph: &CfgGraph,
        start: &CfgNodeId,
    ) -> BranchPathSummary {
        let mut summary = BranchPathSummary::default();
        let mut visited = HashSet::new();
        let mut worklist = VecDeque::new();
        worklist.push_back((*start, 1u32)); // (node_id, depth), start inside branch

        while let Some((node_id, depth)) = worklist.pop_front() {
            if !visited.insert(node_id) {
                continue;
            }

            let node = match graph.nodes.get(&node_id) {
                Some(n) => n,
                None => continue,
            };

            // Collect effects from semantic_effects annotations
            use types::effects::{PlaceRef, SemanticEffectKind};
            for eff in &node.semantic_effects {
                match &eff.kind {
                    SemanticEffectKind::Free {
                        place: PlaceRef::Field { path },
                        ..
                    } => {
                        if !summary.frees.contains(path) {
                            summary.frees.push(path.clone());
                        }
                    }
                    SemanticEffectKind::Alloc {
                        target: PlaceRef::Field { path },
                        ..
                    } => {
                        if !summary.allocates.contains(path) {
                            summary.allocates.push(path.clone());
                        }
                    }
                    SemanticEffectKind::Store {
                        dst: PlaceRef::Field { path },
                        ..
                    }
                    | SemanticEffectKind::Nullify {
                        place: PlaceRef::Field { path },
                        ..
                    }
                    | SemanticEffectKind::Assign {
                        dst: PlaceRef::Field { path },
                        ..
                    } => {
                        if !summary.writes.contains(path) {
                            summary.writes.push(path.clone());
                        }
                    }
                    SemanticEffectKind::Call { callee } => {
                        if !summary.calls.contains(callee) {
                            summary.calls.push(callee.clone());
                        }
                    }
                    _ => {}
                }
            }

            let child_depth = match node.kind {
                CfgNodeKind::Branch => depth + 1,
                CfgNodeKind::Join => {
                    let d = depth.saturating_sub(1);
                    if d == 0 {
                        continue;
                    }
                    d
                }
                CfgNodeKind::Exit => continue,
                _ => depth,
            };

            if let Some(edges) = graph.successors.get(&node_id) {
                for edge in edges {
                    worklist.push_back((edge.target, child_depth));
                }
            }
        }

        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::cfg::CfgEdge;
    use types::effects::{PlaceRef, SemanticEffect, SemanticEffectKind, ValueSource};
    use types::enums::{CfgEdgeKind, CfgNodeKind};
    use types::ids::{CfgNodeId, EffectId, SymbolId};
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

    fn make_test_effect(node_id: CfgNodeId, order: u32, kind: SemanticEffectKind) -> SemanticEffect {
        let kind_name = match &kind {
            SemanticEffectKind::Alloc { .. } => "Alloc",
            SemanticEffectKind::Free { .. } => "Free",
            SemanticEffectKind::Store { .. } => "Store",
            SemanticEffectKind::Assign { .. } => "Assign",
            SemanticEffectKind::Call { .. } => "Call",
            SemanticEffectKind::Nullify { .. } => "Nullify",
            SemanticEffectKind::Return { .. } => "Return",
            SemanticEffectKind::Escape { .. } => "Escape",
        };
        let id = EffectId::generate(&node_id, order, kind_name);
        SemanticEffect {
            id,
            cfg_node_id: node_id,
            order,
            kind,
            confidence: 0.8,
        }
    }

    fn make_node(
        fid: &SymbolId,
        kind: CfgNodeKind,
        line: u32,
        byte: u32,
        effects: Vec<SemanticEffect>,
    ) -> CfgNode {
        let range = text_range(line, byte);
        let id = CfgNodeId::generate(fid, kind.as_str(), byte);
        CfgNode {
            id,
            function_id: *fid,
            kind,
            stmt_range: range,
            semantic_effects: effects,
        }
    }

    fn make_entry_node(fid: &SymbolId, byte: u32) -> CfgNode {
        make_node(fid, CfgNodeKind::Entry, 0, byte, vec![])
    }

    fn make_exit_node(fid: &SymbolId, byte: u32) -> CfgNode {
        make_node(fid, CfgNodeKind::Exit, 0, byte, vec![])
    }

    fn make_branch_node(
        fid: &SymbolId,
        line: u32,
        byte: u32,
    ) -> CfgNode {
        make_node(fid, CfgNodeKind::Branch, line, byte, vec![])
    }

    fn make_stmt_node(
        fid: &SymbolId,
        effects: Vec<SemanticEffect>,
        line: u32,
        byte: u32,
    ) -> CfgNode {
        make_node(fid, CfgNodeKind::Statement, line, byte, effects)
    }

    fn make_join_node(fid: &SymbolId, line: u32, byte: u32) -> CfgNode {
        make_node(fid, CfgNodeKind::Join, line, byte, vec![])
    }

    fn make_edge(source: &CfgNodeId, target: &CfgNodeId, kind: CfgEdgeKind) -> CfgEdge {
        CfgEdge::new(source, target, kind)
    }

    /// Create a Free semantic effect for a field.
    fn se_free(node_id: CfgNodeId, order: u32, field: &str) -> SemanticEffect {
        make_test_effect(node_id, order, SemanticEffectKind::Free {
            place: PlaceRef::Field { path: field.to_string() },
            callee: "?".to_string(),
        })
    }

    /// Create an Alloc semantic effect for a field.
    fn se_alloc(node_id: CfgNodeId, order: u32, field: &str) -> SemanticEffect {
        make_test_effect(node_id, order, SemanticEffectKind::Alloc {
            target: PlaceRef::Field { path: field.to_string() },
            callee: "?".to_string(),
        })
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
        let stmt = make_stmt_node(&fid, vec![], 1, 1);
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
        let branch = make_branch_node(&fid, 10, 1);
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
        let branch = make_branch_node(&fid, 10, 1);
        let stmt_id = CfgNodeId::generate(&fid, CfgNodeKind::Statement.as_str(), 2);
        let stmt = make_stmt_node(&fid, vec![se_free(stmt_id, 0, "ptr")], 11, 2);
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
        let branch1 = make_branch_node(&fid, 1, 1);
        let branch2 = make_branch_node(&fid, 2, 2);
        let stmt_id = CfgNodeId::generate(&fid, CfgNodeKind::Statement.as_str(), 3);
        let stmt = make_stmt_node(&fid, vec![se_free(stmt_id, 0, "field_a")], 3, 3);
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
        let branch = make_branch_node(&fid, 1, 1);
        let alloc_id = CfgNodeId::generate(&fid, CfgNodeKind::Statement.as_str(), 2);
        let alloc = make_stmt_node(&fid, vec![se_alloc(alloc_id, 0, "ctx->buf")], 2, 2);
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
        assert!(!result.is_empty(), "Should detect at least one branch");
        assert!(
            result[0].suspicious_asymmetry.is_some(),
            "Should detect allocate asymmetry"
        );
        if let Some(ref msg) = result[0].suspicious_asymmetry {
            assert!(
                msg.contains("allocated"),
                "Asymmetry message should mention 'allocated': {msg}"
            );
        }
    }

    #[test]
    fn test_multiple_branches_in_function() {
        let fid = test_function_id();
        let entry = make_entry_node(&fid, 0);
        let br1 = make_branch_node(&fid, 1, 1);
        let free1_id = CfgNodeId::generate(&fid, CfgNodeKind::Statement.as_str(), 2);
        let free1 = make_stmt_node(&fid, vec![se_free(free1_id, 0, "x")], 2, 2);
        let join1 = make_join_node(&fid, 3, 3);
        let br2 = make_branch_node(&fid, 4, 4);
        let free2_id = CfgNodeId::generate(&fid, CfgNodeKind::Statement.as_str(), 5);
        let free2 = make_stmt_node(&fid, vec![se_free(free2_id, 0, "y")], 5, 5);
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
