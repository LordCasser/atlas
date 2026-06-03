//! Scope-exit post-pass: emits implicit Free effects for Rust Drop at scope exit
//! and Python `with` context-managed blocks at BlockExit.
//!
//! Runs AFTER `compose_effects` main loop. For each Alloc effect without a matching
//! explicit Free effect, emits a Free:
//! - At the nearest BlockExit node for PythonWith-annotated Allocs
//! - At the function's Exit node for all other Allocs (Rust Drop, etc.)
//!
//! This is primarily for Rust (implicit Drop) and Python (context managers),
//! but runs for all languages as a no-op when there are no unmatched Allocs.

use std::collections::HashMap;

use types::cfg::CfgEdge;
use types::effects::{ConsumptionStyle, PlaceRef, SemanticEffect, SemanticEffectKind};
use types::enums::{CallContext, CfgEdgeKind, CfgNodeKind};
use types::ids::CfgNodeId;

use super::cfg_graph::CfgGraph;
use super::effect_composer::make_effect;

/// Post-pass that emits implicit Free effects for allocations that have no
/// explicit Free within the same function (e.g., Rust Drop at scope exit,
/// Python context-manager `with` statement).
///
/// For Python `with` Allocs (CallContext::PythonWith), the Free is emitted
/// at the nearest BlockExit successor instead of at the function Exit.
pub fn run_scope_exit_pass(effects: &mut HashMap<CfgNodeId, Vec<SemanticEffect>>, cfg: &CfgGraph) {
    // 1. Collect all Allocs and their associated Free places
    let mut allocs: Vec<(CfgNodeId, PlaceRef, String, bool)> = Vec::new(); // (node_id, place, callee, is_python_with)
    let mut freed_places: Vec<PlaceRef> = Vec::new();

    for (_node_id, node_effects) in effects.iter() {
        for effect in node_effects {
            match &effect.kind {
                SemanticEffectKind::Alloc { target, callee } => {
                    // Check if this alloc node has PythonWith context
                    let is_python_with = cfg
                        .nodes
                        .get(&effect.cfg_node_id)
                        .map(|n| n.call_context == CallContext::PythonWith)
                        .unwrap_or(false);
                    allocs.push((
                        effect.cfg_node_id,
                        target.clone(),
                        callee.clone(),
                        is_python_with,
                    ));
                }
                SemanticEffectKind::Free { place, .. } => {
                    freed_places.push(place.clone());
                }
                _ => {}
            }
        }
    }

    if allocs.is_empty() {
        return;
    }

    // 2. Find Exit node (for non-PythonWith allocs)
    let exit_node = cfg.nodes.values().find(|n| n.kind == CfgNodeKind::Exit);
    let Some(exit) = exit_node else {
        return;
    };

    // 3. Emit Free for each allocation that has no matching explicit Free
    for (alloc_node_id, place, callee, is_python_with) in &allocs {
        // Skip if this place is already freed explicitly
        let already_freed = freed_places.iter().any(|fp| match (fp, place) {
            (PlaceRef::Field { path: p1 }, PlaceRef::Field { path: p2 }) => p1 == p2,
            (PlaceRef::Local { name: n1 }, PlaceRef::Local { name: n2 }) => n1 == n2,
            _ => false, // Indeterminate doesn't match anything for safety
        });
        if already_freed {
            continue;
        }

        if *is_python_with {
            // Find the nearest BlockExit successor from the alloc node
            if let Some(block_exit_id) = find_block_exit_successor(alloc_node_id, cfg) {
                let exit_effects = effects.entry(block_exit_id).or_default();
                let mut new_effect = make_effect(
                    &block_exit_id,
                    exit_effects.len() as u32,
                    SemanticEffectKind::Free {
                        place: place.clone(),
                        callee: format!("<block-exit>{}", callee),
                    },
                    0.80,
                );
                new_effect.consumption_style = Some(ConsumptionStyle::ContextManaged);
                exit_effects.push(new_effect);
                continue;
            }
            // Fall through: if no BlockExit found, emit at function Exit
        }

        // Default: emit at function Exit
        let exit_effects = effects.entry(exit.id).or_default();
        let mut new_effect = make_effect(
            &exit.id,
            exit_effects.len() as u32,
            SemanticEffectKind::Free {
                place: place.clone(),
                callee: format!("<scope-exit>{}", callee),
            },
            0.70,
        );
        if *is_python_with {
            new_effect.consumption_style = Some(ConsumptionStyle::ContextManaged);
        }
        exit_effects.push(new_effect);
    }
}

