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

    /// Whether this summary has any tracked effects.
    fn has_any_effect(&self) -> bool {
        !self.frees.is_empty()
            || !self.allocates.is_empty()
            || !self.writes.is_empty()
            || !self.reads.is_empty()
            || !self.calls.is_empty()
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
        let reachable = graph.reachable_from_entry();

        for (nid, node) in &graph.nodes {
            if node.kind != CfgNodeKind::Branch || !reachable.contains(nid) {
                continue;
            }

            let true_targets = graph.successors_by_kind(nid, CfgEdgeKind::TrueBranch);
            let false_targets = graph.successors_by_kind(nid, CfgEdgeKind::FalseBranch);
            let case_targets = graph.successors_by_kind(nid, CfgEdgeKind::CaseBranch);

            // Switch dispatch node: N-way case comparison (CaseBranch edges).
            // A Branch node emitted by walk_switch has CaseBranch successors and
            // no True/FalseBranch successors. Handle it separately, then skip the
            // if/else path so we don't double-report.
            if !case_targets.is_empty() {
                if let Some(diff) = Self::diff_switch_cases(&graph, node, &case_targets) {
                    diffs.push(diff);
                }
                continue;
            }

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
                common_prefix: "?".to_string(),
                path_true: true_path,
                path_false: false_path,
                suspicious_asymmetry: suspicious,
            });
        }

        diffs
    }

    /// N-way switch-case comparison.
    ///
    /// A switch dispatch Branch node has one [`CfgEdgeKind::CaseBranch`] edge per
    /// case body and, when there is no default, one synthetic Branch→Join skip
    /// edge (the "no case matched" path). Supported implicit/explicit
    /// fall-through is represented by ordinary case-tail → next-case edges, so
    /// walking from a case target includes the effects it inherits downstream.
    ///
    /// # False-positive strategy (contract: may under-report, must NOT over-report)
    ///
    /// The builder remains best-effort for labeled jumps and malformed/recovered
    /// syntax. To keep that uncertainty from becoming noisy findings we:
    ///
    /// 1. **Ignore effect-less paths.** Empty case bodies and the synthetic
    ///    Branch→Join skip edge land directly on the Join/Exit and carry no
    ///    effects, so they can never be the flagged outlier.
    /// 2. **Reference-union comparison, O(n).** Compute the union of effects
    ///    across all *effectful* cases once, then compare each case against the
    ///    union. We only flag the **all-but-one** shape: a resource handled in
    ///    every effectful case except exactly one. A lone case doing something no
    ///    other case does (n-1 cases silent) is treated as an intentional
    ///    special-case, not an asymmetry — reporting it would be noise and, under
    ///    fall-through, likely wrong.
    ///
    /// Returns `Some(BranchDiff)` describing the outlier case, or `None`.
    fn diff_switch_cases(
        graph: &CfgGraph,
        branch_node: &CfgNode,
        case_targets: &[&CfgEdge],
    ) -> Option<BranchDiff> {
        // 1. Walk each case path; keep only effectful ones (guards fall-through
        //    empty cases and the synthetic no-match skip edge). Effect-less paths
        //    contribute nothing to the union, so filtering them here does not
        //    change the union — it only bounds the outlier vote below.
        let case_paths: Vec<BranchPathSummary> = case_targets
            .iter()
            .map(|edge| Self::walk_branch_path(graph, &edge.target))
            .filter(|p| p.has_any_effect())
            .collect();

        // 2. Reference union of effects across all effectful cases (O(n)).
        let mut union = BranchPathSummary::default();
        for p in &case_paths {
            union.merge_from(p);
        }

        // 3. Suspicious asymmetry: only the "all-but-one" shape is flagged, and
        //    only with ≥ 3 effectful cases (see `find_all_but_one_outlier` and
        //    the doc comment above). Fewer effectful cases → recorded branch with
        //    no asymmetry flag (mirrors the if/else path, which always records a
        //    BranchDiff even when both sides are effect-free).
        let n = case_paths.len();
        let suspicious = Self::find_all_but_one_outlier(&union.frees, &case_paths, |p| &p.frees)
            .map(|res| {
                format!(
                    "Switch asymmetry: resource '{res}' freed in {} of {n} cases but not in 1 case",
                    n - 1
                )
            })
            .or_else(|| {
                Self::find_all_but_one_outlier(&union.allocates, &case_paths, |p| &p.allocates).map(
                    |res| {
                        format!(
                            "Switch asymmetry: resource '{res}' allocated in {} of {n} cases but not in 1 case",
                            n - 1
                        )
                    },
                )
            });

        // Always record the switch branch (branch_count > 0 for switches), with
        // the asymmetry flag set only under the conservative rule above.
        Some(BranchDiff {
            branch_node_line: branch_node.stmt_range.start_line,
            common_prefix: "switch".to_string(),
            path_true: union,
            path_false: BranchPathSummary::default(),
            suspicious_asymmetry: suspicious,
        })
    }

    /// Find a resource that appears in exactly `n-1` of the case paths (handled
    /// everywhere but one). Returns the resource name of the first such outlier.
    ///
    /// This is the only switch-case shape we flag: it corresponds to a resource
    /// that is consistently managed across cases with a single conspicuous gap —
    /// the pattern most likely to be a real leak/double-free bug. A resource
    /// touched by only 1 case (or by ≤ n-2 cases) is treated as intentional
    /// per-case behavior and NOT flagged, keeping us within the no-over-report
    /// contract even without fall-through modeling.
    fn find_all_but_one_outlier(
        union_resources: &[String],
        case_paths: &[BranchPathSummary],
        select: impl Fn(&BranchPathSummary) -> &Vec<String>,
    ) -> Option<String> {
        let n = case_paths.len();
        if n < 3 {
            // With only 2 cases, "all-but-one" == "one case only", which is
            // indistinguishable from an intentional special-case. Require ≥ 3
            // cases so the majority signal is meaningful.
            return None;
        }
        for res in union_resources {
            let count = case_paths
                .iter()
                .filter(|p| select(p).contains(res))
                .count();
            if count == n - 1 {
                return Some(res.clone());
            }
        }
        None
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
    fn walk_branch_path(graph: &CfgGraph, start: &CfgNodeId) -> BranchPathSummary {
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
                    SemanticEffectKind::Call { callee } if !summary.calls.contains(callee) => {
                        summary.calls.push(callee.clone());
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
    use types::effects::{PlaceRef, SemanticEffect, SemanticEffectKind};
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

    fn make_test_effect(
        node_id: CfgNodeId,
        order: u32,
        kind: SemanticEffectKind,
    ) -> SemanticEffect {
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
            consumption_style: None,
            description: None,
            eligible_for_implicit_cleanup: None,
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
            call_context: types::enums::CallContext::None,
            managed_scope_start_byte: None,
            semantic_effects: effects,
        }
    }

    fn make_entry_node(fid: &SymbolId, byte: u32) -> CfgNode {
        make_node(fid, CfgNodeKind::Entry, 0, byte, vec![])
    }

    fn make_exit_node(fid: &SymbolId, byte: u32) -> CfgNode {
        make_node(fid, CfgNodeKind::Exit, 0, byte, vec![])
    }

    fn make_branch_node(fid: &SymbolId, line: u32, byte: u32) -> CfgNode {
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
        make_test_effect(
            node_id,
            order,
            SemanticEffectKind::Free {
                place: PlaceRef::Field {
                    path: field.to_string(),
                },
                callee: "?".to_string(),
            },
        )
    }

    /// Create an Alloc semantic effect for a field.
    fn se_alloc(node_id: CfgNodeId, order: u32, field: &str) -> SemanticEffect {
        make_test_effect(
            node_id,
            order,
            SemanticEffectKind::Alloc {
                target: PlaceRef::Field {
                    path: field.to_string(),
                },
                callee: "?".to_string(),
            },
        )
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
    fn test_unreachable_branch_after_abrupt_transfer_is_not_analyzed() {
        let fid = test_function_id();
        let entry = make_entry_node(&fid, 0);
        let exit = make_exit_node(&fid, 1);
        let branch = make_branch_node(&fid, 10, 10);
        let stmt_id = CfgNodeId::generate(&fid, CfgNodeKind::Statement.as_str(), 11);
        let stmt = make_stmt_node(&fid, vec![se_free(stmt_id, 0, "ptr")], 11, 11);
        let join = make_join_node(&fid, 12, 12);
        let nodes = vec![
            entry.clone(),
            exit.clone(),
            branch.clone(),
            stmt.clone(),
            join.clone(),
        ];
        let edges = vec![
            make_edge(&entry.id, &exit.id, CfgEdgeKind::Normal),
            make_edge(&branch.id, &stmt.id, CfgEdgeKind::TrueBranch),
            make_edge(&branch.id, &join.id, CfgEdgeKind::FalseBranch),
            make_edge(&stmt.id, &join.id, CfgEdgeKind::Normal),
            make_edge(&join.id, &exit.id, CfgEdgeKind::Normal),
        ];

        assert!(BranchDiffEngine::diff_branches(&nodes, &edges).is_empty());
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

    // ── Switch N-way tests ────────────────────────────────────────────────
    //
    // Switch CFG shape produced by cfg_builder::walk_switch:
    //   Branch --CaseBranch--> case_body_i --Normal--> Join   (per case)
    //   Branch --CaseBranch--> Join                            (synthetic skip)
    //
    // These tests exercise the false-positive strategy in diff_switch_cases:
    // only the "all-but-one" shape (≥3 effectful cases, one gap) is flagged.

    /// Build a switch CFG: `n_cases` case bodies each with the given effects,
    /// plus the synthetic Branch→Join skip edge.
    fn build_switch_cfg(
        fid: &SymbolId,
        case_effects: &[Vec<SemanticEffect>],
    ) -> (Vec<CfgNode>, Vec<CfgEdge>, u32) {
        let entry = make_entry_node(fid, 0);
        let branch = make_branch_node(fid, 10, 1);
        let join = make_join_node(fid, 100, 1000);
        let exit = make_exit_node(fid, 1001);

        let mut nodes = vec![entry.clone(), branch.clone()];
        let mut edges = vec![make_edge(&entry.id, &branch.id, CfgEdgeKind::Normal)];

        for (i, effects) in case_effects.iter().enumerate() {
            let byte = 10 + i as u32; // unique byte → unique node id
            let case_node = make_stmt_node(fid, effects.clone(), 20 + i as u32, byte);
            edges.push(make_edge(
                &branch.id,
                &case_node.id,
                CfgEdgeKind::CaseBranch,
            ));
            edges.push(make_edge(&case_node.id, &join.id, CfgEdgeKind::Normal));
            nodes.push(case_node);
        }
        // Synthetic no-match skip edge.
        edges.push(make_edge(&branch.id, &join.id, CfgEdgeKind::CaseBranch));
        edges.push(make_edge(&join.id, &exit.id, CfgEdgeKind::Normal));
        nodes.push(join);
        nodes.push(exit);

        (nodes, edges, branch.stmt_range.start_line)
    }

    /// Helper to build a case body's free effect for a resource, keyed by byte.
    fn case_free(fid: &SymbolId, byte: u32, res: &str) -> SemanticEffect {
        let nid = CfgNodeId::generate(fid, CfgNodeKind::Statement.as_str(), byte);
        se_free(nid, 0, res)
    }

    /// 3 cases, `res` freed in 2 of them → all-but-one → FLAGGED.
    #[test]
    fn test_switch_all_but_one_free_detected() {
        let fid = test_function_id();
        let case_effects = vec![
            vec![case_free(&fid, 10, "res")],   // case 0: frees res
            vec![case_free(&fid, 11, "res")],   // case 1: frees res
            vec![case_free(&fid, 12, "other")], // case 2: frees something else (gap for res)
        ];
        let (nodes, edges, _line) = build_switch_cfg(&fid, &case_effects);
        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert_eq!(result.len(), 1, "Should produce exactly one switch diff");
        let diff = &result[0];
        assert!(
            diff.suspicious_asymmetry.is_some(),
            "all-but-one free should be flagged, got: {diff:?}"
        );
        let msg = diff.suspicious_asymmetry.as_ref().unwrap();
        assert!(
            msg.contains("res") && msg.contains("freed"),
            "message should name the outlier resource: {msg}"
        );
    }

    /// 3 cases, `res` freed in ONLY 1 of them → unique special-case → NOT flagged.
    /// This is the fall-through-safe case: a lone freeing case must not be
    /// reported as "missing free" in the other two.
    #[test]
    fn test_switch_unique_case_not_flagged() {
        let fid = test_function_id();
        let case_effects = vec![
            vec![case_free(&fid, 10, "res")], // case 0: frees res (unique)
            vec![case_free(&fid, 11, "a")],   // case 1: frees a
            vec![case_free(&fid, 12, "b")],   // case 2: frees b
        ];
        let (nodes, edges, _line) = build_switch_cfg(&fid, &case_effects);
        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        // A switch diff is produced (branch is recorded), but no asymmetry flag.
        assert_eq!(result.len(), 1, "Should still record the switch branch");
        assert!(
            result[0].suspicious_asymmetry.is_none(),
            "a resource freed by only one case must NOT be flagged (fall-through safe), got: {:?}",
            result[0].suspicious_asymmetry
        );
    }

    /// A non-empty case that falls through inherits the next case's cleanup.
    /// It must not become the lone "missing free" outlier.
    #[test]
    fn test_switch_non_empty_fallthrough_inherits_downstream_effects() {
        let fid = test_function_id();
        let case_effects = vec![
            vec![case_free(&fid, 10, "marker")],
            vec![case_free(&fid, 11, "res")],
            vec![case_free(&fid, 12, "res")],
            vec![case_free(&fid, 13, "res")],
        ];
        let (nodes, mut edges, _line) = build_switch_cfg(&fid, &case_effects);
        let branch = nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("switch branch");
        let join = nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("switch join");
        let case_targets: Vec<CfgNodeId> = edges
            .iter()
            .filter(|edge| {
                edge.source == branch.id
                    && edge.kind == CfgEdgeKind::CaseBranch
                    && edge.target != join.id
            })
            .map(|edge| edge.target)
            .collect();
        let first = case_targets[0];
        let second = case_targets[1];
        edges.retain(|edge| !(edge.source == first && edge.target == join.id));
        edges.push(make_edge(&first, &second, CfgEdgeKind::Normal));

        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert_eq!(result.len(), 1);
        assert!(
            result[0].suspicious_asymmetry.is_none(),
            "fall-through path inherits the downstream free: {:?}",
            result[0].suspicious_asymmetry
        );
    }

    /// A plausibly-fall-through empty case (no effects) plus the synthetic skip
    /// edge must not trip the outlier detector. 3 effectful cases all free `res`
    /// symmetrically; the empty case is ignored → no flag.
    #[test]
    fn test_switch_empty_case_ignored() {
        let fid = test_function_id();
        let case_effects = vec![
            vec![case_free(&fid, 10, "res")],
            vec![case_free(&fid, 11, "res")],
            vec![case_free(&fid, 12, "res")],
            vec![], // empty case (fall-through label): no effects → ignored
        ];
        let (nodes, edges, _line) = build_switch_cfg(&fid, &case_effects);
        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert_eq!(result.len(), 1);
        assert!(
            result[0].suspicious_asymmetry.is_none(),
            "symmetric frees + ignored empty case should not be flagged, got: {:?}",
            result[0].suspicious_asymmetry
        );
    }

    /// Two-case switch is never flagged (all-but-one collapses to one-case-only).
    #[test]
    fn test_switch_two_cases_not_flagged() {
        let fid = test_function_id();
        let case_effects = vec![
            vec![case_free(&fid, 10, "res")],
            vec![case_free(&fid, 11, "other")],
        ];
        let (nodes, edges, _line) = build_switch_cfg(&fid, &case_effects);
        let result = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert_eq!(result.len(), 1);
        assert!(
            result[0].suspicious_asymmetry.is_none(),
            "2-case switch must not be flagged, got: {:?}",
            result[0].suspicious_asymmetry
        );
    }

    /// End-to-end: a real switch parsed by CfgBuilder must produce a Branch node
    /// that `diff_branches` records (rather than the old single Statement, which
    /// yielded branch_count=0). Validates the extraction→analysis wiring.
    #[test]
    fn test_switch_end_to_end_cfg_recorded() {
        use extraction::create_frontend;
        use tree_sitter::Parser;
        use types::enums::Language;

        let source = r#"function pick(x: number) {
          switch (x) {
            case 1: freeA(); break;
            case 2: freeB(); break;
            case 3: freeC(); break;
            default: fallback();
          }
        }"#;
        let source_bytes = source.as_bytes().to_vec();
        let frontend = create_frontend(Language::TypeScript).unwrap();
        let mut parser = Parser::new();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .unwrap();
        let tree = parser.parse(&source_bytes, None).unwrap();

        // Find the function node.
        fn find_fn<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if node.kind() == "function_declaration" {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(f) = find_fn(child) {
                    return Some(f);
                }
            }
            None
        }
        let func_node = find_fn(tree.root_node()).expect("function found");
        let fid = test_function_id();
        let cfg =
            extraction::CfgBuilder::build(Language::TypeScript, &fid, func_node, &source_bytes);

        // A Branch (dispatch) node must exist, with CaseBranch edges out of it.
        let branch = cfg
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Branch)
            .expect("switch should produce a Branch node");
        let case_edges = cfg
            .edges
            .iter()
            .filter(|e| e.source == branch.id && e.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert!(
            case_edges >= 4,
            "expected 3 cases + default without a no-match edge, got {case_edges}"
        );

        // branch_diff must now record the switch (branch_count > 0).
        let diffs = BranchDiffEngine::diff_branches(&cfg.nodes, &cfg.edges);
        assert_eq!(
            diffs.len(),
            1,
            "switch should be recorded as one branch diff, got {}",
            diffs.len()
        );
    }
}
