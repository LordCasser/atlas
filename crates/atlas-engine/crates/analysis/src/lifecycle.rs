//! Field Lifecycle Analysis — path-sensitive fixpoint analysis on CfgGraph.
//!
//! Given a function's CFG (nodes + edges) and a target field path, runs a
//! monotone dataflow fixpoint to trace the field's lifecycle through all
//! control-flow paths, accounting for branches and loop convergence.

use std::collections::{HashMap, VecDeque};

use types::cfg::{CfgEdge, CfgNode};
use types::effects::{PlaceRef, SemanticEffect, SemanticEffectKind};
use types::enums::{CfgEdgeKind, CfgNodeKind, EffectKind};
use types::ids::CfgNodeId;

use super::lifecycle_proof::EvidenceLevel;
use super::ownership_rules::CppOwnershipRules;
use crate::cfg_graph::CfgGraph;

type TransitionTrace = Vec<(FieldState, FieldState, Option<EffectKind>)>;

// ── Budget constants ────────────────────────────────────────────────────

/// Maximum total node visits before giving up (safety cap).
const MAX_VISITS: usize = 500;
/// Maximum times a single node can be re-visited (loop convergence cap).
const MAX_VISITS_PER_NODE: u32 = 10;

// ── State types ─────────────────────────────────────────────────────────

/// States a field can be in during its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldState {
    Unknown,
    MaybeLive,
    Assigned,
    Freed,
    Nullified,
    Escaped,
    Returned,
    Invalidated,
    /// Path-sensitive merge result: one path freed, other assigned.
    MaybeFreed,
    /// Path-sensitive merge result: one path assigned, other unknown.
    MaybeAssigned,
}

impl FieldState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::MaybeLive => "maybe_live",
            Self::Assigned => "assigned",
            Self::Freed => "freed",
            Self::Nullified => "nullified",
            Self::Escaped => "escaped",
            Self::Returned => "returned",
            Self::Invalidated => "invalidated",
            Self::MaybeFreed => "maybe_freed",
            Self::MaybeAssigned => "maybe_assigned",
        }
    }
}

/// Represents which branch path a transition occurred on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchPath {
    TruePath,
    FalsePath,
}

/// One frame in a nested branch context stack.
#[derive(Debug, Clone)]
pub struct BranchFrame {
    pub branch_node_line: u32,
    pub path: BranchPath,
}

/// Lattice element with top/bottom for fixpoint analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LatticeState {
    Bottom,
    State(FieldState),
    Top, // budget-exhausted over-approximation
}

impl LatticeState {
    fn as_state(&self) -> Option<FieldState> {
        match self {
            LatticeState::State(s) => Some(*s),
            _ => None,
        }
    }
}

// ── Result types ────────────────────────────────────────────────────────

/// A single state transition in a field's lifecycle.
#[derive(Debug, Clone)]
pub struct FieldTransition {
    pub from_state: FieldState,
    pub to_state: FieldState,
    pub node_id: CfgNodeId,
    pub node_line: u32,
    pub effect: Option<EffectKind>,
    pub branch_frames: Vec<BranchFrame>,
}

/// Result of field lifecycle analysis.
#[derive(Debug, Clone)]
pub struct FieldLifecycleResult {
    pub field_path: String,
    pub function_qname: String,
    pub transitions: Vec<FieldTransition>,
    pub final_state: FieldState,
    pub suspicious_points: Vec<SuspiciousPoint>,
    /// Whether the analysis exceeded its budget and returned partial results.
    pub partial: bool,
    /// Evidence level for the conclusion.
    pub evidence_level: EvidenceLevel,
    /// State at the function exit node, if reachable.
    pub exit_state: Option<FieldState>,
}

