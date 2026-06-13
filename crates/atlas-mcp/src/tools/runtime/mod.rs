//! Atlas v6.0 Runtime Architecture
//!
//! ActiveProject aggregates six focused runtimes that each own
//! a clearly scoped responsibility. This replaces the v5.0 ToolRouter
//! God-object pattern.
//!
//! ## Runtime Map
//! | Runtime           | Responsibility                    |
//! |-------------------|-----------------------------------|
//! | QueryRuntime      | Focus-driven lazy extraction      |
//! | GraphRuntime      | In-memory call graph lifecycle    |
//! | AnalysisRuntime   | CFG/dataflow + lifecycle/branch   |
//! | OverlayRuntime    | fp_dispatches + domain_rules      |
//! | StoreQueryRuntime | Direct store facts (symbols, files)|
//! | JobRuntime        | Background tasks + investigation  |

pub mod query_runtime;
pub mod graph_runtime;
pub mod analysis_runtime;
pub mod overlay_runtime;
pub mod store_query_runtime;
pub mod job_runtime;
