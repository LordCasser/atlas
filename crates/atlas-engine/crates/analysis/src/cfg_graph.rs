//! CfgGraph — adjacency-list representation of a function's control-flow graph.
//!
//! Built from CfgNode + CfgEdge slices. Provides bidirectional traversal
//! (successors and predecessors) for dataflow fixpoint algorithms, branch-path
//! walking, and structural validation.

use std::collections::{HashMap, HashSet, VecDeque};
use types::cfg::{CfgEdge, CfgNode};
use types::enums::{CfgEdgeKind, CfgNodeKind};
use types::ids::{CfgEdgeId, CfgNodeId};

/// Boolean edges required to reach any selected occurrence in this CFG. This
/// describes the supplied graph, not feasibility or complete program behavior.
#[derive(Debug, Default)]
pub struct RequiredBranches {
    pub edges: Vec<CfgEdge>,
    pub target_reachable: bool,
    pub truncated: bool,
}

/// A bidirectional adjacency-list view of a function's CFG.
pub struct CfgGraph {
    pub nodes: HashMap<CfgNodeId, CfgNode>,
    /// Outgoing edges from each node.
    pub successors: HashMap<CfgNodeId, Vec<CfgEdge>>,
    /// Incoming edges to each node (required for fixpoint merge).
    pub predecessors: HashMap<CfgNodeId, Vec<CfgEdge>>,
    /// The unique Entry node.
    pub entry: CfgNodeId,
    /// The unique Exit node.
    pub exit: CfgNodeId,
}

impl CfgGraph {
    /// A branch edge is required exactly when removing it disconnects Entry
    /// from every selected occurrence and every unexamined region. Reaching an
    /// unexamined region conservatively allows a bypass to any selection, so
    /// missing control flow cannot establish a dependent guard. This does not
    /// add those possible bypasses to the stored graph.
    pub fn required_branches(
        &self,
        targets: &HashSet<CfgNodeId>,
        unexamined: &HashSet<CfgNodeId>,
        max_visits: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> anyhow::Result<RequiredBranches> {
        let mut remaining = max_visits;
        let mut result = RequiredBranches::default();
        match self.reaches_any(targets, None, &mut remaining, cancelled)? {
            Some(true) => result.target_reachable = true,
            Some(false) => return Ok(result),
            None => {
                result.truncated = true;
                return Ok(result);
            }
        }
        let possible_targets = targets.union(unexamined).copied().collect();
        let mut branches: Vec<_> = self
            .successors
            .values()
            .flatten()
            .filter(|edge| {
                matches!(
                    edge.kind,
                    CfgEdgeKind::TrueBranch | CfgEdgeKind::FalseBranch
                )
            })
            .collect();
        branches.sort_by_key(|edge| (self.nodes[&edge.source].stmt_range.start_byte, edge.id));
        for edge in branches {
            match self.reaches_any(&possible_targets, Some(edge.id), &mut remaining, cancelled)? {
                Some(false) => result.edges.push(edge.clone()),
                Some(true) => {}
                None => {
                    result.truncated = true;
                    break;
                }
            }
        }
        Ok(result)
    }

    fn reaches_any(
        &self,
        targets: &HashSet<CfgNodeId>,
        omitted: Option<CfgEdgeId>,
        remaining: &mut usize,
        cancelled: &dyn Fn() -> bool,
    ) -> anyhow::Result<Option<bool>> {
        let mut pending = VecDeque::from([self.entry]);
        let mut visited = HashSet::new();
        while let Some(id) = pending.pop_front() {
            anyhow::ensure!(!cancelled(), "control condition analysis canceled");
            if *remaining == 0 {
                return Ok(None);
            }
            *remaining -= 1;
            if !visited.insert(id) {
                continue;
            }
            if targets.contains(&id) {
                return Ok(Some(true));
            }
            pending.extend(
                self.successors
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .filter(|edge| Some(edge.id) != omitted)
                    .map(|edge| edge.target),
            );
        }
        Ok(Some(false))
    }

    /// Build a CfgGraph from node and edge slices. Validates that every edge
    /// endpoint exists in the node set, and that exactly one Entry and one Exit
    /// node are present.
    pub fn build(nodes: &[CfgNode], edges: &[CfgEdge]) -> anyhow::Result<Self> {
        let node_map: HashMap<CfgNodeId, CfgNode> =
            nodes.iter().map(|n| (n.id, n.clone())).collect();

        // Validate edge endpoints
        for e in edges {
            if !node_map.contains_key(&e.source) {
                anyhow::bail!("CfgGraph: edge source {:?} not found in nodes", e.source);
            }
            if !node_map.contains_key(&e.target) {
                anyhow::bail!("CfgGraph: edge target {:?} not found in nodes", e.target);
            }
        }

        // Build successor / predecessor maps
        let mut succ: HashMap<CfgNodeId, Vec<CfgEdge>> = HashMap::new();
        let mut pred: HashMap<CfgNodeId, Vec<CfgEdge>> = HashMap::new();
        for n in nodes {
            succ.entry(n.id).or_default();
            pred.entry(n.id).or_default();
        }
        for e in edges {
            succ.entry(e.source).or_default().push(e.clone());
            pred.entry(e.target).or_default().push(e.clone());
        }

        // Find Entry and Exit
        let entry = nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Entry)
            .ok_or_else(|| anyhow::anyhow!("CfgGraph: no Entry node found"))?;
        let exit = nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Exit)
            .ok_or_else(|| anyhow::anyhow!("CfgGraph: no Exit node found"))?;

