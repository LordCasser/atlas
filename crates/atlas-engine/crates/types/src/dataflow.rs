//! Atlas dataflow types.
//!
//! ## Core concept
//!
//! Data nodes and dataflow edges form a **per-function dataflow graph** that
//! tracks how values move through a program: parameters → locals → field
//! accesses → call arguments → return values.
//!
//! Unlike the symbol graph (which tracks `symbol → symbol` structural
//! relationships), the dataflow graph tracks `DataNode → DataNode` value
//! flow.  DataNodeIds are **NOT** SymbolIds — this is a separate namespace.
//!
//! ## Key types
//!
//! - [`DataNode`] — a point where data exists (parameter, local, field, return, …)
//! - [`DataFlowEdge`] — a directed flow between two data nodes
//!
//! ## Relationship with other types
//!
//! - [`DataNode::binding_id`] → [`super::bindings::BindingDef`]
//! - [`DataNode::callsite_id`] → [`super::structs::Callsite`]
//!
//! ## Invariants
//!
//! - DataNode IDs are deterministic (blake3).
//! - DataFlowEdge IDs are deterministic (blake3(source + target + kind)).
//! - Source/Target of DataFlowEdge are always DataNodeId (NOT SymbolId).

use serde::{Deserialize, Serialize};

use super::enums::{DataFlowKind, DataNodeKind};
use super::ids::{BindingId, CallsiteId, DataFlowEdgeId, DataNodeId, FileId, SymbolId};
use super::structs::TextRange;

// ---------------------------------------------------------------------------
// DataNode — a point of data in the dataflow graph
// ---------------------------------------------------------------------------

/// A single data entity in the dataflow graph.
///
/// Every parameter, local variable, field access, literal, return value,
/// and call argument gets a `DataNode`.  Edges between nodes form the
/// per-function dataflow graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataNode {
    /// Deterministic identity.
    pub id: DataNodeId,

    /// Containing file.
    pub file_id: FileId,

    /// Enclosing function, if inside a function body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_id: Option<SymbolId>,

    /// What kind of data entity this is.
    pub kind: DataNodeKind,

    /// Lexical binding that this node represents, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<BindingId>,

    /// Call-site that this node is associated with, if any
    /// (e.g. call argument nodes, call return nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsite_id: Option<CallsiteId>,

    /// Human-readable name (e.g. "req", "name").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Access path for field chains (e.g. "req.body.name").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_path: Option<String>,

    /// Position within an invocation's argument list (0-based). Populated for
    /// `CallArg` nodes and, when syntax requires explicit mapping such as
    /// destructuring, for every `Parameter` leaf consuming that argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arg_index: Option<u32>,

    /// Source range of the data entity.
    pub range: TextRange,
}

impl DataNode {
    /// Convenience constructor for a parameter node.
    pub fn parameter(
        id: DataNodeId,
        file_id: FileId,
        function_id: Option<SymbolId>,
        binding_id: Option<BindingId>,
        name: &str,
        range: TextRange,
    ) -> Self {
        Self {
            id,
            file_id,
            function_id,
            kind: DataNodeKind::Parameter,
            binding_id,
            callsite_id: None,
            name: Some(name.to_string()),
            access_path: Some(name.to_string()),
            arg_index: None,
            range,
        }
    }

    /// Convenience constructor for a local variable node.
    pub fn local(
        id: DataNodeId,
        file_id: FileId,
        function_id: Option<SymbolId>,
        binding_id: Option<BindingId>,
        name: &str,
        range: TextRange,
    ) -> Self {
        Self {
            id,
            file_id,
            function_id,
            kind: DataNodeKind::Local,
            binding_id,
            callsite_id: None,
            name: Some(name.to_string()),
            access_path: Some(name.to_string()),
            arg_index: None,
            range,
        }
    }

    /// Convenience constructor for a field-access node.
    pub fn field(
        id: DataNodeId,
        file_id: FileId,
        function_id: Option<SymbolId>,
        name: &str,
        access_path: &str,
        range: TextRange,
    ) -> Self {
        Self {
            id,
            file_id,
            function_id,
            kind: DataNodeKind::Field,
            binding_id: None,
            callsite_id: None,
            name: Some(name.to_string()),
            access_path: Some(access_path.to_string()),
            arg_index: None,
            range,
        }
    }

    /// Convenience constructor for a return node.
    pub fn return_(
        id: DataNodeId,
        file_id: FileId,
        function_id: Option<SymbolId>,
        range: TextRange,
    ) -> Self {
        Self {
            id,
            file_id,
            function_id,
            kind: DataNodeKind::Return,
            binding_id: None,
            callsite_id: None,
            name: None,
            access_path: None,
            arg_index: None,
            range,
        }
    }

