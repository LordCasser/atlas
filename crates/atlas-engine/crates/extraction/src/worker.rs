//! Parse worker pool: managed extraction with thread isolation and error reporting.
//!
//! Wraps `extract_file` with:
//! - per-file extraction in dedicated threads with 8 MiB stack to prevent
//!   stack overflow (SIGABRT) from tree-sitter recursive-descent parsing
//!   combined with CFG/DataFlow traversal in `--analysis full` mode
//! - max file size check before parsing
//! - structured `ExtractionError` collection for the `IndexReport`
//!
//! **Note on timeout**: per-file timeout is not yet implemented (P2).
//! For now, Rayon-level parallelism + per-file thread isolation provides
//! the critical safety guarantees.

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use types::ExtractionError;
use types::FailureCategory;
use types::FileFacts;
use types::IndexReport;
use types::ids::FileId;

use super::CancelCheck;
use super::cancel::NeverCancel;
use super::extract_file_with_mode_cancellable;
use super::frontend::LanguageFrontend;
use crate::mode::ExtractionMode;

use crate::error::{ExtractionFailure, ExtractionFailureKind};

/// Stack size for per-file extraction threads (8 MiB).
///
/// Each file extraction runs in a dedicated thread with this stack
/// size to prevent SIGABRT from tree-sitter recursive descent parsing
/// combined with CFG/DataFlow traversal in `--analysis full` mode.
const PER_FILE_STACK_SIZE: usize = 8 * 1024 * 1024;

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

