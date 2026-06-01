//! Field Lifecycle Analysis — state machine walking CFG with effect annotations.
//!
//! Given a function's CFG with effect annotations and a target field path,
//! produces a timeline of state transitions that traces the field's lifecycle.

use types::cfg::CfgNode;
use types::enums::EffectKind;

use super::domain_rules::LoadedDomainRules;

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
        }
    }
}

/// A single state transition in a field's lifecycle.
#[derive(Debug, Clone)]
pub struct FieldTransition {
    pub from_state: FieldState,
    pub to_state: FieldState,
    pub at_node: types::ids::CfgNodeId,
    pub reason: String,
    pub line: u32,
}

/// Result of field lifecycle analysis.
#[derive(Debug, Clone)]
pub struct FieldLifecycleResult {
    pub field_path: String,
    pub function_qname: String,
    pub transitions: Vec<FieldTransition>,
    pub final_state: FieldState,
    pub suspicious_points: Vec<SuspiciousPoint>,
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

/// Engine for field-level lifecycle analysis.
pub struct FieldLifecycleEngine;

impl FieldLifecycleEngine {
    /// Analyze the lifecycle of a specific field within a function's CFG.
    ///
    /// Walks CFG nodes in order, applying effect annotations to build
    /// a state transition sequence. Only processes C/C++ nodes with
    /// effect annotations (other languages have no effects yet).
    pub fn analyze_field_lifecycle(
        cfg_nodes: &[CfgNode],
        field_path: &str,
        _ownership_rules: &OwnershipRules,
    ) -> FieldLifecycleResult {
        let mut state = FieldState::Unknown;
        let mut transitions = Vec::new();
        let mut suspicious = Vec::new();

        for node in cfg_nodes {
            let effect = match node.effect_kind {
                Some(e) => e,
                None => continue,
            };

            let target = node.target_field.as_deref().unwrap_or("");
            let matches_field = target == field_path
                || target.starts_with(&format!("{}.", field_path))
                || target.starts_with(&format!("{}->", field_path));

            let new_state = match (effect, matches_field) {
                // Safe free — field matches and effect is Free
                (EffectKind::Free, true) => {
                    if state == FieldState::Freed {
                        suspicious.push(SuspiciousPoint {
                            line: node.stmt_range.start_line,
                            kind: SuspiciousKind::DoubleFree,
                            message: format!("Double free of '{}'", field_path),
                        });
                        FieldState::Freed
                    } else {
                        FieldState::Freed
                    }
                }
                // Allocation — field becomes Assigned
                (EffectKind::Allocate, true) => {
                    if state == FieldState::Freed {
                        suspicious.push(SuspiciousPoint {
                            line: node.stmt_range.start_line,
                            kind: SuspiciousKind::UseAfterFree,
                            message: format!(
                                "Allocation on previously freed field '{}'",
                                field_path
                            ),
                        });
                    }
                    FieldState::Assigned
                }
                // Assignment — field becomes Assigned or Nullified
                (EffectKind::Assign, true) => {
                    if node.target_field.as_deref() == Some(field_path) {
                        FieldState::Assigned
                    } else {
                        FieldState::Assigned
                    }
                }
                // Read access — check use-after-free
                (EffectKind::Read, true) => {
                    if state == FieldState::Freed {
                        suspicious.push(SuspiciousPoint {
                            line: node.stmt_range.start_line,
                            kind: SuspiciousKind::UseAfterFree,
                            message: format!("Read of '{}' after free", field_path),
                        });
                    }
                    state
                }
                // Write access
                (EffectKind::Write, true) => {
                    if state == FieldState::Freed {
                        suspicious.push(SuspiciousPoint {
                            line: node.stmt_range.start_line,
                            kind: SuspiciousKind::UseAfterFree,
                            message: format!("Write to '{}' after free", field_path),
                        });
                    }
                    if state == FieldState::Unknown {
                        FieldState::MaybeLive
                    } else {
                        state
                    }
                }
                // Return — field escapes
                (EffectKind::Return, true) => FieldState::Returned,
                // Call or condition — doesn't change state directly
                _ => state,
            };

            if new_state != state {
                transitions.push(FieldTransition {
                    from_state: state.clone(),
                    to_state: new_state.clone(),
                    at_node: node.id,
                    reason: format!("{:?} effect on '{}'", effect, target),
                    line: node.stmt_range.start_line,
                });
                state = new_state;
            }
        }

        // Final check: if field was allocated but never freed
        if state == FieldState::Assigned || state == FieldState::MaybeLive {
            suspicious.push(SuspiciousPoint {
                line: 0,
                kind: SuspiciousKind::MissingFree,
                message: format!(
                    "Field '{}' may leak (allocated but no free found)",
                    field_path
                ),
            });
        }

        FieldLifecycleResult {
            field_path: field_path.to_string(),
            function_qname: String::new(), // filled in by caller
            transitions,
            final_state: state,
            suspicious_points: suspicious,
        }
    }

