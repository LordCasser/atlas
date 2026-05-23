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

use atlas_db::{CallGraphReader, SymbolReader};
use atlas_types::caller_path::{CallerChain, CallerChainStep};
use atlas_types::enums::EdgeKind;
use atlas_types::ids::{ReferenceId, SymbolId};
use atlas_types::structs::TextRange;

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
        store: &(impl SymbolReader + CallGraphReader),
        target_id: &SymbolId,
        max_depth: usize,
    ) -> anyhow::Result<Option<CallerChain>> {
        let target = match store.find_symbol_by_id(target_id)? {
            Some(s) => s,
            None => return Ok(None),
        };

        // BFS backward: node_id → (predecessor, edge_kind, ref_id, location)
        let mut predecessors: HashMap<
            String,
            (SymbolId, EdgeKind, Option<ReferenceId>, Option<TextRange>),
        > = HashMap::new();
        let mut visited: HashMap<String, usize> = HashMap::new(); // node_id hex → depth
        let mut queue: VecDeque<(SymbolId, usize)> = VecDeque::new();

        let root_key = hex::encode(target_id.as_bytes());
        visited.insert(root_key.clone(), 0);
        queue.push_back((target_id.clone(), 0));

        let mut farthest_id = target_id.clone();
        let mut farthest_depth: usize = 0;
        let mut truncated = false;

        while let Some((current_id, depth)) = queue.pop_front() {
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
                    if new_depth >= max_depth {
                        // Budget exhausted — check if this caller has unexplored
                        // callers of its own (not just the edge we already followed).
                        let caller_edges = store.find_edges_by_target(caller)?;
                        if caller_edges.iter().any(|e| is_call_edge(&e.kind)) {
                            truncated = true;
                            // Track this frontier node as the farthest known point.
                            if new_depth > farthest_depth {
                                farthest_depth = new_depth;
                                farthest_id = caller.clone();
                            }
                        }
                        continue;
                    }
                    let current_key = hex::encode(current_id.as_bytes());
                    visited.insert(caller_key.clone(), new_depth);
                    // Store current→caller so reconstruct_call_path can walk
                    // backward from target through each caller.  Also preserve
                    // the edge's ref_id and location so we can populate the
                    // step's callsite and range with real evidence.
                    predecessors.insert(
                        current_key,
                        (
                            caller.clone(),
                            edge.kind.clone(),
                            edge.ref_id.clone(),
                            edge.location.clone(),
                        ),
                    );
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
        let steps = reconstruct_call_path(&predecessors, &root.id, target_id, store)?;

        Ok(Some(CallerChain {
            root,
            steps,
            target,
            nodes_visited: visited.len(),
            max_depth_reached: farthest_depth,
            truncated,
        }))
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Only follow structural edges that indicate a call/invoke relationship.
fn is_call_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls | EdgeKind::Instantiates | EdgeKind::Implements
    )
}

/// Reconstruct the forward path from `root_id` to `target_id` by walking
/// the predecessor chain backward (from target up to root, then reversing).
fn reconstruct_call_path(
    predecessors: &HashMap<String, (SymbolId, EdgeKind, Option<ReferenceId>, Option<TextRange>)>,
    root_id: &SymbolId,
    target_id: &SymbolId,
    store: &(impl SymbolReader + CallGraphReader),
) -> anyhow::Result<Vec<CallerChainStep>> {
    // Walk from target upward to root, collecting steps in reverse
    let mut raw_steps: Vec<(
        SymbolId,
        SymbolId,
        EdgeKind,
        Option<ReferenceId>,
        Option<TextRange>,
    )> = Vec::new();
    let mut current = target_id.clone();

    while &current != root_id {
        let key = hex::encode(current.as_bytes());
        match predecessors.get(&key) {
            Some((pred_id, kind, ref_id, location)) => {
                // pred_id calls current_id: pred → current
                raw_steps.push((
                    pred_id.clone(),
                    current.clone(),
                    kind.clone(),
                    ref_id.clone(),
                    location.clone(),
                ));
                current = pred_id.clone();
            }
            None => break,
        }
    }

    // Reverse to get root→target order
    raw_steps.reverse();

    // Prefetch all unique caller/callee symbols once to avoid N+1 queries.
    let mut symbol_cache: std::collections::HashMap<
        atlas_types::ids::SymbolId,
        atlas_types::SymbolDef,
    > = std::collections::HashMap::new();
    {
        let mut unique_ids: std::collections::HashSet<atlas_types::ids::SymbolId> =
            std::collections::HashSet::new();
        for (caller, callee, _, _, _) in &raw_steps {
            unique_ids.insert(caller.clone());
            unique_ids.insert(callee.clone());
        }
        for id in &unique_ids {
            if let Ok(Some(sym)) = store.find_symbol_by_id(id) {
                symbol_cache.insert(id.clone(), sym);
            }
        }
    }

    let mut steps = Vec::new();
    for (idx, (caller, callee, kind, ref_id, edge_location)) in raw_steps.into_iter().enumerate() {
        let caller_sym = symbol_cache.get(&caller);
        let callee_sym = symbol_cache.get(&callee);

        let file_id = caller_sym
            .map(|s| s.file_id)
            .unwrap_or_else(|| atlas_types::ids::FileId::generate("unknown"));

        // Look up the callsite via the edge's ref_id.
        let callsite = if let Some(ref rid) = ref_id {
            store.find_callsite_by_reference_id(rid).ok().flatten()
        } else {
            None
        };

        // Primary range: use the full callsite range (call expression) if
        // available, then the edge location (callee token), then the caller
        // symbol range as last resort.
        let range = if let Some(ref cs) = callsite {
            Some(cs.range)
        } else {
            edge_location
                .clone()
                .or_else(|| caller_sym.map(|s| s.range))
        };

        let caller_name = caller_sym.map(|s| s.name.clone()).unwrap_or_default();
        let callee_name = callee_sym.map(|s| s.name.clone()).unwrap_or_default();

        let mut step = CallerChainStep::new(
            idx as u32,
            caller,
            callee,
            kind,
            file_id,
            range,
            &format!("{} → {}", caller_name, callee_name),
        );
        step.callsite = callsite;
        steps.push(step);
    }

    Ok(steps)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_types::enums::EdgeKind;

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