        Ok(Self {
            nodes: node_map,
            successors: succ,
            predecessors: pred,
            entry: entry.id,
            exit: exit.id,
        })
    }

    /// Return all outgoing edges of a given kind from a node.
    pub fn successors_by_kind(&self, node_id: &CfgNodeId, kind: CfgEdgeKind) -> Vec<&CfgEdge> {
        self.successors
            .get(node_id)
            .map(|edges| edges.iter().filter(|e| e.kind == kind).collect())
            .unwrap_or_default()
    }

    /// Nodes reachable from the unique function Entry through any CFG edge.
    ///
    /// Extraction may retain disconnected syntax so a later direct goto can
    /// target a label after an abrupt statement. Consumers that enumerate
    /// nodes (rather than propagating from Entry) must use this set to avoid
    /// analyzing dead syntax as an executable branch.
    pub fn reachable_from_entry(&self) -> HashSet<CfgNodeId> {
        let mut reachable = HashSet::new();
        let mut pending = VecDeque::from([self.entry]);
        while let Some(node_id) = pending.pop_front() {
            if !reachable.insert(node_id) {
                continue;
            }
            pending.extend(
                self.successors
                    .get(&node_id)
                    .into_iter()
                    .flatten()
                    .map(|edge| edge.target),
            );
        }
        reachable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::TextRange;
    use types::cfg::CfgEdge;
    use types::enums::CfgEdgeKind;
    use types::ids::SymbolId;

    fn empty_range() -> TextRange {
        TextRange {
            start_byte: 0,
            end_byte: 0,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        }
    }

    fn test_symbol_id() -> SymbolId {
        SymbolId::generate(
            &types::ids::FileId::generate("test.c"),
            "c",
            "test_fn",
            "function",
            None,
        )
    }

    #[test]
    fn build_simple_graph() {
        let fid = test_symbol_id();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let nodes = vec![entry.clone(), exit.clone()];
        let edge = CfgEdge::new(&entry.id, &exit.id, CfgEdgeKind::Normal);
        let graph = CfgGraph::build(&nodes, std::slice::from_ref(&edge)).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.entry, entry.id);
        assert_eq!(graph.exit, exit.id);
        assert_eq!(graph.successors[&entry.id].len(), 1);
        assert_eq!(graph.predecessors[&exit.id].len(), 1);
    }

    #[test]
    fn required_branches_handle_shared_targets_budget_and_cancellation() {
        let fid = test_symbol_id();
        let entry = CfgNode::entry(&fid);
        let branch = CfgNode::new(&fid, CfgNodeKind::Branch, empty_range());
        let target = CfgNode::new(&fid, CfgNodeKind::Statement, empty_range());
        let exit = CfgNode::exit(&fid);
        let nodes = [entry.clone(), branch.clone(), target.clone(), exit.clone()];
        let edges = [
            CfgEdge::new(&entry.id, &branch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&branch.id, &target.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&branch.id, &target.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&target.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let graph = CfgGraph::build(&nodes, &edges).unwrap();
        let targets = HashSet::from([target.id]);
        let result = graph
            .required_branches(&targets, &HashSet::new(), 100, &|| false)
            .unwrap();
        assert!(result.target_reachable && result.edges.is_empty() && !result.truncated);
        assert!(
            graph
                .required_branches(&targets, &HashSet::new(), 1, &|| false)
                .unwrap()
                .truncated
        );
        assert!(
            graph
                .required_branches(&targets, &HashSet::new(), 100, &|| true)
                .is_err()
        );
    }

    #[test]
    fn unknown_control_preserves_common_guards_and_rejects_dependent_ones() {
        let fid = test_symbol_id();
        let node = |kind, start_byte| {
            CfgNode::new(
                &fid,
                kind,
                TextRange {
                    start_byte,
                    ..empty_range()
                },
            )
        };
        let entry = CfgNode::entry(&fid);
        let outer = node(CfgNodeKind::Branch, 1);
        let unknown = node(CfgNodeKind::Statement, 2);
        let inner = node(CfgNodeKind::Branch, 3);
        let target = node(CfgNodeKind::Statement, 4);
        let disconnected = node(CfgNodeKind::Statement, 5);
        let exit = CfgNode::exit(&fid);
        let outer_guard = CfgEdge::new(&outer.id, &unknown.id, CfgEdgeKind::TrueBranch);
        let inner_guard = CfgEdge::new(&inner.id, &target.id, CfgEdgeKind::TrueBranch);
        let edges = [
            CfgEdge::new(&entry.id, &outer.id, CfgEdgeKind::Normal),
            outer_guard.clone(),
            CfgEdge::new(&outer.id, &exit.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&unknown.id, &inner.id, CfgEdgeKind::Normal),
            inner_guard.clone(),
            CfgEdge::new(&inner.id, &exit.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&target.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let graph = CfgGraph::build(
            &[
                entry,
                outer,
                unknown.clone(),
                inner,
                target.clone(),
                disconnected.clone(),
                exit,
            ],
            &edges,
        )
        .unwrap();
        let targets = HashSet::from([target.id]);
        let recorded = graph
            .required_branches(&targets, &HashSet::from([disconnected.id]), 1000, &|| false)
            .unwrap();
        assert_eq!(recorded.edges, vec![outer_guard.clone(), inner_guard]);
        let partial = graph
            .required_branches(
                &targets,
                &HashSet::from([unknown.id, disconnected.id]),
                1000,
                &|| false,
            )
            .unwrap();
        assert_eq!(partial.edges, vec![outer_guard]);
        assert!(partial.target_reachable && !partial.truncated);
    }

    #[test]
    fn build_missing_entry_errors() {
        let fid = test_symbol_id();
        let stmt = CfgNode::new(&fid, CfgNodeKind::Statement, empty_range());
        assert!(CfgGraph::build(&[stmt], &[]).is_err());
    }

    #[test]
    fn build_dangling_edge_errors() {
        let fid = test_symbol_id();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let nodes = vec![entry.clone(), exit.clone()];
        let dangling = CfgNodeId::generate(&fid, "ghost", 0);
        let edge = CfgEdge::new(&entry.id, &dangling, CfgEdgeKind::Normal);
        assert!(CfgGraph::build(&nodes, &[edge]).is_err());
    }

    #[test]
    fn successors_by_kind_filters() {
        let fid = test_symbol_id();
        let entry = CfgNode::entry(&fid);
        let branch = CfgNode::new(&fid, CfgNodeKind::Branch, empty_range());
        let stmt = CfgNode::new(&fid, CfgNodeKind::Statement, empty_range());
        let exit = CfgNode::exit(&fid);
        let nodes = vec![entry.clone(), branch.clone(), stmt.clone(), exit.clone()];
        let edges = vec![
            CfgEdge::new(&entry.id, &branch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&branch.id, &stmt.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&branch.id, &exit.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&stmt.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let graph = CfgGraph::build(&nodes, &edges).unwrap();
        let true_edges = graph.successors_by_kind(&branch.id, CfgEdgeKind::TrueBranch);
        assert_eq!(true_edges.len(), 1);
        assert_eq!(true_edges[0].target, stmt.id);
    }

    #[test]
    fn reachable_from_entry_excludes_disconnected_syntax() {
        let fid = test_symbol_id();
        let entry = CfgNode::entry(&fid);
        let live = CfgNode::new(&fid, CfgNodeKind::Statement, empty_range());
        let mut dead = CfgNode::new(&fid, CfgNodeKind::Branch, empty_range());
        dead.id = CfgNodeId::generate(&fid, "dead", 1);
        let exit = CfgNode::exit(&fid);
        let nodes = vec![entry.clone(), live.clone(), dead.clone(), exit.clone()];
        let edges = vec![
            CfgEdge::new(&entry.id, &live.id, CfgEdgeKind::Normal),
            CfgEdge::new(&live.id, &exit.id, CfgEdgeKind::Normal),
            CfgEdge::new(&dead.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let graph = CfgGraph::build(&nodes, &edges).unwrap();
        let reachable = graph.reachable_from_entry();

        assert!(reachable.contains(&entry.id));
        assert!(reachable.contains(&live.id));
        assert!(reachable.contains(&exit.id));
        assert!(!reachable.contains(&dead.id));
    }
}
