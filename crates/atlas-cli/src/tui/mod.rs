//! Terminal UI modules for atlas CLI progress display.
//!
//! - `progress` — terminal progress lifecycle (init, draw loop, summary)
//! - `fallback` — plain-text progress for non-TTY environments

pub mod fallback;
pub mod progress;

pub use fallback::TextFallback;
pub use progress::TuiProgress;