    /// Convenience constructor for a call-argument node.
    pub fn call_arg(
        id: DataNodeId,
        file_id: FileId,
        function_id: Option<SymbolId>,
        callsite_id: Option<CallsiteId>,
        name: Option<&str>,
        range: TextRange,
    ) -> Self {
        Self {
            id,
            file_id,
            function_id,
            kind: DataNodeKind::CallArg,
            binding_id: None,
            callsite_id,
            name: name.map(String::from),
            access_path: name.map(String::from),
            arg_index: None,
            range,
        }
    }

    /// Convenience constructor for a call-target (callee) node.
    pub fn call_target(
        id: DataNodeId,
        file_id: FileId,
        function_id: Option<SymbolId>,
        callsite_id: Option<CallsiteId>,
        name: &str,
        access_path: &str,
        range: TextRange,
    ) -> Self {
        Self {
            id,
            file_id,
            function_id,
            kind: DataNodeKind::CallTarget,
            binding_id: None,
            callsite_id,
            name: Some(name.to_string()),
            access_path: Some(access_path.to_string()),
            arg_index: None,
            range,
        }
    }
}

// ---------------------------------------------------------------------------
// DataFlowEdge — a data-flow relationship between two data nodes
// ---------------------------------------------------------------------------

/// A directed data flow between two [`DataNode`]s.
///
/// Source and target are **always** [`DataNodeId`], never [`SymbolId`].
/// This is the fundamental invariant that separates the dataflow graph
/// from the symbol-level graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataFlowEdge {
    /// Deterministic identity.
    pub id: DataFlowEdgeId,

    /// Source data node (upstream).
    pub source: DataNodeId,

    /// Target data node (downstream).
    pub target: DataNodeId,

    /// What kind of data flow this is.
    pub kind: DataFlowKind,

    /// Source location of the edge (e.g. the assignment or call).
    pub location: TextRange,

    /// Confidence in this dataflow edge (0.0–1.0).
    pub confidence: f64,
}

impl DataFlowEdge {
    /// Create a new dataflow edge.
    pub fn new(
        id: DataFlowEdgeId,
        source: DataNodeId,
        target: DataNodeId,
        kind: DataFlowKind,
        location: TextRange,
        confidence: f64,
    ) -> Self {
        Self {
            id,
            source,
            target,
            kind,
            location,
            confidence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::*;
    use crate::ids::{BindingId, FileId, ScopeId};

    fn make_file_id() -> FileId {
        FileId::generate("test.ts")
    }

    fn make_range(start: u32, end: u32) -> TextRange {
        TextRange {
            start_byte: start,
            end_byte: end,
            start_line: 1,
            start_column: start,
            end_line: 1,
            end_column: end,
        }
    }

    #[test]
    fn test_data_node_parameter_constructor() {
        let file_id = make_file_id();
        let func_id = SymbolId::generate(&file_id, "typescript", "handler", "function", None);
        let scope_id = ScopeId::generate(&file_id, None, "function", 10);
        let binding_id = BindingId::generate(&file_id, &scope_id, "parameter", "req", 42);
        let node_id = DataNodeId::generate(
            &file_id,
            Some(&func_id),
            "parameter",
            Some("req"),
            Some("req"),
            42,
        );
        let node = DataNode::parameter(
            node_id,
            file_id,
            Some(func_id),
            Some(binding_id),
            "req",
            make_range(42, 45),
        );
        assert_eq!(node.kind, DataNodeKind::Parameter);
        assert_eq!(node.name.as_deref(), Some("req"));
        assert_eq!(node.access_path.as_deref(), Some("req"));
    }

    #[test]
    fn test_data_node_local_constructor() {
        let file_id = make_file_id();
        let func_id = SymbolId::generate(&file_id, "typescript", "handler", "function", None);
        let scope_id = ScopeId::generate(&file_id, None, "function", 10);
        let binding_id = BindingId::generate(&file_id, &scope_id, "local", "name", 100);
        let node_id = DataNodeId::generate(
            &file_id,
            Some(&func_id),
            "local",
            Some("name"),
            Some("name"),
            100,
        );
        let node = DataNode::local(
            node_id,
            file_id,
            Some(func_id),
            Some(binding_id),
            "name",
            make_range(100, 104),
        );
        assert_eq!(node.kind, DataNodeKind::Local);
    }

    #[test]
    fn test_dataflow_edge_serialization_roundtrip() {
        let file_id = make_file_id();
        let func_id = SymbolId::generate(&file_id, "typescript", "handler", "function", None);
        let src =
            DataNodeId::generate(&file_id, Some(&func_id), "parameter", Some("req"), None, 42);
        let tgt = DataNodeId::generate(&file_id, Some(&func_id), "local", Some("name"), None, 100);
        let edge_id = DataFlowEdgeId::generate(&src, &tgt, "assign");
        let edge = DataFlowEdge {
            id: edge_id,
            source: src,
            target: tgt,
            kind: DataFlowKind::Assign,
            location: make_range(100, 130),
            confidence: 0.9,
        };
        let json = serde_json::to_string(&edge).unwrap();
        let parsed: DataFlowEdge = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.kind, DataFlowKind::Assign);
        assert_eq!(parsed.confidence, 0.9);
    }
}
