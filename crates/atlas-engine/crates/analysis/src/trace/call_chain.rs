//! Shared utilities for call-chain traversal (both forward and backward).
//!
//! Contains edge filtering, test-name detection, symbol caching,
//! callsite resolution, range prioritization, boundary marker creation,
//! and path reconstruction — common to both
//! [`crate::trace::CallerPathExplorer`] and
//! [`crate::trace::ForwardPathExplorer`].

use std::collections::{HashMap, HashSet};

use db::{CallGraphReader, SymbolReader};
use types::Callsite;
use types::enums::EdgeKind;
use types::ids::{FileId, ReferenceId, SymbolId};
use types::structs::TextRange;
use types::trace::{BoundaryKind, BoundaryMarker};

/// Default maximum depth for call-chain traversal.
///
/// Reserved; all current callers pass depth explicitly.  Remains public
/// for documentation and potential future default-parameter usage.
#[allow(dead_code)]
pub const DEFAULT_MAX_DEPTH: usize = 20;

/// Maps a node key (hex-encoded [`SymbolId`]) to its predecessor info:
/// (predecessor_id, edge_kind, reference_id, location).
pub type PredecessorMap =
    HashMap<String, (SymbolId, EdgeKind, Option<ReferenceId>, Option<TextRange>)>;

/// An intermediate representation of a single reconstructed call-chain step.
/// Both caller-path and forward-path wrappers convert this into their
/// direction-specific step type ([`CallerChainStep`] / [`ForwardChainStep`]).
///
/// [`CallerChainStep`]: types::caller_path::CallerChainStep
/// [`ForwardChainStep`]: types::caller_path::ForwardChainStep
pub struct ReconstructedStep {
    pub caller: SymbolId,
    pub callee: SymbolId,
    pub kind: EdgeKind,
    pub file_id: FileId,
    pub range: Option<TextRange>,
    pub description: String,
    pub callsite: Option<Callsite>,
    pub boundary: Option<BoundaryMarker>,
}

// ── edge filtering ─────────────────────────────────────────────────────────

/// Returns `true` if `kind` is one of the call-graph edges tracked during
/// path reconstruction ([`EdgeKind::Calls`], [`EdgeKind::Instantiates`],
/// [`EdgeKind::Implements`], [`EdgeKind::RegistersCallback`]).
pub fn is_call_graph_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls
            | EdgeKind::Instantiates
            | EdgeKind::Implements
            | EdgeKind::RegistersCallback
    )
}

// ── test-name heuristic ────────────────────────────────────────────────────

/// Heuristic: is this function name likely a test or benchmark?
///
/// Covers Go conventions (`Test*`, `Benchmark*`, `Example*`), common
/// patterns in other languages (`test_*`, `spec_*`), and the plain `test`
/// name used by curl/libtest conventions.  This is intentionally
/// simple — false positives only affect scoring, not correctness.
pub fn is_likely_test_name(name: &str) -> bool {
    name.starts_with("Test")
        || name.starts_with("Benchmark")
        || name.starts_with("Example")
        || name.starts_with("test_")
        || name.starts_with("spec_")
        || name.starts_with("it_")
        || name.starts_with("Fuzz")
        || name == "test"
        || name.ends_with("_test")
        || name.ends_with("_spec")
}

// ── boundary marker ────────────────────────────────────────────────────────

/// Build a [`BoundaryMarker`] for a [`RegistersCallback`](EdgeKind::RegistersCallback) edge.
pub fn create_boundary_marker(
    caller_name: &str,
    callee_name: &str,
    callee_id: &SymbolId,
) -> BoundaryMarker {
    BoundaryMarker {
        kind: BoundaryKind::CallbackRegistration {
            registrant: caller_name.to_string(),
            callback: callee_name.to_string(),
        },
        message: format!(
            "'{callee_name}' is registered as a callback by '{caller_name}'. It will be invoked dynamically. \
             Static call-graph tracing stops here."
        ),
        suggestion: format!(
            "Use explore on '{callee_name}' to find its own callers and understand the invocation context."
        ),
        bridge_target: Some(callee_id.to_hex()),
    }
}

// ── symbol cache ───────────────────────────────────────────────────────────

type RawCallStep = (
    SymbolId,
    SymbolId,
    EdgeKind,
    Option<ReferenceId>,
    Option<TextRange>,
);

/// Prefetch all unique symbols referenced by the raw steps into a cache.
pub fn build_symbol_cache(
    raw_steps: &[RawCallStep],
    store: &(impl SymbolReader + CallGraphReader),
) -> HashMap<SymbolId, types::SymbolDef> {
    let mut cache = HashMap::new();
    let mut unique_ids = HashSet::new();
    for (caller, callee, _, _, _) in raw_steps {
        unique_ids.insert(*caller);
        unique_ids.insert(*callee);
    }
    for id in &unique_ids {
        if let Ok(Some(sym)) = store.find_symbol_by_id(id) {
            cache.insert(*id, sym);
        }
    }
    cache
}

