//! Scope-exit post-pass: emits heuristic implicit Free effects for Rust Drop at function exit,
//! Python `with`, Java `try-with-resources`, C# `using`, Kotlin `.use`, and Ruby
//! block-managed context blocks at BlockExit.
//!
//! Runs AFTER `compose_effects` main loop. For each Alloc effect without a matching
//! explicit Free effect, emits a Free:
//! - At every owner-matched BlockExit clone for context-managed Allocs
//! - At the function's Exit node for other eligible Allocs
//!
//! ## Eligibility for scope-exit cleanup
//! An Alloc is eligible for auto-cleanup only if ALL of:
//! 1. The effect's `eligible_for_implicit_cleanup` is `Some(true)` (or `None` for
//!    backward compat), OR the Alloc node has a context-managed call context
//!    (PythonWith / JavaTryWith / CSharpUsing / KotlinUse / RubyBlock).
//! 2. The allocated PlaceRef does NOT appear in any Return or Escape effect
//!    (a resource that is returned or escapes the function should not be
//!    auto-freed — the caller or escape target owns it).
//!
//! For Rust this is not path-sensitive lexical RAII: branch-local construction,
//! moves, partial moves, and exact nested-scope drop points are not represented.
//! Context-managed Python/Java/C#/Kotlin/Ruby resources instead use owner-matched
//! BlockExit nodes. The pass runs for all languages as a near-no-op when no
//! Allocs are eligible.

use std::collections::{HashMap, HashSet};

use types::effects::{ConsumptionStyle, PlaceRef, SemanticEffect, SemanticEffectKind, ValueSource};
use types::enums::{CallContext, CfgEdgeKind, CfgNodeKind};
use types::ids::CfgNodeId;

use super::cfg_graph::CfgGraph;
use super::effect_composer::make_effect;

