//! Lifecycle Proof Mode — rule-backed ownership verification.
//!
//! Extends the basic lifecycle state machine with domain rules to
//! produce verified proofs (Safe/Suspicious/Incomplete).

use super::lifecycle::{FieldState, SuspiciousPoint};

/// Verdict from a rule-backed lifecycle proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleVerdict {
    Safe,
    Suspicious,
    Incomplete(String),
}

impl LifecycleVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Suspicious => "suspicious",
            Self::Incomplete(_) => "incomplete",
        }
    }
}

/// Evidence level for a lifecycle analysis conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceLevel {
    /// Budget-exhausted or incomplete analysis result.
    Incomplete,
    Heuristic,
    DomainRuleBacked,
    UserAnnotated,
}

impl EvidenceLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Incomplete => "incomplete",
            Self::Heuristic => "heuristic",
            Self::DomainRuleBacked => "domain_rule_backed",
            Self::UserAnnotated => "user_annotated",
        }
    }
}

/// A single path through the CFG, tracked with its conditions.
#[derive(Debug, Clone)]
pub struct PathProof {
    pub conditions: Vec<String>,
    pub states: Vec<(u32, String)>, // (line, state_name)
    pub exit_state: FieldState,
}

/// Result of a lifecycle proof analysis.
#[derive(Debug, Clone)]
pub struct LifecycleProof {
    pub field_path: String,
    pub function: String,
    pub paths: Vec<PathProof>,
    pub verdict: LifecycleVerdict,
    pub reasoning: String,
    pub evidence_level: EvidenceLevel,
}

/// Evaluate whether a lifecycle result constitutes a proof.
pub fn evaluate_proof(
    suspicious: &[SuspiciousPoint],
    final_state: FieldState,
    has_user_rules: bool,
    has_domain_rules: bool,
) -> LifecycleProof {
    let evidence_level = if has_user_rules {
        EvidenceLevel::UserAnnotated
    } else if has_domain_rules {
        EvidenceLevel::DomainRuleBacked
    } else {
        EvidenceLevel::Heuristic
    };

    // Build a single path proof from the lifecycle analysis
    let path = PathProof {
        conditions: Vec::new(),
        states: Vec::new(), // filled by caller
        exit_state: final_state,
    };

    if !suspicious.is_empty() {
        let reasons: Vec<String> = suspicious.iter().map(|s| s.message.clone()).collect();
        return LifecycleProof {
            field_path: String::new(),
            function: String::new(),
            paths: vec![path],
            verdict: LifecycleVerdict::Suspicious,
            reasoning: format!("Suspicious patterns found: {}", reasons.join("; ")),
            evidence_level,
        };
    }

    match final_state {
        FieldState::Freed | FieldState::Nullified | FieldState::Returned => LifecycleProof {
            field_path: String::new(),
            function: String::new(),
            paths: vec![path],
            verdict: LifecycleVerdict::Safe,
            reasoning: format!(
                "Field lifecycle terminates in {final_state:?} state — no leaks detected"
            ),
            evidence_level,
        },
        FieldState::Assigned | FieldState::MaybeLive => LifecycleProof {
            field_path: String::new(),
            function: String::new(),
            paths: vec![path],
            verdict: LifecycleVerdict::Incomplete(
                "Field may leak: allocated/live but no free found".into(),
            ),
            reasoning: "Allocation without visible free".into(),
            evidence_level,
        },
        _ => LifecycleProof {
            field_path: String::new(),
            function: String::new(),
            paths: vec![path],
            verdict: LifecycleVerdict::Incomplete("Unknown final state".into()),
            reasoning: "Cannot determine final state".into(),
            evidence_level,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::lifecycle::{FieldState, SuspiciousKind, SuspiciousPoint};

    #[test]
    fn test_evidence_level_ordering() {
        assert!(EvidenceLevel::UserAnnotated > EvidenceLevel::DomainRuleBacked);
        assert!(EvidenceLevel::DomainRuleBacked > EvidenceLevel::Heuristic);
    }

    #[test]
    fn test_evidence_level_as_str() {
        assert_eq!(EvidenceLevel::Heuristic.as_str(), "heuristic");
        assert_eq!(
            EvidenceLevel::DomainRuleBacked.as_str(),
            "domain_rule_backed"
        );
        assert_eq!(
            EvidenceLevel::UserAnnotated.as_str(),
            "user_annotated"
        );
    }

    #[test]
    fn test_verdict_as_str() {
        assert_eq!(LifecycleVerdict::Safe.as_str(), "safe");
        assert_eq!(LifecycleVerdict::Suspicious.as_str(), "suspicious");
        assert_eq!(
            LifecycleVerdict::Incomplete("test".into()).as_str(),
            "incomplete"
        );
    }

    #[test]
    fn test_evaluate_proof_safe_with_user_rules() {
        let proof = evaluate_proof(&[], FieldState::Freed, true, true);
        assert_eq!(proof.verdict, LifecycleVerdict::Safe);
        assert_eq!(proof.evidence_level, EvidenceLevel::UserAnnotated);
    }

    #[test]
    fn test_evaluate_proof_safe_with_domain_rules() {
        let proof = evaluate_proof(&[], FieldState::Freed, false, true);
        assert_eq!(proof.verdict, LifecycleVerdict::Safe);
        assert_eq!(proof.evidence_level, EvidenceLevel::DomainRuleBacked);
    }

    #[test]
    fn test_evaluate_proof_safe_heuristic() {
        let proof = evaluate_proof(&[], FieldState::Freed, false, false);
        assert_eq!(proof.verdict, LifecycleVerdict::Safe);
        assert_eq!(proof.evidence_level, EvidenceLevel::Heuristic);
    }

    #[test]
    fn test_evaluate_proof_suspicious_with_use_after_free() {
        let suspicious = vec![SuspiciousPoint {
            line: 42,
            kind: SuspiciousKind::UseAfterFree,
            message: "Read after free".into(),
        }];
        let proof = evaluate_proof(&suspicious, FieldState::Freed, false, false);
        assert_eq!(proof.verdict, LifecycleVerdict::Suspicious);
        assert!(proof.reasoning.contains("Read after free"));
    }

    #[test]
    fn test_evaluate_proof_incomplete_alloc_no_free() {
        let proof = evaluate_proof(&[], FieldState::Assigned, false, false);
        assert!(matches!(proof.verdict, LifecycleVerdict::Incomplete(_)));
    }

    #[test]
    fn test_evaluate_proof_returned_is_safe() {
        let proof = evaluate_proof(&[], FieldState::Returned, false, true);
        assert_eq!(proof.verdict, LifecycleVerdict::Safe);
    }

    #[test]
    fn test_lifecycle_proof_reasoning_accessible() {
        let proof = evaluate_proof(&[], FieldState::Freed, false, false);
        assert!(!proof.reasoning.is_empty());
    }
}
