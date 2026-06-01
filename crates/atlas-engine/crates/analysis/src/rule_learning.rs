//! Rule learning — delegates to language-specific RuleLearningStrategy.
//! C/C++ learning strategy is in domain_rules::kinds::c::CLearningStrategy.

pub use domain_rules::learning::{LearnedRuleCandidate, LearningEvidence, RuleLearningStrategy};
