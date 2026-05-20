//! Reverse call-graph traversal — trace how a function gets invoked.
//!
//! The caller path explorer walks backward through `Calls`, `Instantiates`,
//! and `Implements` symbol edges to reconstruct the chain of callers from an
//! entry-point down to a target function.
//!
//! # Algorithm
//!
//! 1. Start from the target symbol.
//! 2. BFS backward: for each symbol, find all incoming caller edges
//!    (`find_edges_by_target`).
//! 3. Keep track of the farthest caller (the one with the longest path from
//!    the target).
//! 4. Reconstruct the path from the farthest caller to the target.
//!
//! # Limitations
//!
//! - Recursive calls create cycles; a visited set prevents infinite loops.
//! - Only direct `Calls`/`Instantiates`/`Implements` edges are followed.
//!   Transitive dependencies (e.g. via `References`) are not included.

use std::collections::{HashMap, VecDeque};

use crate::db::Store;
use crate::types::caller_path::{CallerChain, CallerChainStep};
use crate::types::enums::EdgeKind;
use crate::types::ids::SymbolId;

/// Default maximum depth for caller-chain traversal.
#[allow(dead_code)]
pub const DEFAULT_MAX_DEPTH: usize = 20;

/// Explores reverse call chains from a target symbol.
pub struct CallerPathExplorer;

impl CallerPathExplorer {
    /// Build a [`CallerChain`] from the target function backward to its
    /// farthest caller.
    ///
    /// Returns `Ok(None)` if the target has no callers (it is a root/top-level
    /// function).
    ///
    /// # Arguments
    ///
    /// * `store` — the Atlas database.
    /// * `target_id` — the function to trace callers for.
    /// * `max_depth` — maximum number of backward steps (default: [`DEFAULT_MAX_DEPTH`]).
    pub fn explore(
        store: &Store,
        target_id: &SymbolId,
        max_depth: usize,
    ) -> anyhow::Result<Option<CallerChain>> {
        let target = match store.find_symbol_by_id(target_id)? {
            Some(s) => s,
            None => return Ok(None),
        };

        // BFS backward: node_id → (predecessor, edge_kind)
        let mut predecessors: HashMap<String, (SymbolId, EdgeKind)> = HashMap::new();
        let mut visited: HashMap<String, usize> = HashMap::new(); // node_id hex → depth
        let mut queue: VecDeque<(SymbolId, usize)> = VecDeque::new();

        let root_key = hex::encode(target_id.as_bytes());
        visited.insert(root_key.clone(), 0);
        queue.push_back((target_id.clone(), 0));

        let mut farthest_id = target_id.clone();
        let mut farthest_depth: usize = 0;

        while let Some((current_id, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            let edges = store.find_edges_by_target(&current_id)?;
            for edge in &edges {
                // Only follow call-related edges
                if !is_call_edge(&edge.kind) {
                    continue;
                }

                let caller = &edge.source;
                let caller_key = hex::encode(caller.as_bytes());

                if !visited.contains_key(&caller_key) {
                    let new_depth = depth + 1;
                    let current_key = hex::encode(current_id.as_bytes());
                    visited.insert(caller_key.clone(), new_depth);
                    // Store current→caller so reconstruct_call_path can walk
                    // backward from target through each caller.
                    predecessors.insert(current_key, (caller.clone(), edge.kind.clone()));
                    queue.push_back((caller.clone(), new_depth));

                    if new_depth > farthest_depth {
                        farthest_depth = new_depth;
                        farthest_id = caller.clone();
                    }
                }
            }
        }

        if farthest_id == *target_id {
            // No callers found — this is already a root
            return Ok(None);
        }

        let root = store
            .find_symbol_by_id(&farthest_id)?
            .unwrap_or_else(|| target.clone());

        // Reconstruct path from root to target
        let steps = reconstruct_call_path(
            &predecessors,
            &root.id,
            target_id,
            store,
        )?;

        Ok(Some(CallerChain {
            root,
            steps,
            target,
            nodes_visited: visited.len(),
            max_depth_reached: farthest_depth,
        }))
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Only follow structural edges that indicate a call/invoke relationship.
fn is_call_edge(kind: &EdgeKind) -> bool {
    matches!(kind, EdgeKind::Calls | EdgeKind::Instantiates | EdgeKind::Implements)
}

/// Reconstruct the forward path from `root_id` to `target_id` by walking
/// the predecessor chain backward (from target up to root, then reversing).
fn reconstruct_call_path(
    predecessors: &HashMap<String, (SymbolId, EdgeKind)>,
    root_id: &SymbolId,
    target_id: &SymbolId,
    store: &Store,
) -> anyhow::Result<Vec<CallerChainStep>> {
    // Walk from target upward to root, collecting steps in reverse
    let mut raw_steps: Vec<(SymbolId, SymbolId, EdgeKind)> = Vec::new();
    let mut current = target_id.clone();

    while &current != root_id {
        let key = hex::encode(current.as_bytes());
        match predecessors.get(&key) {
            Some((pred_id, kind)) => {
                // pred_id calls current_id: pred → current
                raw_steps.push((pred_id.clone(), current.clone(), kind.clone()));
                current = pred_id.clone();
            }
            None => break,
        }
    }

    // Reverse to get root→target order
    raw_steps.reverse();

    let mut steps = Vec::new();
    for (idx, (caller, callee, kind)) in raw_steps.into_iter().enumerate() {
        let file_id = store
            .find_symbol_by_id(&caller)?
            .map(|s| s.file_id)
            .unwrap_or_else(|| crate::types::ids::FileId::generate("unknown"));

        let range = store
            .find_symbol_by_id(&caller)?
            .map(|s| s.range);

        let caller_name = store
            .find_symbol_by_id(&caller)?
            .map(|s| s.name)
            .unwrap_or_default();
        let callee_name = store
            .find_symbol_by_id(&callee)?
            .map(|s| s.name)
            .unwrap_or_default();

        steps.push(CallerChainStep::new(
            idx as u32,
            caller,
            callee,
            kind,
            file_id,
            range,
            &format!("{} → {}", caller_name, callee_name),
        ));
    }

    Ok(steps)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::enums::EdgeKind;

    #[test]
    fn is_call_edge_calls() {
        assert!(is_call_edge(&EdgeKind::Calls));
    }

    #[test]
    fn is_call_edge_instantiates() {
        assert!(is_call_edge(&EdgeKind::Instantiates));
    }

    #[test]
    fn is_call_edge_implements() {
        assert!(is_call_edge(&EdgeKind::Implements));
    }

    #[test]
    fn is_not_call_edge_references() {
        assert!(!is_call_edge(&EdgeKind::References));
    }

    #[test]
    fn is_not_call_edge_contains() {
        assert!(!is_call_edge(&EdgeKind::Contains));
    }
}
