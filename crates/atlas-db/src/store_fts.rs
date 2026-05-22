//! FTS5 and locking helpers used by the Store.
//!
//! These are small utility functions factored out of store.rs
//! to keep the main store module focused on the public API.

/// Strip FTS5 special characters to prevent syntax errors.
pub(crate) fn sanitize_fts5_query(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '_' || *c == '.' || *c == '-')
        .collect();
    if sanitized.is_empty() {
        "*".to_string()
    } else {
        sanitized
    }
}

/// Current time in milliseconds since Unix epoch.
pub(crate) fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Check whether a process with the given PID is still alive.
///
/// Uses `kill -0` on Unix (no signal sent, just checks existence).
/// On non-Unix, assumes alive (conservative — won't steal locks).
pub(crate) fn is_process_alive(pid: i64) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(true)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}
