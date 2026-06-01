//! Ratatui widgets for the Atlas TUI.
//!
//! Each widget is a pure rendering component: its `render()` methods only
//! call ratatui drawing primitives and read pre-computed state — never I/O
//! or Store/GraphEngine access.

pub mod context_view;
pub mod results_list;
pub mod search_bar;
pub mod status_bar;
pub mod trace_view;
