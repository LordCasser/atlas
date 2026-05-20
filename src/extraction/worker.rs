//! Parse worker pool: managed extraction with panic isolation and error reporting.
//!
//! Wraps `extract_file` with:
//! - `panic::catch_unwind` to isolate grammar crashes
//! - max file size check before parsing
//! - structured `ExtractionError` collection for the `IndexReport`
//!
//! **Note on timeout**: per-file timeout is not yet implemented because
//! `LanguageAdapter` is not `Send`.  It will be added once the adapter
//! trait gains `Send + Sync` bounds (P2).  For now, Rayon-level
//! parallelism + `catch_unwind` provides the critical safety guarantees.

use std::path::Path;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;

use crate::types::{ExtractionError, FailureCategory, FileFacts, IndexReport};
use crate::types::ids::FileId;

use super::extract_file;
use super::languages::LanguageAdapter;

// ---------------------------------------------------------------------------
// WorkerConfig
// ---------------------------------------------------------------------------

/// Configuration for the parse worker pool.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Skip files larger than this (bytes). `None` = no limit.
    pub max_file_size_bytes: Option<u64>,
    /// Per-file parse timeout (reserved for future use; currently not enforced).
    pub parse_timeout_secs: u64,
    /// Maximum number of Rayon worker threads. 0 = use Rayon default
    /// (typically number of CPU cores).
    pub max_workers: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_file_size_bytes: Some(4 * 1024 * 1024), // 4 MiB
            parse_timeout_secs: 30,
            max_workers: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// ParseWorkerPool
// ---------------------------------------------------------------------------

/// Manages per-file extraction with panic isolation and error collection.
///
/// **Thread safety**: `extract_one()` may be called from any Rayon worker
/// thread.  The internal counters are protected by `Mutex`.
pub struct ParseWorkerPool {
    config: WorkerConfig,
    errors: Mutex<Vec<ExtractionError>>,
    indexed: Mutex<usize>,
    skipped: Mutex<usize>,
}

impl ParseWorkerPool {
    /// Create a new pool with the given configuration.
    pub fn new(config: WorkerConfig) -> Self {
        Self {
            config,
            errors: Mutex::new(Vec::new()),
            indexed: Mutex::new(0),
            skipped: Mutex::new(0),
        }
    }

    /// Create a pool with default configuration.
    pub fn default_pool() -> Self {
        Self::new(WorkerConfig::default())
    }

