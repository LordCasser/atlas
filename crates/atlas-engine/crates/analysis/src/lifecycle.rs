//! Field Lifecycle Analysis — path-sensitive fixpoint analysis on CfgGraph.
//!
//! Given a function's CFG (nodes + edges) and a target field path, runs a
//! monotone dataflow fixpoint to trace the field's lifecycle through all
//! control-flow paths, accounting for branches and loop convergence.

use std::collections::{HashMap, HashSet, VecDeque};

use types::cfg::{CfgEdge, CfgNode};
use types::effects::{PlaceRef, SemanticEffect, SemanticEffectKind};
use types::enums::{CfgEdgeKind, CfgNodeKind, EffectKind};
use types::ids::CfgNodeId;

use super::lifecycle_proof::EvidenceLevel;
use super::ownership_rules::CppOwnershipRules;
use crate::cfg_graph::CfgGraph;
use crate::effect_composer::EffectComposition;

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
    CasePath,
    ExceptionPath,
}

impl BranchPath {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TruePath => "true",
            Self::FalsePath => "false",
            Self::CasePath => "case",
            Self::ExceptionPath => "exception",
        }
    }
}

/// One frame in a nested branch context stack.
#[derive(Debug, Clone)]
pub struct BranchFrame {
    pub branch_node_id: CfgNodeId,
    pub join_node_id: Option<CfgNodeId>,
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

fn matching_join_for_branch(graph: &CfgGraph, branch: &CfgNode) -> Option<CfgNodeId> {
    let expected_start = branch.stmt_range.start_byte.checked_add(1)?;
    let mut pending: VecDeque<_> = graph
        .successors
        .get(&branch.id)
        .into_iter()
        .flatten()
        .map(|edge| edge.target)
        .collect();
    let mut visited = HashSet::new();
    while let Some(node_id) = pending.pop_front() {
        if !visited.insert(node_id) {
            continue;
        }
        let node = graph.nodes.get(&node_id)?;
        if node.kind == CfgNodeKind::Join && node.stmt_range.start_byte == expected_start {
            return Some(node.id);
        }
        pending.extend(
            graph
                .successors
                .get(&node_id)
                .into_iter()
                .flatten()
                .map(|edge| edge.target),
        );
    }
    None
}

fn control_owner_for_join(graph: &CfgGraph, join_id: CfgNodeId) -> Option<&CfgNode> {
    let join = graph.nodes.get(&join_id)?;
    let expected_start = join.stmt_range.start_byte.checked_sub(1)?;
    let mut pending: VecDeque<_> = graph
        .predecessors
        .get(&join_id)
        .into_iter()
        .flatten()
        .map(|edge| edge.source)
        .collect();
    let mut visited = HashSet::new();
    while let Some(node_id) = pending.pop_front() {
        if !visited.insert(node_id) {
            continue;
        }
        let node = graph.nodes.get(&node_id)?;
        if matches!(node.kind, CfgNodeKind::Branch | CfgNodeKind::Loop)
            && node.stmt_range.start_byte == expected_start
        {
            return Some(node);
        }
        pending.extend(
            graph
                .predecessors
                .get(&node_id)
                .into_iter()
                .flatten()
                .map(|edge| edge.source),
        );
    }
    None
}

fn retain_frames_outside_owner(context: &mut Vec<BranchFrame>, graph: &CfgGraph, owner: &CfgNode) {
    context.retain(|frame| {
        graph
            .nodes
            .get(&frame.branch_node_id)
            .map(|branch| {
                branch.stmt_range.start_byte < owner.stmt_range.start_byte
                    || branch.stmt_range.start_byte >= owner.stmt_range.end_byte
            })
            .unwrap_or(false)
    });
}

fn enter_exception_handler_context(
    context: &mut Vec<BranchFrame>,
    graph: &CfgGraph,
    handler_id: CfgNodeId,
) {
    let Some(owner) = graph
        .predecessors
        .get(&handler_id)
        .into_iter()
        .flatten()
        .filter(|edge| edge.kind == CfgEdgeKind::Exception)
        .filter_map(|edge| graph.nodes.get(&edge.source))
        .find(|node| node.kind == CfgNodeKind::Branch)
    else {
        return;
    };

    // An exception abandons every conditional frame lexically owned by the
    // try region before entering its handler. Frames outside the try remain
    // relevant (for example, a try nested in one side of an outer `if`).
    retain_frames_outside_owner(context, graph, owner);

    // Empty handlers target the try Join directly and contain no transition
    // that needs an exception frame. Avoid creating a frame only to leak it
    // past that Join.
    if graph
        .nodes
        .get(&handler_id)
        .is_some_and(|node| node.kind == CfgNodeKind::Join)
    {
        return;
    }

    context.push(BranchFrame {
        branch_node_id: owner.id,
        join_node_id: matching_join_for_branch(graph, owner),
        branch_node_line: owner.stmt_range.start_line,
        path: BranchPath::ExceptionPath,
    });
}

fn unwind_context_at_join(context: &mut Vec<BranchFrame>, graph: &CfgGraph, join_id: CfgNodeId) {
    if let Some(frame_index) = context
        .iter()
        .rposition(|frame| frame.join_node_id == Some(join_id))
    {
        context.truncate(frame_index);
    } else if let Some(owner) = control_owner_for_join(graph, join_id) {
        retain_frames_outside_owner(context, graph, owner);
    } else {
        // Hand-built/legacy CFGs may not follow the builder's start+1 Join ID
        // invariant. Preserve their previous one-level behavior.
        context.pop();
    }
}

fn unwind_context_for_loop(context: &mut Vec<BranchFrame>, graph: &CfgGraph, loop_id: CfgNodeId) {
    if let Some(loop_node) = graph
        .nodes
        .get(&loop_id)
        .filter(|node| node.kind == CfgNodeKind::Loop)
    {
        retain_frames_outside_owner(context, graph, loop_node);
    } else {
        context.pop();
    }
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
                                let target_is_join = graph
                                    .nodes
                                    .get(&edge.target)
                                    .is_some_and(|node| node.kind == CfgNodeKind::Join);
                                if !target_is_join {
                                    next_ctx.push(BranchFrame {
                                        branch_node_id: node.id,
                                        join_node_id: matching_join_for_branch(&graph, node),
                                        branch_node_line: node.stmt_range.start_line,
                                        path: BranchPath::TruePath,
                                    });
                                }
                            }
                            CfgEdgeKind::FalseBranch => {
                                let target_is_join = graph
                                    .nodes
                                    .get(&edge.target)
                                    .is_some_and(|node| node.kind == CfgNodeKind::Join);
                                if !target_is_join {
                                    next_ctx.push(BranchFrame {
                                        branch_node_id: node.id,
                                        join_node_id: matching_join_for_branch(&graph, node),
                                        branch_node_line: node.stmt_range.start_line,
                                        path: BranchPath::FalsePath,
                                    });
                                }
                            }
                            CfgEdgeKind::CaseBranch => {
                                let target_is_join = graph
                                    .nodes
                                    .get(&edge.target)
                                    .map(|n| n.kind == CfgNodeKind::Join)
                                    .unwrap_or(false);
                                if !target_is_join {
                                    next_ctx.push(BranchFrame {
                                        branch_node_id: node.id,
                                        join_node_id: matching_join_for_branch(&graph, node),
                                        branch_node_line: node.stmt_range.start_line,
                                        path: BranchPath::CasePath,
                                    });
                                }
                            }
                            CfgEdgeKind::Break => {
                                unwind_context_at_join(&mut next_ctx, &graph, edge.target);
                            }
                            CfgEdgeKind::Continue => {
                                unwind_context_for_loop(&mut next_ctx, &graph, edge.target);
                            }
                            CfgEdgeKind::Goto => {
                                // A goto may bypass any number of branch joins
                                // or enter a different arm. Without retaining
                                // lexical label ownership in CFG facts, no
                                // active path frame can be proven to remain
                                // valid at the target. Dropping the context is
                                // conservative; preserving it would publish a
                                // false path condition.
                                next_ctx.clear();
                            }
                            CfgEdgeKind::Exception => {
                                enter_exception_handler_context(&mut next_ctx, &graph, edge.target);
                                if graph
                                    .nodes
                                    .get(&edge.target)
                                    .is_some_and(|node| node.kind == CfgNodeKind::Join)
                                {
                                    unwind_context_at_join(&mut next_ctx, &graph, edge.target);
                                }
                            }
                            _ => {
                                // Leave the branch region when its matching Join
                                // is reached. The ID-bound frame also handles a
                                // jump that crosses multiple nested branches.
                                if graph
                                    .nodes
                                    .get(&edge.target)
                                    .map(|n| n.kind == CfgNodeKind::Join)
                                    .unwrap_or(false)
                                {
                                    unwind_context_at_join(&mut next_ctx, &graph, edge.target);
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

    /// Analyze query-time semantic effects without persisting a second CFG.
    pub fn analyze_with_composition(
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        field_path: &str,
        ownership_rules: &OwnershipRules,
        rules: &CppOwnershipRules,
        composition: &EffectComposition,
    ) -> FieldLifecycleResult {
        let mut enriched_nodes = cfg_nodes.to_vec();
        for node in &mut enriched_nodes {
            if let Some(effects) = composition.node_effects.get(&node.id) {
                node.semantic_effects.clone_from(effects);
            }
        }
        Self::analyze_with_rules(
            &enriched_nodes,
            cfg_edges,
            field_path,
            ownership_rules,
            rules,
        )
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
        SemanticEffectKind::Free { place, .. } => {
            if !place_matches(place, canonical_target) {
                return (state, None);
            }
            (FieldState::Freed, Some(EffectKind::Free))
        }
        SemanticEffectKind::Alloc { target, .. } => {
            if !place_matches(target, canonical_target) {
                return (state, None);
            }
            (FieldState::Assigned, Some(EffectKind::Allocate))
        }
        SemanticEffectKind::Store { dst, .. } => {
            if !place_matches(dst, canonical_target) {
                return (state, None);
            }
            (FieldState::Assigned, Some(EffectKind::Assign))
        }
        SemanticEffectKind::Nullify { place } => {
            if !place_matches(place, canonical_target) {
                return (state, None);
            }
            (FieldState::Nullified, Some(EffectKind::Assign))
        }
        SemanticEffectKind::Assign { dst, .. } => {
            if !place_matches(dst, canonical_target) {
                return (state, None);
            }
            (FieldState::Assigned, Some(EffectKind::Assign))
        }
        SemanticEffectKind::Escape { .. } => (FieldState::Escaped, None),
        SemanticEffectKind::Return { .. } => (state, Some(EffectKind::Return)),
        // Call: field effect is determined by callee classification, handled in legacy path
        // or through the effect composer's Alloc/Free decomposition
        SemanticEffectKind::Call { .. } => (state, None),
    }
}

fn place_matches(place: &PlaceRef, canonical_target: &str) -> bool {
    match place {
        PlaceRef::Field { path } => field_matches(
            &types::structs::canonicalize_field_path(path),
            canonical_target,
        ),
        PlaceRef::Local { name } => {
            types::structs::canonicalize_field_path(name) == canonical_target
        }
        PlaceRef::Indeterminate => false,
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
            managed_scope_start_byte: None,
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

    fn se_local_alloc(node_id: CfgNodeId, order: u32, name: &str) -> SemanticEffect {
        test_effect(
            node_id,
            order,
            SemanticEffectKind::Alloc {
                target: PlaceRef::Local {
                    name: name.to_string(),
                },
                callee: "kzalloc_obj".to_string(),
            },
        )
    }

    fn se_local_free(node_id: CfgNodeId, order: u32, name: &str) -> SemanticEffect {
        test_effect(
            node_id,
            order,
            SemanticEffectKind::Free {
                place: PlaceRef::Local {
                    name: name.to_string(),
                },
                callee: "kfree".to_string(),
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
    fn branch_path_labels_are_stable_for_analysis_responses() {
        assert_eq!(BranchPath::TruePath.as_str(), "true");
        assert_eq!(BranchPath::FalsePath.as_str(), "false");
        assert_eq!(BranchPath::CasePath.as_str(), "case");
        assert_eq!(BranchPath::ExceptionPath.as_str(), "exception");
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
    fn test_local_resource_lifecycle() {
        let fid = test_fid();
        let alloc_id = CfgNodeId::generate(&fid, "test", 1);
        let free_id = CfgNodeId::generate(&fid, "test", 2);
        let nodes = vec![
            make_stmt_node(vec![se_local_alloc(alloc_id, 0, "priv")], 10, 1),
            make_stmt_node(vec![se_local_free(free_id, 0, "priv")], 12, 2),
        ];
        let (all_nodes, edges) = make_entry_exit_graph(&nodes);
        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&all_nodes, &edges, "priv", &rules);

        assert_eq!(result.transitions.len(), 2);
        assert_eq!(result.final_state, FieldState::Freed);
    }

    #[test]
    fn test_composed_effects_are_applied_without_persisting_cfg_mutations() {
        let node = make_stmt_node(Vec::new(), 10, 1);
        let effect = se_local_alloc(node.id, 0, "priv");
        let (all_nodes, edges) = make_entry_exit_graph(std::slice::from_ref(&node));
        let composition = EffectComposition {
            node_effects: HashMap::from([(node.id, vec![effect])]),
            ..EffectComposition::default()
        };
        let result = FieldLifecycleEngine::analyze_with_composition(
            &all_nodes,
            &edges,
            "priv",
            &OwnershipRules::default(),
            &CppOwnershipRules::default(),
            &composition,
        );

        assert_eq!(result.transitions.len(), 1);
        assert_eq!(result.final_state, FieldState::Assigned);
        assert!(
            all_nodes
                .iter()
                .all(|node| node.semantic_effects.is_empty())
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
    fn test_switch_case_branch_context_is_path_sensitive() {
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let branch = make_node(Vec::new(), 10, CfgNodeKind::Branch, 1);
        let free_id = CfgNodeId::generate(&fid, "test", 2);
        let alloc_id = CfgNodeId::generate(&fid, "test", 3);
        let post_id = CfgNodeId::generate(&fid, "test", 5);
        let free_case = make_stmt_node(vec![se_free(free_id, 0, "ptr")], 11, 2);
        let alloc_case = make_stmt_node(vec![se_alloc(alloc_id, 0, "ptr")], 12, 3);
        let join = make_node(Vec::new(), 13, CfgNodeKind::Join, 4);
        let post_switch = make_stmt_node(vec![se_store(post_id, 0, "ptr")], 20, 5);

        let all_nodes = vec![
            entry.clone(),
            branch.clone(),
            free_case.clone(),
            alloc_case.clone(),
            join.clone(),
            post_switch.clone(),
            exit.clone(),
        ];
        let edges = vec![
            CfgEdge::new(&entry.id, &branch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&branch.id, &free_case.id, CfgEdgeKind::CaseBranch),
            CfgEdge::new(&branch.id, &alloc_case.id, CfgEdgeKind::CaseBranch),
            CfgEdge::new(&branch.id, &join.id, CfgEdgeKind::CaseBranch),
            CfgEdge::new(&free_case.id, &join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&alloc_case.id, &join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&join.id, &post_switch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&post_switch.id, &exit.id, CfgEdgeKind::Normal),
        ];

        let result = FieldLifecycleEngine::analyze_field_lifecycle(
            &all_nodes,
            &edges,
            "ptr",
            &OwnershipRules::default(),
        );

        let case_transitions: Vec<_> = result
            .transitions
            .iter()
            .filter(|transition| transition.node_line == 11 || transition.node_line == 12)
            .collect();
        assert_eq!(case_transitions.len(), 2);
        assert!(case_transitions.iter().all(|transition| {
            transition
                .branch_frames
                .iter()
                .any(|frame| frame.path == BranchPath::CasePath)
        }));

        let post_transition = result
            .transitions
            .iter()
            .find(|transition| transition.node_line == 20)
            .expect("post-switch transition");
        assert!(
            post_transition.branch_frames.is_empty(),
            "case branch context must be popped at the switch join"
        );
    }

    #[test]
    fn matching_join_and_owner_follow_clone_local_graph_edges() {
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let branch_one = make_node(Vec::new(), 1, CfgNodeKind::Branch, 10);
        let join_one = make_node(Vec::new(), 2, CfgNodeKind::Join, 11);
        let mut branch_two = make_node(Vec::new(), 1, CfgNodeKind::Branch, 110);
        branch_two.stmt_range.start_byte = 10;
        let mut join_two = make_node(Vec::new(), 2, CfgNodeKind::Join, 111);
        join_two.stmt_range.start_byte = 11;
        let nodes = vec![
            entry.clone(),
            branch_one.clone(),
            join_one.clone(),
            branch_two.clone(),
            join_two.clone(),
            exit.clone(),
        ];
        let edges = vec![
            CfgEdge::new(&entry.id, &branch_one.id, CfgEdgeKind::Normal),
            CfgEdge::new(&entry.id, &branch_two.id, CfgEdgeKind::Normal),
            CfgEdge::new(&branch_one.id, &join_one.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&branch_two.id, &join_two.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&join_one.id, &exit.id, CfgEdgeKind::Normal),
            CfgEdge::new(&join_two.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let graph = CfgGraph::build(&nodes, &edges).expect("valid cloned CFG");

        assert_eq!(
            matching_join_for_branch(&graph, &branch_one),
            Some(join_one.id)
        );
        assert_eq!(
            matching_join_for_branch(&graph, &branch_two),
            Some(join_two.id)
        );
        assert_eq!(
            control_owner_for_join(&graph, join_one.id).map(|node| node.id),
            Some(branch_one.id)
        );
        assert_eq!(
            control_owner_for_join(&graph, join_two.id).map(|node| node.id),
            Some(branch_two.id)
        );
    }

    #[test]
    fn test_nested_break_unwinds_to_switch_join_but_preserves_outer_branch() {
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let outer_branch = make_node(Vec::new(), 1, CfgNodeKind::Branch, 10);
        let outer_join = make_node(Vec::new(), 9, CfgNodeKind::Join, 11);
        let switch_branch = make_node(Vec::new(), 2, CfgNodeKind::Branch, 20);
        let switch_join = make_node(Vec::new(), 7, CfgNodeKind::Join, 21);
        let inner_branch = make_node(Vec::new(), 3, CfgNodeKind::Branch, 30);
        let inner_join = make_node(Vec::new(), 5, CfgNodeKind::Join, 31);
        let break_node = make_node(Vec::new(), 4, CfgNodeKind::Statement, 40);
        let post_id = CfgNodeId::generate(&fid, "test", 50);
        let post_switch = make_stmt_node(vec![se_store(post_id, 0, "ptr")], 8, 50);

        let nodes = vec![
            entry.clone(),
            outer_branch.clone(),
            switch_branch.clone(),
            inner_branch.clone(),
            break_node.clone(),
            inner_join.clone(),
            switch_join.clone(),
            post_switch.clone(),
            outer_join.clone(),
            exit.clone(),
        ];
        let edges = vec![
            CfgEdge::new(&entry.id, &outer_branch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&outer_branch.id, &switch_branch.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&outer_branch.id, &outer_join.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&switch_branch.id, &inner_branch.id, CfgEdgeKind::CaseBranch),
            CfgEdge::new(&switch_branch.id, &switch_join.id, CfgEdgeKind::CaseBranch),
            CfgEdge::new(&inner_branch.id, &break_node.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&inner_branch.id, &inner_join.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&break_node.id, &switch_join.id, CfgEdgeKind::Break),
            CfgEdge::new(&inner_join.id, &switch_join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&switch_join.id, &post_switch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&post_switch.id, &outer_join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&outer_join.id, &exit.id, CfgEdgeKind::Normal),
        ];

        let result = FieldLifecycleEngine::analyze_field_lifecycle(
            &nodes,
            &edges,
            "ptr",
            &OwnershipRules::default(),
        );
        let post_transition = result
            .transitions
            .iter()
            .find(|transition| transition.node_line == 8)
            .expect("post-switch transition");
        assert_eq!(post_transition.branch_frames.len(), 1);
        assert_eq!(
            post_transition.branch_frames[0].branch_node_id,
            outer_branch.id
        );
        assert_eq!(post_transition.branch_frames[0].path, BranchPath::TruePath);
    }

    #[test]
    fn goto_clears_branch_frames_that_may_no_longer_describe_the_target() {
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let outer_branch = make_node(Vec::new(), 1, CfgNodeKind::Branch, 10);
        let inner_branch = make_node(Vec::new(), 2, CfgNodeKind::Branch, 20);
        let goto_node = make_node(Vec::new(), 3, CfgNodeKind::Statement, 30);
        let outer_join = make_node(Vec::new(), 8, CfgNodeKind::Join, 11);
        let inner_join = make_node(Vec::new(), 7, CfgNodeKind::Join, 21);
        let target_id = CfgNodeId::generate(&fid, "test", 40);
        let target = make_stmt_node(vec![se_store(target_id, 0, "ptr")], 9, 40);
        let nodes = vec![
            entry.clone(),
            outer_branch.clone(),
            inner_branch.clone(),
            goto_node.clone(),
            inner_join.clone(),
            outer_join.clone(),
            target.clone(),
            exit.clone(),
        ];
        let edges = vec![
            CfgEdge::new(&entry.id, &outer_branch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&outer_branch.id, &inner_branch.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&outer_branch.id, &outer_join.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&inner_branch.id, &goto_node.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&inner_branch.id, &inner_join.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&inner_join.id, &outer_join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&goto_node.id, &target.id, CfgEdgeKind::Goto),
            CfgEdge::new(&target.id, &exit.id, CfgEdgeKind::Normal),
        ];

        let result = FieldLifecycleEngine::analyze_field_lifecycle(
            &nodes,
            &edges,
            "ptr",
            &OwnershipRules::default(),
        );
        let target_transition = result
            .transitions
            .iter()
            .find(|transition| transition.node_line == 9)
            .expect("goto target transition");
        assert!(
            target_transition.branch_frames.is_empty(),
            "a goto may bypass any active lexical branch joins"
        );
    }

    #[test]
    fn test_exception_handler_transition_has_owner_bound_frame() {
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let dispatch = make_node(Vec::new(), 10, CfgNodeKind::Branch, 10);
        let body_throw = make_node(Vec::new(), 20, CfgNodeKind::Throw, 20);
        let handler_id = CfgNodeId::generate(&fid, "test", 30);
        let handler = make_stmt_node(vec![se_store(handler_id, 0, "ptr")], 30, 30);
        let join = make_node(Vec::new(), 31, CfgNodeKind::Join, 11);
        let post_id = CfgNodeId::generate(&fid, "test", 40);
        let post = make_stmt_node(vec![se_free(post_id, 0, "ptr")], 40, 40);
        let nodes = vec![
            entry.clone(),
            dispatch.clone(),
            body_throw.clone(),
            handler.clone(),
            join.clone(),
            post.clone(),
            exit.clone(),
        ];
        let edges = vec![
            CfgEdge::new(&entry.id, &dispatch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&dispatch.id, &body_throw.id, CfgEdgeKind::Normal),
            CfgEdge::new(&dispatch.id, &handler.id, CfgEdgeKind::Exception),
            CfgEdge::new(&body_throw.id, &handler.id, CfgEdgeKind::Exception),
            CfgEdge::new(&handler.id, &join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&join.id, &post.id, CfgEdgeKind::Normal),
            CfgEdge::new(&post.id, &exit.id, CfgEdgeKind::Normal),
        ];

        let result = FieldLifecycleEngine::analyze_field_lifecycle(
            &nodes,
            &edges,
            "ptr",
            &OwnershipRules::default(),
        );
        let handler_transition = result
            .transitions
            .iter()
            .find(|transition| transition.node_line == 30)
            .expect("handler transition");
        assert_eq!(handler_transition.branch_frames.len(), 1);
        assert_eq!(
            handler_transition.branch_frames[0].branch_node_id,
            dispatch.id
        );
        assert_eq!(
            handler_transition.branch_frames[0].path,
            BranchPath::ExceptionPath
        );

        let post_transition = result
            .transitions
            .iter()
            .find(|transition| transition.node_line == 40)
            .expect("post-try transition");
        assert!(
            post_transition.branch_frames.is_empty(),
            "exception frame must be removed at the try Join"
        );
    }

    #[test]
    fn test_exception_handler_preserves_enclosing_branch_frame() {
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let outer_branch = make_node(Vec::new(), 1, CfgNodeKind::Branch, 1);
        let outer_join = make_node(Vec::new(), 50, CfgNodeKind::Join, 2);
        let dispatch = make_node(Vec::new(), 10, CfgNodeKind::Branch, 10);
        let body_throw = make_node(Vec::new(), 20, CfgNodeKind::Throw, 20);
        let handler_id = CfgNodeId::generate(&fid, "test", 30);
        let handler = make_stmt_node(vec![se_store(handler_id, 0, "ptr")], 30, 30);
        let try_join = make_node(Vec::new(), 31, CfgNodeKind::Join, 11);
        let post_id = CfgNodeId::generate(&fid, "test", 60);
        let post = make_stmt_node(vec![se_free(post_id, 0, "ptr")], 60, 60);
        let nodes = vec![
            entry.clone(),
            outer_branch.clone(),
            dispatch.clone(),
            body_throw.clone(),
            handler.clone(),
            try_join.clone(),
            outer_join.clone(),
            post.clone(),
            exit.clone(),
        ];
        let edges = vec![
            CfgEdge::new(&entry.id, &outer_branch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&outer_branch.id, &dispatch.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&outer_branch.id, &outer_join.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&dispatch.id, &body_throw.id, CfgEdgeKind::Normal),
            CfgEdge::new(&dispatch.id, &handler.id, CfgEdgeKind::Exception),
            CfgEdge::new(&body_throw.id, &handler.id, CfgEdgeKind::Exception),
            CfgEdge::new(&handler.id, &try_join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&try_join.id, &outer_join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&outer_join.id, &post.id, CfgEdgeKind::Normal),
            CfgEdge::new(&post.id, &exit.id, CfgEdgeKind::Normal),
        ];

        let result = FieldLifecycleEngine::analyze_field_lifecycle(
            &nodes,
            &edges,
            "ptr",
            &OwnershipRules::default(),
        );
        let handler_transition = result
            .transitions
            .iter()
            .find(|transition| transition.node_line == 30)
            .expect("handler transition");
        assert_eq!(handler_transition.branch_frames.len(), 2);
        assert_eq!(
            handler_transition.branch_frames[0].path,
            BranchPath::TruePath
        );
        assert_eq!(
            handler_transition.branch_frames[1].path,
            BranchPath::ExceptionPath
        );

        let post_transition = result
            .transitions
            .iter()
            .find(|transition| transition.node_line == 60)
            .expect("post-outer-branch transition");
        assert!(post_transition.branch_frames.is_empty());
    }

    #[test]
    fn entering_exception_handler_discards_frames_owned_by_try_region() {
        let fid = test_fid();
        let entry = CfgNode::entry(&fid);
        let exit = CfgNode::exit(&fid);
        let outer_branch = make_node(Vec::new(), 1, CfgNodeKind::Branch, 1);
        let mut dispatch = make_node(Vec::new(), 10, CfgNodeKind::Branch, 10);
        dispatch.stmt_range.end_byte = 100;
        let inner_branch = make_node(Vec::new(), 20, CfgNodeKind::Branch, 20);
        let handler = make_stmt_node(Vec::new(), 30, 30);
        let join = make_node(Vec::new(), 40, CfgNodeKind::Join, 11);
        let nodes = vec![
            entry.clone(),
            outer_branch.clone(),
            dispatch.clone(),
            inner_branch.clone(),
            handler.clone(),
            join.clone(),
            exit.clone(),
        ];
        let edges = vec![
            CfgEdge::new(&entry.id, &outer_branch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&outer_branch.id, &dispatch.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&dispatch.id, &inner_branch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&dispatch.id, &handler.id, CfgEdgeKind::Exception),
            CfgEdge::new(&handler.id, &join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&join.id, &exit.id, CfgEdgeKind::Normal),
        ];
        let graph = CfgGraph::build(&nodes, &edges).expect("valid try graph");
        let mut context = vec![
            BranchFrame {
                branch_node_id: outer_branch.id,
                join_node_id: None,
                branch_node_line: outer_branch.stmt_range.start_line,
                path: BranchPath::TruePath,
            },
            BranchFrame {
                branch_node_id: inner_branch.id,
                join_node_id: None,
                branch_node_line: inner_branch.stmt_range.start_line,
                path: BranchPath::TruePath,
            },
        ];

        enter_exception_handler_context(&mut context, &graph, handler.id);

        assert_eq!(context.len(), 2);
        assert_eq!(context[0].branch_node_id, outer_branch.id);
        assert_eq!(context[1].branch_node_id, dispatch.id);
        assert_eq!(context[1].path, BranchPath::ExceptionPath);
    }

    #[test]
    fn test_real_jemalloc_cpp_catch_transition_has_exception_frame() {
        use extraction::create_frontend;
        use tree_sitter::Parser;
        use types::enums::Language;

        // `examples/` is git-ignored, so a fresh clone does not have it. Read at
        // run time and skip: `include_str!` would fail the whole crate to compile.
        let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../examples/redis/deps/jemalloc/src/jemalloc_cpp.cpp");
        let Ok(source) = std::fs::read_to_string(&corpus) else {
            eprintln!(
                "skipping test: examples corpus file {} is unavailable. \
                 Populate `examples/` to run real-project regressions.",
                corpus.display()
            );
            return;
        };
        let source = source.as_str();
        let source_bytes = source.as_bytes().to_vec();
        let frontend = create_frontend(Language::Cpp).expect("C++ frontend");
        let mut parser = Parser::new();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .expect("C++ grammar");
        let tree = parser.parse(&source_bytes, None).expect("C++ parse");

        fn find_handle_oom<'a>(
            node: tree_sitter::Node<'a>,
            source: &[u8],
        ) -> Option<tree_sitter::Node<'a>> {
            if node.kind() == "function_definition"
                && node
                    .utf8_text(source)
                    .is_ok_and(|text| text.contains("handleOOM(std::size_t size, bool nothrow)"))
            {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(found) = find_handle_oom(child, source) {
                    return Some(found);
                }
            }
            None
        }

        let function = find_handle_oom(tree.root_node(), &source_bytes).expect("handleOOM");
        let fid = SymbolId::generate(
            &types::ids::FileId::generate("jemalloc_cpp.cpp"),
            "",
            "handleOOM",
            "function",
            None,
        );
        let mut cfg = extraction::CfgBuilder::build(Language::Cpp, &fid, function, &source_bytes);
        let catch_start = source
            .find("catch (const std::bad_alloc &)")
            .expect("real catch clause");
        let handler = cfg
            .nodes
            .iter_mut()
            .find(|node| {
                node.kind == CfgNodeKind::Statement
                    && node.stmt_range.start_byte as usize > catch_start
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text.trim() == "break;")
            })
            .expect("catch break");
        handler.semantic_effects = vec![se_store(handler.id, 0, "exception_marker")];
        let handler_id = handler.id;

        let result = FieldLifecycleEngine::analyze_field_lifecycle(
            &cfg.nodes,
            &cfg.edges,
            "exception_marker",
            &OwnershipRules::default(),
        );
        let transition = result
            .transitions
            .iter()
            .find(|transition| transition.node_id == handler_id)
            .expect("catch transition");
        assert!(transition.branch_frames.iter().any(|frame| {
            frame.path == BranchPath::ExceptionPath
                && cfg
                    .nodes
                    .iter()
                    .any(|node| node.id == frame.branch_node_id && node.kind == CfgNodeKind::Branch)
        }));
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
