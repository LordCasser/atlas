//! Taint engine: forward dataflow propagation for source-to-sink analysis.
//!
//! # Architecture
//!
//! The taint engine performs forward taint propagation:
//! 1. **Identify sources** — match DataNodes against source rules
//! 2. **Propagate forward** — follow DataFlowEdges from sources, tracking taint
//! 3. **Detect sinks** — when taint reaches a DataNode matching a sink rule, create a TaintFinding
//! 4. **Apply sanitizers** — stop propagation when taint passes through a sanitizer node
//!
//! # Algorithm
//!
//! Worklist-based forward propagation:
//! - Seed the worklist with source DataNodes
//! - For each node, find outgoing DataFlowEdges
//! - If target matches a sink → record finding
//! - If target matches a sanitizer → stop propagation from that path
//! - Otherwise → add target to worklist
//! - Track visited nodes (limited by file scope, max depth 20)

use crate::types::dataflow::{DataFlowEdge, DataNode};
use crate::types::taint::{TaintFinding, TaintRule, TaintRuleKind};
use crate::types::ids::DataNodeId;
use crate::types::TaintFindingId;
use std::collections::{HashMap, HashSet};

/// Result of running the taint engine over a set of files.
#[derive(Debug, Default)]
pub struct TaintEngineResult {
    /// Taint findings discovered during propagation.
    pub findings: Vec<TaintFinding>,
    /// Number of source nodes matched.
    pub sources_matched: usize,
    /// Number of sink nodes matched (distinct).
    pub sinks_matched: usize,
    /// Number of paths explored.
    pub paths_explored: usize,
}

// ── TaintEngine ─────────────────────────────────────────────────────────────

/// Forward taint propagation engine.
///
/// Takes dataflow graphs (DataNodes + DataFlowEdges), matches against
/// taint rules, and produces TaintFindings for source→sink flows.
pub struct TaintEngine {
    rules: Vec<TaintRule>,
    max_depth: usize,
}

impl TaintEngine {
    /// Create a new taint engine with the given rules.
    pub fn new(rules: Vec<TaintRule>) -> Self {
        Self {
            rules,
            max_depth: 20,
        }
    }