/// A point in the lifecycle that may indicate a bug.
#[derive(Debug, Clone)]
pub struct SuspiciousPoint {
    pub line: u32,
    pub kind: SuspiciousKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuspiciousKind {
    UseAfterFree,
    DoubleFree,
    MissingFree,
    NullDeref,
}

/// Ownership rules for C/C++ lifecycle analysis.
#[derive(Debug, Clone, Default)]
pub struct OwnershipRules {
    pub track_field_based: bool,
}

// ── Engine ──────────────────────────────────────────────────────────────

/// Engine for field-level lifecycle analysis.
pub struct FieldLifecycleEngine;

impl FieldLifecycleEngine {
    /// Analyze the lifecycle of a specific field within a function's CFG.
    ///
    /// Delegates to `analyze_with_rules` using default CppOwnershipRules.
    pub fn analyze_field_lifecycle(
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        field_path: &str,
        _rules: &OwnershipRules,
    ) -> FieldLifecycleResult {
        let rules = CppOwnershipRules::default();
        Self::analyze_with_rules(cfg_nodes, cfg_edges, field_path, _rules, &rules)
    }

    /// Analyze with domain rules — uses rule-backed function matching.
    ///
    /// Runs a path-sensitive fixpoint analysis on the CfgGraph. Entry starts
    /// as `Unknown`; the fixpoint propagates lattice states through the graph
    /// using merge over predecessors and transfer over node effects.
    pub fn analyze_with_rules(
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        field_path: &str,
        _ownership_rules: &OwnershipRules,
        rules: &CppOwnershipRules,
    ) -> FieldLifecycleResult {
        // Build graph
        let graph = match CfgGraph::build(cfg_nodes, cfg_edges) {
            Ok(g) => g,
            Err(_) => {
                return FieldLifecycleResult {
                    field_path: field_path.to_string(),
                    function_qname: String::new(),
                    transitions: vec![],
                    suspicious_points: vec![],
                    final_state: FieldState::Unknown,
                    partial: true,
                    evidence_level: EvidenceLevel::Incomplete,
                    exit_state: None,
                };
            }
        };

        // Canonicalize field path for matching
        let canonical_target = types::structs::canonicalize_field_path(field_path);

        // Lattice state per node (out_state), initialized to BOTTOM everywhere
        let mut out_state: HashMap<CfgNodeId, LatticeState> = HashMap::new();
        for nid in graph.nodes.keys() {
            out_state.insert(*nid, LatticeState::Bottom);
        }
        // Entry starts as Unknown
        out_state.insert(graph.entry, LatticeState::State(FieldState::Unknown));

        // Branch context stack (inherited through edges)
        let mut branch_contexts: HashMap<CfgNodeId, Vec<BranchFrame>> = HashMap::new();
        branch_contexts.insert(graph.entry, vec![]);

        // Worklist and visit tracking
        let mut worklist: VecDeque<CfgNodeId> = VecDeque::new();
        worklist.push_back(graph.entry);
        let mut visit_count: HashMap<CfgNodeId, u32> = HashMap::new();
        let mut total_visits: usize = 0;
        let mut partial = false;

        // Transitions + suspicious points (collected during traversal)
        let mut transitions: Vec<FieldTransition> = Vec::new();
        let mut suspicious_points: Vec<SuspiciousPoint> = Vec::new();

        while let Some(nid) = worklist.pop_front() {
            total_visits += 1;
            if total_visits > MAX_VISITS {
                partial = true;
                break;
            }

            let vc = visit_count.entry(nid).or_insert(0);
            *vc += 1;
            if *vc > MAX_VISITS_PER_NODE {
                out_state.insert(nid, LatticeState::Top);
                partial = true;
                continue;
            }

            // MERGE: merge all predecessor out_states → in_state for this node.
            // Entry node has no predecessors; use its initial state directly.
            let merged = if nid == graph.entry {
                out_state.get(&nid).copied().unwrap_or(LatticeState::Bottom)
            } else {
                merge_predecessors(&graph, &out_state, &nid)
            };
            if merged == LatticeState::Bottom {
                continue; // No predecessor ready yet
            }

            let old_out = out_state.get(&nid).copied();

            // TRANSFER: apply node effect
            let node = match graph.nodes.get(&nid) {
                Some(n) => n,
                None => continue,
            };

            // Get inherited branch context
            let ctx = branch_contexts.get(&nid).cloned().unwrap_or_default();

            // Apply effect → new state + any suspicious points + intermediate transitions
            let (new_state, mut sus, intermediate) = transfer_state(
                merged.as_state().unwrap_or(FieldState::Unknown),
                node,
                &canonical_target,
                rules,
            );

            // Record transition(s) — one per state change (multi-effect nodes produce multiple)
            let from_state = merged.as_state().unwrap_or(FieldState::Unknown);
            if intermediate.is_empty() && from_state != new_state {
                // Legacy single-transition for effect_kind path
                transitions.push(FieldTransition {
                    from_state,
                    to_state: new_state,
                    node_id: node.id,
                    node_line: node.stmt_range.start_line,
                    effect: None,
                    branch_frames: ctx.clone(),
                });
            } else {
                for (from, to, eff) in &intermediate {
                    transitions.push(FieldTransition {
                        from_state: *from,
                        to_state: *to,
                        node_id: node.id,
                        node_line: node.stmt_range.start_line,
                        effect: *eff,
                        branch_frames: ctx.clone(),
                    });
                }
            }
            suspicious_points.append(&mut sus);

            let new_lattice = LatticeState::State(new_state);

            // Only propagate if state changed (Entry always propagates)
            if old_out != Some(new_lattice) || nid == graph.entry {
                out_state.insert(nid, new_lattice);

                // Propagate to successors
                if let Some(succ_edges) = graph.successors.get(&nid) {
                    for edge in succ_edges {
                        // Compute branch context for successor
                        let mut next_ctx = ctx.clone();
                        match edge.kind {
                            CfgEdgeKind::TrueBranch => {
                                next_ctx.push(BranchFrame {
                                    branch_node_line: node.stmt_range.start_line,
                                    path: BranchPath::TruePath,
                                });
                            }
                            CfgEdgeKind::FalseBranch => {
                                next_ctx.push(BranchFrame {
                                    branch_node_line: node.stmt_range.start_line,
                                    path: BranchPath::FalsePath,
                                });
                            }
                            _ => {
                                // Pop branch context when arriving at Join via normal edge
                                if graph
                                    .nodes
                                    .get(&edge.target)
                                    .map(|n| n.kind == CfgNodeKind::Join)
                                    .unwrap_or(false)
                                    && !next_ctx.is_empty()
                                {
                                    next_ctx.pop();
                                }
                            }
                        }
                        branch_contexts.insert(edge.target, next_ctx);
                        worklist.push_back(edge.target);
                    }
                }
            }
        }

        // Determine final/exit state
        let final_state = out_state
            .get(&graph.exit)
            .and_then(|s| s.as_state())
            .unwrap_or(FieldState::Unknown);

        FieldLifecycleResult {
            field_path: field_path.to_string(),
            function_qname: String::new(),
            transitions,
            suspicious_points,
            final_state,
            partial,
            evidence_level: if partial {
                EvidenceLevel::Incomplete
            } else {
                EvidenceLevel::Heuristic
            },
            exit_state: Some(final_state),
        }
    }
}

// ── Fixpoint helpers ────────────────────────────────────────────────────

/// Merge all predecessor out_states for a node using the state lattice.
fn merge_predecessors(
    graph: &CfgGraph,
    out_state: &HashMap<CfgNodeId, LatticeState>,
    nid: &CfgNodeId,
) -> LatticeState {
    let preds = match graph.predecessors.get(nid) {
        Some(p) => p,
        None => return LatticeState::Bottom,
    };
    if preds.is_empty() {
        return LatticeState::Bottom;
    }
    let mut merged = LatticeState::Bottom;
    for edge in preds {
        if let Some(s) = out_state.get(&edge.source) {
            merged = lattice_merge(merged, *s);
        }
    }
    merged
}

/// Merge two lattice states. Implements the complete merge table.
fn lattice_merge(a: LatticeState, b: LatticeState) -> LatticeState {
    use LatticeState::*;
    match (a, b) {
        (Bottom, x) | (x, Bottom) => x,
        (Top, _) | (_, Top) => Top,
        (State(x), State(y)) if x == y => State(x),
        // Freed vs non-Freed
        (State(FieldState::Freed), State(FieldState::Assigned))
        | (State(FieldState::Assigned), State(FieldState::Freed)) => State(FieldState::MaybeFreed),
        // Freed vs others
        (State(FieldState::Freed), State(FieldState::Unknown))
        | (State(FieldState::Unknown), State(FieldState::Freed)) => State(FieldState::MaybeFreed),
        (State(FieldState::Freed), _) if b != State(FieldState::MaybeFreed) => {
            State(FieldState::MaybeFreed)
        }
        (_, State(FieldState::Freed)) if a != State(FieldState::MaybeFreed) => {
            State(FieldState::MaybeFreed)
        }
        // Assigned vs others
        (State(FieldState::Assigned), State(FieldState::Unknown))
        | (State(FieldState::Unknown), State(FieldState::Assigned)) => {
            State(FieldState::MaybeAssigned)
        }
        // MaybeFreed consolidators
        (State(FieldState::MaybeFreed), _) | (_, State(FieldState::MaybeFreed)) => {
            State(FieldState::MaybeFreed)
        }
        // MaybeAssigned consolidators
        (State(FieldState::MaybeAssigned), _) | (_, State(FieldState::MaybeAssigned)) => {
            State(FieldState::MaybeAssigned)
        }
        // Default: conservative merge
        _ => State(FieldState::Unknown),
    }
}

/// Apply a node's effect(s) to the current state and produce a new state.
///
/// Phase 2: semantic_effects-aware.  When `node.semantic_effects` is non-empty
/// the function processes each effect in order and returns all intermediate
/// (from → to) transitions so the caller can record them individually.
/// Falls back to legacy `effect_kind`/`target_field`/`callee_name` otherwise.
///
/// Returns (final_state, suspicious_points, intermediate_transitions).
fn transfer_state(
    state: FieldState,
    node: &CfgNode,
    canonical_target: &str,
    _rules: &CppOwnershipRules,
) -> (FieldState, Vec<SuspiciousPoint>, TransitionTrace) {
    // ── Semantic-effects path ─────────────────────────────────────────────
    if !node.semantic_effects.is_empty() {
        let mut current = state;
        let mut suspicious: Vec<SuspiciousPoint> = Vec::new();
        let mut transitions: Vec<(FieldState, FieldState, Option<EffectKind>)> = Vec::new();

        for eff in &node.semantic_effects {
            let (next, mapped_eff) = apply_semantic_effect(current, eff, canonical_target);
            // Always check for suspicious patterns (e.g., double-free stays Freed)
            let sus = check_transition_suspicious(current, next, node, canonical_target);
            suspicious.extend(sus);
            if next != current {
                transitions.push((current, next, mapped_eff));
                current = next;
            }
        }

        return (current, suspicious, transitions);
    }

    // ── No semantic effects — nothing to transfer ──────────────────────────
    (state, vec![], vec![])
}

/// Apply a single `SemanticEffect` to the current state.
/// Returns (new_state, mapped_legacy_effect_kind) — suspicious-point detection
/// happens in the caller (`transfer_state`) after the transition.
fn apply_semantic_effect(
    state: FieldState,
    eff: &SemanticEffect,
    canonical_target: &str,
) -> (FieldState, Option<EffectKind>) {
    match &eff.kind {
        SemanticEffectKind::Free {
            place: PlaceRef::Field { path },
            ..
        } => {
            let path_canon = types::structs::canonicalize_field_path(path);
            if !field_matches(&path_canon, canonical_target) {
                return (state, None);
            }
            (FieldState::Freed, Some(EffectKind::Free))
        }
        SemanticEffectKind::Alloc {
            target: PlaceRef::Field { path },
            ..
        } => {
            let path_canon = types::structs::canonicalize_field_path(path);
            if !field_matches(&path_canon, canonical_target) {
                return (state, None);
            }
            (FieldState::Assigned, Some(EffectKind::Allocate))
        }
        SemanticEffectKind::Store {
            dst: PlaceRef::Field { path },
            ..
        } => {
            let path_canon = types::structs::canonicalize_field_path(path);
            if !field_matches(&path_canon, canonical_target) {
                return (state, None);
            }
            (FieldState::Assigned, Some(EffectKind::Assign))
        }
        SemanticEffectKind::Nullify {
            place: PlaceRef::Field { path },
            ..
        } => {
            let path_canon = types::structs::canonicalize_field_path(path);
            if !field_matches(&path_canon, canonical_target) {
                return (state, None);
            }
            (FieldState::Nullified, Some(EffectKind::Assign))
        }
        SemanticEffectKind::Assign {
            dst: PlaceRef::Field { path },
            ..
        } => {
            let path_canon = types::structs::canonicalize_field_path(path);
            if !field_matches(&path_canon, canonical_target) {
                return (state, None);
            }
            (FieldState::Assigned, Some(EffectKind::Assign))
        }
        SemanticEffectKind::Escape { .. } => (FieldState::Escaped, None),
        SemanticEffectKind::Return { .. } => (state, Some(EffectKind::Return)),
        // Untethered alloc/assign/free (to locals or indeterminate): no field-state change
        SemanticEffectKind::Alloc {
            target: PlaceRef::Local { .. } | PlaceRef::Indeterminate,
            ..
        } => (state, None),
        SemanticEffectKind::Free {
            place: PlaceRef::Local { .. } | PlaceRef::Indeterminate,
            ..
        } => (state, None),
        SemanticEffectKind::Assign {
            dst: PlaceRef::Local { .. } | PlaceRef::Indeterminate,
            ..
        } => (state, None),
        SemanticEffectKind::Store {
            dst: PlaceRef::Local { .. } | PlaceRef::Indeterminate,
            ..
        } => (state, None),
        SemanticEffectKind::Nullify {
            place: PlaceRef::Local { .. } | PlaceRef::Indeterminate,
            ..
        } => (state, None),
        // Call: field effect is determined by callee classification, handled in legacy path
        // or through the effect composer's Alloc/Free decomposition
        SemanticEffectKind::Call { .. } => (state, None),
    }
}

/// Detect suspicious patterns when state transitions from `prev` → `next`.
fn check_transition_suspicious(
    prev: FieldState,
    next: FieldState,
    node: &CfgNode,
    field: &str,
) -> Vec<SuspiciousPoint> {
    let line = node.stmt_range.start_line;

    // Double-free: transitioning to Freed while already Freed/MaybeFreed
    if next == FieldState::Freed && (prev == FieldState::Freed || prev == FieldState::MaybeFreed) {
        return vec![SuspiciousPoint {
            line,
            kind: SuspiciousKind::DoubleFree,
            message: format!("Double free of '{field}'"),
        }];
    }

    // Use-after-free: assigning/allocating/reading after Freed/MaybeFreed
    if (next == FieldState::Assigned || next == FieldState::Nullified)
        && (prev == FieldState::Freed || prev == FieldState::MaybeFreed)
    {
        return vec![SuspiciousPoint {
            line,
            kind: SuspiciousKind::UseAfterFree,
            message: format!("Write to '{field}' after free"),
        }];
    }

    // Use-after-free: Escaped → but only if previously freed
    if next == FieldState::Escaped && (prev == FieldState::Freed || prev == FieldState::MaybeFreed)
    {
        return vec![SuspiciousPoint {
            line,
            kind: SuspiciousKind::UseAfterFree,
            message: format!("Escape of '{field}' after free"),
        }];
    }

    vec![]
}

/// Check if the target field path matches the tracked canonical target.
/// Handles prefix-based matching (e.g. "data.aptr" matches "data.aptr.cookiehost").
fn field_matches(target_canon: &str, canonical_target: &str) -> bool {
    target_canon == canonical_target
        || canonical_target.starts_with(&format!("{target_canon}."))
        || target_canon.starts_with(&format!("{canonical_target}."))
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use types::effects::{PlaceRef, SemanticEffect, SemanticEffectKind, ValueSource};
    use types::enums::CfgNodeKind;
    use types::ids::{CfgNodeId, EffectId, SymbolId};
    use types::structs::TextRange;

    /// Counter for generating unique CfgNodeIds in tests.
    fn test_fid() -> SymbolId {
        SymbolId::default()
    }

    /// Create a semantic effect for test use.
    fn test_effect(node_id: CfgNodeId, order: u32, kind: SemanticEffectKind) -> SemanticEffect {
        let kind_name = match &kind {
            SemanticEffectKind::Alloc { .. } => "Alloc",
            SemanticEffectKind::Free { .. } => "Free",
            SemanticEffectKind::Store { .. } => "Store",
            SemanticEffectKind::Assign { .. } => "Assign",
            SemanticEffectKind::Call { .. } => "Call",
            SemanticEffectKind::Nullify { .. } => "Nullify",
            SemanticEffectKind::Return { .. } => "Return",
            SemanticEffectKind::Escape { .. } => "Escape",
        };
        let id = EffectId::generate(&node_id, order, kind_name);
        SemanticEffect {
            id,
            cfg_node_id: node_id,
            order,
            kind,
            confidence: 0.8,
            consumption_style: None,
            description: None,
            eligible_for_implicit_cleanup: None,
        }
    }

    fn make_node(effects: Vec<SemanticEffect>, line: u32, kind: CfgNodeKind, seq: u32) -> CfgNode {
        let fid = test_fid();
        let id = CfgNodeId::generate(&fid, "test", seq);
        CfgNode {
            id,
            function_id: fid,
            kind,
            stmt_range: TextRange {
                start_byte: seq,
                end_byte: seq,
                start_line: line,
                start_column: 0,
                end_line: line,
                end_column: 0,
            },
            call_context: types::enums::CallContext::None,
            semantic_effects: effects,
        }
    }

    fn make_stmt_node(effects: Vec<SemanticEffect>, line: u32, seq: u32) -> CfgNode {
        make_node(effects, line, CfgNodeKind::Statement, seq)
    }

    /// Create a "Free field" semantic effect.
    fn se_free(node_id: CfgNodeId, order: u32, field: &str) -> SemanticEffect {
        test_effect(
            node_id,
            order,
            SemanticEffectKind::Free {
                place: PlaceRef::Field {
                    path: field.to_string(),
                },
                callee: "?".to_string(),
            },
        )
    }

    /// Create an "Alloc field" semantic effect.
    fn se_alloc(node_id: CfgNodeId, order: u32, field: &str) -> SemanticEffect {
        test_effect(
            node_id,
            order,
            SemanticEffectKind::Alloc {
                target: PlaceRef::Field {
                    path: field.to_string(),
                },
                callee: "?".to_string(),
            },
        )
    }

    /// Create a "Store to field" semantic effect.
    fn se_store(node_id: CfgNodeId, order: u32, field: &str) -> SemanticEffect {
        test_effect(
            node_id,
            order,
            SemanticEffectKind::Store {
                dst: PlaceRef::Field {
                    path: field.to_string(),
                },
                src: ValueSource::Unknown,
            },
        )
    }

    /// Create a "Return" semantic effect.
    fn se_return(node_id: CfgNodeId, order: u32) -> SemanticEffect {
        test_effect(
            node_id,
            order,
            SemanticEffectKind::Return {
                value: ValueSource::Unknown,
            },
        )
    }

    fn make_entry_exit_graph(nodes: &[CfgNode]) -> (Vec<CfgNode>, Vec<CfgEdge>) {
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let mut all_nodes = vec![entry.clone(), exit.clone()];
        all_nodes.extend_from_slice(nodes);

        let mut edges = Vec::new();
        let mut prev_id = entry.id;
        for n in nodes {
            edges.push(CfgEdge::new(&prev_id, &n.id, CfgEdgeKind::Normal));
            prev_id = n.id;
        }
        edges.push(CfgEdge::new(&prev_id, &exit.id, CfgEdgeKind::Normal));

        (all_nodes, edges)
    }

    #[test]
    fn test_use_after_free_detected() {
        // Free followed by Store (write after free) triggers UseAfterFree
        let fid = test_fid();
        let id1 = CfgNodeId::generate(&fid, "test", 1);
        let id2 = CfgNodeId::generate(&fid, "test", 2);
        let nodes = vec![
            make_stmt_node(vec![se_free(id1, 0, "ptr")], 10, 1),
            make_stmt_node(vec![se_store(id2, 0, "ptr")], 12, 2),
        ];
        let (all_nodes, edges) = make_entry_exit_graph(&nodes);
        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "ptr", &rules);
        assert!(!result.suspicious_points.is_empty());
        assert_eq!(
            result.suspicious_points[0].kind,
            SuspiciousKind::UseAfterFree
        );
    }

