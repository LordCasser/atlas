//! Tests for edge conflict resolution policy.

use super::edge_policy::*;
use types::structs::{CoverageTier, Precision, SemanticConfidence};

fn closure_precision(confidence: SemanticConfidence) -> Precision {
    Precision {
        coverage: CoverageTier::ClosureComplete {
            closure_id: "test-closure-1".into(),
        },
        confidence,
    }
}

fn manifest_precision(confidence: SemanticConfidence) -> Precision {
    Precision {
        coverage: CoverageTier::Manifest,
        confidence,
    }
}

fn certain_precision() -> Precision {
    Precision {
        coverage: CoverageTier::RepoComplete,
        confidence: SemanticConfidence::Certain,
    }
}

// ── Existing edge rules ────────────────────────────────────────────────

#[test]
fn test_certain_immutable() {
    // Existing Certain edge + incoming Low → Keep
    let existing = certain_precision();
    let incoming = manifest_precision(SemanticConfidence::Low);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(result, EdgeResolution::Keep);
}

#[test]
fn test_higher_coverage_wins() {
    // Existing Manifest + incoming ClosureComplete → Replace
    let existing = manifest_precision(SemanticConfidence::High);
    let incoming = closure_precision(SemanticConfidence::Medium);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(result, EdgeResolution::Replace);
}

#[test]
fn test_lower_coverage_loses() {
    // Existing ClosureComplete + incoming Manifest → Keep
    let existing = closure_precision(SemanticConfidence::High);
    let incoming = manifest_precision(SemanticConfidence::High);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(result, EdgeResolution::Keep);
}

#[test]
fn test_same_coverage_higher_confidence_wins() {
    // Both ClosureComplete, existing Medium + incoming High → Replace
    let existing = closure_precision(SemanticConfidence::Medium);
    let incoming = closure_precision(SemanticConfidence::High);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(result, EdgeResolution::Replace);
}

#[test]
fn test_same_coverage_lower_confidence_loses() {
    // Both ClosureComplete, existing High + incoming Medium → Keep
    let existing = closure_precision(SemanticConfidence::High);
    let incoming = closure_precision(SemanticConfidence::Medium);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(result, EdgeResolution::Keep);
}

#[test]
fn test_same_precision_keep_existing() {
    // Same precision → keep existing (don't churn)
    let existing = closure_precision(SemanticConfidence::High);
    let incoming = closure_precision(SemanticConfidence::High);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(result, EdgeResolution::Keep);
}

// ── No existing edge ───────────────────────────────────────────────────

#[test]
fn test_no_existing_edge() {
    // None existing + incoming High → Replace
    let incoming = closure_precision(SemanticConfidence::High);
    let result = EdgeConflictPolicy::resolve(None, &incoming, None);
    assert_eq!(result, EdgeResolution::Replace);
}

#[test]
fn test_no_existing_edge_medium() {
    // None existing + incoming Medium → KeepAsCandidates
    let incoming = closure_precision(SemanticConfidence::Medium);
    let result = EdgeConflictPolicy::resolve(None, &incoming, None);
    assert_eq!(result, EdgeResolution::KeepAsCandidates);
}

#[test]
fn test_no_existing_edge_low() {
    // None existing + incoming Low → KeepAsCandidates
    let incoming = closure_precision(SemanticConfidence::Low);
    let result = EdgeConflictPolicy::resolve(None, &incoming, None);
    assert_eq!(result, EdgeResolution::KeepAsCandidates);
}

// ── Fanout rules ───────────────────────────────────────────────────────

#[test]
fn test_high_fanout_candidates() {
    // Fanout > 20: even Certain → KeepAsCandidates
    let incoming = certain_precision();
    let result = EdgeConflictPolicy::resolve(None, &incoming, Some(21));
    assert_eq!(result, EdgeResolution::KeepAsCandidates);
}

