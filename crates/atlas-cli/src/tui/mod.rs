//! Terminal UI modules for atlas CLI progress display.
//!
//! - `render` — ratatui multi-panel layout rendering
//! - `progress` — TUI lifecycle (init, draw loop, Ctrl+C, summary)
//! - `fallback` — plain-text progress for non-TTY environments

pub mod fallback;
pub mod progress;
pub mod render;

pub use fallback::TextFallback;
pub use progress::TuiProgress;
