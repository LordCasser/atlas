//! Domain rules — language-agnostic rule matching engine for Atlas.
//!
//! Provides a plugin-based generic rule engine with language-specific
//! rule kind registries. C/C++ ownership rules are built-in.
//!
//! # Architecture
//!
//! ```text
//! GenericRuleEngine
//!   ├── LanguageRuleKinds (plugin trait)
//!   │   ├── CRegistry (C/C++ ownership rules)
//!   │   └── (future: Rust, Python, etc.)
//!   ├── pattern matching (exact, prefix, suffix, glob, regex)
//!   ├── rule learning strategy
//!   └── store (DB persistence via db::Store)
//! ```

pub mod types;
pub mod pattern;
pub mod engine;
pub mod store;
pub mod registry;
pub mod learning;
pub mod kinds;

pub use types::*;
pub use engine::GenericRuleEngine;
pub use registry::{LanguageRuleKinds, RuleKindSpec, RuleValidationResult};
pub use learning::{LearnedRuleCandidate, LearningEvidence, RuleLearningStrategy};
pub use store::GenericRuleStore;