#[test]
fn test_low_fanout_normal() {
    // Fanout <= 20: normal resolution applies
    let incoming = closure_precision(SemanticConfidence::High);
    let result = EdgeConflictPolicy::resolve(None, &incoming, Some(20));
    assert_eq!(result, EdgeResolution::Replace);
}

// ── EdgePersistence ───────────────────────────────────────────────────

#[test]
fn test_certain_durable() {
    assert!(EdgeConflictPolicy::is_durable(SemanticConfidence::Certain));
}

#[test]
fn test_high_durable() {
    assert!(EdgeConflictPolicy::is_durable(SemanticConfidence::High));
}

#[test]
fn test_medium_not_durable() {
    assert!(!EdgeConflictPolicy::is_durable(SemanticConfidence::Medium));
}

#[test]
fn test_low_not_durable() {
    assert!(!EdgeConflictPolicy::is_durable(SemanticConfidence::Low));
}

#[test]
fn test_edge_persistence_canonical() {
    let p = EdgePersistence::from_confidence(SemanticConfidence::Certain, None);
    assert_eq!(p, EdgePersistence::Canonical);
    let p = EdgePersistence::from_confidence(SemanticConfidence::High, None);
    assert_eq!(p, EdgePersistence::Canonical);
}

#[test]
fn test_edge_persistence_candidate() {
    let p = EdgePersistence::from_confidence(SemanticConfidence::Medium, None);
    assert_eq!(p, EdgePersistence::Candidate);
}

#[test]
fn test_edge_persistence_none() {
    let p = EdgePersistence::from_confidence(SemanticConfidence::Low, None);
    assert_eq!(p, EdgePersistence::None);
}

#[test]
fn test_edge_persistence_gap() {
    // High fanout → Gap regardless of confidence
    let p = EdgePersistence::from_confidence(SemanticConfidence::High, Some(25));
    assert_eq!(p, EdgePersistence::Gap);
}

// ── Helpers for Boundary / Partial coverage ───────────────────────────

fn boundary_precision(confidence: SemanticConfidence) -> Precision {
    Precision {
        coverage: CoverageTier::Boundary {
            target_tier: types::structs::SymbolTier::Full,
        },
        confidence,
    }
}

fn partial_precision(confidence: SemanticConfidence) -> Precision {
    Precision {
        coverage: CoverageTier::Partial { gaps: vec![] },
        confidence,
    }
}

fn repo_complete_precision(confidence: SemanticConfidence) -> Precision {
    Precision {
        coverage: CoverageTier::RepoComplete,
        confidence,
    }
}

// ── Coverage tier comparison: Boundary vs Partial ─────────────────────

#[test]
fn test_boundary_vs_partial_coverage() {
    // Boundary (rank 3) should win over Partial (rank 2).
    let existing = partial_precision(SemanticConfidence::High);
    let incoming = boundary_precision(SemanticConfidence::Medium);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(
        result,
        EdgeResolution::Replace,
        "Boundary coverage should beat Partial coverage"
    );
}

// ── Coverage tier comparison: Boundary beats Manifest ─────────────────

#[test]
fn test_boundary_beats_manifest() {
    // Boundary (rank 3) should win over Manifest (rank 1), even with
    // same or higher confidence on Manifest. Note: existing Manifest
    // with Certain confidence is immutable by rule 1.
    let existing = manifest_precision(SemanticConfidence::High);
    let incoming = boundary_precision(SemanticConfidence::Medium);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(
        result,
        EdgeResolution::Replace,
        "Boundary should beat Manifest on coverage, even with lower confidence"
    );
}

// ── Coverage tier comparison: Partial beats Manifest ─────────────────

#[test]
fn test_partial_beats_manifest() {
    // Partial (rank 2) should win over Manifest (rank 1).
    let existing = manifest_precision(SemanticConfidence::High);
    let incoming = partial_precision(SemanticConfidence::Medium);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(
        result,
        EdgeResolution::Replace,
        "Partial coverage should beat Manifest coverage"
    );
}

