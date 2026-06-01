//! Branch Effect Diff — compare sibling branch paths for suspicious asymmetries.
//!
//! Compares the side effects of true/false branches (if/else) or switch cases.
//! Detects patterns like: one branch frees a field but the other doesn't.

use types::cfg::CfgNode;
use types::enums::{CfgNodeKind, EffectKind};

/// Summary of effects on one side of a branch.
#[derive(Debug, Clone, Default)]
pub struct BranchPathSummary {
    pub frees: Vec<String>,
    pub allocates: Vec<String>,
    pub writes: Vec<String>,
    pub reads: Vec<String>,
    pub calls: Vec<String>,
}

/// Diff between two branch paths.
#[derive(Debug, Clone)]
pub struct BranchDiff {
    pub branch_node_line: u32,
    pub common_prefix: String,
    pub path_true: BranchPathSummary,
    pub path_false: BranchPathSummary,
    pub suspicious_asymmetry: Option<String>,
}

/// Engine for comparing branch side effects.
pub struct BranchDiffEngine;

impl BranchDiffEngine {
    /// Find and diff all sibling branch pairs in a function's CFG.
    pub fn diff_branches(cfg_nodes: &[CfgNode]) -> Vec<BranchDiff> {
        let mut diffs = Vec::new();
        let mut i = 0;

        // Use CfgNodeKind::Branch to find branch points
        while i < cfg_nodes.len() {
            if cfg_nodes[i].kind == CfgNodeKind::Branch {
                if let Some(diff) = Self::diff_single_branch(cfg_nodes, i) {
                    diffs.push(diff);
                }
            }
            i += 1;
        }

        diffs
    }

    /// Diff a single branch: collect effects from true-path and false-path.
    fn diff_single_branch(cfg_nodes: &[CfgNode], branch_idx: usize) -> Option<BranchDiff> {
        let branch_node = &cfg_nodes[branch_idx];

        // Collect common field prefix from branch condition
        let common_prefix = branch_node.target_field.clone().unwrap_or_default();

        // Walk forward from this branch to find the join point.
        // Collect nodes until we hit a Join node.
        let mut true_path = BranchPathSummary::default();
        let mut false_path = BranchPathSummary::default();
        let mut current_path = 0u8; // 0=unknown, 1=true, 2=false
        let mut depth = 0u32;

        for j in (branch_idx + 1)..cfg_nodes.len() {
            let node = &cfg_nodes[j];
            match node.kind {
                CfgNodeKind::Branch => {
                    depth += 1;
                    Self::accumulate(
                        node,
                        if current_path == 1 {
                            &mut true_path
                        } else {
                            &mut false_path
                        },
                    );
                }
                CfgNodeKind::Join => {
                    if depth == 0 {
                        break; // found the matching join
                    }
                    depth = depth.saturating_sub(1);
                    Self::accumulate(
                        node,
                        if current_path == 1 {
                            &mut true_path
                        } else {
                            &mut false_path
                        },
                    );
                }
                _ => {
                    // First statement after branch → true path
                    if current_path == 0 && branch_idx + 1 == j {
                        current_path = 1;
                    }
                    // Rough heuristic: if we see a cfg node far away from the branch
                    // on the same line or different pattern, we might be in false path.
                    // For simplicity, collect into the current path.
                    Self::accumulate(
                        node,
                        if current_path == 1 {
                            &mut true_path
                        } else {
                            &mut false_path
                        },
                    );
                }
            }
        }

        // Detect suspicious asymmetry
        let asymmetry = Self::detect_asymmetry(&true_path, &false_path, &common_prefix);

        Some(BranchDiff {
            branch_node_line: branch_node.stmt_range.start_line,
            common_prefix: if common_prefix.is_empty() {
                "?".to_string()
            } else {
                common_prefix
            },
            path_true: true_path,
            path_false: false_path,
            suspicious_asymmetry: asymmetry,
        })
    }

    fn accumulate(node: &CfgNode, summary: &mut BranchPathSummary) {
        let target = node.target_field.as_deref().unwrap_or("").to_string();
        match node.effect_kind {
            Some(EffectKind::Free) => summary.frees.push(target),
            Some(EffectKind::Allocate) => summary.allocates.push(target),
            Some(EffectKind::Write) | Some(EffectKind::Assign) => summary.writes.push(target),
            Some(EffectKind::Read) => summary.reads.push(target),
            Some(EffectKind::Call) => summary.calls.push(target),
            _ => {}
        }
    }

