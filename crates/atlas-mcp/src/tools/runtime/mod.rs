//! Runtime modules — the v6.0 architecture backbone.
//!
//! Each runtime module wraps a specific concern that was previously
//! crammed into the 15-field ToolRouter god object:
//!
//! | Module | Concern | Owns |
//! |--------|---------|------|
//! | `query_runtime` | Focus-driven lazy extraction | FocusRuntime, CacheState, LazyRefreshQueue |
//! | `graph_runtime` | In-memory call-graph snapshot | GraphState, SearchEngine, ContextBuilder, GraphProvider |
//! | `analysis_runtime` | On-demand CFG/dataflow extraction | LazyDataflowService |
//! | `overlay_runtime` | User annotations (fp_dispatches, domain_rules) | Store (mutation path), generation counter |
//! | `store_query_runtime` | Direct store queries + source extraction | Store (read path), SourceExtractor |
//! | `job_runtime` | Background task orchestration | TaskManager, InvestigationState |
//! | `cache_state` | Index-signature and manual-full-index caching | (data-only) |
//! | `graph_provider` | Trait contract for graph backends | (trait definition) |

pub(crate) mod cache_state;
pub(crate) mod graph_provider;
pub mod query_runtime;
pub mod graph_runtime;
pub mod analysis_runtime;
pub mod overlay_runtime;
pub mod store_query_runtime;
pub mod job_runtime;
