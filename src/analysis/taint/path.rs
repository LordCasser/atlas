//! Taint path tracer: reverse BFS from sink back to source.
//!
//! # Architecture
//!
//! For each taint finding, the path tracer performs reverse BFS from the sink
//! DataNode back to the source DataNode, then reconstructs the forward path
//! as ordered TaintPathSteps.
//!
//! # Algorithm
//!
//! 1. Reverse BFS from sink, storing predecessor links
//! 2. When source is reached (or max depth exceeded), reconstruct forward path
//! 3. Build ordered TaintPathStep list: source → ... → sink

use crate::types::dataflow::{DataFlowEdge, DataNode};
use crate::types::taint::{TaintFinding, TaintPathStep, TaintFindingId};
use crate::types::ids::DataNodeId;
use std::collections::{HashMap, HashSet, VecDeque};

/// Result of tracing a taint path.
#[derive(Debug, Clone)]
pub struct PathTraceResult {
    /// The finding this path belongs to.
    pub finding_id: TaintFindingId,
    /// Ordered steps from source to sink.
    pub steps: Vec<TaintPathStep>,
    /// Whether the path reaches the source.
    pub complete: bool,
}

// ── TaintPathTracer ─────────────────────────────────────────────────────────

/// Reverse BFS path tracer for taint findings.
pub struct TaintPathTracer {
    max_depth: usize,
}

impl TaintPathTracer {
    pub fn new() -> Self {
        Self { max_depth: 20 }
    }

    pub fn with_max_depth(max_depth: usize) -> Self {
        Self { max_depth }
    }

    /// Trace a single finding back to its source.
    pub fn trace_one(
        &self,
        finding: &TaintFinding,
        _nodes: &[DataNode],
        edges: &[DataFlowEdge],
    ) -> PathTraceResult {
        // Build reverse adjacency: target → sources
        let mut rev_adj: HashMap<DataNodeId, Vec<&DataFlowEdge>> = HashMap::new();
        for edge in edges {
            rev_adj.entry(edge.target).or_default().push(edge);
        }

        // Reverse BFS from sink
        let mut queue: VecDeque<DataNodeId> = VecDeque::new();
        // predecessor[current] = (previous_node, edge) — reversed direction
        let mut predecessor: HashMap<DataNodeId, (DataNodeId, &DataFlowEdge)> = HashMap::new();
        let mut visited: HashSet<DataNodeId> = HashSet::new();
        let mut depth: HashMap<DataNodeId, usize> = HashMap::new();

        queue.push_back(finding.sink_node);
        visited.insert(finding.sink_node);
        depth.insert(finding.sink_node, 0);

        let mut reached_source = false;

        while let Some(node_id) = queue.pop_front() {
            let d = depth[&node_id];
            if d >= self.max_depth {
                continue;
            }
            if node_id == finding.source_node {
                reached_source = true;
                break;
            }
            if let Some(incoming) = rev_adj.get(&node_id) {
                for edge in incoming {
                    if !visited.contains(&edge.source) {
                        visited.insert(edge.source);
                        depth.insert(edge.source, d + 1);
                        // In reverse BFS, edge goes source→target. So predecessor[edge.source] = (node_id, edge)
                        predecessor.insert(edge.source, (node_id, *edge));
                        queue.push_back(edge.source);
                    }
                }
            }
        }

        // Reconstruct forward path: source → ... → sink
        let mut steps = Vec::new();
        let mut current = finding.source_node;
        let mut step_idx: u32 = 0;

        // Add source node
        steps.push(TaintPathStep {
            finding_id: finding.id,
            index: step_idx,
            data_node: current,
            edge_id: None,
            file_id: finding.file_id,
            range: Default::default(),
            message: "source".into(),
        });
        step_idx += 1;

        // Walk from source toward sink using predecessor links
        while current != finding.sink_node {
            if let Some((next, edge)) = predecessor.get(&current) {
                current = *next;
                steps.push(TaintPathStep {
                    finding_id: finding.id,
                    index: step_idx,
                    data_node: current,
                    edge_id: Some(edge.id),
                    file_id: finding.file_id,
                    range: edge.location,
                    message: format!("flow via {}", edge.kind.as_str()),
                });
                step_idx += 1;
            } else {
                break; // Path broken
            }
        }

        PathTraceResult {
            finding_id: finding.id,
            steps,
            complete: reached_source,
        }
    }

    /// Trace multiple findings, returning per-finding path results.
    pub fn trace_all(
        &self,
        findings: &[TaintFinding],
        nodes: &[DataNode],
        edges: &[DataFlowEdge],
    ) -> Vec<PathTraceResult> {
        findings.iter()
            .map(|f| self.trace_one(f, nodes, edges))
            .collect()
    }
}

impl Default for TaintPathTracer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::FileId;
    use crate::types::taint::TaintFindingId;
    use crate::types::structs::TextRange;
    use crate::types::enums::DataNodeKind;
    use crate::types::Confidence;

    fn make_node(file_id: FileId, name: &str, kind: DataNodeKind) -> DataNode {
        DataNode {
            id: DataNodeId::generate(
                &file_id, None, name, Some(name), None, 0,
            ),
            file_id,
            function_id: None,
            kind,
            binding_id: None,
            callsite_id: None,
            name: Some(name.to_string()),
            access_path: None,
            range: TextRange::default(),
        }
    }

    fn make_edge(source: DataNodeId, target: DataNodeId) -> DataFlowEdge {
        DataFlowEdge {
            id: crate::types::ids::DataFlowEdgeId::generate(&source, &target, "assign"),
            source,
            target,
            kind: crate::types::enums::DataFlowKind::Assign,
            location: TextRange::default(),
            confidence: 1.0,
        }
    }

    #[test]
    fn test_trace_simple_path() {
        let file_id = FileId::generate("test.ts");
        let src = make_node(file_id, "query", DataNodeKind::Parameter);
        let mid = make_node(file_id, "x", DataNodeKind::Local);
        let sink = make_node(file_id, "exec", DataNodeKind::CallArg);

        let nodes = vec![src.clone(), mid.clone(), sink.clone()];
        let edges = vec![
            make_edge(src.id, mid.id),
            make_edge(mid.id, sink.id),
        ];

        let finding = TaintFinding {
            id: TaintFindingId::generate("r1", &src.id, &sink.id, &file_id),
            source_node: src.id,
            sink_node: sink.id,
            rule_id: "ts.exec".into(),
            severity: crate::types::taint::Severity::Critical,
            confidence: Confidence::new(0.8),
            file_id,
        };

        let tracer = TaintPathTracer::new();
        let result = tracer.trace_one(&finding, &nodes, &edges);

        assert!(result.steps.len() >= 2, "Path should have at least source and sink");
        assert_eq!(result.steps[0].data_node, src.id);
        assert_eq!(result.steps.last().unwrap().data_node, sink.id);
    }
}
