//! Analysis layer: taint tracking, vulnerability detection, and code quality checks.
//!
//! Architecture:
//! - `taint/` — source-to-sink taint analysis (rule-based, YAML-configurable)

pub mod taint;
