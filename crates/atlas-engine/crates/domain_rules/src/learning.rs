//! Rule learning — traits for discovering ownership conventions from codebases.

use super::types::PatternKind;

/// Strategy for discovering rule candidates from a codebase.
pub trait RuleLearningStrategy: std::fmt::Debug + Send + Sync {
    /// The language this strategy operates on.
    fn language(&self) -> &'static str;

    /// Discover candidate rules from the database.
    fn discover_candidates(&self, store: &db::Store) -> anyhow::Result<Vec<LearnedRuleCandidate>>;

    /// Human-readable explanation for a candidate.
    fn explain_candidate(&self, candidate: &LearnedRuleCandidate) -> String;

    /// Minimum number of usage sites before a candidate is considered.
    fn min_usage_count(&self) -> usize {
        5
    }

    /// Confidence threshold for auto-approval.
    fn confidence_threshold(&self) -> f64 {
        0.8
    }
}

/// A candidate rule discovered by analyzing the codebase.
#[derive(Debug, Clone)]
pub struct LearnedRuleCandidate {
    pub language: String,
    pub rule_kind: String,
    pub pattern: String,
    pub pattern_kind: PatternKind,
    pub usage_count: usize,
    pub confidence: f64,
    pub evidence: Vec<LearningEvidence>,
}

/// A single piece of evidence supporting a learned rule candidate.
#[derive(Debug, Clone)]
pub struct LearningEvidence {
    pub file_id: String,
    pub symbol_id: Option<String>,
    pub line: u32,
    pub evidence_kind: String,
    pub confidence: f64,
}
