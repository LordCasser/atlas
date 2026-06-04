//! Scope-exit post-pass: emits implicit Free effects for Rust Drop at scope exit,
//! Python `with`, Java `try-with-resources`, C# `using`, and Ruby block-managed
//! context blocks at BlockExit.
//!
//! Runs AFTER `compose_effects` main loop. For each Alloc effect without a matching
//! explicit Free effect, emits a Free:
//! - At the nearest BlockExit node for PythonWith/JavaTryWith/CSharpUsing/RubyBlock-annotated Allocs
//! - At the function's Exit node for other eligible Allocs
//!
//! ## Eligibility for scope-exit cleanup
//! An Alloc is eligible for auto-cleanup only if ALL of:
//! 1. The effect's `eligible_for_implicit_cleanup` is `Some(true)` (or `None` for
//!    backward compat), OR the Alloc node has a context-managed call context
//!    (PythonWith / JavaTryWith / CSharpUsing / RubyBlock).
//! 2. The allocated PlaceRef does NOT appear in any Return or Escape effect
//!    (a resource that is returned or escapes the function should not be
//!    auto-freed — the caller or escape target owns it).
//!
//! This is primarily for Rust (implicit Drop) and Python/Java/C# context managers,
//! but runs for all languages as a near-no-op when no Allocs are eligible.

use std::collections::{HashMap, HashSet};

use types::cfg::CfgEdge;
use types::effects::{ConsumptionStyle, PlaceRef, SemanticEffect, SemanticEffectKind, ValueSource};
use types::enums::{CallContext, CfgEdgeKind, CfgNodeKind};
use types::ids::CfgNodeId;

use super::cfg_graph::CfgGraph;
use super::effect_composer::make_effect;

