//! Atlas focus-driven analysis modules.
//!
//! - `inventory` — Tier 0: lightweight file inventory from `atlas open`.
//! - `symbol_hints` — Tier 1: manifest-level symbol name index.

pub mod inventory;
pub mod symbol_hints;

#[cfg(test)]
mod inventory_tests;
