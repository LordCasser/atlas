//! Forward call-graph traversal — trace how function A reaches function B.
//!
//! The forward path explorer walks forward through `Calls`, `Instantiates`,
//! `Implements`, and `RegistersCallback` symbol edges to reconstruct the
//! chain from a source function to a target function.
//!
//! # Algorithm
//!
//! 1. BFS forward from source, following outgoing call edges.
//! 2. Stop when target is found or max_depth is reached.
//! 3. Reconstruct path from source to target.

use std::collections::{HashMap, VecDeque};

use db::{CallGraphReader, SymbolReader};
use types::caller_path::{ForwardChain, ForwardChainStep};
use types::enums::EdgeKind;
use types::ids::{ReferenceId, SymbolId};
use types::structs::TextRange;
use types::trace::{BoundaryKind, BoundaryMarker};

/// Default maximum depth for forward-chain traversal.
#[allow(dead_code)]
pub const DEFAULT_MAX_DEPTH: usize = 20;

/// Explores forward call chains from a source symbol to a target.
pub struct ForwardPathExplorer;

impl ForwardPathExplorer {
    /// Build a [`ForwardChain`] from `source_id` forward to `target_id`.
    pub fn explore(
        store: &(impl SymbolReader + CallGraphReader),
        source_id: &SymbolId,
        target_id: &SymbolId,
        max_depth: usize,
    ) -> anyhow::Result<Option<ForwardChain>> {
        // source == target: the trivial path has zero steps; return
        // Ok(None) so callers treat it the same as "no path found"
        // rather than getting a confusing empty chain.
        if source_id == target_id {
            return Ok(None);
        }

        let source = match store.find_symbol_by_id(source_id)? {
            Some(s) => s,
            None => return Ok(None),
        };
        let target = match store.find_symbol_by_id(target_id)? {
            Some(s) => s,
            None => return Ok(None),
        };

        // BFS forward: node_id → (predecessor, edge_kind, ref_id, location)
        let mut predecessors: HashMap<
            String,
            (SymbolId, EdgeKind, Option<ReferenceId>, Option<TextRange>),
        > = HashMap::new();
        let mut visited: HashMap<String, usize> = HashMap::new();
        let mut queue: VecDeque<(SymbolId, usize)> = VecDeque::new();

        let source_key = hex::encode(source_id.as_bytes());
        visited.insert(source_key, 0);
        queue.push_back((source_id.clone(), 0));

        let mut found = false;
        let mut truncated = false;
        let mut nodes_visited = 0;

        while let Some((current_id, depth)) = queue.pop_front() {
            nodes_visited += 1;

            if &current_id == target_id {
                found = true;
                break;
            }

            if depth >= max_depth {
                // Depth budget exhausted.  Set truncated = true and stop
                // expanding from this node.  We don't query the DB here to
                // check for unexplored edges — at max_depth the result is
                // always a best-effort partial path.
                truncated = true;
                continue;
            }

            let edges = store.find_edges_by_source(&current_id)?;
            for edge in &edges {
                if !is_fwd_edge(&edge.kind) {
                    continue;
                }

                let callee = &edge.target;

                let callee_key = hex::encode(callee.as_bytes());
                if visited.contains_key(&callee_key) {
                    continue;
                }

                let new_depth = depth + 1;
                visited.insert(callee_key.clone(), new_depth);
                predecessors.insert(
                    callee_key,
                    (
                        current_id.clone(),
                        edge.kind.clone(),
                        edge.ref_id.clone(),
                        edge.location.clone(),
                    ),
                );
                queue.push_back((callee.clone(), new_depth));
            }
        }

        if !found {
            return Ok(None);
        }

        let steps =
            reconstruct_fwd_path(&predecessors, source_id, target_id, store)?;

        Ok(Some(ForwardChain {
            source,
            steps,
            target,
            nodes_visited,
            max_depth_reached: if found {
                visited.get(&hex::encode(target_id.as_bytes())).copied().unwrap_or(0)
            } else {
                max_depth
            },
            truncated,
        }))
    }
}

// ── helpers ──────────────────────────────────────────────────────────────

fn is_fwd_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls
            | EdgeKind::Instantiates
            | EdgeKind::Implements
            | EdgeKind::RegistersCallback
    )
}

