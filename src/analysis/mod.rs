//! Analysis layer: taint tracking, vulnerability detection, and code quality checks.
//!
//! Architecture:
//! - `taint/` — source-to-sink taint analysis (rule-based, YAML-configurable)
//! - `trace/` — location-driven variable tracking (where does this value come from?)

pub mod taint;
pub mod trace;