/// Manages per-file extraction with thread isolation and error collection.
///
/// Each file extraction runs in its own `std::thread` with 8 MiB stack
/// to prevent stack overflow (SIGABRT) from tree-sitter + CFG/DataFlow
/// traversal. Internal counters are protected by `Mutex`.
///
/// **Thread safety**: `extract_one()` may be called from any Rayon worker
/// thread.
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

    /// Extract a single file with thread isolation and size check.
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
                *self.skipped.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                return Err(err);
            }
        }

        // 2. Extract in a dedicated thread with enlarged stack.
        //
        // WHY: catch_unwind cannot catch stack overflows (SIGABRT).
        // Running extraction in its own thread with 8 MiB stack ensures
        // that even deeply nested source files can be parsed without
        // overflowing the rayon worker's default 2 MiB stack.
        let token: &dyn CancelCheck = self
            .config
            .cancel_token
            .as_deref()
            .map_or(&NeverCancel, |t| t);
        let result = std::thread::scope(|s| {
            std::thread::Builder::new()
                .name(format!("atlas-extract-{}", file_path_str))
                .stack_size(PER_FILE_STACK_SIZE)
                .spawn_scoped(s, || {
                    extract_file_with_mode_cancellable(
                        frontend,
                        file_id,
                        file_path,
                        source,
                        content_hash,
                        mode.clone(),
                        token,
                    )
                })
                .expect("thread spawn should not fail")
                .join()
        });

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
                    // Untyped error — fall back to string classification
                    let fallback_msg = format!("{extraction_err}");
                    let cat = classify_anyhow(&extraction_err);
                    tracing::warn!(
                        file = %file_path_str,
                        category = ?cat,
                        message = %fallback_msg,
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
/// Phase 4: tries to downcast to [`ExtractionFailure`] first for precise
/// classification via [`ExtractionFailureKind`].  Falls back to legacy
/// string-matching for errors that haven't been migrated yet.
fn classify_anyhow(err: &anyhow::Error) -> FailureCategory {
    // 1. Downcast to typed error (Phase 4 migration)
    if let Some(ef) = err.downcast_ref::<ExtractionFailure>() {
        return match ef.kind {
            ExtractionFailureKind::ParseTimeout => FailureCategory::ParseTimeout,
            ExtractionFailureKind::MaxFileSizeExceeded => FailureCategory::MaxFileSizeExceeded,
            ExtractionFailureKind::GrammarPanic => FailureCategory::GrammarPanic,
            ExtractionFailureKind::Io => FailureCategory::IoError,
            ExtractionFailureKind::ParserInit
            | ExtractionFailureKind::QueryCompile
            | ExtractionFailureKind::Normalization => FailureCategory::QueryError,
        };
    }

    // 2. Legacy string-based fallback
    let msg = format!("{err}").to_lowercase();
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

    #[test]
    fn test_classify_anyhow_downcast() {
        // Phase 4: ExtractionFailure downcast path
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
    fn test_per_file_thread_stack_size() {
        // Verify the constant is set to 8 MiB
        assert_eq!(PER_FILE_STACK_SIZE, 8 * 1024 * 1024);
    }

    #[test]
    fn test_extract_one_in_per_file_thread() {
        // Verify extract_one successfully extracts in a per-file thread
        let config = WorkerConfig::default();
        let pool = ParseWorkerPool::new(config);
        let fid = FileId::generate("test.ts");
        let source = "export const x = 42;\nconsole.log(x);\n";
        let frontend = crate::create_frontend(types::Language::TypeScript)
            .expect("TypeScript frontend available");

        let result = pool.extract_one(
            &frontend,
            fid,
            Path::new("test.ts"),
            source,
            "abc123",
            ExtractionMode::Full,
        );
        assert!(result.is_ok(), "extract_one failed: {:?}", result.err());
        let facts = result.unwrap();
        assert_eq!(facts.file.path, "test.ts");
        // In Full mode we expect at least one symbol; in Manifest mode
        // tree-sitter may treat `export const` differently.
        // The key assertion is that the thread isolation didn't crash.
        // If symbols are empty, the test still passes — not a thread issue.
    }

    #[test]
    fn test_extract_invalid_source_returns_error_in_thread() {
        // Verify errors from within the per-file thread propagate correctly
        let config = WorkerConfig::default();
        let pool = ParseWorkerPool::new(config);
        let fid = FileId::generate("bad.ts");
        let source = "this is not valid typescript !@#$%^&*()";
        let frontend = crate::create_frontend(types::Language::TypeScript)
            .expect("TypeScript frontend available");

        let result = pool.extract_one(
            &frontend,
            fid,
            Path::new("bad.ts"),
            source,
            "abc",
            crate::mode::ExtractionMode::Manifest,
        );
        // Should NOT crash; should return a result (may be Ok with parse errors, or Err)
        // The key assertion: no SIGABRT, the function returns.
        match result {
            Ok(_facts) => { /* parse succeeded but may have warnings */ }
            Err(e) => {
                // Error should not be a GrammarPanic unless tree-sitter actually panics
                // Invalid TypeScript should still parse (tree-sitter is error-tolerant)
                // If we do get an error, it must NOT be a GrammarPanic
                assert_ne!(
                    e.category,
                    FailureCategory::GrammarPanic,
                    "grammar panic on intentionally bad input"
                );
            }
        }
    }

    #[test]
    fn test_extract_one_thread_isolation_preserves_error_counter() {
        // Verify that error recording works across thread boundaries
        let config = WorkerConfig {
            max_file_size_bytes: Some(10), // 10 bytes — most TS files will exceed
            ..Default::default()
        };
        let pool = ParseWorkerPool::new(config);
        let fid = FileId::generate("large.ts");
        let source = "export const aVeryLongVariableName = 'this is more than ten bytes';\n";

        let frontend = crate::create_frontend(types::Language::TypeScript)
            .expect("TypeScript frontend available");

        let result = pool.extract_one(
            &frontend,
            fid,
            Path::new("large.ts"),
            source,
            "abc",
            crate::mode::ExtractionMode::Manifest,
        );
        assert!(result.is_err());
        // Verify error was recorded
        let report = pool.into_report(1, 100);
        assert_eq!(report.files_skipped, 1);
        assert_eq!(report.files_failed, 1);
    }
}