/// Walk CFG forward from `start_node_id` to find the nearest BlockExit node.
/// Only follows Normal edges (single successor path — BlockExit is on the
/// straight-line path after a with_statement body).
fn find_block_exit_successor(start_id: &CfgNodeId, cfg: &CfgGraph) -> Option<CfgNodeId> {
    let mut current = *start_id;
    let mut visited: Vec<CfgNodeId> = Vec::new();
    let max_steps = 20; // safety limit

    for _ in 0..max_steps {
        if visited.contains(&current) {
            return None; // cycle detected
        }
        visited.push(current);

        // Check if current node is BlockExit
        if let Some(node) = cfg.nodes.get(&current) {
            if node.kind == CfgNodeKind::BlockExit {
                return Some(current);
            }
        }

        // Follow Normal successors
        let successors = cfg.successors.get(&current)?;
        let normal_edges: Vec<&CfgEdge> = successors
            .iter()
            .filter(|e| e.kind == CfgEdgeKind::Normal)
            .collect();

        if normal_edges.len() == 1 {
            current = normal_edges[0].target;
        } else {
            // Multiple successors or none — we're not on a straight path
            return None;
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use types::cfg::CfgNode;
    use types::effects::PlaceRef;
    use types::enums::{CfgEdgeKind, CfgNodeKind};
    use types::ids::{CfgNodeId, FileId, SymbolId};

    /// Create a SymbolId for testing.
    fn make_sym_id() -> SymbolId {
        let file_id = FileId::generate("test/test.go");
        SymbolId::generate(&file_id, "go", "test_fn", "function", None)
    }

    /// Create a CfgGraph with Entry → Statement/BlockExit → Exit.
    /// If `block_exit` is true, inserts a BlockExit node between the Statement and Exit.
    fn make_cfg_with_alloc_node(
        sym_id: &SymbolId,
        alloc_node_id: &mut CfgNodeId,
        _alloc_node_kind: CfgNodeKind,
        alloc_has_python_with: bool,
        block_exit: bool,
    ) -> CfgGraph {
        let mut nodes: Vec<CfgNode> = Vec::new();

        let entry = CfgNode::entry(sym_id);
        nodes.push(entry.clone());

        let stmt_range = types::structs::TextRange {
            start_byte: 1,
            end_byte: 10,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        let mut stmt = CfgNode::new(sym_id, CfgNodeKind::Statement, stmt_range);
        if alloc_has_python_with {
            stmt.call_context = CallContext::PythonWith;
        }
        *alloc_node_id = stmt.id;
        nodes.push(stmt.clone());

        let mut last_id = stmt.id;

        if block_exit {
            let be_range = types::structs::TextRange {
                start_byte: 12,
                end_byte: 12,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            };
            let be = CfgNode::new(sym_id, CfgNodeKind::BlockExit, be_range);
            nodes.push(be.clone());
            last_id = be.id;
        }

        let exit = CfgNode::exit(sym_id);
        nodes.push(exit.clone());

        let mut edges: Vec<types::cfg::CfgEdge> = Vec::new();
        edges.push(types::cfg::CfgEdge::new(
            &entry.id,
            &stmt.id,
            CfgEdgeKind::Normal,
        ));
        if block_exit {
            edges.push(types::cfg::CfgEdge::new(
                &stmt.id,
                &last_id,
                CfgEdgeKind::Normal,
            ));
        }
        edges.push(types::cfg::CfgEdge::new(
            &last_id,
            &exit.id,
            CfgEdgeKind::Normal,
        ));

        CfgGraph::build(&nodes, &edges).expect("CfgGraph build should succeed")
    }

    /// Build a CFG with just Entry → Exit (no Statement nodes, no allocs).
    fn make_empty_cfg(sym_id: &SymbolId) -> CfgGraph {
        let entry = CfgNode::entry(sym_id);
        let exit = CfgNode::exit(sym_id);
        let nodes = vec![entry.clone(), exit.clone()];
        let edges = vec![types::cfg::CfgEdge::new(
            &entry.id,
            &exit.id,
            CfgEdgeKind::Normal,
        )];
        CfgGraph::build(&nodes, &edges).expect("CfgGraph build should succeed")
    }

    #[test]
    fn test_no_allocs_returns_early() {
        let sym_id = make_sym_id();
        let cfg = make_empty_cfg(&sym_id);
        let mut effects: HashMap<CfgNodeId, Vec<SemanticEffect>> = HashMap::new();
        // effects is empty — should be a no-op
        run_scope_exit_pass(&mut effects, &cfg);
        assert!(effects.is_empty(), "empty effects should remain empty");
    }

    #[test]
    fn test_alloc_with_explicit_free_no_scope_exit() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg = make_cfg_with_alloc_node(
            &sym_id,
            &mut node_id,
            CfgNodeKind::Statement,
            false,
            false,
        );

        let mut effects: HashMap<CfgNodeId, Vec<SemanticEffect>> = HashMap::new();
        let place = PlaceRef::Local {
            name: "f".to_string(),
        };

        // Insert an Alloc at node_id
        effects.insert(
            node_id,
            vec![make_effect(
                &node_id,
                0,
                SemanticEffectKind::Alloc {
                    target: place.clone(),
                    callee: "open".to_string(),
                },
                0.85,
            )],
        );

        // Insert a matching Free at the same node
        // (Entry node could also hold the Free — scope_exit only checks place equality)
        let exit = cfg.nodes.values().find(|n| n.kind == CfgNodeKind::Exit).unwrap();
        let exit_node_id = exit.id;
        effects.insert(
            exit_node_id,
            vec![make_effect(
                &exit_node_id,
                0,
                SemanticEffectKind::Free {
                    place,
                    callee: "close".to_string(),
                },
                0.85,
            )],
        );

        let effects_before = effects.len();
        run_scope_exit_pass(&mut effects, &cfg);

        // We had 2 effect entries (node_id + exit). No new effects should be added.
        assert_eq!(
            effects.len(),
            effects_before,
            "no new effect nodes should be added when Free already exists"
        );
        let exit_effects = effects.get(&exit_node_id).unwrap();
        assert_eq!(
            exit_effects.len(),
            1,
            "exit should still have exactly 1 effect (the explicit Free)"
        );
    }

    #[test]
    fn test_unfreed_alloc_gets_scope_exit_free() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg = make_cfg_with_alloc_node(
            &sym_id,
            &mut node_id,
            CfgNodeKind::Statement,
            false,
            false,
        );

        let mut effects: HashMap<CfgNodeId, Vec<SemanticEffect>> = HashMap::new();
        let place = PlaceRef::Local {
            name: "x".to_string(),
        };

        // Insert an Alloc without matching Free
        effects.insert(
            node_id,
            vec![make_effect(
                &node_id,
                0,
                SemanticEffectKind::Alloc {
                    target: place.clone(),
                    callee: "Box::new".to_string(),
                },
                0.85,
            )],
        );

        run_scope_exit_pass(&mut effects, &cfg);

        // The Exit node should now have a scope-exit Free
        let exit_node = cfg.nodes.values().find(|n| n.kind == CfgNodeKind::Exit).unwrap();
        let exit_effects = effects.get(&exit_node.id);
        assert!(
            exit_effects.is_some(),
            "Exit node should have effects after scope-exit pass"
        );
        let exit_effects = exit_effects.unwrap();
        let free_effect = exit_effects
            .iter()
            .find(|e| matches!(&e.kind, SemanticEffectKind::Free { .. }));
        assert!(
            free_effect.is_some(),
            "Exit node should have a Free effect for the unfreed alloc"
        );
        let free = free_effect.unwrap();
        assert!(
            free.confidence < 1.0,
            "scope-exit Free should have confidence < 1.0 (inferred)"
        );
        // Verify callee prefix
        match &free.kind {
            SemanticEffectKind::Free { callee, .. } => {
                assert!(
                    callee.contains("<scope-exit>"),
                    "callee should contain <scope-exit> prefix, got: {}",
                    callee
                );
            }
            _ => panic!("expected Free effect"),
        }
    }

    #[test]
    fn test_python_with_context_finds_block_exit() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg = make_cfg_with_alloc_node(
            &sym_id,
            &mut node_id,
            CfgNodeKind::Statement,
            true,  // PythonWith context
            true,  // include BlockExit
        );

        let mut effects: HashMap<CfgNodeId, Vec<SemanticEffect>> = HashMap::new();
        let place = PlaceRef::Local {
            name: "f".to_string(),
        };

        // Insert an Alloc with PythonWith context
        effects.insert(
            node_id,
            vec![make_effect(
                &node_id,
                0,
                SemanticEffectKind::Alloc {
                    target: place.clone(),
                    callee: "open".to_string(),
                },
                0.85,
            )],
        );

        run_scope_exit_pass(&mut effects, &cfg);

        // Find BlockExit node
        let be_node = cfg
            .nodes
            .values()
            .find(|n| n.kind == CfgNodeKind::BlockExit)
            .expect("BlockExit node should exist");

        let be_effects = effects.get(&be_node.id);
        assert!(
            be_effects.is_some(),
            "BlockExit node should have effects for PythonWith alloc"
        );
        let be_effects = be_effects.unwrap();
        let free_effect = be_effects
            .iter()
            .find(|e| matches!(&e.kind, SemanticEffectKind::Free { .. }));
        assert!(
            free_effect.is_some(),
            "BlockExit node should have a Free effect"
        );
        let free = free_effect.unwrap();
        // Verify <block-exit> prefix in callee
        match &free.kind {
            SemanticEffectKind::Free { callee, .. } => {
                assert!(
                    callee.contains("<block-exit>"),
                    "callee should contain <block-exit> prefix, got: {}",
                    callee
                );
            }
            _ => panic!("expected Free effect"),
        }
        // Should have ContextManaged consumption style
        assert_eq!(
            free.consumption_style,
            Some(ConsumptionStyle::ContextManaged),
            "PythonWith free should have ContextManaged style"
        );

        // Exit node should NOT have this Free (it went to BlockExit instead)
        let exit_node = cfg.nodes.values().find(|n| n.kind == CfgNodeKind::Exit).unwrap();
        if let Some(exit_effects) = effects.get(&exit_node.id) {
            let has_our_free = exit_effects.iter().any(|e| {
                matches!(&e.kind, SemanticEffectKind::Free { callee, .. } if callee.contains("<block-exit>") || callee.contains("<scope-exit>"))
            });
            // Exit may have a scope-exit Free only if BlockExit wasn't reached
            // In this test, BlockExit is reachable, so Exit should NOT have it
            assert!(
                !has_our_free,
                "Exit node should NOT have the scope/block-exit Free when BlockExit was reached"
            );
        }
    }

    #[test]
    fn test_alloc_with_different_place_not_matched() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg = make_cfg_with_alloc_node(
            &sym_id,
            &mut node_id,
            CfgNodeKind::Statement,
            false,
            false,
        );

        let mut effects: HashMap<CfgNodeId, Vec<SemanticEffect>> = HashMap::new();
        let alloc_place = PlaceRef::Local {
            name: "a".to_string(),
        };
        let free_place = PlaceRef::Local {
            name: "b".to_string(),
        };

        // Insert an Alloc for place "a"
        effects.insert(
            node_id,
            vec![make_effect(
                &node_id,
                0,
                SemanticEffectKind::Alloc {
                    target: alloc_place,
                    callee: "open".to_string(),
                },
                0.85,
            )],
        );

        // Insert a Free for a DIFFERENT place "b" (at same node)
        let exit_node = cfg.nodes.values().find(|n| n.kind == CfgNodeKind::Exit).unwrap();
        let exit_id = exit_node.id;
        effects.insert(
            exit_id,
            vec![make_effect(
                &exit_id,
                0,
                SemanticEffectKind::Free {
                    place: free_place,
                    callee: "close".to_string(),
                },
                0.85,
            )],
        );

        let _effects_before_count: usize = effects.values().map(|v| v.len()).sum();
        run_scope_exit_pass(&mut effects, &cfg);

        // The Exit node should now have TWO effects: the original Free (place "b") + new scope-exit Free (place "a")
        let exit_effects = effects.get(&exit_id).unwrap();
        assert_eq!(
            exit_effects.len(),
            2,
            "Exit should have 2 effects: original Free (place b) + scope-exit Free (place a)"
        );
        // Verify one of them is the scope-exit Free
        let scope_exit_free = exit_effects
            .iter()
            .find(|e| matches!(&e.kind, SemanticEffectKind::Free { callee, .. } if callee.contains("<scope-exit>")));
        assert!(
            scope_exit_free.is_some(),
            "An unfreed alloc (different place) should produce a scope-exit Free"
        );
    }
}
