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
use super::trace::Evidence;

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
        }
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::{FileId, SymbolId};

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