    #[test]
    fn test_double_free_detected() {
        let fid = test_fid();
        let id1 = CfgNodeId::generate(&fid, "test", 1);
        let id2 = CfgNodeId::generate(&fid, "test", 2);
        let nodes = vec![
            make_stmt_node(vec![se_free(id1, 0, "ptr")], 10, 1),
            make_stmt_node(vec![se_free(id2, 0, "ptr")], 15, 2),
        ];
        let (all_nodes, edges) = make_entry_exit_graph(&nodes);
        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "ptr", &rules);
        let double_frees: Vec<_> = result
            .suspicious_points
            .iter()
            .filter(|p| p.kind == SuspiciousKind::DoubleFree)
            .collect();
        assert!(!double_frees.is_empty());
    }

    #[test]
    fn test_clean_lifecycle() {
        let fid = test_fid();
        let id1 = CfgNodeId::generate(&fid, "test", 1);
        let id2 = CfgNodeId::generate(&fid, "test", 2);
        let id4 = CfgNodeId::generate(&fid, "test", 4);
        let nodes = vec![
            make_stmt_node(vec![se_alloc(id1, 0, "ptr")], 10, 1),
            make_stmt_node(vec![se_store(id2, 0, "ptr")], 11, 2),
            make_stmt_node(Vec::new(), 12, 3), // Read has no semantic field effect
            make_stmt_node(vec![se_free(id4, 0, "ptr")], 13, 4),
        ];
        let (all_nodes, edges) = make_entry_exit_graph(&nodes);
        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "ptr", &rules);
        assert_eq!(result.final_state, FieldState::Freed);
        assert!(
            result
                .suspicious_points
                .iter()
                .all(|p| p.kind != SuspiciousKind::UseAfterFree)
        );
    }

    #[test]
    fn test_nullify_after_alloc() {
        let fid = test_fid();
        let id1 = CfgNodeId::generate(&fid, "test", 1);
        let id2 = CfgNodeId::generate(&fid, "test", 2);
        let nodes = vec![
            make_stmt_node(vec![se_alloc(id1, 0, "ptr")], 10, 1),
            make_stmt_node(vec![se_store(id2, 0, "ptr")], 12, 2),
        ];
        let (all_nodes, edges) = make_entry_exit_graph(&nodes);
        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "ptr", &rules);
        assert!(
            !result
                .suspicious_points
                .iter()
                .any(|p| p.kind == SuspiciousKind::UseAfterFree)
        );
    }

    #[test]
    fn test_return_escaped_state() {
        let fid = test_fid();
        let id1 = CfgNodeId::generate(&fid, "test", 1);
        let id2 = CfgNodeId::generate(&fid, "test", 2);
        let nodes = vec![
            make_stmt_node(vec![se_alloc(id1, 0, "ptr")], 10, 1),
            make_stmt_node(vec![se_return(id2, 0)], 15, 2),
        ];
        let (all_nodes, edges) = make_entry_exit_graph(&nodes);
        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "ptr", &rules);
        assert_eq!(result.final_state, FieldState::Assigned);
    }

    #[test]
    fn test_interleaved_fields_dont_cross_contaminate() {
        let fid = test_fid();
        let id1 = CfgNodeId::generate(&fid, "test", 1);
        let id2 = CfgNodeId::generate(&fid, "test", 2);
        let id3 = CfgNodeId::generate(&fid, "test", 3);
        let id5 = CfgNodeId::generate(&fid, "test", 5);
        let nodes = vec![
            make_stmt_node(vec![se_alloc(id1, 0, "a")], 10, 1),
            make_stmt_node(vec![se_alloc(id2, 0, "b")], 11, 2),
            make_stmt_node(vec![se_free(id3, 0, "b")], 12, 3),
            make_stmt_node(Vec::new(), 13, 4), // Read("a") has no semantic field effect
            make_stmt_node(vec![se_free(id5, 0, "a")], 14, 5),
        ];
        let (all_nodes, edges) = make_entry_exit_graph(&nodes);
        let rules = OwnershipRules::default();
        let result = FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "a", &rules);
        assert!(
            result
                .suspicious_points
                .iter()
                .all(|p| p.kind != SuspiciousKind::UseAfterFree),
            "Field 'a' should not trigger use-after-free from 'b' operations"
        );
        assert_eq!(result.final_state, FieldState::Freed);
    }

    #[test]
    fn test_no_effects_produces_unknown_state() {
        let nodes = vec![make_stmt_node(Vec::new(), 10, 1)];
        let (all_nodes, edges) = make_entry_exit_graph(&nodes);
        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "any_field", &rules);
        assert_eq!(result.final_state, FieldState::Unknown);
        assert!(result.transitions.is_empty());
    }

    #[test]
    fn test_allocate_free_allocate_reuse() {
        let fid = test_fid();
        let id1 = CfgNodeId::generate(&fid, "test", 1);
        let id2 = CfgNodeId::generate(&fid, "test", 2);
        let id3 = CfgNodeId::generate(&fid, "test", 3);
        let id4 = CfgNodeId::generate(&fid, "test", 4);
        let nodes = vec![
            make_stmt_node(vec![se_alloc(id1, 0, "ptr")], 10, 1),
            make_stmt_node(vec![se_free(id2, 0, "ptr")], 15, 2),
            make_stmt_node(vec![se_alloc(id3, 0, "ptr")], 20, 3),
            make_stmt_node(vec![se_free(id4, 0, "ptr")], 25, 4),
        ];
        let (all_nodes, edges) = make_entry_exit_graph(&nodes);
        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "ptr", &rules);
        assert_eq!(result.final_state, FieldState::Freed);
    }

    #[test]
    fn test_fixpoint_branch_merge() {
        // Branch: True path frees, False path allocates → merge should be MaybeFreed
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let id_free = CfgNodeId::generate(&fid, "test", 2);
        let id_alloc = CfgNodeId::generate(&fid, "test", 3);
        let branch = make_node(Vec::new(), 10, CfgNodeKind::Branch, 1);
        let free_node = make_stmt_node(vec![se_free(id_free, 0, "ptr")], 11, 2);
        let alloc_node = make_stmt_node(vec![se_alloc(id_alloc, 0, "ptr")], 12, 3);
        let join = make_node(Vec::new(), 13, CfgNodeKind::Join, 4);

        let all_nodes = vec![
            entry.clone(),
            branch.clone(),
            free_node.clone(),
            alloc_node.clone(),
            join.clone(),
            exit.clone(),
        ];
        let edges = vec![
            CfgEdge::new(&entry.id, &branch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&branch.id, &free_node.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&branch.id, &alloc_node.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&free_node.id, &join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&alloc_node.id, &join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&join.id, &exit.id, CfgEdgeKind::Normal),
        ];

        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "ptr", &rules);
        // At join, one path is freed, the other is assigned → MaybeFreed
        assert_eq!(result.final_state, FieldState::MaybeFreed);
        assert!(!result.partial);
    }

    #[test]
    fn test_loop_converges_via_lattice() {
        // A self-loop that doesn't change state converges quickly.
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let loop_node = make_node(Vec::new(), 10, CfgNodeKind::Loop, 1);

        let all_nodes = vec![entry.clone(), loop_node.clone(), exit.clone()];
        let edges = vec![
            CfgEdge::new(&entry.id, &loop_node.id, CfgEdgeKind::Normal),
            CfgEdge::new(&loop_node.id, &loop_node.id, CfgEdgeKind::LoopBack),
            CfgEdge::new(&loop_node.id, &exit.id, CfgEdgeKind::Normal),
        ];

        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "nonexist", &rules);
        // Loop converges before budget is exhausted.
        assert!(!result.partial);
        assert_ne!(result.evidence_level, EvidenceLevel::Incomplete);
    }

    #[test]
    fn test_budget_exhaustion_marks_partial() {
        // Create enough nodes to exceed MAX_VISITS (500).
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);

        let mut all_nodes = vec![entry.clone()];
        let mut edges = Vec::new();
        let mut prev_id = entry.id;

        // Build 600 nodes in a chain → exceeds MAX_VISITS=500
        for i in 0..600u32 {
            let n = make_stmt_node(Vec::new(), i, i);
            edges.push(CfgEdge::new(&prev_id, &n.id, CfgEdgeKind::Normal));
            prev_id = n.id;
            all_nodes.push(n);
        }
        all_nodes.push(exit.clone());
        edges.push(CfgEdge::new(&prev_id, &exit.id, CfgEdgeKind::Normal));

        let rules = OwnershipRules::default();
        let result = FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "x", &rules);
        assert!(result.partial);
        assert_eq!(result.evidence_level, EvidenceLevel::Incomplete);
    }
}