    /// Extract a single file with panic isolation and size check.
    ///
    /// Returns `Ok(FileFacts)` on success, `Err(ExtractionError)` on failure.
    /// Failures are also recorded internally for the final `IndexReport`.
    pub fn extract_one(
        &self,
        adapter: &dyn LanguageAdapter,
        file_id: FileId,
        file_path: &Path,
        source: &str,
        content_hash: &str,
    ) -> Result<FileFacts, ExtractionError> {
        let file_path_str = file_path.to_string_lossy().to_string();

        // 1. Size check
        if let Some(max_size) = self.config.max_file_size_bytes {
            if source.len() as u64 > max_size {
                let err = ExtractionError {
                    file_path: file_path_str,
                    category: FailureCategory::MaxFileSizeExceeded,
                    message: format!(
                        "File size {} bytes exceeds limit {} bytes",
                        source.len(),
                        max_size
                    ),
                };
                self.record_error(err.clone());
                *self.skipped.lock().unwrap() += 1;
                return Err(err);
            }
        }

        // 2. Extract with panic isolation
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            extract_file(adapter, file_id, file_path, source, content_hash)
        }));

        match result {
            Ok(Ok(facts)) => {
                *self.indexed.lock().unwrap() += 1;
                Ok(facts)
            }
            Ok(Err(extraction_err)) => {
                let category = classify_anyhow(&extraction_err);
                let err = ExtractionError {
                    file_path: file_path_str,
                    category,
                    message: format!("{}", extraction_err),
                };
                self.record_error(err.clone());
                Err(err)
            }
            Err(panic_payload) => {
                // Panic caught — grammar or normalization code panicked
                let message = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                let err = ExtractionError {
                    file_path: file_path_str,
                    category: FailureCategory::GrammarPanic,
                    message,
                };
                self.record_error(err.clone());
                Err(err)
            }
        }
    }

    /// Build the final `IndexReport` from collected statistics.
    ///
    /// `files_discovered`, `references_total`, `references_resolved`, and
    /// `duration_ms` must be filled by the caller (they depend on context
    /// outside the pool).
    pub fn into_report(self, files_discovered: usize, duration_ms: u64) -> IndexReport {
        let errors = self.errors.into_inner().unwrap_or_default();
        let indexed = *self.indexed.lock().unwrap_or_else(|e| e.into_inner());
        let skipped = *self.skipped.lock().unwrap_or_else(|e| e.into_inner());
        let files_failed = errors.len();

        let mut failures_by_category = std::collections::HashMap::new();
        for err in &errors {
            *failures_by_category
                .entry(err.category.as_str().to_string())
                .or_insert(0usize) += 1;
        }

        IndexReport {
            files_discovered,
            files_indexed: indexed,
            files_skipped: skipped,
            files_failed,
            failures_by_category,
            references_total: 0,    // caller fills
            references_resolved: 0, // caller fills
            resolution_rate: 0.0,   // caller fills via finalize()
            duration_ms,
        }
    }

    /// Record a pre-extraction failure directly (e.g. no adapter, I/O error).
    ///
    /// Use this for failures that happen before `extract_one()` is called.
    pub(crate) fn push_failure(&self, file_path: &str, category: FailureCategory, message: String) {
        self.errors.lock().unwrap().push(ExtractionError {
            file_path: file_path.to_string(),
            category,
            message,
        });
    }

    // --- internal ---

    fn record_error(&self, err: ExtractionError) {
        self.errors.lock().unwrap().push(err);
    }
}

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// Classify an `anyhow::Error` from `extract_file` into a `FailureCategory`.
fn classify_anyhow(err: &anyhow::Error) -> FailureCategory {
    let msg = format!("{}", err).to_lowercase();
    if msg.contains("timeout") || msg.contains("timed out") {
        FailureCategory::ParseTimeout
    } else if msg.contains("io") || msg.contains("read") || msg.contains("utf8") {
        FailureCategory::IoError
    } else {
        FailureCategory::QueryError
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_config_default() {
        let config = WorkerConfig::default();
        assert_eq!(config.max_file_size_bytes, Some(4 * 1024 * 1024));
        assert_eq!(config.parse_timeout_secs, 30);
        assert_eq!(config.max_workers, 0);
    }

    #[test]
    fn test_pool_into_report() {
        let pool = ParseWorkerPool::new(WorkerConfig::default());
        let report = pool.into_report(100, 5000);
        assert_eq!(report.files_discovered, 100);
        assert_eq!(report.files_indexed, 0);
        assert_eq!(report.files_skipped, 0);
        assert_eq!(report.files_failed, 0);
        assert_eq!(report.duration_ms, 5000);
    }

    #[test]
    fn test_pool_size_check() {
        let config = WorkerConfig {
            max_file_size_bytes: Some(10),
            ..Default::default()
        };
        let pool = ParseWorkerPool::new(config);
        let fid = FileId::generate("test.ts");
        let source = "x".repeat(100); // 100 bytes > 10 byte limit

        let adapter = crate::extraction::create_adapter(crate::types::Language::TypeScript)
            .expect("TypeScript adapter available");

        let result = pool.extract_one(
            adapter.as_ref(),
            fid,
            Path::new("test.ts"),
            &source,
            "abc",
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.category, FailureCategory::MaxFileSizeExceeded);

        // Verify error was recorded
        let report = pool.into_report(1, 100);
        assert_eq!(report.files_skipped, 1);
        assert_eq!(report.files_failed, 1);
    }

    #[test]
    fn test_classify_anyhow() {
        assert_eq!(
            classify_anyhow(&anyhow::anyhow!("read error: permission denied")),
            FailureCategory::IoError
        );
        assert_eq!(
            classify_anyhow(&anyhow::anyhow!("some query failed")),
            FailureCategory::QueryError
        );
        assert_eq!(
            classify_anyhow(&anyhow::anyhow!("operation timed out")),
            FailureCategory::ParseTimeout
        );
    }
}