/// Post-pass that emits implicit Free effects for allocations that have no
/// explicit Free within the same function (e.g., heuristic Rust Drop at function exit,
/// Python context-manager `with` statement, Ruby block resources).
///
/// Eligibility is gated per-Alloc:
/// - Context-managed Allocs always get a Free at every path-isolated BlockExit
///   clone owned by the same lexical resource scope.
/// - Other Allocs only get a Free if `eligible_for_implicit_cleanup` is true
///   (or absent on legacy effects) AND the allocated place is NOT returned or
///   escaped.
pub fn run_scope_exit_pass(effects: &mut HashMap<CfgNodeId, Vec<SemanticEffect>>, cfg: &CfgGraph) {
    // 0. Collect Return/Escape value names (to skip auto-free for escaped resources)
    let mut escaped_locals: HashSet<String> = HashSet::new();
    let mut escaped_call_returns: HashSet<String> = HashSet::new();
    for node_effects in effects.values() {
        for effect in node_effects {
            match &effect.kind {
                SemanticEffectKind::Return { value } => {
                    collect_escaped_value(value, &mut escaped_locals, &mut escaped_call_returns);
                }
                SemanticEffectKind::Escape { value, .. } => {
                    collect_escaped_value(value, &mut escaped_locals, &mut escaped_call_returns);
                }
                _ => {}
            }
        }
    }

    // 1. Collect all Allocs and their associated Free places
    let mut allocs: Vec<(CfgNodeId, u32, PlaceRef, String, Option<u32>, bool)> = Vec::new();
    // (node_id, effect_order, place, callee, managed_scope_start_byte, eligible)
    let mut freed_places: Vec<PlaceRef> = Vec::new();

    for node_effects in effects.values() {
        for effect in node_effects {
            match &effect.kind {
                SemanticEffectKind::Alloc { target, callee } => {
                    let managed_scope_start_byte =
                        managed_scope_for_alloc(&effect.cfg_node_id, cfg, 3);

                    // Per-effect eligibility (backward compat: None = eligible)
                    let eligible = effect.eligible_for_implicit_cleanup.unwrap_or(true);

                    // Check if the allocated place is returned or escaped
                    let is_escaped = escaped_call_returns.contains(callee)
                        || match target {
                            PlaceRef::Local { name } => escaped_locals.contains(name),
                            PlaceRef::Field { .. } | PlaceRef::Indeterminate => false,
                        };

                    allocs.push((
                        effect.cfg_node_id,
                        effect.order,
                        target.clone(),
                        callee.clone(),
                        managed_scope_start_byte,
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

    // Scope cleanup is LIFO. Stable source/effect ordering also prevents the
    // HashMap traversal order above from changing the persisted effect vector.
    allocs.sort_by(|left, right| {
        let left_start = cfg
            .nodes
            .get(&left.0)
            .map(|node| node.stmt_range.start_byte)
            .unwrap_or_default();
        let right_start = cfg
            .nodes
            .get(&right.0)
            .map(|node| node.stmt_range.start_byte)
            .unwrap_or_default();
        right_start
            .cmp(&left_start)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.0.cmp(&right.0))
    });

    // 2. Find Exit node (for non-context-managed allocs)
    let exit_node = cfg.nodes.values().find(|n| n.kind == CfgNodeKind::Exit);
    let Some(exit) = exit_node else {
        return;
    };

    // 3. Emit Free for each allocation that has no matching explicit Free
    for (
        _alloc_node_id,
        _effect_order,
        place,
        callee,
        managed_scope_start_byte,
        eligible_for_cleanup,
    ) in &allocs
    {
        // Skip if this place is already freed explicitly
        let already_freed = freed_places.iter().any(|fp| match (fp, place) {
            (PlaceRef::Field { path: p1 }, PlaceRef::Field { path: p2 }) => p1 == p2,
            (PlaceRef::Local { name: n1 }, PlaceRef::Local { name: n2 }) => n1 == n2,
            _ => false, // Indeterminate doesn't match anything for safety
        });
        // Managed constructs execute their exit protocol regardless of an
        // explicit close/free in the body. A function-global Free match must
        // not erase cleanup from sibling paths (or hide a possible double
        // close on the path where the explicit operation occurred).
        if managed_scope_start_byte.is_none() && already_freed {
            continue;
        }

        // Context-managed allocs always get auto-free (at BlockExit)
        // Non-context-managed allocs must be eligible
        if managed_scope_start_byte.is_none() && !eligible_for_cleanup {
            continue;
        }

        if let Some(scope_start_byte) = managed_scope_start_byte {
            let block_exit_ids = managed_block_exits(*scope_start_byte, cfg);
            if !block_exit_ids.is_empty() {
                for block_exit_id in block_exit_ids {
                    let exit_effects = effects.entry(block_exit_id).or_default();
                    let mut new_effect = make_effect(
                        &block_exit_id,
                        exit_effects.len() as u32,
                        SemanticEffectKind::Free {
                            place: place.clone(),
                            callee: format!("<block-exit>{callee}"),
                        },
                        0.80,
                    );
                    new_effect.consumption_style = Some(ConsumptionStyle::ContextManaged);
                    exit_effects.push(new_effect);
                }
                continue;
            }
            // A malformed/incomplete CFG may lack its owned BlockExit; retain a
            // visible function-exit fallback rather than dropping cleanup.
        }

        // Default: emit at function Exit
        let exit_effects = effects.entry(exit.id).or_default();
        let mut new_effect = make_effect(
            &exit.id,
            exit_effects.len() as u32,
            SemanticEffectKind::Free {
                place: place.clone(),
                callee: format!("<scope-exit>{callee}"),
            },
            0.70,
        );
        if managed_scope_start_byte.is_some() {
            new_effect.consumption_style = Some(ConsumptionStyle::ContextManaged);
        }
        exit_effects.push(new_effect);
    }
}

fn collect_escaped_value(
    value: &ValueSource,
    escaped_locals: &mut HashSet<String>,
    escaped_call_returns: &mut HashSet<String>,
) {
    match value {
        ValueSource::Local { name } => {
            escaped_locals.insert(name.clone());
        }
        ValueSource::CallReturn { callee } => {
            escaped_call_returns.insert(callee.clone());
        }
        ValueSource::Param { .. } | ValueSource::LiteralNull | ValueSource::Unknown => {}
    }
}

fn is_managed_context(context: CallContext) -> bool {
    matches!(
        context,
        CallContext::PythonWith
            | CallContext::JavaTryWith
            | CallContext::CSharpUsing
            | CallContext::RubyBlock
            | CallContext::KotlinUse
    )
}

/// Resolve the lexical managed-scope owner of an allocation. Kotlin may place
/// the allocation on a preceding statement, so a bounded straight-line bridge
/// to the `.use` call is retained.
fn managed_scope_for_alloc(start: &CfgNodeId, cfg: &CfgGraph, max_hops: usize) -> Option<u32> {
    let node = cfg.nodes.get(start)?;
    if is_managed_context(node.call_context) {
        return node.managed_scope_start_byte;
    }
    if max_hops == 0 {
        return None;
    }
    let normal_successors: Vec<_> = cfg
        .successors
        .get(start)?
        .iter()
        .filter(|edge| edge.kind == CfgEdgeKind::Normal)
        .collect();
    if normal_successors.len() != 1 {
        return None;
    }
    let successor = normal_successors[0].target;
    let target = cfg.nodes.get(&successor)?;
    if target.call_context == CallContext::KotlinUse {
        return target.managed_scope_start_byte;
    }
    managed_scope_for_alloc(&successor, cfg, max_hops - 1)
}

fn managed_block_exits(scope_start_byte: u32, cfg: &CfgGraph) -> Vec<CfgNodeId> {
    let mut exits: Vec<_> = cfg
        .nodes
        .values()
        .filter(|node| {
            node.kind == CfgNodeKind::BlockExit
                && node.managed_scope_start_byte == Some(scope_start_byte)
        })
        .map(|node| node.id)
        .collect();
    exits.sort_unstable();
    exits
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
            stmt.managed_scope_start_byte = Some(10);
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
            let mut be = CfgNode::new(sym_id, CfgNodeKind::BlockExit, be_range);
            if alloc_has_python_with {
                be.call_context = CallContext::PythonWith;
                be.managed_scope_start_byte = Some(10);
            }
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
    fn owner_matching_covers_all_clones_without_crossing_nested_scopes() {
        let sym_id = make_sym_id();
        let entry = CfgNode::entry(&sym_id);
        let exit = CfgNode::exit(&sym_id);
        let range = |byte| types::structs::TextRange {
            start_byte: byte,
            end_byte: byte + 1,
            start_line: 0,
            start_column: byte,
            end_line: 0,
            end_column: byte + 1,
        };

        let mut outer_alloc = CfgNode::new(&sym_id, CfgNodeKind::Statement, range(10));
        outer_alloc.call_context = CallContext::PythonWith;
        outer_alloc.managed_scope_start_byte = Some(10);
        let mut inner_alloc = CfgNode::new(&sym_id, CfgNodeKind::Statement, range(20));
        inner_alloc.call_context = CallContext::PythonWith;
        inner_alloc.managed_scope_start_byte = Some(20);
        let mut inner_exit = CfgNode::new(&sym_id, CfgNodeKind::BlockExit, range(30));
        inner_exit.call_context = CallContext::PythonWith;
        inner_exit.managed_scope_start_byte = Some(20);
        let mut outer_exit_one =
            CfgNode::new_with_instance(&sym_id, CfgNodeKind::BlockExit, range(40), 1);
        outer_exit_one.call_context = CallContext::PythonWith;
        outer_exit_one.managed_scope_start_byte = Some(10);
        let mut outer_exit_two =
            CfgNode::new_with_instance(&sym_id, CfgNodeKind::BlockExit, range(40), 2);
        outer_exit_two.call_context = CallContext::PythonWith;
        outer_exit_two.managed_scope_start_byte = Some(10);

        let nodes = vec![
            entry.clone(),
            outer_alloc.clone(),
            inner_alloc.clone(),
            inner_exit.clone(),
            outer_exit_one.clone(),
            outer_exit_two.clone(),
            exit.clone(),
        ];
        let edges = vec![
            types::cfg::CfgEdge::new(&entry.id, &outer_alloc.id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&outer_alloc.id, &inner_alloc.id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&inner_alloc.id, &inner_exit.id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&inner_exit.id, &outer_exit_one.id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&outer_alloc.id, &outer_exit_two.id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&outer_exit_one.id, &exit.id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&outer_exit_two.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let cfg = CfgGraph::build(&nodes, &edges).expect("valid nested managed-scope CFG");
        let mut effects = HashMap::from([
            (
                outer_alloc.id,
                vec![make_effect(
                    &outer_alloc.id,
                    0,
                    SemanticEffectKind::Alloc {
                        target: PlaceRef::Local {
                            name: "outer".into(),
                        },
                        callee: "open_outer".into(),
                    },
                    0.9,
                )],
            ),
            (
                inner_alloc.id,
                vec![make_effect(
                    &inner_alloc.id,
                    0,
                    SemanticEffectKind::Alloc {
                        target: PlaceRef::Local {
                            name: "inner".into(),
                        },
                        callee: "open_inner".into(),
                    },
                    0.9,
                )],
            ),
        ]);

        run_scope_exit_pass(&mut effects, &cfg);

        let freed_names = |node_id| {
            effects
                .get(&node_id)
                .into_iter()
                .flatten()
                .filter_map(|effect| match &effect.kind {
                    SemanticEffectKind::Free {
                        place: PlaceRef::Local { name },
                        ..
                    } => Some(name.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(freed_names(inner_exit.id), vec!["inner"]);
        assert_eq!(freed_names(outer_exit_one.id), vec!["outer"]);
        assert_eq!(freed_names(outer_exit_two.id), vec!["outer"]);
    }

    #[test]
    fn managed_scope_cleanup_is_deterministic_lifo() {
        let sym_id = make_sym_id();
        let entry = CfgNode::entry(&sym_id);
        let exit = CfgNode::exit(&sym_id);
        let range = |byte| types::structs::TextRange {
            start_byte: byte,
            end_byte: byte + 1,
            start_line: 0,
            start_column: byte,
            end_line: 0,
            end_column: byte + 1,
        };
        let mut first = CfgNode::new(&sym_id, CfgNodeKind::Statement, range(10));
        first.call_context = CallContext::JavaTryWith;
        first.managed_scope_start_byte = Some(5);
        let mut second = CfgNode::new(&sym_id, CfgNodeKind::Statement, range(20));
        second.call_context = CallContext::JavaTryWith;
        second.managed_scope_start_byte = Some(5);
        let mut block_exit = CfgNode::new(&sym_id, CfgNodeKind::BlockExit, range(30));
        block_exit.call_context = CallContext::JavaTryWith;
        block_exit.managed_scope_start_byte = Some(5);
        let nodes = vec![
            entry.clone(),
            first.clone(),
            second.clone(),
            block_exit.clone(),
            exit.clone(),
        ];
        let edges = vec![
            types::cfg::CfgEdge::new(&entry.id, &first.id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&first.id, &second.id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&second.id, &block_exit.id, CfgEdgeKind::Normal),
            types::cfg::CfgEdge::new(&block_exit.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let cfg = CfgGraph::build(&nodes, &edges).expect("valid multi-resource CFG");
        let alloc = |node: &CfgNode, order, name: &str| {
            make_effect(
                &node.id,
                order,
                SemanticEffectKind::Alloc {
                    target: PlaceRef::Local { name: name.into() },
                    callee: format!("open_{name}"),
                },
                0.9,
            )
        };
        // Deliberately insert in acquisition order; HashMap iteration must not
        // determine the resulting cleanup order.
        let mut effects = HashMap::from([
            (first.id, vec![alloc(&first, 0, "first")]),
            (
                second.id,
                vec![alloc(&second, 0, "second_a"), alloc(&second, 1, "second_b")],
            ),
        ]);

        run_scope_exit_pass(&mut effects, &cfg);

        let freed_names: Vec<_> = effects[&block_exit.id]
            .iter()
            .filter_map(|effect| match &effect.kind {
                SemanticEffectKind::Free {
                    place: PlaceRef::Local { name },
                    ..
                } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(freed_names, vec!["second_b", "second_a", "first"]);
        assert_eq!(effects[&block_exit.id][0].order, 0);
        assert_eq!(effects[&block_exit.id][1].order, 1);
        assert_eq!(effects[&block_exit.id][2].order, 2);
    }

    #[test]
    fn test_alloc_with_explicit_free_no_scope_exit() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg =
            make_cfg_with_alloc_node(&sym_id, &mut node_id, CfgNodeKind::Statement, false, false);

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
        let exit = cfg
            .nodes
            .values()
            .find(|n| n.kind == CfgNodeKind::Exit)
            .unwrap();
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
    fn managed_scope_exit_is_not_suppressed_by_explicit_free_elsewhere() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg =
            make_cfg_with_alloc_node(&sym_id, &mut node_id, CfgNodeKind::Statement, true, true);
        let place = PlaceRef::Local { name: "f".into() };
        let mut effects = HashMap::from([(
            node_id,
            vec![
                make_effect(
                    &node_id,
                    0,
                    SemanticEffectKind::Alloc {
                        target: place.clone(),
                        callee: "open".into(),
                    },
                    0.85,
                ),
                make_effect(
                    &node_id,
                    1,
                    SemanticEffectKind::Free {
                        place,
                        callee: "close".into(),
                    },
                    0.85,
                ),
            ],
        )]);

        run_scope_exit_pass(&mut effects, &cfg);

        let block_exit = cfg
            .nodes
            .values()
            .find(|node| node.kind == CfgNodeKind::BlockExit)
            .expect("managed CFG BlockExit");
        assert!(effects.get(&block_exit.id).is_some_and(|node_effects| {
            node_effects.iter().any(|effect| {
                matches!(effect.kind, SemanticEffectKind::Free { .. })
                    && effect.consumption_style == Some(ConsumptionStyle::ContextManaged)
            })
        }));
    }

    #[test]
    fn test_unfreed_alloc_gets_scope_exit_free() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg =
            make_cfg_with_alloc_node(&sym_id, &mut node_id, CfgNodeKind::Statement, false, false);

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
        let exit_node = cfg
            .nodes
            .values()
            .find(|n| n.kind == CfgNodeKind::Exit)
            .unwrap();
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
                    "callee should contain <scope-exit> prefix, got: {callee}"
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
            true, // PythonWith context
            true, // include BlockExit
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
                    "callee should contain <block-exit> prefix, got: {callee}"
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
        let exit_node = cfg
            .nodes
            .values()
            .find(|n| n.kind == CfgNodeKind::Exit)
            .unwrap();
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
        let cfg =
            make_cfg_with_alloc_node(&sym_id, &mut node_id, CfgNodeKind::Statement, false, false);

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
        let exit_node = cfg
            .nodes
            .values()
            .find(|n| n.kind == CfgNodeKind::Exit)
            .unwrap();
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

        // Replace the PythonWith context with JavaTryWith — WIP: build a fresh
        // graph with JavaTryWith context through make_cfg_with_alloc_node.
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
        stmt.managed_scope_start_byte = Some(10);
        let java_node_id = stmt.id;

        let be_range = types::structs::TextRange {
            start_byte: 12,
            end_byte: 12,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        let mut be = CfgNode::new(&sym_id, CfgNodeKind::BlockExit, be_range);
        be.call_context = CallContext::JavaTryWith;
        be.managed_scope_start_byte = Some(10);
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
                    "callee should contain <block-exit> prefix, got: {callee}"
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
        stmt.managed_scope_start_byte = Some(10);
        let cs_node_id = stmt.id;

        let be_range = types::structs::TextRange {
            start_byte: 12,
            end_byte: 12,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
        let mut be = CfgNode::new(&sym_id, CfgNodeKind::BlockExit, be_range);
        be.call_context = CallContext::CSharpUsing;
        be.managed_scope_start_byte = Some(10);
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
                    "callee should contain <block-exit> prefix, got: {callee}"
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
        let cfg =
            make_cfg_with_alloc_node(&sym_id, &mut node_id, CfgNodeKind::Statement, false, false);

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
        let cfg =
            make_cfg_with_alloc_node(&sym_id, &mut node_id, CfgNodeKind::Statement, false, false);

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
        let exit_node = cfg
            .nodes
            .values()
            .find(|n| n.kind == CfgNodeKind::Exit)
            .unwrap();
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
        let cfg =
            make_cfg_with_alloc_node(&sym_id, &mut node_id, CfgNodeKind::Statement, false, false);

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
            let ret_node = types::cfg::CfgNode::new(&sym_id, CfgNodeKind::Statement, stmt_range);
            let ret_id = ret_node.id;
            effects.insert(
                ret_id,
                vec![make_effect(
                    &ret_id,
                    0,
                    SemanticEffectKind::Return {
                        value: ValueSource::Local {
                            name: "p".to_string(),
                        },
                    },
                    0.9,
                )],
            );
            ret_id
        };

        run_scope_exit_pass(&mut effects, &cfg);

        // Exit should NOT have a scope-exit Free for "p" (it was returned)
        let exit_node = cfg
            .nodes
            .values()
            .find(|n| n.kind == CfgNodeKind::Exit)
            .unwrap();
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

    /// A directly returned allocation transfers ownership to the caller and
    /// must not receive an implicit scope-exit free.
    #[test]
    fn test_returned_resource_via_callreturn_not_auto_freed() {
        let sym_id = make_sym_id();
        let mut node_id = CfgNodeId::generate(&sym_id, "dummy", 0);
        let cfg =
            make_cfg_with_alloc_node(&sym_id, &mut node_id, CfgNodeKind::Statement, false, false);

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

        // Return via CallReturn simulates `return make_resource()`.
        let return_node_id = {
            let stmt_range = types::structs::TextRange {
                start_byte: 20,
                end_byte: 30,
                start_line: 2,
                start_column: 0,
                end_line: 2,
                end_column: 0,
            };
            let ret_node = types::cfg::CfgNode::new(&sym_id, CfgNodeKind::Statement, stmt_range);
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

        // Exit should not free "handle": ownership moved to the caller.
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
                "scope-exit must not auto-free a resource returned via CallReturn"
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
