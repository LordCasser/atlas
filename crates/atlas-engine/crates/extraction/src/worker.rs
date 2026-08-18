//! Parse worker pool: managed extraction with panic isolation and error reporting.
//!
//! Wraps extraction with:
//! - `panic::catch_unwind` to isolate grammar crashes
//! - max file size check before parsing
//! - structured `ExtractionError` collection for the `IndexReport`
//!
//! **Note on timeout**: per-file timeout is not yet implemented (P2).
//! For now, Rayon-level parallelism + `catch_unwind` provides the
//! critical safety guarantees.

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use types::ExtractionError;
use types::FailureCategory;
use types::FileFacts;
use types::IndexReport;
use types::ids::FileId;

use super::CancelCheck;
use super::extract_file_with_mode;
use super::frontend::LanguageFrontend;
use crate::mode::ExtractionMode;

use crate::error::{ExtractionFailure, ExtractionFailureKind};

// ---------------------------------------------------------------------------
// WorkerConfig
// ---------------------------------------------------------------------------

/// Configuration for the parse worker pool.
#[derive(Clone)]
pub struct WorkerConfig {
    /// Skip files larger than this (bytes). `None` = no limit.
    pub max_file_size_bytes: Option<u64>,
    /// Per-file parse timeout (reserved for future use; currently not enforced).
    pub parse_timeout_secs: u64,
    /// Maximum number of Rayon worker threads. 0 = use Rayon default
    /// (typically number of CPU cores).
    pub max_workers: usize,
    /// Optional cancellation token for interruptible extraction.
    /// `None` (default) means extractions run to completion with no
    /// cancellation checks — identical to pre-cancellable behaviour.
    pub cancel_token: Option<Arc<dyn CancelCheck + Send + Sync>>,
}

