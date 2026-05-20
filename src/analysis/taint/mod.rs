//! Taint analysis: source-to-sink dataflow tracking for vulnerability detection.
//!
//! # Architecture
//!
//! 1. **TaintRuleLoader** — loads YAML rules from `.atlas/rules/`
//! 2. **TaintEngine** — forward propagation from sources through DataFlowEdges
//! 3. **TaintPathTracer** — reverse BFS from sink back to source for explainability
//!
//! # Default rules
//!
//! Built-in rules for TypeScript (`req.query`, `req.body`, `exec`, `eval`,
//! `innerHTML`) and Python (`request.args`, `os.system`) are embedded as
//! default-matching logic. Users can override via `.atlas/rules/*.yaml`.

mod rules;
mod engine;
mod path;
mod findings;

pub use rules::TaintRuleLoader;
pub use engine::TaintEngine;
pub use path::TaintPathTracer;
