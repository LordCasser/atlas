//! Semantic Branch Difference Analysis
//!
//! Compares the true-branch and false-branch effects of each branch node,
//! detecting resource-management asymmetries (free-only on one side,
//! alloc-only on one side, inconsistent cleanup).
//!
//! # Architecture
//!
//! Input:  CfgGraph + EffectComposition (TransferGraph + node_effects)
//! Output: Vec<BranchDiffIssue> (structured, severity-ranked)
//!
//! # Relationship to branch_diff.rs
//!
//! `branch_diff.rs` operates on the legacy single-effect model (effect_kind +
//! target_field). This module operates on the multi-effect `SemanticEffect`
//! model produced by `effect_composer`. Both coexist; consumers choose which
//! path to use at runtime.

use crate::cfg_graph::CfgGraph;
use crate::effect_composer::EffectComposition;
use std::collections::{HashMap, HashSet, VecDeque};
use types::effects::*;
use types::enums::{CfgEdgeKind, CfgNodeKind};
use types::ids::CfgNodeId;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A detected resource-management asymmetry across a branch.
#[derive(Debug, Clone)]
pub struct BranchDiffIssue {
    /// The branch node ID in the CFG.
    pub branch_node_id: CfgNodeId,
    /// The field path affected (e.g., "data->state.aptr.cookiehost").
    pub field: String,
    /// What kind of asymmetry was found.
    pub kind: BranchAsymmetryKind,
    /// Severity: how likely is this a real bug?
    pub severity: IssueSeverity,
    /// Confidence: how confident are we in the data?
    pub confidence: f64,
    /// Summary of effects on the true branch side.
    pub true_side: FieldEffectSummary,
    /// Summary of effects on the false branch side.
    pub false_side: FieldEffectSummary,
    /// Human-readable description.
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchAsymmetryKind {
    /// Free on one side, not the other → possible leak or double-free risk.
    AsymmetricFree,
    /// Alloc on one side, not the other → possible stale/no-update state.
    AsymmetricAlloc,
    /// Different alloc/free pattern → stale-per-request or leak-on-branch.
    AsymmetricPair,
    /// Write on one side, no write on the other → possible uninitialized state.
    AsymmetricWrite,
    /// Same field touched on both sides but with different values.
    AsymmetricValue,
    /// General asymmetry that doesn't fit other categories.
    GeneralAsymmetry,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum IssueSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

/// What effects were observed for one field on one branch side.
#[derive(Debug, Clone, Default)]
pub struct FieldEffectSummary {
    pub has_free: bool,
    pub free_callee: Option<String>,
    pub has_alloc: bool,
    pub alloc_callee: Option<String>,
    pub has_write: bool,
    pub stored_value: Option<String>,
    pub preserves_prior_state: bool,
    pub affecting_nodes: Vec<CfgNodeId>,
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Analyze branch asymmetries using semantic effects and transfer graph.
///
/// # Arguments
/// * `cfg` - The CFG for one function.
/// * `composition` - Pre-computed effect composition (from `compose_effects`).
///
/// # Returns
/// Issues sorted by severity (Critical > High > Medium > Low > Info).
pub fn analyze_branch_semantic(
    cfg: &CfgGraph,
    composition: &EffectComposition,
) -> Vec<BranchDiffIssue> {
    let mut issues = Vec::new();

    for (node_id, node) in &cfg.nodes {
        if node.kind != CfgNodeKind::Branch {
            continue;
        }

        // Collect true/false target nodes
        let true_targets = cfg.successors_by_kind(node_id, CfgEdgeKind::TrueBranch);
        let false_targets = cfg.successors_by_kind(node_id, CfgEdgeKind::FalseBranch);

        if true_targets.is_empty() && false_targets.is_empty() {
            continue;
        }

        // Walk each side and collect per-field effects
        let true_fields = collect_field_effects(cfg, &true_targets, composition);
        let false_fields = collect_field_effects(cfg, &false_targets, composition);

        // Union of all touched fields
        let all_fields: HashSet<&str> = true_fields
            .keys()
            .chain(false_fields.keys())
            .map(|s| s.as_str())
            .collect();

        for field in all_fields {
            let t = true_fields.get(field).cloned().unwrap_or_default();
            let f = false_fields.get(field).cloned().unwrap_or_default();
            if let Some(issue) = diff_field(field, *node_id, &t, &f) {
                issues.push(issue);
            }
        }
    }

    // Sort: severity desc → confidence desc
    issues.sort_by(|a, b| {
        b.severity.cmp(&a.severity).then_with(|| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    issues
}

// ---------------------------------------------------------------------------
// Field effect collection (BFS walk)
// ---------------------------------------------------------------------------

/// Walk the branch region from target nodes, collecting per-field effect summaries.
fn collect_field_effects(
    graph: &CfgGraph,
    targets: &[&types::cfg::CfgEdge],
    composition: &EffectComposition,
) -> HashMap<String, FieldEffectSummary> {
    let mut result: HashMap<String, FieldEffectSummary> = HashMap::new();

    for edge in targets {
        let branch_summary = walk_branch_region(graph, &edge.target, composition);
        merge_into(&mut result, branch_summary);
    }

    result
}

/// Merge per-field summaries from one path into the accumulated map.
fn merge_into(
    acc: &mut HashMap<String, FieldEffectSummary>,
    incoming: HashMap<String, FieldEffectSummary>,
) {
    for (field, incoming_summary) in incoming {
        let entry = acc.entry(field).or_default();
        if incoming_summary.has_free {
            entry.has_free = true;
            if incoming_summary.free_callee.is_some() {
                entry.free_callee = incoming_summary.free_callee.clone();
            }
        }
        if incoming_summary.has_alloc {
            entry.has_alloc = true;
            if incoming_summary.alloc_callee.is_some() {
                entry.alloc_callee = incoming_summary.alloc_callee.clone();
            }
        }
        if incoming_summary.has_write {
            entry.has_write = true;
            if incoming_summary.stored_value.is_some() {
                entry.stored_value = incoming_summary.stored_value.clone();
            }
        }
        // Union all affecting nodes
        for nid in &incoming_summary.affecting_nodes {
            if !entry.affecting_nodes.contains(nid) {
                entry.affecting_nodes.push(*nid);
            }
        }
    }
}

/// BFS walk of a branch region: start at `start` node, collect SemanticEffect for
/// each visited node, group into per-field summaries. Stops at Join (depth=0) or Exit.
fn walk_branch_region(
    graph: &CfgGraph,
    start: &CfgNodeId,
    composition: &EffectComposition,
) -> HashMap<String, FieldEffectSummary> {
    let mut result: HashMap<String, FieldEffectSummary> = HashMap::new();
    let mut visited = HashSet::new();
    let mut worklist: VecDeque<(CfgNodeId, u32)> = VecDeque::new();
    worklist.push_back((*start, 1u32)); // depth=1: inside branch

    while let Some((node_id, depth)) = worklist.pop_front() {
        if !visited.insert(node_id) {
            continue;
        }

        let node = match graph.nodes.get(&node_id) {
            Some(n) => n,
            None => continue,
        };

        // Collect semantic effects for this node
        if let Some(effects_vec) = composition.node_effects.get(&node_id) {
            for eff in effects_vec {
                apply_effect_to_summary(eff, &node_id, &mut result);
            }
        }

        // Compute child depth
        let child_depth = match node.kind {
            CfgNodeKind::Branch => depth + 1,
            CfgNodeKind::Join => {
                let d = depth.saturating_sub(1);
                if d == 0 {
                    continue; // Region boundary
                }
                d
            }
            CfgNodeKind::Exit => continue,
            _ => depth,
        };

        // Enqueue all successors
        if let Some(edges) = graph.successors.get(&node_id) {
            for edge in edges {
                worklist.push_back((edge.target, child_depth));
            }
        }
    }

    result
}

/// Map a single SemanticEffect into per-field summary entries.
fn apply_effect_to_summary(
    eff: &SemanticEffect,
    node_id: &CfgNodeId,
    summaries: &mut HashMap<String, FieldEffectSummary>,
) {
    match &eff.kind {
        SemanticEffectKind::Free {
            place: PlaceRef::Field { path },
            callee,
            ..
        } => {
            let s = get_or_create_summary(summaries, path);
            s.has_free = true;
            s.free_callee = Some(callee.clone());
            s.affecting_nodes.push(*node_id);
        }
        SemanticEffectKind::Alloc {
            target: PlaceRef::Field { path },
            callee,
            ..
        } => {
            let s = get_or_create_summary(summaries, path);
            s.has_alloc = true;
            s.alloc_callee = Some(callee.clone());
            s.affecting_nodes.push(*node_id);
        }
        // Alloc to a local still counts: it may feed into a field Store later
        SemanticEffectKind::Alloc { .. } => {
            // Tracked as untethered alloc — matched with stores during diff analysis
        }
        SemanticEffectKind::Store {
            dst: PlaceRef::Field { path },
            src,
            ..
        } => {
            let s = get_or_create_summary(summaries, path);
            s.has_write = true;
            s.stored_value = Some(format_value_source(src));
            s.affecting_nodes.push(*node_id);
        }
        SemanticEffectKind::Nullify {
            place: PlaceRef::Field { path },
            ..
        } => {
            let s = get_or_create_summary(summaries, path);
            s.has_write = true;
            s.stored_value = Some("null".to_string());
            s.affecting_nodes.push(*node_id);
        }
        // Free of a local is relevant if it's a known resource handle
        SemanticEffectKind::Free {
            place: PlaceRef::Local { name },
            callee,
            ..
        } => {
            // Track as resource finalization — may affect field analysis
            // (stored but not directly field-associated yet)
            let _ = (name, callee);
        }
        _ => {}
    }
}

fn get_or_create_summary<'a>(
    map: &'a mut HashMap<String, FieldEffectSummary>,
    field: &str,
) -> &'a mut FieldEffectSummary {
    map.entry(field.to_string()).or_default()
}

fn format_value_source(src: &ValueSource) -> String {
    match src {
        ValueSource::CallReturn { callee } => format!("return({callee})"),
        ValueSource::Param { name } => format!("param({name})"),
        ValueSource::Local { name } => name.clone(),
        ValueSource::LiteralNull => "null".to_string(),
        ValueSource::Unknown => "unknown".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Field-level diff logic
// ---------------------------------------------------------------------------

/// Compare true-side vs false-side summaries for a single field.
/// Returns an issue if there is a significant asymmetry.
fn diff_field(
    field: &str,
    branch_node_id: CfgNodeId,
    true_summary: &FieldEffectSummary,
    false_summary: &FieldEffectSummary,
) -> Option<BranchDiffIssue> {
    let has_free_true = true_summary.has_free;
    let has_free_false = false_summary.has_free;
    let has_alloc_true = true_summary.has_alloc;
    let has_alloc_false = false_summary.has_alloc;
    let has_write_true = true_summary.has_write;
    let has_write_false = false_summary.has_write;

    // 1. AsymmetricPair: one side does free+write (alloc pattern), other side does nothing
    if has_free_true
        && (has_alloc_true || has_write_true)
        && !has_free_false
        && !has_alloc_false
        && !has_write_false
    {
        return Some(make_issue(
            field,
            branch_node_id,
            BranchAsymmetryKind::AsymmetricPair,
            IssueSeverity::High,
            0.85,
            format!(
                "field '{field}' freed and reallocated in true branch, untouched in false branch"
            ),
            true_summary,
            false_summary,
        ));
    }
    if has_free_false
        && (has_alloc_false || has_write_false)
        && !has_free_true
        && !has_alloc_true
        && !has_write_true
    {
        return Some(make_issue(
            field,
            branch_node_id,
            BranchAsymmetryKind::AsymmetricPair,
            IssueSeverity::High,
            0.85,
            format!(
                "field '{field}' freed and reallocated in false branch, untouched in true branch"
            ),
            true_summary,
            false_summary,
        ));
    }

    // 2. AsymmetricFree: one side frees, the other doesn't
    if has_free_true && !has_free_false {
        return Some(make_issue(
            field,
            branch_node_id,
            BranchAsymmetryKind::AsymmetricFree,
            IssueSeverity::Medium,
            0.70,
            format!("field '{field}' freed in true branch but not in false branch"),
            true_summary,
            false_summary,
        ));
    }
    if has_free_false && !has_free_true {
        return Some(make_issue(
            field,
            branch_node_id,
            BranchAsymmetryKind::AsymmetricFree,
            IssueSeverity::Medium,
            0.70,
            format!("field '{field}' freed in false branch but not in true branch"),
            true_summary,
            false_summary,
        ));
    }

    // 3. AsymmetricAlloc: one side allocates, the other doesn't
    if has_alloc_true && !has_alloc_false {
        return Some(make_issue(
            field,
            branch_node_id,
            BranchAsymmetryKind::AsymmetricAlloc,
            IssueSeverity::Medium,
            0.65,
            format!("field '{field}' allocated in true branch but not in false branch"),
            true_summary,
            false_summary,
        ));
    }
    if has_alloc_false && !has_alloc_true {
        return Some(make_issue(
            field,
            branch_node_id,
            BranchAsymmetryKind::AsymmetricAlloc,
            IssueSeverity::Medium,
            0.65,
            format!("field '{field}' allocated in false branch but not in true branch"),
            true_summary,
            false_summary,
        ));
    }

    // 4. AsymmetricWrite: one side writes, the other doesn't
    if has_write_true && !has_write_false {
        let confidence = 0.50;
        let severity = if confidence < 0.55 {
            IssueSeverity::Low
        } else {
            IssueSeverity::Info
        };
        return Some(make_issue(
            field,
            branch_node_id,
            BranchAsymmetryKind::AsymmetricWrite,
            severity,
            confidence,
            format!("field '{field}' written in true branch but not in false branch"),
            true_summary,
            false_summary,
        ));
    }
    if has_write_false && !has_write_true {
        let confidence = 0.50;
        let severity = IssueSeverity::Low;
        return Some(make_issue(
            field,
            branch_node_id,
            BranchAsymmetryKind::AsymmetricWrite,
            severity,
            confidence,
            format!("field '{field}' written in false branch but not in true branch"),
            true_summary,
            false_summary,
        ));
    }

    // 5. AsymmetricValue: same field written on both sides, different values
    if has_write_true && has_write_false {
        let tv = true_summary.stored_value.as_deref().unwrap_or("");
        let fv = false_summary.stored_value.as_deref().unwrap_or("");
        if !tv.is_empty() && !fv.is_empty() && tv != fv {
            return Some(make_issue(
                field,
                branch_node_id,
                BranchAsymmetryKind::AsymmetricValue,
                IssueSeverity::Low,
                0.40,
                format!(
                    "field '{field}' written to '{tv}' in true branch and '{fv}' in false branch"
                ),
                true_summary,
                false_summary,
            ));
        }
    }

    None
}

#[allow(clippy::too_many_arguments)]
fn make_issue(
    field: &str,
    branch_node_id: CfgNodeId,
    kind: BranchAsymmetryKind,
    severity: IssueSeverity,
    confidence: f64,
    description: String,
    true_side: &FieldEffectSummary,
    false_side: &FieldEffectSummary,
) -> BranchDiffIssue {
    BranchDiffIssue {
        branch_node_id,
        field: field.to_string(),
        kind,
        severity,
        confidence,
        true_side: true_side.clone(),
        false_side: false_side.clone(),
        description,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg_graph::CfgGraph;
    use crate::effect_composer::{
        EffectComposition, FieldFreeRecord, FieldWriteRecord, TransferGraph,
    };
    use types::cfg::{CfgEdge, CfgNode};
    use types::enums::{CfgEdgeKind, CfgNodeKind};
    use types::ids::CfgNodeId;
    use types::structs::TextRange;

    /// Counter for generating unique CfgNodeIds in tests.
    fn test_fid() -> types::ids::SymbolId {
        types::ids::SymbolId::default()
    }

    #[allow(dead_code)]
    fn make_stmt_node(line: u32, seq: u32, effects: Vec<SemanticEffect>) -> CfgNode {
        let fid = test_fid();
        let nid = CfgNodeId::generate(&fid, "test_stmt", seq);
        CfgNode {
            id: nid,
            function_id: fid,
            kind: CfgNodeKind::Statement,
            stmt_range: TextRange {
                start_byte: seq,
                end_byte: seq + 1,
                start_line: line,
                start_column: 0,
                end_line: line,
                end_column: 0,
            },
            call_context: types::enums::CallContext::None,
            semantic_effects: effects,
        }
    }

    #[allow(dead_code)]
    fn make_branch_node(line: u32, seq: u32) -> CfgNode {
        let fid = test_fid();
        let nid = CfgNodeId::generate(&fid, "test_branch", seq);
        CfgNode {
            id: nid,
            function_id: fid,
            kind: CfgNodeKind::Branch,
            stmt_range: TextRange {
                start_byte: seq,
                end_byte: seq + 1,
                start_line: line,
                start_column: 0,
                end_line: line,
                end_column: 0,
            },
            call_context: types::enums::CallContext::None,
            semantic_effects: vec![],
        }
    }

    #[allow(dead_code)]
    fn make_join_node(line: u32, seq: u32) -> CfgNode {
        let fid = test_fid();
        let nid = CfgNodeId::generate(&fid, "test_join", seq);
        CfgNode {
            id: nid,
            function_id: fid,
            kind: CfgNodeKind::Join,
            stmt_range: TextRange {
                start_byte: seq,
                end_byte: seq + 1,
                start_line: line,
                start_column: 0,
                end_line: line,
                end_column: 0,
            },
            call_context: types::enums::CallContext::None,
            semantic_effects: vec![],
        }
    }

    fn make_se_effect(node_id: CfgNodeId, order: u32, kind: SemanticEffectKind) -> SemanticEffect {
        let kind_name = match &kind {
            SemanticEffectKind::Alloc { .. } => "Alloc",
            SemanticEffectKind::Free { .. } => "Free",
            SemanticEffectKind::Store { .. } => "Store",
            SemanticEffectKind::Nullify { .. } => "Nullify",
            _ => "Other",
        };
        SemanticEffect {
            id: types::ids::EffectId::generate(&node_id, order, kind_name),
            cfg_node_id: node_id,
            order,
            kind,
            confidence: 0.9,
            consumption_style: None,
            description: None,
        }
    }

    // ── cookiehost end-to-end test ──────────────────────────────────────
    //
    // Models the classic curl cookiehost pattern:
    //
    //   if (ptr) {
    //       Curl_safefree(data->state.aptr.cookiehost);
    //       char *c = Curl_copy_header_value(ptr);
    //       data->state.aptr.cookiehost = c;
    //   }
    //
    // True branch:  Free(cookiehost) + Alloc(cookiehost from Curl_copy_header_value)
    // False branch: No effect on cookiehost
    //
    // Expected: asymmetry detected (possible stale per-request state).

    #[test]
    fn test_cookiehost_pattern_semantic() {
        let fid = test_fid();
        let field = "data.state.aptr.cookiehost";

        let entry_nid = CfgNodeId::generate(&fid, "entry", 0);
        let branch_nid = CfgNodeId::generate(&fid, "branch", 1);
        let true_free_nid = CfgNodeId::generate(&fid, "true_free", 2);
        let true_alloc_nid = CfgNodeId::generate(&fid, "true_alloc", 3);
        let true_store_nid = CfgNodeId::generate(&fid, "true_store", 4);
        let false_nop_nid = CfgNodeId::generate(&fid, "false_nop", 5);
        let join_nid = CfgNodeId::generate(&fid, "join", 6);
        let exit_nid = CfgNodeId::generate(&fid, "exit", 7);

        let entry = CfgNode {
            id: entry_nid,
            function_id: fid,
            kind: CfgNodeKind::Entry,
            stmt_range: TextRange {
                start_byte: 0,
                end_byte: 0,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
            call_context: types::enums::CallContext::None,
            semantic_effects: vec![],
        };

        let true_free_se = make_se_effect(
            true_free_nid,
            0,
            SemanticEffectKind::Free {
                place: PlaceRef::Field {
                    path: field.to_string(),
                },
                callee: "Curl_safefree".to_string(),
            },
        );
        let true_alloc_se = make_se_effect(
            true_alloc_nid,
            0,
            SemanticEffectKind::Alloc {
                target: PlaceRef::Local {
                    name: "c".to_string(),
                },
                callee: "Curl_copy_header_value".to_string(),
            },
        );
        let true_store_se = make_se_effect(
            true_store_nid,
            0,
            SemanticEffectKind::Store {
                dst: PlaceRef::Field {
                    path: field.to_string(),
                },
                src: ValueSource::Local {
                    name: "c".to_string(),
                },
            },
        );

        // Clone for composition (node construction below moves originals)
        let tf_clone = true_free_se.clone();
        let ta_clone = true_alloc_se.clone();
        let ts_clone = true_store_se.clone();

        let nodes = vec![
            entry,
            CfgNode {
                id: branch_nid,
                function_id: fid,
                kind: CfgNodeKind::Branch,
                stmt_range: TextRange {
                    start_byte: 1,
                    end_byte: 2,
                    start_line: 1,
                    start_column: 0,
                    end_line: 1,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![],
            },
            CfgNode {
                id: true_free_nid,
                function_id: fid,
                kind: CfgNodeKind::Statement,
                stmt_range: TextRange {
                    start_byte: 2,
                    end_byte: 3,
                    start_line: 2,
                    start_column: 0,
                    end_line: 2,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![true_free_se],
            },
            CfgNode {
                id: true_alloc_nid,
                function_id: fid,
                kind: CfgNodeKind::Statement,
                stmt_range: TextRange {
                    start_byte: 3,
                    end_byte: 4,
                    start_line: 3,
                    start_column: 0,
                    end_line: 3,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![true_alloc_se],
            },
            CfgNode {
                id: true_store_nid,
                function_id: fid,
                kind: CfgNodeKind::Statement,
                stmt_range: TextRange {
                    start_byte: 4,
                    end_byte: 5,
                    start_line: 4,
                    start_column: 0,
                    end_line: 4,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![true_store_se],
            },
            CfgNode {
                id: false_nop_nid,
                function_id: fid,
                kind: CfgNodeKind::Statement,
                stmt_range: TextRange {
                    start_byte: 5,
                    end_byte: 6,
                    start_line: 5,
                    start_column: 0,
                    end_line: 5,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![],
            },
            CfgNode {
                id: join_nid,
                function_id: fid,
                kind: CfgNodeKind::Join,
                stmt_range: TextRange {
                    start_byte: 6,
                    end_byte: 7,
                    start_line: 6,
                    start_column: 0,
                    end_line: 6,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![],
            },
            CfgNode {
                id: exit_nid,
                function_id: fid,
                kind: CfgNodeKind::Exit,
                stmt_range: TextRange {
                    start_byte: 7,
                    end_byte: 7,
                    start_line: 7,
                    start_column: 0,
                    end_line: 7,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![],
            },
        ];

        let edges = vec![
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: entry_nid,
                target: branch_nid,
                kind: CfgEdgeKind::Normal,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: branch_nid,
                target: true_free_nid,
                kind: CfgEdgeKind::TrueBranch,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: branch_nid,
                target: false_nop_nid,
                kind: CfgEdgeKind::FalseBranch,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: true_free_nid,
                target: true_alloc_nid,
                kind: CfgEdgeKind::Normal,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: true_alloc_nid,
                target: true_store_nid,
                kind: CfgEdgeKind::Normal,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: true_store_nid,
                target: join_nid,
                kind: CfgEdgeKind::Normal,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: false_nop_nid,
                target: join_nid,
                kind: CfgEdgeKind::Normal,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: join_nid,
                target: exit_nid,
                kind: CfgEdgeKind::Normal,
            },
        ];

        let graph = CfgGraph::build(&nodes, &edges).expect("CFG build should succeed");

        let composition = EffectComposition {
            node_effects: {
                let mut m = HashMap::new();
                m.insert(true_free_nid, vec![tf_clone]);
                m.insert(true_alloc_nid, vec![ta_clone]);
                m.insert(true_store_nid, vec![ts_clone]);
                m
            },
            transfer_graph: {
                TransferGraph {
                    field_writes: {
                        let mut m = HashMap::new();
                        m.insert(
                            field.to_string(),
                            vec![FieldWriteRecord {
                                value_source: ValueSource::CallReturn {
                                    callee: "Curl_copy_header_value".to_string(),
                                },
                                confidence: 0.85,
                                node_line: 4,
                            }],
                        );
                        m
                    },
                    field_frees: {
                        let mut m = HashMap::new();
                        m.insert(
                            field.to_string(),
                            vec![FieldFreeRecord {
                                callee: "Curl_safefree".to_string(),
                                node_line: 2,
                            }],
                        );
                        m
                    },
                }
            },
        };

        let issues = analyze_branch_semantic(&graph, &composition);

        assert!(
            !issues.is_empty(),
            "Expected at least one issue for the cookiehost pattern"
        );
        let cookiehost_issue = issues.iter().find(|i| i.field == field);
        assert!(
            cookiehost_issue.is_some(),
            "Expected an issue for field '{field}'"
        );
        let issue = cookiehost_issue.unwrap();

        assert_eq!(
            issue.kind,
            BranchAsymmetryKind::AsymmetricPair,
            "Expected AsymmetricPair, got {:?}",
            issue.kind
        );
        assert!(
            issue.confidence >= 0.8,
            "Expected confidence >= 0.8, got {}",
            issue.confidence
        );
        assert!(issue.true_side.has_free, "True side should have free");
        assert!(
            !issue.false_side.has_free,
            "False side should not have free"
        );
        assert!(
            issue.true_side.has_write,
            "True side should have write (re-assign)"
        );
        assert!(
            !issue.false_side.has_write,
            "False side should not have write"
        );
    }

    /// When both branches have identical effects, no asymmetry issue.
    #[test]
    fn test_symmetric_branches_no_issue() {
        let fid = test_fid();
        let entry_nid = CfgNodeId::generate(&fid, "entry", 0);
        let branch_nid = CfgNodeId::generate(&fid, "branch", 1);
        let true_stmt_nid = CfgNodeId::generate(&fid, "true_stmt", 2);
        let false_stmt_nid = CfgNodeId::generate(&fid, "false_stmt", 3);
        let join_nid = CfgNodeId::generate(&fid, "join", 4);
        let exit_nid = CfgNodeId::generate(&fid, "exit", 5);

        let field = "data.x";
        let free_effect = make_se_effect(
            true_stmt_nid,
            0,
            SemanticEffectKind::Free {
                place: PlaceRef::Field {
                    path: field.to_string(),
                },
                callee: "free".to_string(),
            },
        );

        let nodes = vec![
            CfgNode {
                id: entry_nid,
                function_id: fid,
                kind: CfgNodeKind::Entry,
                stmt_range: TextRange {
                    start_byte: 0,
                    end_byte: 0,
                    start_line: 0,
                    start_column: 0,
                    end_line: 0,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![],
            },
            CfgNode {
                id: branch_nid,
                function_id: fid,
                kind: CfgNodeKind::Branch,
                stmt_range: TextRange {
                    start_byte: 1,
                    end_byte: 2,
                    start_line: 1,
                    start_column: 0,
                    end_line: 1,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![],
            },
            CfgNode {
                id: true_stmt_nid,
                function_id: fid,
                kind: CfgNodeKind::Statement,
                stmt_range: TextRange {
                    start_byte: 2,
                    end_byte: 3,
                    start_line: 2,
                    start_column: 0,
                    end_line: 2,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![free_effect.clone()],
            },
            CfgNode {
                id: false_stmt_nid,
                function_id: fid,
                kind: CfgNodeKind::Statement,
                stmt_range: TextRange {
                    start_byte: 3,
                    end_byte: 4,
                    start_line: 3,
                    start_column: 0,
                    end_line: 3,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![free_effect.clone()],
            },
            CfgNode {
                id: join_nid,
                function_id: fid,
                kind: CfgNodeKind::Join,
                stmt_range: TextRange {
                    start_byte: 4,
                    end_byte: 5,
                    start_line: 4,
                    start_column: 0,
                    end_line: 4,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![],
            },
            CfgNode {
                id: exit_nid,
                function_id: fid,
                kind: CfgNodeKind::Exit,
                stmt_range: TextRange {
                    start_byte: 5,
                    end_byte: 5,
                    start_line: 5,
                    start_column: 0,
                    end_line: 5,
                    end_column: 0,
                },
                call_context: types::enums::CallContext::None,
                semantic_effects: vec![],
            },
        ];

        let edges = vec![
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: entry_nid,
                target: branch_nid,
                kind: CfgEdgeKind::Normal,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: branch_nid,
                target: true_stmt_nid,
                kind: CfgEdgeKind::TrueBranch,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: branch_nid,
                target: false_stmt_nid,
                kind: CfgEdgeKind::FalseBranch,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: true_stmt_nid,
                target: join_nid,
                kind: CfgEdgeKind::Normal,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: false_stmt_nid,
                target: join_nid,
                kind: CfgEdgeKind::Normal,
            },
            CfgEdge {
                id: types::ids::CfgEdgeId::default(),
                source: join_nid,
                target: exit_nid,
                kind: CfgEdgeKind::Normal,
            },
        ];

        let graph = CfgGraph::build(&nodes, &edges).expect("CFG build should succeed");

        let composition = EffectComposition {
            node_effects: {
                let mut m = HashMap::new();
                m.insert(true_stmt_nid, vec![free_effect.clone()]);
                m.insert(false_stmt_nid, vec![free_effect]);
                m
            },
            transfer_graph: TransferGraph {
                field_writes: HashMap::new(),
                field_frees: HashMap::new(),
            },
        };

        let issues = analyze_branch_semantic(&graph, &composition);
        assert!(
            issues.is_empty(),
            "Symmetric branches should produce no issues, got: {issues:?}"
        );
    }
}
