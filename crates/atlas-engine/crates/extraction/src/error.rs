//! Typed extraction failure — replaces string-based error classification.
//!
//! `classify_anyhow()` in the worker module used to match substrings against
//! error messages to determine a [`FailureCategory`].  This module provides
//! an [`ExtractionFailure`] that can be attached to `anyhow::Error` via
//! `.context()` at every origination point in the extraction pipeline.
//!
//! The worker then downcasts to `ExtractionFailure` for category selection.
//! Untyped errors are reported as query errors; extraction-originated failures
//! should use this type.
//!
//! ## Corresponding tracing events
//!
//! - `GrammarPanic` → `tracing::error!` in [`ParseWorkerPool`]
//! - `QueryCompile`  → carries the failing `slot` name so callers can add
//!   `tracing::warn!` with precise attribution

use std::error::Error;
use types::Language;

// ---------------------------------------------------------------------------
// ExtractionFailureKind
// ---------------------------------------------------------------------------

/// Categorised reason a single-file extraction produced no facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtractionFailureKind {
    /// File I/O or source encoding error.
    Io,
    /// tree-sitter parser creation, language assignment, or parse failure.
    ParserInit,
    /// A tree-sitter query (`.scm` file) failed to compile.
    QueryCompile,
    /// Per-file parse exceeded the configured timeout (reserved).
    ParseTimeout,
    /// Extraction was cancelled by the active job budget or user request.
    Cancelled,
    /// The grammar's tree-sitter bindings or a normalizer panicked.
    GrammarPanic,
    /// A normalizer returned data that failed downstream validation.
    Normalization,
    /// Source file is larger than the configured `max_file_size_bytes`.
    MaxFileSizeExceeded,
}

impl ExtractionFailureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::ParserInit => "parser_init",
            Self::QueryCompile => "query_compile",
            Self::ParseTimeout => "parse_timeout",
            Self::Cancelled => "cancelled",
            Self::GrammarPanic => "grammar_panic",
            Self::Normalization => "normalization",
            Self::MaxFileSizeExceeded => "max_file_size_exceeded",
        }
    }
}

impl std::fmt::Display for ExtractionFailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// ExtractionFailure
// ---------------------------------------------------------------------------

/// A single-file extraction failure carrying the full diagnostic context.
///
/// Attach this to `anyhow::Error` at the error origination point:
///
/// ```ignore
/// return Err(anyhow::Error::new(ExtractionFailure {
///     kind: ExtractionFailureKind::QueryCompile,
///     file_path: file_path.to_string_lossy().into(),
///     language,
///     slot: Some("symbols"),
///     message: format!("{}", query_err),
/// }));
/// ```
///
/// The [`ParseWorkerPool`] will downcast to this type for category selection.
#[derive(Debug, Clone)]
pub struct ExtractionFailure {
    pub kind: ExtractionFailureKind,
    pub file_path: String,
    pub language: Language,
    /// Which extraction slot (query name) failed — e.g. `"symbols"`,
    /// `"references"`, `"imports"`, `"scopes"`, `"lexical"`, `"dataflow"`.
    pub slot: Option<&'static str>,
    pub message: String,
}

impl ExtractionFailure {
    pub fn new(
        kind: ExtractionFailureKind,
        file_path: impl Into<String>,
        language: Language,
    ) -> Self {
        Self {
            kind,
            file_path: file_path.into(),
            language,
            slot: None,
            message: String::new(),
        }
    }

    pub fn with_slot(mut self, slot: &'static str) -> Self {
        self.slot = Some(slot);
        self
    }

    pub fn with_message(mut self, msg: impl Into<String>) -> Self {
        self.message = msg.into();
        self
    }
}

impl std::fmt::Display for ExtractionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {}{} (lang={}, file={})",
            self.kind,
            self.message,
            self.slot.map(|s| format!(" slot={s}")).unwrap_or_default(),
            self.language.as_str(),
            self.file_path,
        )
    }
}

impl Error for ExtractionFailure {}