// ── path reconstruction ────────────────────────────────────────────────────

/// Walk the predecessor chain from `start_id` to `stop_id`, collect raw
/// (caller, callee, kind, ref_id, location) tuples in order, reverse them,
/// resolve every step's derived data (callsite, range, description,
/// boundary marker), and return [`ReconstructedStep`] values ready to be
/// wrapped into direction-specific step types.
pub fn reconstruct_path(
    predecessors: &PredecessorMap,
    start_id: &SymbolId,
    stop_id: &SymbolId,
    store: &(impl SymbolReader + CallGraphReader),
) -> anyhow::Result<Vec<ReconstructedStep>> {
    // Collect raw steps walking backward from start_id to stop_id.
    let mut raw_steps: Vec<RawCallStep> = Vec::new();
    let mut current = *start_id;

    while &current != stop_id {
        let key = hex::encode(current.as_bytes());
        match predecessors.get(&key) {
            Some((pred_id, kind, ref_id, location)) => {
                raw_steps.push((*pred_id, current, *kind, *ref_id, *location));
                current = *pred_id;
            }
            None => break,
        }
    }

    // Reverse to get start_id → stop_id order.
    raw_steps.reverse();

    let symbol_cache = build_symbol_cache(&raw_steps, store);

    let mut steps = Vec::new();
    for (caller, callee, kind, ref_id, edge_location) in raw_steps {
        let caller_sym = symbol_cache.get(&caller);
        let callee_sym = symbol_cache.get(&callee);

        let file_id = caller_sym
            .map(|s| s.file_id)
            .unwrap_or_else(|| FileId::generate("unknown"));

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
            edge_location.or_else(|| caller_sym.map(|s| s.range))
        };

        let caller_name = caller_sym.map(|s| s.name.clone()).unwrap_or_default();
        let callee_name = callee_sym.map(|s| s.name.clone()).unwrap_or_default();

        let description = match &kind {
            EdgeKind::RegistersCallback => format!("{caller_name} registers {callee_name}"),
            _ => format!("{caller_name} → {callee_name}"),
        };

        let boundary = if kind == EdgeKind::RegistersCallback {
            Some(create_boundary_marker(&caller_name, &callee_name, &callee))
        } else {
            None
        };

        steps.push(ReconstructedStep {
            caller,
            callee,
            kind,
            file_id,
            range,
            description,
            callsite,
            boundary,
        });
    }

    Ok(steps)
}

// ───────────────────────────────────────────────────────────────────────────
// tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use types::enums::EdgeKind;

    #[test]
    fn is_call_graph_edge_calls() {
        assert!(is_call_graph_edge(&EdgeKind::Calls));
    }

    #[test]
    fn is_call_graph_edge_instantiates() {
        assert!(is_call_graph_edge(&EdgeKind::Instantiates));
    }

    #[test]
    fn is_call_graph_edge_implements() {
        assert!(is_call_graph_edge(&EdgeKind::Implements));
    }

    #[test]
    fn is_call_graph_edge_registers_callback() {
        assert!(is_call_graph_edge(&EdgeKind::RegistersCallback));
    }

    #[test]
    fn is_not_call_graph_edge_references() {
        assert!(!is_call_graph_edge(&EdgeKind::References));
    }

    #[test]
    fn is_not_call_graph_edge_contains() {
        assert!(!is_call_graph_edge(&EdgeKind::Contains));
    }

    #[test]
    fn is_likely_test_name_matches_go_convention() {
        assert!(is_likely_test_name("TestFoo"));
        assert!(is_likely_test_name("BenchmarkBar"));
        assert!(is_likely_test_name("ExampleQux"));
    }

    #[test]
    fn is_likely_test_name_matches_snake_prefix() {
        assert!(is_likely_test_name("test_integration"));
        assert!(is_likely_test_name("spec_model"));
        assert!(is_likely_test_name("it_should_work"));
    }

    #[test]
    fn is_likely_test_name_matches_plain_test() {
        assert!(is_likely_test_name("test"));
    }

    #[test]
    fn is_likely_test_name_matches_suffix() {
        assert!(is_likely_test_name("my_test"));
        assert!(is_likely_test_name("user_spec"));
    }

    #[test]
    fn is_likely_test_name_rejects_normal_names() {
        assert!(!is_likely_test_name("handle_request"));
        assert!(!is_likely_test_name("main"));
        assert!(!is_likely_test_name("calculate"));
        assert!(!is_likely_test_name("contest"));
    }

    #[test]
    fn is_likely_test_name_matches_fuzz_prefix() {
        assert!(is_likely_test_name("FuzzParser"));
    }
}