impl std::fmt::Debug for WorkerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerConfig")
            .field("max_file_size_bytes", &self.max_file_size_bytes)
            .field("parse_timeout_secs", &self.parse_timeout_secs)
            .field("max_workers", &self.max_workers)
            .field("cancel_token", &self.cancel_token.as_ref().map(|_| ".."))
            .finish()
    }
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_file_size_bytes: Some(4 * 1024 * 1024), // 4 MiB
            parse_timeout_secs: 30,
            max_workers: 0,
            cancel_token: None,
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
        frontend: &LanguageFrontend,
        file_id: FileId,
        file_path: &Path,
        source: &str,
        content_hash: &str,
        mode: ExtractionMode,
    ) -> Result<FileFacts, ExtractionError> {
        let file_path_str = file_path.to_string_lossy().to_string();

        // 1. Size check
        if let Some(max_size) = self.config.max_file_size_bytes
            && source.len() as u64 > max_size
        {
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
            *self.skipped.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            return Err(err);
        }

        // 2. Extract with panic isolation
        let no_cancel = ();
        let token: &dyn CancelCheck = self
            .config
            .cancel_token
            .as_deref()
            .map_or(&no_cancel, |t| t);
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            extract_file_with_mode(
                frontend,
                file_id,
                file_path,
                source,
                content_hash,
                mode.clone(),
                token,
            )
        }));

        match result {
            Ok(Ok(facts)) => {
                *self.indexed.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                Ok(facts)
            }
            Ok(Err(extraction_err)) => {
                // Phase 4.1: emit structured tracing before classification
                if let Some(ef) = extraction_err.downcast_ref::<ExtractionFailure>() {
                    tracing::warn!(
                        file = %ef.file_path,
                        language = %ef.language.as_str(),
                        kind = %ef.kind,
                        slot = ef.slot.unwrap_or("unknown"),
                        message = %ef.message,
                        "extraction failed"
                    );
                } else {
                    let message = format!("{extraction_err}");
                    tracing::warn!(
                        file = %file_path_str,
                        category = ?FailureCategory::QueryError,
                        message = %message,
                        "extraction failed (untyped)"
                    );
                }
                let category = classify_anyhow(&extraction_err);
                let err = ExtractionError {
                    file_path: file_path_str,
                    category,
                    message: format!("{extraction_err}"),
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
                tracing::error!(
                    file = %file_path_str,
                    "grammar panic in extraction: {message}"
                );
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
            phase_timings: Default::default(),
            per_language: Default::default(),
        }
    }

    /// Record a pre-extraction failure directly (e.g. no adapter, I/O error).
    ///
    /// Use this for failures that happen before `extract_one()` is called.
    pub fn push_failure(&self, file_path: &str, category: FailureCategory, message: String) {
        self.errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ExtractionError {
                file_path: file_path.to_string(),
                category,
                message,
            });
    }

    // --- internal ---

    fn record_error(&self, err: ExtractionError) {
        self.errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(err);
    }
}

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// Classify an `anyhow::Error` from `extract_file` into a `FailureCategory`.
///
/// Downcast typed extraction failures into the stable report category.
/// Untyped errors are query errors; extraction-originated failures should use
/// [`ExtractionFailure`] rather than relying on message text.
fn classify_anyhow(err: &anyhow::Error) -> FailureCategory {
    if let Some(ef) = err.downcast_ref::<ExtractionFailure>() {
        return match ef.kind {
            ExtractionFailureKind::ParseTimeout => FailureCategory::ParseTimeout,
            ExtractionFailureKind::Cancelled => FailureCategory::Cancelled,
            ExtractionFailureKind::MaxFileSizeExceeded => FailureCategory::MaxFileSizeExceeded,
            ExtractionFailureKind::GrammarPanic => FailureCategory::GrammarPanic,
            ExtractionFailureKind::Io => FailureCategory::IoError,
            ExtractionFailureKind::ParserInit
            | ExtractionFailureKind::QueryCompile
            | ExtractionFailureKind::Normalization => FailureCategory::QueryError,
        };
    }

    FailureCategory::QueryError
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysCancel;

    impl CancelCheck for AlwaysCancel {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

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

        let frontend = crate::create_frontend(types::Language::TypeScript)
            .expect("TypeScript frontend available");

        let result = pool.extract_one(
            &frontend,
            fid,
            Path::new("test.ts"),
            &source,
            "abc",
            ExtractionMode::Full,
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
            FailureCategory::QueryError
        );
        assert_eq!(
            classify_anyhow(&anyhow::anyhow!("some query failed")),
            FailureCategory::QueryError
        );
        assert_eq!(
            classify_anyhow(&anyhow::anyhow!("operation timed out")),
            FailureCategory::QueryError
        );
    }

    #[test]
    fn test_classify_anyhow_downcast() {
        use crate::error::{ExtractionFailure, ExtractionFailureKind};
        use types::Language;

        let ef = ExtractionFailure::new(
            ExtractionFailureKind::ParseTimeout,
            "test.ts",
            Language::TypeScript,
        )
        .with_slot("symbols")
        .with_message("timed out after 30s");
        let err = anyhow::Error::from(ef);
        assert_eq!(classify_anyhow(&err), FailureCategory::ParseTimeout);

        let ef = ExtractionFailure::new(
            ExtractionFailureKind::Cancelled,
            "test.ts",
            Language::TypeScript,
        )
        .with_message("cancelled");
        let err = anyhow::Error::from(ef);
        assert_eq!(classify_anyhow(&err), FailureCategory::Cancelled);

        let ef = ExtractionFailure::new(
            ExtractionFailureKind::MaxFileSizeExceeded,
            "large.ts",
            Language::TypeScript,
        )
        .with_message("file too large");
        let err = anyhow::Error::from(ef);
        assert_eq!(classify_anyhow(&err), FailureCategory::MaxFileSizeExceeded);

        let ef = ExtractionFailure::new(
            ExtractionFailureKind::QueryCompile,
            "test.java",
            Language::Java,
        )
        .with_slot("references")
        .with_message("syntax error in query");
        let err = anyhow::Error::from(ef);
        assert_eq!(classify_anyhow(&err), FailureCategory::QueryError);

        let ef = ExtractionFailure::new(ExtractionFailureKind::Io, "test.py", Language::Python)
            .with_message("permission denied");
        let err = anyhow::Error::from(ef);
        assert_eq!(classify_anyhow(&err), FailureCategory::IoError);
    }

    #[test]
    fn extract_one_reports_typed_cancellation() {
        let config = WorkerConfig {
            cancel_token: Some(Arc::new(AlwaysCancel)),
            ..Default::default()
        };
        let pool = ParseWorkerPool::new(config);
        let frontend = crate::create_frontend(types::Language::TypeScript)
            .expect("TypeScript frontend available");

        let result = pool.extract_one(
            &frontend,
            FileId::generate("test.ts"),
            Path::new("test.ts"),
            "function test() {}",
            "abc",
            ExtractionMode::Full,
        );

        let err = result.expect_err("cancelled extraction should fail");
        assert_eq!(err.category, FailureCategory::Cancelled);

        let report = pool.into_report(1, 100);
        assert_eq!(report.files_failed, 1);
        assert_eq!(*report.failures_by_category.get("cancelled").unwrap(), 1);
    }
}