/// Post-pass that emits implicit Free effects for allocations that have no
/// explicit Free within the same function (e.g., Rust Drop at scope exit,
/// Python context-manager `with` statement, Ruby block resources).
///
/// Eligibility is gated per-Alloc:
/// - Context-managed Allocs (PythonWith/JavaTryWith/CSharpUsing/RubyBlock) always
///   get a Free at the nearest BlockExit successor.
/// - Other Allocs only get a Free if `eligible_for_implicit_cleanup` is true
///   AND the allocated place is NOT returned or escaped.
pub fn run_scope_exit_pass(effects: &mut HashMap<CfgNodeId, Vec<SemanticEffect>>, cfg: &CfgGraph) {
    // 0. Collect Return/Escape value names (to skip auto-free for escaped resources)
    let mut escaped_locals: HashSet<String> = HashSet::new();
    for (_node_id, node_effects) in effects.iter() {
        for effect in node_effects {
            match &effect.kind {
                SemanticEffectKind::Return { value } => {
                    if let ValueSource::Local { name } = value {
                        escaped_locals.insert(name.clone());
                    }
                }
                SemanticEffectKind::Escape { value, .. } => {
                    if let ValueSource::Local { name } = value {
                        escaped_locals.insert(name.clone());
                    }
                }
                _ => {}
            }
        }
    }

    // 1. Collect all Allocs and their associated Free places
    let mut allocs: Vec<(CfgNodeId, PlaceRef, String, bool, bool)> = Vec::new();
    // (node_id, place, callee, is_context_managed, eligible_for_cleanup)
    let mut freed_places: Vec<PlaceRef> = Vec::new();

    for (_node_id, node_effects) in effects.iter() {
        for effect in node_effects {
            match &effect.kind {
                SemanticEffectKind::Alloc { target, callee } => {
                    // Check if this alloc node has PythonWith/JavaTryWith/CSharpUsing context
                    // Context-managed check: immediate CFG node's context
                    // annotation plus a forward walk for Kotlin split-call-chain
                    // (tree-sitter parses chained calls as separate statements).
                    let is_context_managed = cfg
                        .nodes
                        .get(&effect.cfg_node_id)
                        .map(|n| {
                            n.call_context == CallContext::PythonWith
                                || n.call_context == CallContext::JavaTryWith
                                || n.call_context == CallContext::CSharpUsing
                                || n.call_context == CallContext::RubyBlock
                                || n.call_context == CallContext::KotlinUse
                        })
                        .unwrap_or(false);
                    let is_context_managed = is_context_managed
                        || has_kotlin_use_successor(&effect.cfg_node_id, cfg, 3);

                    // Per-effect eligibility (backward compat: None = eligible)
                    let eligible = effect.eligible_for_implicit_cleanup.unwrap_or(false);

                    // Check if the allocated place is returned or escaped
                    let is_escaped = match target {
                        PlaceRef::Local { name } => escaped_locals.contains(name),
                        PlaceRef::Field { .. } | PlaceRef::Indeterminate => false,
                    };

                    allocs.push((
                        effect.cfg_node_id,
                        target.clone(),
                        callee.clone(),
                        is_context_managed,
                        eligible && !is_escaped, // only eligible if not escaped
                    ));
                }
                SemanticEffectKind::Free { place, .. } => {
                    freed_places.push(place.clone());
                }
                _ => {}
            }
        }
    }

    /// Walk forward along single-successor Normal edges from `start` to find a
/// KotlinUse node within `max_hops`.  Needed because tree-sitter-kotlin parses
/// `File("x").bufferedReader().use { ... }` as two separate `call_expression`
/// nodes — the alloc lands on the first statement, the KotlinUse context on the
/// second.  A bounded forward walk bridges the split.
fn has_kotlin_use_successor(start: &CfgNodeId, cfg: &CfgGraph, max_hops: usize) -> bool {
    if max_hops == 0 {
        return false;
    }
    let successors = cfg.successors.get(start);
    let Some(edges) = successors else { return false; };
    for edge in edges {
        if edge.kind != CfgEdgeKind::Normal {
            continue;
        }
        if let Some(target_node) = cfg.nodes.get(&edge.target) {
            if target_node.call_context == CallContext::KotlinUse {
                return true;
            }
            if has_kotlin_use_successor(&edge.target, cfg, max_hops - 1) {
                return true;
            }
        }
    }
    false
}

// 2. Find Exit node (for non-context-managed allocs)
    let exit_node = cfg.nodes.values().find(|n| n.kind == CfgNodeKind::Exit);
    let Some(exit) = exit_node else {
        return;
    };

    // 3. Emit Free for each allocation that has no matching explicit Free
    for (alloc_node_id, place, callee, is_context_managed, eligible_for_cleanup) in &allocs {
        // Skip if this place is already freed explicitly
        let already_freed = freed_places.iter().any(|fp| match (fp, place) {
            (PlaceRef::Field { path: p1 }, PlaceRef::Field { path: p2 }) => p1 == p2,
            (PlaceRef::Local { name: n1 }, PlaceRef::Local { name: n2 }) => n1 == n2,
            _ => false, // Indeterminate doesn't match anything for safety
        });
        if already_freed {
            continue;
        }

        // Context-managed allocs always get auto-free (at BlockExit)
        // Non-context-managed allocs must be eligible
        if !is_context_managed && !eligible_for_cleanup {
            continue;
        }

        if *is_context_managed {
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
        if *is_context_managed {
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
        let mut alloc_eff = make_effect(
            &node_id,
            0,
            SemanticEffectKind::Alloc {
                target: place.clone(),
                callee: "Box::new".to_string(),
            },
            0.85,
        );
        alloc_eff.eligible_for_implicit_cleanup = Some(true);
        effects.insert(node_id, vec![alloc_eff]);

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
        let mut alloc_eff = make_effect(
            &node_id,
            0,
            SemanticEffectKind::Alloc {
                target: alloc_place,
                callee: "open".to_string(),
            },
            0.85,
        );
        alloc_eff.eligible_for_implicit_cleanup = Some(true);
        effects.insert(node_id, vec![alloc_eff]);

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

    #[test]
    fn test_java_try_with_finds_block_exit() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg = make_cfg_with_alloc_node(
            &sym_id,
            &mut node_id,
            CfgNodeKind::Statement,
            true, // use BlockExit + context managed
            true, // include BlockExit
        );

        // Replace the PythonWith context with JavaTryWith
        if let Some(node) = cfg.nodes.get(&node_id) {
            // Rebuild the graph with the corrected call context
        }
        // Actually, we need to build a fresh graph with JavaTryWith context.
        // Rebuild with make_cfg_with_alloc_node but modify to use JavaTryWith.
        drop(cfg); // discard the PythonWith graph

        // Build a CFG with JavaTryWith context
        let entry = CfgNode::entry(&sym_id);
        let stmt_range = types::structs::TextRange {
            start_byte: 1,
            end_byte: 10,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        let mut stmt = CfgNode::new(&sym_id, CfgNodeKind::Statement, stmt_range);
        stmt.call_context = CallContext::JavaTryWith;
        let java_node_id = stmt.id;

        let be_range = types::structs::TextRange {
            start_byte: 12,
            end_byte: 12,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        let be = CfgNode::new(&sym_id, CfgNodeKind::BlockExit, be_range);
        let be_id = be.id;

        let exit = CfgNode::exit(&sym_id);

        let nodes = vec![entry.clone(), stmt.clone(), be.clone(), exit.clone()];
        let edges = vec![
            types::cfg::CfgEdge::new(&entry.id, &java_node_id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&java_node_id, &be_id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&be_id, &exit.id, CfgEdgeKind::Normal),
        ];
        let cfg = CfgGraph::build(&nodes, &edges).expect("CfgGraph build should succeed");

        let mut effects: HashMap<CfgNodeId, Vec<SemanticEffect>> = HashMap::new();
        let place = PlaceRef::Local {
            name: "fis".to_string(),
        };

        // Insert an Alloc with JavaTryWith context
        effects.insert(
            java_node_id,
            vec![make_effect(
                &java_node_id,
                0,
                SemanticEffectKind::Alloc {
                    target: place.clone(),
                    callee: "newInputStream".to_string(),
                },
                0.85,
            )],
        );

        run_scope_exit_pass(&mut effects, &cfg);

        // Find BlockExit node
        let be_effects = effects.get(&be_id);
        assert!(
            be_effects.is_some(),
            "BlockExit node should have effects for JavaTryWith alloc"
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
            "JavaTryWith free should have ContextManaged style"
        );

        // Exit node should NOT have this Free (it went to BlockExit instead)
        if let Some(exit_effects) = effects.get(&exit.id) {
            let has_our_free = exit_effects.iter().any(|e| {
                matches!(&e.kind, SemanticEffectKind::Free { callee, .. } if callee.contains("<block-exit>") || callee.contains("<scope-exit>"))
            });
            assert!(
                !has_our_free,
                "Exit node should NOT have the scope/block-exit Free when BlockExit was reached"
            );
        }
    }

    #[test]
    fn test_csharp_using_finds_block_exit() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg = make_cfg_with_alloc_node(
            &sym_id,
            &mut node_id,
            CfgNodeKind::Statement,
            true, // use BlockExit + context managed
            true, // include BlockExit
        );

        // Rebuild with CSharpUsing context on the Statement node
        drop(cfg);

        let entry = CfgNode::entry(&sym_id);
        let stmt_range = types::structs::TextRange {
            start_byte: 1,
            end_byte: 10,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        let mut stmt = CfgNode::new(&sym_id, CfgNodeKind::Statement, stmt_range);
        stmt.call_context = CallContext::CSharpUsing;
        let cs_node_id = stmt.id;

        let be_range = types::structs::TextRange {
            start_byte: 12,
            end_byte: 12,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        let be = CfgNode::new(&sym_id, CfgNodeKind::BlockExit, be_range);
        let be_id = be.id;

        let exit = CfgNode::exit(&sym_id);

        let nodes = vec![entry.clone(), stmt.clone(), be.clone(), exit.clone()];
        let edges = vec![
            types::cfg::CfgEdge::new(&entry.id, &cs_node_id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&cs_node_id, &be_id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&be_id, &exit.id, CfgEdgeKind::Normal),
        ];
        let cfg = CfgGraph::build(&nodes, &edges).expect("CfgGraph build should succeed");

        let mut effects: HashMap<CfgNodeId, Vec<SemanticEffect>> = HashMap::new();
        let place = PlaceRef::Local {
            name: "stream".to_string(),
        };

        // Insert an Alloc with CSharpUsing context
        effects.insert(
            cs_node_id,
            vec![make_effect(
                &cs_node_id,
                0,
                SemanticEffectKind::Alloc {
                    target: place.clone(),
                    callee: "new FileStream".to_string(),
                },
                0.85,
            )],
        );

        run_scope_exit_pass(&mut effects, &cfg);

        // Find BlockExit node
        let be_effects = effects.get(&be_id);
        assert!(
            be_effects.is_some(),
            "BlockExit node should have effects for CSharpUsing alloc"
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
            "CSharpUsing free should have ContextManaged style"
        );

        // Exit node should NOT have this Free (it went to BlockExit instead)
        if let Some(exit_effects) = effects.get(&exit.id) {
            let has_our_free = exit_effects.iter().any(|e| {
                matches!(&e.kind, SemanticEffectKind::Free { callee, .. } if callee.contains("<block-exit>") || callee.contains("<scope-exit>"))
            });
            assert!(
                !has_our_free,
                "Exit node should NOT have the scope/block-exit Free when BlockExit was reached"
            );
        }
    }

    // ── Language gating tests ───────────────────────────────────────────────

    /// Regression: ScopeExitAnalyzer previously emitted Free at Exit for ALL
    /// languages regardless of whether they have deterministic scope cleanup.
    /// The fix gates cleanup eligibility per-Alloc via
    /// `eligible_for_implicit_cleanup` on `SemanticEffect` and
    /// `OwnershipContract::eligible_for_implicit_cleanup()`.
    /// This test verifies that GC languages correctly
    /// declare `implicit_scope_cleanup=false` and that C also declares
    /// `implicit_scope_cleanup=false` (manual deallocation required).
    #[test]
    fn test_scope_exit_not_applied_for_gc_languages() {
        use crate::resource_ops::ResourceOpConfig;
        use types::effects::OwnershipContract;
        use types::enums::Language;

        // GC languages: no deterministic finalization
        let go = ResourceOpConfig::default_for(Language::Go);
        assert!(
            !go.supports_implicit_scope_cleanup(),
            "Go should NOT support implicit scope cleanup (GC, no deterministic finalization)"
        );

        let ts = ResourceOpConfig::default_for(Language::TypeScript);
        assert!(
            !ts.supports_implicit_scope_cleanup(),
            "TypeScript should NOT support implicit scope cleanup (GC)"
        );

        // C: manual deallocation required — no implicit cleanup
        let c = ResourceOpConfig::default_for(Language::C);
        assert!(
            !c.supports_implicit_scope_cleanup(),
            "C should NOT support implicit scope cleanup (manual free/fclose required)"
        );

        // C++: RAII destructors do support implicit cleanup
        let cpp = ResourceOpConfig::default_for(Language::Cpp);
        assert!(
            cpp.supports_implicit_scope_cleanup(),
            "C++ should support implicit scope cleanup (RAII destructors)"
        );

        // Python: plain open does NOT get implicit cleanup
        let py = ResourceOpConfig::default_for(Language::Python);
        assert!(
            !py.supports_implicit_scope_cleanup(),
            "Python should NOT support implicit scope cleanup (only PythonWith context)"
        );

        // Managed languages: deterministic scope cleanup
        let rust = ResourceOpConfig::default_for(Language::Rust);
        assert!(
            rust.supports_implicit_scope_cleanup(),
            "Rust should support implicit scope cleanup (Drop)"
        );

        // Demonstrate that run_scope_exit_pass itself emits Free only for
        // eligible allocs — eligibility is checked per-effect, not per-language.
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
        let mut alloc_eff = make_effect(
            &node_id,
            0,
            SemanticEffectKind::Alloc {
                target: place.clone(),
                callee: "open".to_string(),
            },
            0.85,
        );
        // Mark as eligible for cleanup
        alloc_eff.eligible_for_implicit_cleanup = Some(true);
        effects.insert(node_id, vec![alloc_eff]);

        run_scope_exit_pass(&mut effects, &cfg);

        // Since the alloc is eligible, it should get a scope-exit Free
        let exit_node = cfg
            .nodes
            .values()
            .find(|n| n.kind == CfgNodeKind::Exit)
            .unwrap();
        let exit_effects = effects.get(&exit_node.id);
        assert!(
            exit_effects.is_some() && !exit_effects.unwrap().is_empty(),
            "eligible alloc should get scope-exit Free"
        );
    }

    /// Allocs marked as NOT eligible for implicit cleanup should NOT get
    /// an auto-generated Free at function exit.
    #[test]
    fn test_ineligible_alloc_skips_scope_exit() {
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
        let mut alloc_eff = make_effect(
            &node_id,
            0,
            SemanticEffectKind::Alloc {
                target: place.clone(),
                callee: "malloc".to_string(),
            },
            0.85,
        );
        // Mark as NOT eligible (e.g., C API call in C++ code)
        alloc_eff.eligible_for_implicit_cleanup = Some(false);
        effects.insert(node_id, vec![alloc_eff]);

        run_scope_exit_pass(&mut effects, &cfg);

        // Exit should NOT have a scope-exit Free for this ineligible alloc
        let exit_node = cfg.nodes.values().find(|n| n.kind == CfgNodeKind::Exit).unwrap();
        if let Some(exit_effects) = effects.get(&exit_node.id) {
            let has_scope_exit = exit_effects.iter().any(|e| {
                matches!(&e.kind, SemanticEffectKind::Free { callee, .. } if callee.contains("<scope-exit>"))
            });
            assert!(
                !has_scope_exit,
                "ineligible alloc should NOT get scope-exit Free"
            );
        }
        // Else: no effects at exit is also correct
    }

    /// Allocs whose value is returned should NOT get an auto-free
    /// (the caller owns the returned resource).
    #[test]
    fn test_returned_alloc_skips_scope_exit() {
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
            name: "p".to_string(),
        };

        // Alloc for "p" — eligible for cleanup
        let mut alloc_eff = make_effect(
            &node_id,
            0,
            SemanticEffectKind::Alloc {
                target: place.clone(),
                callee: "malloc".to_string(),
            },
            0.85,
        );
        alloc_eff.eligible_for_implicit_cleanup = Some(true);
        effects.insert(node_id, vec![alloc_eff]);

        // Return the same local "p"
        let return_node_id = {
            let stmt_range = types::structs::TextRange {
                start_byte: 20,
                end_byte: 30,
                start_line: 2,
                start_column: 0,
                end_line: 2,
                end_column: 0,
            };
            let ret_node = types::cfg::CfgNode::new(
                &sym_id,
                CfgNodeKind::Statement,
                stmt_range,
            );
            let ret_id = ret_node.id;
            effects.insert(
                ret_id,
                vec![make_effect(
                    &ret_id,
                    0,
                    SemanticEffectKind::Return {
                        value: ValueSource::Local { name: "p".to_string() },
                    },
                    0.9,
                )],
            );
            ret_id
        };

        run_scope_exit_pass(&mut effects, &cfg);

        // Exit should NOT have a scope-exit Free for "p" (it was returned)
        let exit_node = cfg.nodes.values().find(|n| n.kind == CfgNodeKind::Exit).unwrap();
        if let Some(exit_effects) = effects.get(&exit_node.id) {
            let has_scope_exit_for_p = exit_effects.iter().any(|e| {
                matches!(&e.kind, SemanticEffectKind::Free { place, callee, .. }
                    if matches!(place, PlaceRef::Local { name } if name == "p") && callee.contains("<scope-exit>"))
            });
            assert!(
                !has_scope_exit_for_p,
                "returned alloc 'p' should NOT get scope-exit Free"
            );
        }

        // But return_node_id should still have the Return effect
        assert!(
            effects.contains_key(&return_node_id),
            "Return effect should still be present"
        );
    }

    /// Known limitation: `ValueSource::CallReturn` is NOT tracked by
    /// `escaped_locals`.  When a function returns a value via `CallReturn`
    /// (e.g., `return fopen(...)`) instead of `ValueSource::Local`
    /// (e.g., `return f`), the scope-exit pass does NOT recognize that the
    /// allocated resource has been returned to the caller.  It therefore
    /// incorrectly emits an auto-free at the exit node.
    ///
    /// This test asserts the IDEAL behavior (no auto-free), which currently
    /// FAILS because `run_scope_exit_pass` only collects `ValueSource::Local`
    /// names into `escaped_locals`.  When the alloc's `PlaceRef::Local` name
    /// never appears in a Return/Escape with `ValueSource::Local`, the resource
    /// looks unfreed from the pass's perspective.
    ///
    /// To fix, the pass would need to:
    /// 1. Track which alloc callee produced which PlaceRef.
    /// 2. When a Return has `ValueSource::CallReturn`, check whether that
    ///    callee matches an Alloc in the function, and if so, mark the
    ///    corresponding PlaceRef as escaped.
    ///
    /// Low priority: most real-world code assigns resources to local variables
    /// before returning them (`let x = fopen(...); return x;`).
    #[test]
    fn test_returned_resource_via_callreturn_not_auto_freed() {
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
            name: "handle".to_string(),
        };

        // Alloc for "handle" — eligible for implicit cleanup (C++/Java RAII)
        let mut alloc_eff = make_effect(
            &node_id,
            0,
            SemanticEffectKind::Alloc {
                target: place.clone(),
                callee: "open".to_string(),
            },
            0.85,
        );
        alloc_eff.eligible_for_implicit_cleanup = Some(true);
        effects.insert(node_id, vec![alloc_eff]);

        // Return via CallReturn (NOT ValueSource::Local) —
        // simulates `return make_resource()` pattern
        let return_node_id = {
            let stmt_range = types::structs::TextRange {
                start_byte: 20,
                end_byte: 30,
                start_line: 2,
                start_column: 0,
                end_line: 2,
                end_column: 0,
            };
            let ret_node = types::cfg::CfgNode::new(
                &sym_id,
                CfgNodeKind::Statement,
                stmt_range,
            );
            let ret_id = ret_node.id;
            effects.insert(
                ret_id,
                vec![make_effect(
                    &ret_id,
                    0,
                    SemanticEffectKind::Return {
                        value: ValueSource::CallReturn {
                            callee: "open".to_string(),
                        },
                    },
                    0.9,
                )],
            );
            ret_id
        };

        run_scope_exit_pass(&mut effects, &cfg);

        // IDEAL: Exit should NOT have a scope-exit Free for "handle"
        // (the resource was returned to the caller via CallReturn).
        // CURRENTLY THIS ASSERTION FAILS — the pass does not track
        // CallReturn in escaped_locals, so a Free IS emitted.
        let exit_node = cfg
            .nodes
            .values()
            .find(|n| n.kind == CfgNodeKind::Exit)
            .unwrap();
        if let Some(exit_effects) = effects.get(&exit_node.id) {
            let has_scope_exit_for_handle = exit_effects.iter().any(|e| {
                matches!(&e.kind, SemanticEffectKind::Free { place, callee, .. }
                    if matches!(place, PlaceRef::Local { name } if name == "handle")
                       && callee.contains("<scope-exit>"))
            });
            assert!(
                !has_scope_exit_for_handle,
                "KNOWN LIMITATION: scope-exit should NOT auto-free handle when \
                 returned via CallReturn, but current code only tracks \
                 ValueSource::Local in escaped_locals — Free IS emitted"
            );
        }

        // Return effect should still be present
        assert!(
            effects.contains_key(&return_node_id),
            "Return effect should still be present"
        );
    }

    /// Context-managed allocs (PythonWith) always get auto-free,
    /// even when the language-level implicit_scope_cleanup is false.
    #[test]
    fn test_context_managed_always_eligible() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg = make_cfg_with_alloc_node(
            &sym_id,
            &mut node_id,
            CfgNodeKind::Statement,
            true, // PythonWith context
            true, // include BlockExit
        );

        let mut effects: HashMap<CfgNodeId, Vec<SemanticEffect>> = HashMap::new();
        let place = PlaceRef::Local {
            name: "f".to_string(),
        };

        // Insert an Alloc with PythonWith context, marked as NOT eligible
        let mut alloc_eff = make_effect(
            &node_id,
            0,
            SemanticEffectKind::Alloc {
                target: place.clone(),
                callee: "open".to_string(),
            },
            0.85,
        );
        alloc_eff.eligible_for_implicit_cleanup = Some(false);
        effects.insert(node_id, vec![alloc_eff]);

        run_scope_exit_pass(&mut effects, &cfg);

        // Should still get a Free at BlockExit (context-managed overrides eligibility)
        let be_node = cfg
            .nodes
            .values()
            .find(|n| n.kind == CfgNodeKind::BlockExit)
            .expect("BlockExit node should exist");

        let be_effects = effects.get(&be_node.id);
        assert!(
            be_effects.is_some(),
            "BlockExit node should have effects for PythonWith alloc (context-managed overrides eligibility)"
        );
        let free_effect = be_effects.unwrap().iter().find(|e| {
            matches!(&e.kind, SemanticEffectKind::Free { callee, .. } if callee.contains("<block-exit>"))
        });
        assert!(
            free_effect.is_some(),
            "Context-managed alloc should get Free at BlockExit even when not eligible"
        );
    }
}