fn reconstruct_fwd_path(
    predecessors: &HashMap<String, (SymbolId, EdgeKind, Option<ReferenceId>, Option<TextRange>)>,
    source_id: &SymbolId,
    target_id: &SymbolId,
    store: &(impl SymbolReader + CallGraphReader),
) -> anyhow::Result<Vec<ForwardChainStep>> {
    // Walk backward from target to source
    let mut raw_steps: Vec<(
        SymbolId,
        SymbolId,
        EdgeKind,
        Option<ReferenceId>,
        Option<TextRange>,
    )> = Vec::new();
    let mut current = target_id.clone();

    while &current != source_id {
        let key = hex::encode(current.as_bytes());
        match predecessors.get(&key) {
            Some((pred_id, kind, ref_id, location)) => {
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

    raw_steps.reverse();

    // Prefetch symbols
    let mut symbol_cache: HashMap<SymbolId, types::SymbolDef> = HashMap::new();
    {
        let mut unique_ids = std::collections::HashSet::new();
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
            .unwrap_or_else(|| types::ids::FileId::generate("unknown"));

        let callsite = if let Some(ref rid) = ref_id {
            store.find_callsite_by_reference_id(rid).ok().flatten()
        } else {
            None
        };

        let range = if let Some(ref cs) = callsite {
            Some(cs.range)
        } else {
            edge_location
                .clone()
                .or_else(|| caller_sym.map(|s| s.range))
        };

        let caller_name = caller_sym.map(|s| s.name.clone()).unwrap_or_default();
        let callee_name = callee_sym.map(|s| s.name.clone()).unwrap_or_default();

        let desc = match &kind {
            EdgeKind::RegistersCallback => format!("{} registers {}", caller_name, callee_name),
            _ => format!("{} → {}", caller_name, callee_name),
        };

        let mut step = ForwardChainStep::new(
            idx as u32,
            caller,
            callee,
            kind.clone(),
            file_id,
            range,
            &desc,
        );
        step.callsite = callsite;

        if kind == EdgeKind::RegistersCallback {
            step.boundary = Some(BoundaryMarker {
                kind: BoundaryKind::CallbackRegistration {
                    registrant: caller_name.clone(),
                    callback: callee_name.clone(),
                },
                message: format!(
                    "'{}' is registered as a callback by '{}'. It will be invoked dynamically.",
                    callee_name, caller_name
                ),
                suggestion: format!(
                    "Use explore on '{}' to find its own callers.",
                    callee_name
                ),
                bridge_target: Some(callee.to_hex()),
            });
        }

        steps.push(step);
    }

    Ok(steps)
}

// ───────────────────────────────────────────────────────────────────────────
// tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use db::Store;
    use std::sync::Arc;
    use types::*;
    use super::ForwardPathExplorer;

    /// Helper: build a minimal test store with a few functions.
    struct TestHarness {
        store: Arc<Store>,
        file_id: FileId,
    }

    impl TestHarness {
        fn new() -> Self {
            let store = Arc::new(Store::open_in_memory().unwrap());
            store.init_schema().unwrap();
            let fid = FileId::generate("test.c");
            let _ = store.upsert_file(&FileInfo {
                file_id: fid, path: "test.c".into(), language: Language::C,
                content_hash: "abc".into(), status: ParseStatus::Success,
            });
            Self { store, file_id: fid }
        }

        fn make_fun(&self, name: &str) -> SymbolDef {
            let fid = self.file_id;
            SymbolDef {
                id: SymbolId::generate(&fid, "c", name, "Function", None),
                file_id: fid, kind: SymbolKind::Function, name: name.into(),
                qualified_name: name.into(), symbol_path: vec![name.into()],
                language: Language::C,
                range: TextRange::default(), name_range: TextRange::default(),
                signature: None, visibility: None, exported: false, static_: false,
                async_: false, container: None, scope_id: None, package_name: None,
                namespace_path: vec![], layer: "structural".into(),
            }
        }

        fn connect(&self, caller: &SymbolDef, callee: &SymbolDef, kind: EdgeKind) {
            self.store.batch_insert_edges(&[RawEdge::new(
                EdgeId::generate(&caller.id, &callee.id, kind.as_str(), None, "test"),
                caller.id, callee.id, kind, Confidence::certain(), Provenance::TreeSitter,
            )]).unwrap();
        }

        fn seed(&self, syms: &[SymbolDef]) {
            self.store.insert_symbols(syms).unwrap();
        }

        fn store_ref(&self) -> &Store { &self.store }
    }

    #[test]
    fn test_forward_smoke() {
        let h = TestHarness::new();
        let main = h.make_fun("main");
        let helper = h.make_fun("helper");
        h.seed(&[main.clone(), helper.clone()]);
        h.connect(&main, &helper, EdgeKind::Calls);

        let chain = ForwardPathExplorer::explore(h.store_ref(), &main.id, &helper.id, 10)
            .unwrap()
            .expect("should find path");
        assert_eq!(chain.steps.len(), 1);
    }

    #[test]
    fn test_forward_multi_step() {
        let h = TestHarness::new();
        let a = h.make_fun("a"); let b = h.make_fun("b"); let c = h.make_fun("c");
        h.seed(&[a.clone(), b.clone(), c.clone()]);
        h.connect(&a, &b, EdgeKind::Calls);
        h.connect(&b, &c, EdgeKind::Calls);

        let chain = ForwardPathExplorer::explore(h.store_ref(), &a.id, &c.id, 10)
            .unwrap()
            .expect("should find a→b→c");
        assert_eq!(chain.steps.len(), 2);
        assert_eq!(chain.source.name, "a");
        assert_eq!(chain.target.name, "c");
        assert!(!chain.truncated);
    }

    #[test]
    fn test_forward_no_path() {
        let h = TestHarness::new();
        let a = h.make_fun("a"); let b = h.make_fun("b");
        h.seed(&[a.clone(), b.clone()]);
        // No edge → a cannot reach b
        let result = ForwardPathExplorer::explore(h.store_ref(), &a.id, &b.id, 10).unwrap();
        assert!(result.is_none(), "disconnected graph should yield None");
    }

    #[test]
    fn test_forward_source_equals_target() {
        let h = TestHarness::new();
        let a = h.make_fun("a");
        h.seed(&[a.clone()]);
        // source == target: trivial path → None
        let result = ForwardPathExplorer::explore(h.store_ref(), &a.id, &a.id, 10).unwrap();
        assert!(result.is_none(), "source == target should return None");
    }

    #[test]
    fn test_forward_max_depth_truncation() {
        let h = TestHarness::new();
        let a = h.make_fun("a"); let b = h.make_fun("b"); let c = h.make_fun("c");
        h.seed(&[a.clone(), b.clone(), c.clone()]);
        h.connect(&a, &b, EdgeKind::Calls);
        h.connect(&b, &c, EdgeKind::Calls);

        // max_depth=1: can only reach b, not c
        let result = ForwardPathExplorer::explore(h.store_ref(), &a.id, &c.id, 1).unwrap();
        assert!(result.is_none(), "should not reach c within max_depth=1");
    }

    #[test]
    fn test_forward_registers_callback_marker() {
        let h = TestHarness::new();
        let registrant = h.make_fun("set_callback");
        let callback = h.make_fun("on_event");
        h.seed(&[registrant.clone(), callback.clone()]);
        h.connect(&registrant, &callback, EdgeKind::RegistersCallback);

        let chain = ForwardPathExplorer::explore(h.store_ref(), &registrant.id, &callback.id, 10)
            .unwrap()
            .expect("should find callback registration");
        assert_eq!(chain.steps.len(), 1);
        let step = &chain.steps[0];
        assert!(step.boundary.is_some(), "RegistersCallback step must have boundary marker");
        let marker = step.boundary.as_ref().unwrap();
        assert!(marker.message.contains("callback"), "message should mention callback");
        assert!(marker.suggestion.contains("explore"), "suggestion should mention explore");
        assert!(marker.bridge_target.is_some(), "should have bridge_target");
    }

    #[test]
    fn test_forward_branching_picks_shortest() {
        let h = TestHarness::new();
        let a = h.make_fun("a"); let b = h.make_fun("b"); let c = h.make_fun("c");
        let d = h.make_fun("d"); let e = h.make_fun("e");
        h.seed(&[a.clone(), b.clone(), c.clone(), d.clone(), e.clone()]);
        // Two paths to e:  a→b→e (2 hops)  and  a→c→d→e (3 hops)
        h.connect(&a, &b, EdgeKind::Calls);
        h.connect(&b, &e, EdgeKind::Calls);
        h.connect(&a, &c, EdgeKind::Calls);
        h.connect(&c, &d, EdgeKind::Calls);
        h.connect(&d, &e, EdgeKind::Calls);

        let chain = ForwardPathExplorer::explore(h.store_ref(), &a.id, &e.id, 10)
            .unwrap()
            .expect("should find path");
        // BFS guarantees shortest path: a→b→e = 2 steps
        assert_eq!(chain.steps.len(), 2);
    }
}