    /// Run taint analysis over nodes and edges from one or more files.
    ///
    /// Returns detected findings and summary statistics.
    pub fn analyze(
        &self,
        nodes: &[DataNode],
        edges: &[DataFlowEdge],
    ) -> TaintEngineResult {
        let source_rules: Vec<(usize, &TaintRule)> = self.rules.iter().enumerate()
            .filter(|(_, r)| r.kind == TaintRuleKind::Source)
            .map(|(i, r)| (i, r))
            .collect();
        let sink_rules: Vec<&TaintRule> = self.rules.iter()
            .filter(|r| r.kind == TaintRuleKind::Sink)
            .collect();
        let sanitizer_rules: Vec<&TaintRule> = self.rules.iter()
            .filter(|r| r.kind == TaintRuleKind::Sanitizer)
            .collect();

        // Build adjacency: node_id → outgoing edge targets
        let mut adj: HashMap<crate::types::ids::DataNodeId, Vec<&DataFlowEdge>> = HashMap::new();
        for edge in edges {
            adj.entry(edge.source).or_default().push(edge);
        }

        // Node lookup by ID
        let node_by_id: HashMap<crate::types::ids::DataNodeId, &DataNode> = nodes.iter()
            .map(|n| (n.id, n))
            .collect();

        // 1. Identify sources
        let mut sources_matched = 0;
        // Worklist entries: (current_node_id, source_node_id, source_rule_idx)
        let mut worklist: Vec<(DataNodeId, DataNodeId, usize)> = Vec::new();
        // source_node_for tracks which source a tainted node came from
        let mut source_node_for: HashMap<DataNodeId, DataNodeId> = HashMap::new();
        // source_rule_for tracks which rule matched the source
        let mut source_rule_for: HashMap<DataNodeId, usize> = HashMap::new();

        for node in nodes {
            for (rule_idx, rule) in &source_rules {
                if match_node_against_rule(node, rule) {
                    worklist.push((node.id, node.id, *rule_idx));
                    source_node_for.insert(node.id, node.id);
                    source_rule_for.insert(node.id, *rule_idx);
                    sources_matched += 1;
                    break; // First matching rule wins
                }
            }
        }

        // 2. Forward propagation
        let mut visited: HashSet<DataNodeId> = HashSet::new();
        let mut findings: Vec<TaintFinding> = Vec::new();
        let mut paths_explored = 0;
        let mut sinks_matched = 0;
        let mut depth_at: HashMap<DataNodeId, usize> = HashMap::new();

        let mut work_idx = 0;
        while work_idx < worklist.len() {
            let (node_id, source_node_id, source_rule_idx) = worklist[work_idx];
            let source_rule = &self.rules[source_rule_idx];
            work_idx += 1;

            if !visited.insert(node_id) {
                continue;
            }

            let current_depth = depth_at.get(&node_id).copied().unwrap_or(0);
            if current_depth >= self.max_depth {
                continue;
            }

            paths_explored += 1;

            // Check if this node matches a sink rule
            if let Some(node) = node_by_id.get(&node_id) {
                for sink_rule in &sink_rules {
                    if match_node_against_rule(node, sink_rule) {
                        let finding_id = TaintFindingId::generate(
                            &sink_rule.id,
                            &source_node_id,
                            &node_id,
                            &node.file_id,
                        );
                        let severity = std::cmp::max(
                            source_rule.severity.clone(),
                            sink_rule.severity.clone(),
                        );
                    findings.push(TaintFinding {
                        id: finding_id,
                        source_node: source_node_id,
                        sink_node: node_id,
                        rule_id: sink_rule.id.clone(),
                        severity,
                        confidence: crate::types::Confidence::new(0.8),
                        file_id: node.file_id,
                    });
                        sinks_matched += 1;
                        break;
                    }
                }

                // Check if this node is a sanitizer — stop propagation from it
                let is_sanitized = sanitizer_rules.iter()
                    .any(|r| match_node_against_rule(node, r));
                if is_sanitized {
                    continue;
                }
            }

            // Propagate to outgoing targets
            if let Some(outgoing) = adj.get(&node_id) {
                for edge in outgoing {
                    let next_depth = current_depth + 1;
                    depth_at.insert(edge.target, next_depth);
                    // Carry source node and rule forward
                    if !source_node_for.contains_key(&edge.target) {
                        source_node_for.insert(edge.target, source_node_id);
                        source_rule_for.insert(edge.target, source_rule_idx);
                    }
                    worklist.push((edge.target, source_node_id, source_rule_idx));
                }
            }
        }

        TaintEngineResult {
            findings,
            sources_matched,
            sinks_matched,
            paths_explored,
        }
    }
}

// ── Rule Matching ───────────────────────────────────────────────────────────

