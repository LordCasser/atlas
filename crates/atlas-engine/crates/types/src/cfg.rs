//! CFG (Control-Flow Graph) — per-function control-flow node & edge types.
//!
//! # Architecture
//!
//! - `CfgNode`  represents a node in a function's control-flow graph.
//! - `CfgEdge`  represents a directed edge between two CFG nodes.
//! - All IDs are deterministic (blake3).
//! - CFG is per-function: each `CfgNode` belongs to exactly one `function_id`.
//!
//! # Invariants
//!
//! - `CfgNodeId` is unique within a function (deterministic from function_id + kind + byte).
//! - `CfgEdgeId` is deterministic from source + target + kind.
//! - Every `CfgNode` has a non-empty `stmt_range`.
//! - Every function has exactly one `Entry` and one `Exit` node.

use serde::{Deserialize, Serialize};

use super::enums::{CfgEdgeKind, CfgNodeKind, EffectKind};
use super::ids::{CfgEdgeId, CfgNodeId, SymbolId};
use super::structs::TextRange;

// ── CfgNode ──────────────────────────────────────────────────────────────────

/// A node in a per-function control-flow graph.
///
/// Each `CfgNode` belongs to exactly one function (identified by `function_id`).
/// Virtual `Entry` and `Exit` nodes have a zero-length range at position 0.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CfgNode {
    pub id: CfgNodeId,
    pub function_id: SymbolId,
    pub kind: CfgNodeKind,
    pub stmt_range: TextRange,
    /// What side effect this node has (C/C++ only initially).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_kind: Option<EffectKind>,
    /// Target field/expression path when relevant (e.g., "data->state.ptr").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_field: Option<String>,
    /// Callee function name for Call-effect nodes (e.g., "free", "Safefree").
    /// Populated by cfg_builder for call_expression nodes.
    /// Used by lifecycle analysis to match domain rules (free_fn/alloc_fn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callee_name: Option<String>,
}

impl CfgNode {
    /// Create a new CFG node.
    pub fn new(function_id: &SymbolId, kind: CfgNodeKind, range: TextRange) -> Self {
        let id = CfgNodeId::generate(function_id, kind.as_str(), range.start_byte);
        Self {
            id,
            function_id: function_id.clone(),
            kind,
            stmt_range: range,
            effect_kind: None,
            target_field: None,
            callee_name: None,
        }
    }

    /// Create a virtual Entry node (range = (0,0)).
    pub fn entry(function_id: &SymbolId) -> Self {
        let range = TextRange {
            start_byte: 0,
            end_byte: 0,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        Self::new(function_id, CfgNodeKind::Entry, range)
    }

    /// Create a virtual Exit node (range = (0,0)).
    pub fn exit(function_id: &SymbolId) -> Self {
        Self::entry(function_id).with_kind(CfgNodeKind::Exit)
    }

    /// Create a node with a different kind but same function/range → new ID.
    fn with_kind(mut self, kind: CfgNodeKind) -> Self {
        self.kind = kind;
        self.id = CfgNodeId::generate(&self.function_id, kind.as_str(), self.stmt_range.start_byte);
        self
    }
}

// ── CfgEdge ──────────────────────────────────────────────────────────────────

/// A directed edge between two CFG nodes.
///
/// Edges represent control flow: sequential flow (`Normal`), conditional
/// branching (`TrueBranch`/`FalseBranch`), loop back edges (`LoopBack`),
/// and exception flow (`Exception`).

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CfgEdge {
    pub id: CfgEdgeId,
    pub source: CfgNodeId,
    pub target: CfgNodeId,
    pub kind: CfgEdgeKind,
}

impl CfgEdge {
    /// Create a new CFG edge.
    pub fn new(source: &CfgNodeId, target: &CfgNodeId, kind: CfgEdgeKind) -> Self {
        let id = CfgEdgeId::generate(source, target, kind.as_str());
        Self {
            id,
            source: source.clone(),
            target: target.clone(),
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::FileId;

    fn sample_func_id() -> SymbolId {
        let file_id = FileId::generate("src/main.ts");
        SymbolId::generate(&file_id, "typescript", "handler", "function", None)
    }

    #[test]
    fn test_cfg_node_serde_roundtrip() {
        let func_id = sample_func_id();
        let range = TextRange {
            start_byte: 100,
            end_byte: 120,
            start_line: 5,
            start_column: 0,
            end_line: 5,
            end_column: 20,
        };
        let mut node = CfgNode::new(&func_id, CfgNodeKind::Statement, range);
        node.effect_kind = Some(EffectKind::Assign);
        node.target_field = Some("data->state.ptr".to_string());
        let json = serde_json::to_string(&node).unwrap();
        let parsed: CfgNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn test_cfg_edge_serde_roundtrip() {
        let func_id = sample_func_id();
        let range = TextRange {
            start_byte: 100,
            end_byte: 120,
            start_line: 5,
            start_column: 0,
            end_line: 5,
            end_column: 20,
        };
        let src = CfgNode::new(&func_id, CfgNodeKind::Entry, range);
        let range2 = TextRange {
            start_byte: 130,
            end_byte: 150,
            start_line: 6,
            start_column: 0,
            end_line: 6,
            end_column: 20,
        };
        let tgt = CfgNode::new(&func_id, CfgNodeKind::Statement, range2);
        let edge = CfgEdge::new(&src.id, &tgt.id, CfgEdgeKind::Normal);
        let json = serde_json::to_string(&edge).unwrap();
        let parsed: CfgEdge = serde_json::from_str(&json).unwrap();
        assert_eq!(edge, parsed);
    }
}
