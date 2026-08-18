//! Edge conflict resolution for focus analysis graph building.
//!
//! When building graph edges during focus closure expansion, existing edges
//! may already exist from prior closures, full index, or other focus jobs.
//! This module defines the conflict resolution policy.

use types::structs::{AnswerQuality, CoverageTier, SemanticConfidence};

/// Resolution for an edge conflict between existing and incoming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeResolution {
    /// Keep the existing edge, skip the incoming one.
    Keep,
    /// Replace the existing edge with the incoming one.
    Replace,
    /// Keep both as candidate edges (for high-fanout names).
    KeepAsCandidates,
}

/// Policy for resolving conflicts between existing and new graph edges.
pub struct EdgeConflictPolicy;

impl EdgeConflictPolicy {
    /// Resolve a conflict between an existing edge precision and a new one.
    ///
    /// Rules (in priority order):
    /// 1. CERTAIN edges are immutable — never overwritten.
    /// 2. Higher coverage wins.
    /// 3. Same coverage → higher confidence wins.
    /// 4. Low/Medium confidence → candidate edges, not canonical.
    pub fn resolve(
        existing: Option<&AnswerQuality>,
        incoming: &AnswerQuality,
        fanout: Option<usize>,
    ) -> EdgeResolution {
        // High fanout names never produce canonical edges
        if let Some(count) = fanout
            && count > 20
        {
            return EdgeResolution::KeepAsCandidates;
        }

        let existing = match existing {
            Some(e) => e,
            None => {
                // No existing edge → classify the new one
                return Self::classify_new(incoming);
            }
        };

        // Rule 1: Certain edges are immutable
        if existing.confidence == SemanticConfidence::Certain {
            return EdgeResolution::Keep;
        }

        // Rule 2: Higher coverage wins
        let existing_coverage_rank = Self::coverage_rank(&existing.coverage);
        let incoming_coverage_rank = Self::coverage_rank(&incoming.coverage);

        if incoming_coverage_rank > existing_coverage_rank {
            return EdgeResolution::Replace;
        }
        if incoming_coverage_rank < existing_coverage_rank {
            return EdgeResolution::Keep;
        }

        // Rule 3: Same coverage → higher confidence wins
        if incoming.confidence > existing.confidence {
            return EdgeResolution::Replace;
        }
        if incoming.confidence < existing.confidence {
            return EdgeResolution::Keep;
        }

        // Same precision → keep existing (don't churn)
        EdgeResolution::Keep
    }

    /// Classify a new edge into canonical vs candidate based on confidence.
    fn classify_new(precision: &AnswerQuality) -> EdgeResolution {
        match precision.confidence {
            SemanticConfidence::Certain | SemanticConfidence::High => {
                EdgeResolution::Replace // canonical edge
            }
            SemanticConfidence::Medium | SemanticConfidence::Low => {
                EdgeResolution::KeepAsCandidates // candidate only
            }
        }
    }

    /// Numeric rank for coverage tier comparison.
    fn coverage_rank(coverage: &CoverageTier) -> u8 {
        match coverage {
            CoverageTier::RepoComplete => 5,
            CoverageTier::ClosureComplete { .. } => 4,
            CoverageTier::Boundary { .. } => 3,
            CoverageTier::Partial { .. } => 2,
            CoverageTier::Manifest => 1,
        }
    }

    /// Check if a new edge should be persisted as durable (canonical).
    pub fn is_durable(confidence: SemanticConfidence) -> bool {
        matches!(
            confidence,
            SemanticConfidence::Certain | SemanticConfidence::High
        )
    }
}

/// Durable vs Response-Only edge classification:
///
/// | Confidence | Persistence |
/// |-----------|-------------|
/// | Certain   | symbol_edges (canonical) |
/// | High      | symbol_edges (canonical) |
/// | Medium    | symbol_edge_candidates (response-only) |
/// | Low       | Not persisted, returned in gaps |
/// | HighFanout (>20 candidates) | KnownGap, no edge |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgePersistence {
    Canonical,
    Candidate,
    Gap,
    None,
}

impl EdgePersistence {
    pub fn from_confidence(confidence: SemanticConfidence, fanout: Option<usize>) -> Self {
        if let Some(count) = fanout
            && count > 20
        {
            return EdgePersistence::Gap;
        }
        match confidence {
            SemanticConfidence::Certain | SemanticConfidence::High => EdgePersistence::Canonical,
            SemanticConfidence::Medium => EdgePersistence::Candidate,
            SemanticConfidence::Low => EdgePersistence::None,
        }
    }
}
