//! Reverse call-graph traversal — trace how a function gets invoked.
//!
//! The caller path explorer walks backward through `Calls`, `Instantiates`,
//! and `Implements` symbol edges to reconstruct the chain of callers from an
//! entry-point down to a target function.  `RegistersCallback` edges are
//! included in the path but annotated with a [`BoundaryMarker`] and halt
//! further backward traversal.
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
//! - Only direct `Calls`/`Instantiates`/`Implements` edges are followed for
//!   traversal continuation.  `RegistersCallback` edges are shown but stop
//!   the traversal (dynamic dispatch boundary).
//! - Transitive dependencies (e.g. via `References`) are not included.

use std::collections::{HashMap, VecDeque};

use db::{CallGraphReader, SymbolReader};
use types::caller_path::{CallerChain, CallerChainStep};
use types::enums::EdgeKind;
use types::ids::SymbolId;

use super::call_chain;

/// Default maximum depth for caller-chain traversal.
#[allow(unused_imports)]
pub use call_chain::DEFAULT_MAX_DEPTH;

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
        let mut predecessors: call_chain::PredecessorMap = HashMap::new();
        let mut visited: HashMap<String, usize> = HashMap::new(); // node_id hex → depth
        let mut queue: VecDeque<(SymbolId, usize)> = VecDeque::new();

        let root_key = hex::encode(target_id.as_bytes());
        visited.insert(root_key.clone(), 0);
        queue.push_back((target_id.clone(), 0));

        let mut farthest_id = target_id.clone();
        let mut farthest_depth: usize = 0;
        let mut truncated = false;

        // Path quality scoring: prefer production callers over test/Benchmark
        // functions.  Uses a scoring heuristic rather than pure depth so that
        // `ServeHTTP → handleHTTPRequest → Next` wins over deeper test chains.
        let mut best_candidate_id = target_id.clone();
        let mut best_score: f64 = 0.0;

        while let Some((current_id, depth)) = queue.pop_front() {
            let edges = store.find_edges_by_target(&current_id)?;
            for edge in &edges {
                // Only follow call-related edges (include RegistersCallback for
                // path display, but they halt further BFS traversal).
                if !call_chain::is_call_graph_edge(&edge.kind) {
                    continue;
                }

                let is_boundary = edge.kind == EdgeKind::RegistersCallback;

                let caller = &edge.source;
                let caller_key = hex::encode(caller.as_bytes());

                if !visited.contains_key(&caller_key) {
                    let new_depth = depth + 1;

                    // Score this candidate: production callers score higher
                    // than test/Benchmark functions.
                    let caller_score = caller_path_score(store, caller, new_depth);

                    // Track best candidate by score (not pure depth)
                    if caller_score > best_score {
                        best_score = caller_score;
                        best_candidate_id = caller.clone();
                    }

                    // If this is a callback boundary, enqueue the caller but
                    // do NOT continue BFS from it — mark it as a boundary stop.
                    if is_boundary {
                        let current_key = hex::encode(current_id.as_bytes());
                        visited.insert(caller_key.clone(), new_depth);
                        predecessors.insert(
                            current_key,
                            (
                                caller.clone(),
                                edge.kind.clone(),
                                edge.ref_id.clone(),
                                edge.location.clone(),
                            ),
                        );
                        // Record this as farthest, but do NOT push to queue
                        if new_depth > farthest_depth {
                            farthest_depth = new_depth;
                            farthest_id = caller.clone();
                        }
                        continue;
                    }

                    if new_depth >= max_depth {
                        // Budget exhausted — check if this caller has unexplored
                        // callers of its own (not just the edge we already followed).
                        let caller_edges = store.find_edges_by_target(caller)?;
                        if caller_edges
                            .iter()
                            .any(|e| call_chain::is_call_graph_edge(&e.kind))
                        {
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

        if best_candidate_id == *target_id && farthest_id == *target_id {
            // No callers found — this is already a root
            return Ok(None);
        }

        // Prefer the best-scored candidate over the farthest-depth candidate.
        // Fall back to farthest if the best candidate wasn't reachable
        // (shouldn't happen in practice since both use the same BFS).
        let root_id = if best_candidate_id != *target_id {
            best_candidate_id
        } else {
            farthest_id
        };

        let root = store
            .find_symbol_by_id(&root_id)?
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

/// Score a caller candidate for path quality.
///
/// Production functions (non-test, non-benchmark) score much higher than
/// test/Benchmark/Example functions.  Among production callers, medium-depth
/// paths (2-5 hops) are slightly preferred over very short or very long chains.
///
/// Returns 0.0 for the target itself (depth 0, no caller).
fn caller_path_score(
    store: &(impl SymbolReader + CallGraphReader),
    caller_id: &SymbolId,
    depth: usize,
) -> f64 {
    if depth == 0 {
        return 0.0;
    }
    // Look up the caller's name to check if it's a test function.
    // This is a fast indexed lookup (primary key on SymbolId).
    let is_test = store
        .find_symbol_by_id(caller_id)
        .ok()
        .flatten()
        .map(|s| call_chain::is_likely_test_name(&s.name))
        .unwrap_or(false);

    let production_bonus: f64 = if is_test { 0.0 } else { 100.0 };
    // Prefer medium-depth production paths (2-5 hops).
    // Gently decay beyond 5 to avoid excessively deep chains.
    let depth_value: f64 = if depth <= 5 {
        depth as f64 * 0.5
    } else {
        2.5 - (depth as f64 - 5.0) * 0.2
    };
    production_bonus + depth_value
}

/// Reconstruct the forward path from `root_id` to `target_id` by walking
/// the predecessor chain backward (from target up to root, then reversing).
fn reconstruct_call_path(
    predecessors: &call_chain::PredecessorMap,
    root_id: &SymbolId,
    target_id: &SymbolId,
    store: &(impl SymbolReader + CallGraphReader),
) -> anyhow::Result<Vec<CallerChainStep>> {
    let raw = call_chain::reconstruct_path(predecessors, target_id, root_id, store)?;
    let steps = raw
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let mut step = CallerChainStep::new(
                i as u32,
                s.caller,
                s.callee,
                s.kind,
                s.file_id,
                s.range,
                &s.description,
            );
            step.callsite = s.callsite;
            step.boundary = s.boundary;
            step
        })
        .collect();
    Ok(steps)
}
