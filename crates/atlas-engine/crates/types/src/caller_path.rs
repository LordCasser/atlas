//! Atlas caller-path types — reverse call-graph traversal.
//!
//! ## Core concept
//!
//! Caller path tracing answers "who calls this function?" by walking backward
//! through `Calls` / `Instantiates` / `Implements` symbol edges.  Unlike
//! dataflow slicing (which follows value flow), caller paths follow control
//! flow through the call graph.
//!
//! ## Current semantics
//!
//! The explorer returns the **single farthest** caller chain (BFS from target
//! to root).  This is NOT an exhaustive enumeration of all possible call paths
//! — it finds one path to the most distant known entry-point.  Future versions
//! may support multi-path enumeration or top-N search.
//!
//! ## Key types
//!
//! - [`CallerChain`] — a reverse call-graph path from an entry-point down to a
//!   target function.
//! - [`CallerChainStep`] — a single step: caller symbol → callee symbol via a
//!   call edge.

use serde::{Deserialize, Serialize};

use super::enums::EdgeKind;
use super::ids::{FileId, SymbolId};
use super::structs::{Callsite, SymbolDef, TextRange};
use super::trace::{BoundaryMarker, Evidence};

// ---------------------------------------------------------------------------
// CallerChain — reverse call-graph path
// ---------------------------------------------------------------------------

/// A single chain of callers leading from an entry-point function down to a
/// target.
///
/// **Note:** This is the single farthest chain found by BFS, NOT an exhaustive
/// enumeration of all call paths.  For a given target, there may be multiple
/// entry-points that reach it; this type represents only one path to the most
/// distant caller found.
///
/// The chain is ordered from the **root** (farthest caller found, often an
/// entry-point or exported function) to the **target** (the function the user
/// asked about).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerChain {
    /// The farthest caller in the chain (entry-point or exported function).
    pub root: SymbolDef,
    /// Steps from root to target, in order.
    pub steps: Vec<CallerChainStep>,
    /// The target function (the one the user asked about).
    pub target: SymbolDef,
    /// Total number of nodes visited during the search.
    pub nodes_visited: usize,
    /// Depth of the root from the target (number of call edges traversed).
    pub max_depth_reached: usize,
    /// Whether the traversal was truncated by the max_depth budget.
    pub truncated: bool,
}

/// A single step in a caller chain: `caller` calls `callee`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerChainStep {
    /// Step index (0-based, from root to target).
    pub index: u32,
    /// The calling function.
    pub caller: SymbolId,
    /// The called function.
    pub callee: SymbolId,
    /// The edge kind that created this step (`Calls`, `Instantiates`, or `Implements`).
    pub edge_kind: EdgeKind,
    /// The callsite where the call occurs (if known).
    pub callsite: Option<Callsite>,
    /// File where the call happens.
    pub file_id: FileId,
    /// Source range of the call.
    pub range: Option<TextRange>,
    /// Human-readable description.
    pub description: String,
    /// Human-readable evidence (file path, symbol name) for agent consumption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// Dynamic dispatch boundary marker for this step (if it hits a
    /// callback registration, function pointer, or similar boundary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<BoundaryMarker>,
    /// Source code snippet at the call site (the line where `callee` is invoked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_snippet: Option<String>,
    /// Source code snippet of the callee definition (first line / signature).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callee_snippet: Option<String>,
}

// ---------------------------------------------------------------------------
// Document-stable type alias (for agent contract compatibility)
// ---------------------------------------------------------------------------

/// Document-stable alias for [`CallerChain`], used in MCP tool schemas and
/// JSON external contracts.
#[allow(non_camel_case_types)]
pub type CallerPath = CallerChain;

// ---------------------------------------------------------------------------
// constructors
// ---------------------------------------------------------------------------

impl CallerChainStep {
    pub fn new(
        index: u32,
        caller: SymbolId,
        callee: SymbolId,
        edge_kind: EdgeKind,
        file_id: FileId,
        range: Option<TextRange>,
        description: &str,
    ) -> Self {
        Self {
            index,
            caller,
            callee,
            edge_kind,
            callsite: None,
            file_id,
            range,
            description: description.to_string(),
            evidence: None,
            boundary: None,
            caller_snippet: None,
            callee_snippet: None,
        }
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{FileId, SymbolId};

    #[test]
    fn caller_chain_step_creation() {
        let file_id = FileId::generate("test.ts");
        let caller = SymbolId::generate(&file_id, "ts", "main", "Function", None);
        let callee = SymbolId::generate(&file_id, "ts", "helper", "Function", None);

        let step = CallerChainStep::new(
            0,
            caller.clone(),
            callee.clone(),
            EdgeKind::Calls,
            file_id,
            None,
            "main → helper",
        );
        assert_eq!(step.index, 0);
        assert_eq!(step.caller, caller);
        assert_eq!(step.callee, callee);
        assert_eq!(step.edge_kind, EdgeKind::Calls);
        assert_eq!(step.description, "main → helper");
    }
}

// ───────────────────────────────────────────────────────────────────────────
// ForwardChain — forward call-graph path (from source to target)
// ───────────────────────────────────────────────────────────────────────────

/// A forward call-graph trace from a source function to a target function.
///
/// This is the forward-direction counterpart of [`CallerChain`](super::CallerChain).
/// It answers "how does A reach B?" by walking forward through `Calls`,
/// `Instantiates`, `Implements`, and `RegistersCallback` edges.
///
/// The chain is ordered from **source** (the starting function) to **target**
/// (the destination function the user asked about).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardChain {
    /// The source function (starting point).
    pub source: SymbolDef,
    /// Steps from source to target, in order.
    pub steps: Vec<ForwardChainStep>,
    /// The target function (destination).
    pub target: SymbolDef,
    /// Total number of nodes visited during the search.
    pub nodes_visited: usize,
    /// Depth of the target from the source (number of call edges traversed).
    pub max_depth_reached: usize,
    /// Whether the traversal was truncated by the max_depth budget.
    pub truncated: bool,
}

/// A single step in a forward chain: `caller` calls `callee`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardChainStep {
    /// Step index (0-based, from source to target).
    pub index: u32,
    /// The calling function.
    pub caller: SymbolId,
    /// The called function.
    pub callee: SymbolId,
    /// The edge kind: `Calls`, `Instantiates`, `Implements`, or `RegistersCallback`.
    pub edge_kind: EdgeKind,
    /// The callsite where the call occurs (if known).
    pub callsite: Option<Callsite>,
    /// File where the call happens.
    pub file_id: FileId,
    /// Source range of the call.
    pub range: Option<TextRange>,
    /// Human-readable description.
    pub description: String,
    /// Human-readable evidence (file path, symbol name) for agent consumption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// Dynamic dispatch boundary marker (callback registration, function pointer, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<BoundaryMarker>,
    /// Source code snippet at the call site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_snippet: Option<String>,
    /// Source code snippet of the callee definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callee_snippet: Option<String>,
}

impl ForwardChainStep {
    pub fn new(
        index: u32,
        caller: SymbolId,
        callee: SymbolId,
        edge_kind: EdgeKind,
        file_id: FileId,
        range: Option<TextRange>,
        description: &str,
    ) -> Self {
        Self {
            index,
            caller,
            callee,
            edge_kind,
            callsite: None,
            file_id,
            range,
            description: description.to_string(),
            evidence: None,
            boundary: None,
            caller_snippet: None,
            callee_snippet: None,
        }
    }
}