    /// Detect suspicious patterns — e.g., one branch frees a field but the other doesn't.
    fn detect_asymmetry(
        path_a: &BranchPathSummary,
        path_b: &BranchPathSummary,
        prefix: &str,
    ) -> Option<String> {
        // Check: one branch frees a field, the other doesn't
        for free_target in &path_a.frees {
            if !path_b
                .frees
                .iter()
                .any(|f| f == free_target || f.starts_with(prefix))
            {
                return Some(format!(
                    "Asymmetric free: '{}' freed in true branch but not in false branch",
                    free_target
                ));
            }
        }
        for free_target in &path_b.frees {
            if !path_a
                .frees
                .iter()
                .any(|f| f == free_target || f.starts_with(prefix))
            {
                return Some(format!(
                    "Asymmetric free: '{}' freed in false branch but not in true branch",
                    free_target
                ));
            }
        }

        // Check: one branch allocates, the other doesn't (possible allocation pattern)
        for alloc_target in &path_a.allocates {
            if !path_b.allocates.iter().any(|a| a == alloc_target) {
                return Some(format!(
                    "Asymmetric allocation: '{}' allocated in true branch but not false branch",
                    alloc_target
                ));
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::enums::CfgNodeKind;
    use types::ids::CfgNodeId;
    use types::structs::TextRange;

    fn make_branch_node(
        effect: Option<EffectKind>,
        target: Option<&str>,
        line: u32,
    ) -> CfgNode {
        CfgNode {
            id: CfgNodeId::default(),
            function_id: types::ids::SymbolId::default(),
            kind: CfgNodeKind::Branch,
            stmt_range: TextRange {
                start_byte: 0,
                end_byte: 0,
                start_line: line,
                start_column: 0,
                end_line: line,
                end_column: 0,
            },
            effect_kind: effect,
            target_field: target.map(|s| s.to_string()),
        }
    }

    fn make_stmt_node(
        effect: Option<EffectKind>,
        target: Option<&str>,
        line: u32,
    ) -> CfgNode {
        CfgNode {
            id: CfgNodeId::default(),
            function_id: types::ids::SymbolId::default(),
            kind: CfgNodeKind::Statement,
            stmt_range: TextRange {
                start_byte: 0,
                end_byte: 0,
                start_line: line,
                start_column: 0,
                end_line: line,
                end_column: 0,
            },
            effect_kind: effect,
            target_field: target.map(|s| s.to_string()),
        }
    }

    fn make_join_node(line: u32) -> CfgNode {
        CfgNode {
            id: CfgNodeId::default(),
            function_id: types::ids::SymbolId::default(),
            kind: CfgNodeKind::Join,
            stmt_range: TextRange {
                start_byte: 0,
                end_byte: 0,
                start_line: line,
                start_column: 0,
                end_line: line,
                end_column: 0,
            },
            effect_kind: None,
            target_field: None,
        }
    }

    #[test]
    fn test_empty_cfg() {
        let result = BranchDiffEngine::diff_branches(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_no_branches() {
        let node = make_stmt_node(None, None, 1);
        let result = BranchDiffEngine::diff_branches(&[node]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_branch_without_join() {
        let nodes = vec![make_branch_node(Some(EffectKind::Condition), Some("ptr"), 10)];
        let result = BranchDiffEngine::diff_branches(&nodes);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].branch_node_line, 10);
    }

    #[test]
    fn test_branch_with_asymmetric_free() {
        let nodes = vec![
            make_branch_node(Some(EffectKind::Condition), Some("ptr"), 10),
            make_stmt_node(Some(EffectKind::Free), Some("ptr"), 11),
            make_join_node(20),
        ];
        let result = BranchDiffEngine::diff_branches(&nodes);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_branch_with_nested_branch() {
        // Branch -> (Branch -> Free -> Join) -> Join
        let nodes = vec![
            make_branch_node(Some(EffectKind::Condition), Some("field_a"), 1),
            CfgNode {
                id: CfgNodeId::default(),
                function_id: types::ids::SymbolId::default(),
                kind: CfgNodeKind::Branch,
                stmt_range: TextRange {
                    start_byte: 0, end_byte: 0,
                    start_line: 2, start_column: 0,
                    end_line: 2, end_column: 0,
                },
                effect_kind: Some(EffectKind::Condition),
                target_field: Some("field_b".into()),
            },
            make_stmt_node(Some(EffectKind::Free), Some("field_a"), 3),
            make_join_node(4),
            make_join_node(5),
        ];
        let result = BranchDiffEngine::diff_branches(&nodes);
        assert!(!result.is_empty(), "Should find at least the outer branch");
    }

    #[test]
    fn test_branch_alloc_vs_no_alloc_asymmetry() {
        // Branch -> Alloc -> Join (only in one path => asymmetry)
        let nodes = vec![
            make_branch_node(Some(EffectKind::Condition), Some("ctx->buf"), 1),
            make_stmt_node(Some(EffectKind::Allocate), Some("ctx->buf"), 2),
            make_join_node(3),
        ];
        let result = BranchDiffEngine::diff_branches(&nodes);
        assert!(!result.is_empty());
        // The branch should be found
        assert!(result.len() >= 1, "Should detect at least one branch");
    }

    #[test]
    fn test_multiple_branches_in_function() {
        let nodes = vec![
            make_branch_node(Some(EffectKind::Condition), Some("x"), 1),
            make_stmt_node(Some(EffectKind::Free), Some("x"), 2),
            make_join_node(3),
            make_branch_node(Some(EffectKind::Condition), Some("y"), 4),
            make_stmt_node(Some(EffectKind::Free), Some("y"), 5),
            make_join_node(6),
        ];
        let result = BranchDiffEngine::diff_branches(&nodes);
        assert_eq!(result.len(), 2, "Should find both branches");
    }
}
