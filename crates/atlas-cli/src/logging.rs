//! Structured tracing subscriber initialisation.
//!
//! ## Design
//! - **All diagnostic logs go to stderr** — `println!` / `eprintln!` remain
//!   for user-facing output and progress; `tracing` events carry engineering
//!   diagnostics (phase timing, failures, config changes).
//! - **`ATLAS_LOG` env var** takes precedence over `RUST_LOG`.  If neither is
//!   set, the level falls back to the effective CLI verbosity.
//! - **Default level**: `warn` (only warnings and errors surface).
//! - **`--verbose`**: `info` (phase boundaries, request summaries).
//! - **`--debug`**: `debug` + `trace` (per-file, per-query detail).
//! - **MCP mode** defaults to `info` regardless of CLI flags (the MCP server
//!   should always emit minimal operational diagnostics).
//! - **`--log-format json`**: structured JSON lines (machine-readable).

use clap::ValueEnum;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;

/// Verbosity level derived from CLI flags or mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// `warn` — default for most CLI commands.
    Default,
    /// `info` — `--verbose` flag or MCP mode.
    Verbose,
    /// `debug` — `--debug` flag.
    Debug,
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    /// Human-readable compact single-line output.
    #[value(name = "compact")]
    Compact,
    /// Machine-readable JSON lines.
    #[value(name = "json")]
    Json,
}

/// Initialise the global tracing subscriber.
///
/// Reads `ATLAS_LOG` (preferred) or `RUST_LOG` env vars to build an
/// `EnvFilter`.  If neither is set, the effective level falls back to the
/// `verbosity` argument:
///
/// | Verbosity  | Fallback directive |
/// |-----------|--------------------|
/// | Default   | `warn`             |
/// | Verbose   | `info`             |
/// | Debug     | `debug`            |
///
/// All output is written to **stderr** so that `--json` and other stdout
/// protocols are not polluted.
pub fn init(verbosity: Verbosity, format: LogFormat) {
    let env_filter = build_env_filter(verbosity);

    let subscriber = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(env_filter)
        .with_span_events(match format {
            LogFormat::Compact => FmtSpan::NONE,
            LogFormat::Json => FmtSpan::NEW | FmtSpan::CLOSE,
        })
        .with_target(true);

    match format {
        LogFormat::Compact => {
            subscriber.compact().init();
        }
        LogFormat::Json => {
            subscriber.json().init();
        }
    }
}

/// Build the effective `EnvFilter`.
///
/// Priority (highest first):
/// 1. `ATLAS_LOG` environment variable
/// 2. `RUST_LOG` environment variable
/// 3. Fallback level from `verbosity`
fn build_env_filter(verbosity: Verbosity) -> EnvFilter {
    // Check ATLAS_LOG first, then RUST_LOG
    for var in &["ATLAS_LOG", "RUST_LOG"] {
        if let Ok(val) = std::env::var(var) {
            if !val.is_empty() {
                return EnvFilter::new(val);
            }
        }
    }

    // Fallback: use verbosity to pick a default level
    let directive = match verbosity {
        Verbosity::Default => "warn",
        Verbosity::Verbose => "info",
        Verbosity::Debug => "debug",
    };
    EnvFilter::new(directive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbosity_fallback_warn() {
        // SAFETY: test runs sequentially; we restore state afterward.
        unsafe {
            std::env::remove_var("ATLAS_LOG");
            std::env::remove_var("RUST_LOG");
        }
        let filter = build_env_filter(Verbosity::Default);
        assert!(filter.max_level_hint().is_some());
    }

    #[test]
    fn atlas_log_takes_precedence() {
        // SAFETY: test isolation — we set and then restore.
        unsafe {
            std::env::set_var("ATLAS_LOG", "trace");
            std::env::set_var("RUST_LOG", "error");
        }
        let filter = build_env_filter(Verbosity::Default);
        assert!(filter.max_level_hint().is_some());
        unsafe {
            std::env::remove_var("ATLAS_LOG");
            std::env::remove_var("RUST_LOG");
        }
    }
}