/// Check if a DataNode matches a taint rule.
///
/// Matching logic:
/// - `symbol_pattern`: the node's name contains the pattern (case-insensitive)
/// - `callee`: if set, the node's access_path must contain the callee pattern
/// - `access_path_pattern`: if set, the node's access_path must contain the pattern
/// - No file_id/language filtering (caller is responsible for language-scoped rules)
fn match_node_against_rule(node: &DataNode, rule: &TaintRule) -> bool {
    // symbol_pattern matching
    if let Some(ref pattern) = rule.symbol_pattern {
        if let Some(ref name) = node.name {
            if !name.to_lowercase().contains(&pattern.to_lowercase()) {
                return false;
            }
        } else {
            return false;
        }
    }

    // callee matching (legacy: checks access_path contains callee)
    if let Some(ref callee) = rule.callee {
        if let Some(ref access_path) = node.access_path {
            if !access_path.to_lowercase().contains(&callee.to_lowercase()) {
                return false;
            }
        } else {
            return false;
        }
    }

    // access_path_pattern matching (checks access_path contains pattern)
    if let Some(ref pattern) = rule.access_path_pattern {
        if let Some(ref access_path) = node.access_path {
            if !access_path.to_lowercase().contains(&pattern.to_lowercase()) {
                return false;
            }
        } else {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::FileId;
    use crate::types::structs::TextRange;
    use crate::types::enums::{DataNodeKind, Language};
    use crate::types::taint::Severity;

    fn make_data_node(
        file_id: FileId,
        name: &str,
        kind: DataNodeKind,
        access_path: Option<&str>,
    ) -> DataNode {
        let range = TextRange::default();
        DataNode {
            id: crate::types::ids::DataNodeId::generate(
                &file_id, None, &format!("{}", kind.as_str()), Some(name), access_path, 0,
            ),
            file_id,
            function_id: None,
            kind,
            binding_id: None,
            callsite_id: None,
            name: Some(name.to_string()),
            access_path: access_path.map(|s| s.to_string()),
            range,
        }
    }

    #[test]
    fn test_engine_detects_source_to_sink() {
        let file_id = FileId::generate("test.ts");
        let src = make_data_node(file_id, "query", DataNodeKind::Parameter, Some("req.query"));
        let sink = make_data_node(file_id, "exec", DataNodeKind::CallArg, Some("child_process.exec"));

        let nodes = vec![src.clone(), sink.clone()];

        // Edge: src → sink (data flows from query to exec)
        let edge = DataFlowEdge {
            id: crate::types::ids::DataFlowEdgeId::generate(&src.id, &sink.id, "assign"),
            source: src.id,
            target: sink.id,
            kind: crate::types::enums::DataFlowKind::Assign,
            location: TextRange::default(),
            confidence: 1.0,
        };
        let edges = vec![edge];

        let rules = vec![
            TaintRule {
                id: "ts.req.query".into(),
                language: Some(Language::TypeScript),
                kind: TaintRuleKind::Source,
                callee: Some("req".into()),
                symbol_pattern: Some("query".into()),
                access_path_pattern: None,
                argument_index: None,
                applies_to_return: false,
                severity: Severity::High,
            },
            TaintRule {
                id: "ts.exec".into(),
                language: Some(Language::TypeScript),
                kind: TaintRuleKind::Sink,
                callee: Some("child_process".into()),
                symbol_pattern: Some("exec".into()),
                access_path_pattern: None,
                argument_index: None,
                applies_to_return: false,
                severity: Severity::Critical,
            },
        ];

        let engine = TaintEngine::new(rules);
        let result = engine.analyze(&nodes, &edges);

        assert!(!result.findings.is_empty(), "Should detect source→sink");
        assert_eq!(result.sources_matched, 1);
        assert!(result.sinks_matched >= 1);
    }

    #[test]
    fn test_sanitizer_blocks_taint() {
        let file_id = FileId::generate("test.ts");
        let src = make_data_node(file_id, "query", DataNodeKind::Parameter, Some("req.query"));
        let sanitizer = make_data_node(file_id, "sanitize", DataNodeKind::Expr, Some("sanitize"));
        let sink = make_data_node(file_id, "innerHTML", DataNodeKind::Field, Some("element.innerHTML"));

        let nodes = vec![src.clone(), sanitizer.clone(), sink.clone()];

        let edge1 = DataFlowEdge {
            id: crate::types::ids::DataFlowEdgeId::generate(&src.id, &sanitizer.id, "assign"),
            source: src.id,
            target: sanitizer.id,
            kind: crate::types::enums::DataFlowKind::Assign,
            location: TextRange::default(),
            confidence: 1.0,
        };
        let edge2 = DataFlowEdge {
            id: crate::types::ids::DataFlowEdgeId::generate(&sanitizer.id, &sink.id, "assign"),
            source: sanitizer.id,
            target: sink.id,
            kind: crate::types::enums::DataFlowKind::Assign,
            location: TextRange::default(),
            confidence: 1.0,
        };
        let edges = vec![edge1, edge2];

        let rules = vec![
            TaintRule {
                id: "ts.req.query".into(),
                language: Some(Language::TypeScript),
                kind: TaintRuleKind::Source,
                callee: Some("req".into()),
                symbol_pattern: Some("query".into()),
                access_path_pattern: None,
                argument_index: None,
                applies_to_return: false,
                severity: Severity::High,
            },
            TaintRule {
                id: "ts.sanitize".into(),
                language: Some(Language::TypeScript),
                kind: TaintRuleKind::Sanitizer,
                callee: None,
                symbol_pattern: Some("sanitize".into()),
                access_path_pattern: None,
                argument_index: None,
                applies_to_return: false,
                severity: Severity::Info,
            },
            TaintRule {
                id: "ts.innerHTML".into(),
                language: Some(Language::TypeScript),
                kind: TaintRuleKind::Sink,
                callee: None,
                symbol_pattern: Some("innerHTML".into()),
                access_path_pattern: None,
                argument_index: None,
                applies_to_return: false,
                severity: Severity::High,
            },
        ];

        let engine = TaintEngine::new(rules);
        let result = engine.analyze(&nodes, &edges);

        // Taint should NOT reach the sink because sanitizer blocks it
        assert!(
            result.findings.is_empty(),
            "Sanitizer should block taint propagation"
        );
    }
}
