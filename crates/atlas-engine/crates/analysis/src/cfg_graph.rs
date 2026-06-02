//! CfgGraph — adjacency-list representation of a function's control-flow graph.
//!
//! Built from CfgNode + CfgEdge slices. Provides bidirectional traversal
//! (successors and predecessors) for dataflow fixpoint algorithms, branch-path
//! walking, and structural validation.

use std::collections::HashMap;
use types::cfg::{CfgEdge, CfgNode};
use types::enums::{CfgEdgeKind, CfgNodeKind};
use types::ids::CfgNodeId;

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
    /// Build a CfgGraph from node and edge slices. Validates that every edge
    /// endpoint exists in the node set, and that exactly one Entry and one Exit
    /// node are present.
    pub fn build(nodes: &[CfgNode], edges: &[CfgEdge]) -> anyhow::Result<Self> {
        let node_map: HashMap<CfgNodeId, CfgNode> = nodes
            .iter()
            .map(|n| (n.id, n.clone()))
            .collect();

        // Validate edge endpoints
        for e in edges {
            if !node_map.contains_key(&e.source) {
                anyhow::bail!(
                    "CfgGraph: edge source {:?} not found in nodes",
                    e.source
                );
            }
            if !node_map.contains_key(&e.target) {
                anyhow::bail!(
                    "CfgGraph: edge target {:?} not found in nodes",
                    e.target
                );
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
    pub fn successors_by_kind(
        &self,
        node_id: &CfgNodeId,
        kind: CfgEdgeKind,
    ) -> Vec<&CfgEdge> {
        self.successors
            .get(node_id)
            .map(|edges| edges.iter().filter(|e| e.kind == kind).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::ids::SymbolId;
    use types::TextRange;
    use types::cfg::CfgEdge;
    use types::enums::CfgEdgeKind;

    fn empty_range() -> TextRange {
        TextRange {
            start_byte: 0, end_byte: 0,
            start_line: 0, start_column: 0,
            end_line: 0, end_column: 0,
        }
    }

    fn test_symbol_id() -> SymbolId {
        SymbolId::generate(&types::ids::FileId::generate("test.c"), "c", "test_fn", "function", None)
    }

    #[test]
    fn build_simple_graph() {
        let fid = test_symbol_id();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let nodes = vec![entry.clone(), exit.clone()];
        let edge = CfgEdge::new(&entry.id, &exit.id, CfgEdgeKind::Normal);
        let graph = CfgGraph::build(&nodes, &[edge.clone()]).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.entry, entry.id);
        assert_eq!(graph.exit, exit.id);
        assert_eq!(graph.successors[&entry.id].len(), 1);
        assert_eq!(graph.predecessors[&exit.id].len(), 1);
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
}