// ── RepoComplete is top coverage ─────────────────────────────────────

#[test]
fn test_repo_complete_top_coverage() {
    // RepoComplete (rank 5) is the highest coverage. No other coverage
    // (ClosureComplete=4, Boundary=3, Partial=2, Manifest=1) can replace it.
    let existing = repo_complete_precision(SemanticConfidence::Medium);
    let incoming = closure_precision(SemanticConfidence::Certain);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(
        result,
        EdgeResolution::Keep,
        "RepoComplete should not be replaced by ClosureComplete even with Certain confidence"
    );
}

// ── Fanout boundary: exactly 20 → NOT high fanout ────────────────────

#[test]
fn test_fanout_exactly_20_not_high() {
    // Fanout count = 20 matches the threshold ("count > 20" condition).
    // It should NOT trigger HighFanout logic.
    let incoming = certain_precision();
    let result = EdgeConflictPolicy::resolve(None, &incoming, Some(20));
    assert_eq!(
        result,
        EdgeResolution::Replace,
        "Fanout=20 should NOT trigger HighFanout (threshold is >20, not >=20)"
    );
}

// ── Fanout boundary: exactly 21 → IS high fanout ─────────────────────

#[test]
fn test_fanout_exactly_21_is_high() {
    // Fanout count = 21 exceeds the threshold.
    // Should trigger HighFanout → KeepAsCandidates regardless of precision.
    let incoming = certain_precision();
    let result = EdgeConflictPolicy::resolve(None, &incoming, Some(21));
    assert_eq!(
        result,
        EdgeResolution::KeepAsCandidates,
        "Fanout=21 should trigger HighFanout even for Certain precision"
    );
}

// ── EdgePersistence fanout boundary ───────────────────────────────────

#[test]
fn test_edge_persistence_fanout_boundary() {
    // Fanout = 20 (at threshold): High confidence → Canonical
    let p = EdgePersistence::from_confidence(SemanticConfidence::High, Some(20));
    assert_eq!(
        p,
        EdgePersistence::Canonical,
        "Fanout=20 with High confidence → Canonical"
    );

    // Fanout = 21 (above threshold): High confidence → Gap
    let p = EdgePersistence::from_confidence(SemanticConfidence::High, Some(21));
    assert_eq!(
        p,
        EdgePersistence::Gap,
        "Fanout=21 with High confidence → Gap (high fanout)"
    );
}

// ── Existing Boundary replaced by ClosureComplete ─────────────────────

#[test]
fn test_existing_boundary_incoming_closurecomplete() {
    // ClosureComplete (rank 4) should replace Boundary (rank 3).
    let existing = boundary_precision(SemanticConfidence::High);
    let incoming = closure_precision(SemanticConfidence::Medium);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(
        result,
        EdgeResolution::Replace,
        "ClosureComplete should replace Boundary on coverage"
    );
}

// ── Same coverage Boundary: higher confidence wins ────────────────────

#[test]
fn test_same_coverage_boundary_conflict() {
    // Two Boundary precisions: higher confidence wins.
    let existing = boundary_precision(SemanticConfidence::Medium);
    let incoming = boundary_precision(SemanticConfidence::High);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(
        result,
        EdgeResolution::Replace,
        "Boundary with higher confidence should replace same-coverage Boundary"
    );
}

// ── Existing Manifest replaced by Boundary High ───────────────────────

#[test]
fn test_existing_manifest_incoming_boundary_high() {
    // Boundary (rank 3) should replace Manifest (rank 1).
    let existing = manifest_precision(SemanticConfidence::Medium);
    let incoming = boundary_precision(SemanticConfidence::High);
    let result = EdgeConflictPolicy::resolve(Some(&existing), &incoming, None);
    assert_eq!(
        result,
        EdgeResolution::Replace,
        "Boundary High should replace Manifest Medium on coverage"
    );
}