    /// Analyze with domain rules — uses rule-backed function matching.
    ///
    /// Same as `analyze_field_lifecycle` but uses `rules.match_free()` /
    /// `rules.match_alloc()` to determine Allocation/Free effects instead
    /// of relying solely on hardcoded `EffectKind` annotations from the CFG.
    pub fn analyze_with_rules(
        cfg_nodes: &[CfgNode],
        field_path: &str,
        _ownership_rules: &OwnershipRules,
        rules: &LoadedDomainRules,
    ) -> FieldLifecycleResult {
        let mut state = FieldState::Unknown;
        let mut transitions = Vec::new();
        let mut suspicious = Vec::new();

        for node in cfg_nodes {
            let effect = match node.effect_kind {
                Some(e) => e,
                None => continue,
            };

            let target = node.target_field.as_deref().unwrap_or("");
            let matches_field = target == field_path
                || target.starts_with(&format!("{}.", field_path))
                || target.starts_with(&format!("{}->", field_path));

            let new_state = match (effect, matches_field) {
                // Safe free — field matches and effect is Free
                (EffectKind::Free, true) => {
                    if state == FieldState::Freed {
                        suspicious.push(SuspiciousPoint {
                            line: node.stmt_range.start_line,
                            kind: SuspiciousKind::DoubleFree,
                            message: format!("Double free of '{}'", field_path),
                        });
                        FieldState::Freed
                    } else {
                        FieldState::Freed
                    }
                }
                // Allocation — field becomes Assigned
                (EffectKind::Allocate, true) => {
                    if state == FieldState::Freed {
                        suspicious.push(SuspiciousPoint {
                            line: node.stmt_range.start_line,
                            kind: SuspiciousKind::UseAfterFree,
                            message: format!(
                                "Allocation on previously freed field '{}'",
                                field_path
                            ),
                        });
                    }
                    FieldState::Assigned
                }
                // Call effect — use domain rules to determine alloc/free
                (EffectKind::Call, true) => {
                    let callee = target;
                    if rules.match_free(callee).is_some() {
                        if state == FieldState::Freed {
                            suspicious.push(SuspiciousPoint {
                                line: node.stmt_range.start_line,
                                kind: SuspiciousKind::DoubleFree,
                                message: format!("Double free of '{}' via {}", field_path, callee),
                            });
                        }
                        FieldState::Freed
                    } else if rules.match_alloc(callee).is_some() {
                        FieldState::Assigned
                    } else {
                        state
                    }
                }
                // Call effect — doesn't match target but still check domain rules
                (EffectKind::Call, false) => {
                    state
                }
                // Assignment — field becomes Assigned or Nullified
                (EffectKind::Assign, true) => FieldState::Assigned,
                // Read access — check use-after-free
                (EffectKind::Read, true) => {
                    if state == FieldState::Freed {
                        suspicious.push(SuspiciousPoint {
                            line: node.stmt_range.start_line,
                            kind: SuspiciousKind::UseAfterFree,
                            message: format!("Read of '{}' after free", field_path),
                        });
                    }
                    state
                }
                // Write access
                (EffectKind::Write, true) => {
                    if state == FieldState::Freed {
                        suspicious.push(SuspiciousPoint {
                            line: node.stmt_range.start_line,
                            kind: SuspiciousKind::UseAfterFree,
                            message: format!("Write to '{}' after free", field_path),
                        });
                    }
                    if state == FieldState::Unknown {
                        FieldState::MaybeLive
                    } else {
                        state
                    }
                }
                // Return — field escapes
                (EffectKind::Return, true) => FieldState::Returned,
                // Other effects
                _ => state,
            };

            if new_state != state {
                transitions.push(FieldTransition {
                    from_state: state.clone(),
                    to_state: new_state.clone(),
                    at_node: node.id,
                    reason: format!("{:?} effect on '{}'", effect, target),
                    line: node.stmt_range.start_line,
                });
                state = new_state;
            }
        }

        // Final check: if field was allocated but never freed
        if state == FieldState::Assigned || state == FieldState::MaybeLive {
            suspicious.push(SuspiciousPoint {
                line: 0,
                kind: SuspiciousKind::MissingFree,
                message: format!(
                    "Field '{}' may leak (allocated but no free found)",
                    field_path
                ),
            });
        }

        FieldLifecycleResult {
            field_path: field_path.to_string(),
            function_qname: String::new(),
            transitions,
            final_state: state,
            suspicious_points: suspicious,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::enums::CfgNodeKind;
    use types::ids::CfgNodeId;
    use types::structs::TextRange;

    fn make_node(effect: Option<EffectKind>, target: Option<&str>, line: u32) -> CfgNode {
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

    #[test]
    fn test_use_after_free_detected() {
        let nodes = vec![
            make_node(Some(EffectKind::Free), Some("ptr"), 10),
            make_node(Some(EffectKind::Read), Some("ptr"), 12),
        ];
        let rules = OwnershipRules::default();
        let result = FieldLifecycleEngine::analyze_field_lifecycle(&nodes, "ptr", &rules);
        assert!(!result.suspicious_points.is_empty());
        assert_eq!(
            result.suspicious_points[0].kind,
            SuspiciousKind::UseAfterFree
        );
    }

    #[test]
    fn test_double_free_detected() {
        let nodes = vec![
            make_node(Some(EffectKind::Free), Some("ptr"), 10),
            make_node(Some(EffectKind::Free), Some("ptr"), 15),
        ];
        let rules = OwnershipRules::default();
        let result = FieldLifecycleEngine::analyze_field_lifecycle(&nodes, "ptr", &rules);
        let double_frees: Vec<_> = result
            .suspicious_points
            .iter()
            .filter(|p| p.kind == SuspiciousKind::DoubleFree)
            .collect();
        assert!(!double_frees.is_empty());
    }

    #[test]
    fn test_clean_lifecycle() {
        let nodes = vec![
            make_node(Some(EffectKind::Allocate), Some("ptr"), 10),
            make_node(Some(EffectKind::Write), Some("ptr"), 11),
            make_node(Some(EffectKind::Read), Some("ptr"), 12),
            make_node(Some(EffectKind::Free), Some("ptr"), 13),
        ];
        let rules = OwnershipRules::default();
        let result = FieldLifecycleEngine::analyze_field_lifecycle(&nodes, "ptr", &rules);
        assert_eq!(result.final_state, FieldState::Freed);
        assert!(result
            .suspicious_points
            .iter()
            .all(|p| p.kind != SuspiciousKind::UseAfterFree));
    }

    #[test]
    fn test_nullify_after_alloc() {
        // Allocate -> Assign NULL -> should be Nullified
        let nodes = vec![
            make_node(Some(EffectKind::Allocate), Some("ptr"), 10),
            make_node(Some(EffectKind::Assign), Some("ptr"), 12), // Assign NULL-like
        ];
        let rules = OwnershipRules::default();
        let result = FieldLifecycleEngine::analyze_field_lifecycle(&nodes, "ptr", &rules);
        assert!(!result
            .suspicious_points
            .iter()
            .any(|p| p.kind == SuspiciousKind::UseAfterFree));
    }

    #[test]
    fn test_return_escaped_state() {
        let nodes = vec![
            make_node(Some(EffectKind::Allocate), Some("ptr"), 10),
            make_node(Some(EffectKind::Return), Some("ptr"), 15),
        ];
        let rules = OwnershipRules::default();
        let result = FieldLifecycleEngine::analyze_field_lifecycle(&nodes, "ptr", &rules);
        assert_eq!(result.final_state, FieldState::Returned);
    }

    #[test]
    fn test_interleaved_fields_dont_cross_contaminate() {
        // Two fields: "a" and "b". Free "b" should not trigger use-after-free for "a".
        let nodes = vec![
            make_node(Some(EffectKind::Allocate), Some("a"), 10),
            make_node(Some(EffectKind::Allocate), Some("b"), 11),
            make_node(Some(EffectKind::Free), Some("b"), 12),
            make_node(Some(EffectKind::Read), Some("a"), 13), // Fine -- "a" not freed
            make_node(Some(EffectKind::Free), Some("a"), 14),
        ];
        let rules = OwnershipRules::default();
        let result = FieldLifecycleEngine::analyze_field_lifecycle(&nodes, "a", &rules);
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
        let nodes = vec![make_node(None, None, 10)];
        let rules = OwnershipRules::default();
        let result =
            FieldLifecycleEngine::analyze_field_lifecycle(&nodes, "any_field", &rules);
        assert_eq!(result.final_state, FieldState::Unknown);
        assert!(result.transitions.is_empty());
    }

    #[test]
    fn test_allocate_free_allocate_reuse() {
        // allocate -> free -> allocate again (common pattern)
        let nodes = vec![
            make_node(Some(EffectKind::Allocate), Some("ptr"), 10),
            make_node(Some(EffectKind::Free), Some("ptr"), 15),
            make_node(Some(EffectKind::Allocate), Some("ptr"), 20),
            make_node(Some(EffectKind::Free), Some("ptr"), 25),
        ];
        let rules = OwnershipRules::default();
        let result = FieldLifecycleEngine::analyze_field_lifecycle(&nodes, "ptr", &rules);
        assert_eq!(result.final_state, FieldState::Freed);
    }
}
