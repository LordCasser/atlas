//! Atlas Intermediate Representation (IR) — core data structures.
//!
//! These types form the data model that flows through the entire pipeline:
//!   Extraction → Resolution → Storage → Graph → Context → Search
//!
//! All positions use byte offsets (for tree-sitter) AND line/column (for humans).
//! All semantic facts carry confidence and provenance.

use crate::types::enums::*;
use crate::types::ids::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// TextRange — dual byte-offset + line/column position
// ---------------------------------------------------------------------------

/// A range in source text, stored as both byte offsets (machine) and
/// line/column (human).  Both representations are always valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextRange {
    /// 0-based absolute byte offset of the first character (start).
    pub start_byte: u32,
    /// 0-based absolute byte offset of the character after the last (end, exclusive).
    pub end_byte: u32,
    /// 1-based line number where the range starts.
    pub start_line: u32,
    /// 1-based column (UTF-16 code units) where the range starts.
    pub start_column: u32,
    /// 1-based line number where the range ends.
    pub end_line: u32,
    /// 1-based column (UTF-16 code units) where the range ends.
    pub end_column: u32,
}

impl TextRange {
    /// Byte length of this range.
    pub fn byte_len(self) -> u32 {
        self.end_byte.saturating_sub(self.start_byte)
    }

    /// Whether the range contains a given byte offset (inclusive start, exclusive end).
    pub fn contains_byte(self, offset: u32) -> bool {
        offset >= self.start_byte && offset < self.end_byte
    }
}

// ---------------------------------------------------------------------------
// SymbolDef — a symbol definition in source code
// ---------------------------------------------------------------------------

/// A definition of a named code entity (class, function, variable, etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolDef {
    /// Deterministic identity of this symbol.
    pub id: SymbolId,

    /// What kind of symbol this is.
    pub kind: SymbolKind,

    /// Simple (unqualified) name, e.g. "run".
    pub name: String,

    /// Fully qualified name, e.g. "com.example.Main.run".
    pub qualified_name: String,

    /// Dot-separated path segments for hierarchical lookup, e.g. ["com","example","Main","run"].
    pub symbol_path: Vec<String>,

    /// The file that contains this definition.
    pub file_id: FileId,

    /// Programming language.
    pub language: Language,

    /// Full range of the symbol body in source.
    pub range: TextRange,

    /// Range of just the symbol name (for go-to-definition highlighting).
    pub name_range: TextRange,

    /// Function/method signature, e.g. "run(foo: string): void".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    /// Access modifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,

    /// Whether exported from the module/package.
    #[serde(default)]
    pub exported: bool,

    /// Whether `static` (or module-level for languages without classes).
    #[serde(default)]
    pub static_: bool,

    /// Whether declared `async`.
    #[serde(default)]
    pub async_: bool,

    /// Containing symbol (e.g. the class that owns a method). None for top-level symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<SymbolId>,

    /// Scope that contains this symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<ScopeId>,

    /// Package or module name (e.g. "os" for Python, "com.example" for Java).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,

    /// Namespace path segments (e.g. ["std", "collections"] for C++).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub namespace_path: Vec<String>,
}

impl SymbolDef {
    /// Human-readable label for this symbol.
    pub fn display_name(&self) -> &str {
        &self.name
    }
}

// ---------------------------------------------------------------------------
// ResolvedTarget — what a reference resolved to
// ---------------------------------------------------------------------------

/// Result of resolving a reference to a target symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedTarget {
    /// The resolved symbol.
    pub symbol_id: SymbolId,

    /// Confidence in this resolution (0.0–1.0).
    pub confidence: Confidence,

    /// Strategy used to resolve.
    pub strategy: ResolutionStrategy,

    /// Provenance of the resolution data.
    pub provenance: Provenance,
}

// ---------------------------------------------------------------------------
// ReferenceUse — a reference to a symbol at a specific location
// ---------------------------------------------------------------------------

/// A usage of a symbol at a source location.
/// References are NEVER deleted — they persist with or without a resolved target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceUse {
    /// Deterministic identity.
    pub id: ReferenceId,

    /// File containing this reference.
    pub file_id: FileId,

    /// The symbol that contains this reference (source scope).
    /// Not always known at extraction time; filled by the resolver.
    pub source_symbol: Option<SymbolId>,

    /// Scope containing the reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<ScopeId>,

    /// What kind of reference this is.
    pub kind: ReferenceKind,

    /// The raw text of the reference (e.g. "foo", "MyClass").
    pub text: String,

    /// The name portion of the reference (same as text for simple refs).
    pub name: String,

    /// Receiver expression if method call, e.g. "obj" in obj.method().
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,

    /// Number of arguments if a call reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arity: Option<u32>,

    /// Range of the reference in source.
    pub range: TextRange,

    /// Resolution result. None if not yet resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedTarget>,
}

impl ReferenceUse {
    /// Whether this reference has been resolved.
    pub fn is_resolved(&self) -> bool {
        self.resolved.is_some()
    }
}

// ---------------------------------------------------------------------------
// ScopeDef — a scope (containment region)
// ---------------------------------------------------------------------------

/// A scope defines a region of code that can contain symbols (e.g. a class body, function body).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopeDef {
    /// Deterministic identity.
    pub id: ScopeId,

    /// File containing this scope.
    pub file_id: FileId,

    /// Type of scope.
    pub kind: ScopeKind,

    /// Name of the scope (e.g. function name, class name, or "file" for file scope).
    pub name: String,

    /// Scoped path (e.g. "module:Class:method").
    pub scope_path: String,

    /// Range covered by this scope.
    pub range: TextRange,

    /// Parent scope. None for file-level scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<ScopeId>,
}

// ---------------------------------------------------------------------------
// ImportDef — an import statement
// ---------------------------------------------------------------------------

/// Describes a single import relationship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportDef {
    /// Deterministic identity.
    pub id: ImportId,

    /// File containing this import.
    pub file_id: FileId,

    /// Type of import statement.
    pub kind: ImportKind,

    /// Module path being imported (e.g. "os.path", "java.util.List").
    pub module: String,

    /// The symbol name as defined in the source module.
    pub imported_name: String,

    /// Local alias if renamed (e.g. `import foo as bar` → local_name = "bar").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_name: Option<String>,

    /// Whether this is a wildcard import (e.g. `from foo import *`).
    #[serde(default)]
    pub is_wildcard: bool,

    /// Whether this is a relative import (e.g. `from . import foo`).
    #[serde(default)]
    pub is_relative: bool,

    /// Range of the import statement.
    pub range: TextRange,

    /// Import alias (different from local_name — this is the `as` part for some languages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

// ---------------------------------------------------------------------------
// ArgumentFact — a named argument at a call site
// ---------------------------------------------------------------------------

/// A single argument at a call site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgumentFact {
    /// Parameter name if named argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Value expression text.
    pub value: String,

    /// Source range of the argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<TextRange>,
}

// ---------------------------------------------------------------------------
// Callsite — a call expression
// ---------------------------------------------------------------------------

/// A function/method call site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Callsite {
    /// Deterministic identity.
    pub id: CallsiteId,

    /// The reference that this call originates from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<ReferenceId>,

    /// Symbol that contains the call (the caller).
    pub caller: SymbolId,

    /// Symbol that is called (the callee), if resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callee: Option<SymbolId>,

    /// Receiver expression (e.g. "obj" in obj.method()).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,

    /// Arguments at this call site.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<ArgumentFact>,

    /// Source range of the entire call expression.
    pub range: TextRange,
}

// ---------------------------------------------------------------------------
// RawEdge — a semantic edge between two symbols
// ---------------------------------------------------------------------------

/// A single semantic edge extracted from source.  Edges are NOT stored as
/// source→target only — they carry confidence and provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawEdge {
    /// Deterministic identity.
    pub id: EdgeId,

    /// Source symbol.
    pub source: SymbolId,

    /// Target symbol.
    pub target: SymbolId,

    /// Relationship kind.
    pub kind: EdgeKind,

    /// Confidence score for this edge.
    pub confidence: Confidence,

    /// How this edge was derived.
    pub provenance: Provenance,
}

// ---------------------------------------------------------------------------
// FileInfo — metadata about a parsed file
// ---------------------------------------------------------------------------

/// Per-file metadata stored alongside extraction results.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    /// File identity.
    pub file_id: FileId,

    /// Relative path from project root.
    pub path: String,

    /// Programming language of this file.
    pub language: Language,

    /// blake3 hex hash of the file contents (for change detection).
    pub content_hash: String,

    /// Parse outcome.
    pub status: ParseStatus,
}

// ---------------------------------------------------------------------------
// DiagnosticLevel & ExtractDiagnostic — extraction warnings/errors
// ---------------------------------------------------------------------------

/// Severity level of an extraction diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Error,
    Warning,
    Info,
}

/// A diagnostic message produced during extraction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractDiagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
    /// Source range the diagnostic applies to. None for file-level messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<TextRange>,
}

// ---------------------------------------------------------------------------
// FileFacts — the output of extraction for a single file
// ---------------------------------------------------------------------------

/// The complete extraction result for one source file.
/// This is the core unit of work that flows from extraction through resolution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileFacts {
    /// File metadata.
    pub file: FileInfo,

    /// All symbol definitions found in this file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<SymbolDef>,

    /// All scopes found in this file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<ScopeDef>,

    /// All references found in this file (preserved even if unresolved).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<ReferenceUse>,

    /// All import statements.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<ImportDef>,

    /// Symbol IDs that are exported from this file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<SymbolId>,

    /// Intra-file edges extracted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_edges: Vec<RawEdge>,

    /// All call sites in this file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callsites: Vec<Callsite>,

    /// Extraction warnings/errors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ExtractDiagnostic>,
}

impl FileFacts {
    /// Total symbol count (for quick stats).
    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    /// Total reference count.
    pub fn reference_count(&self) -> usize {
        self.references.len()
    }

    /// Number of unresolved references.
    pub fn unresolved_count(&self) -> usize {
        self.references.iter().filter(|r| !r.is_resolved()).count()
    }

    /// Whether extraction produced any errors.
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| matches!(d.level, DiagnosticLevel::Error))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_range() -> TextRange {
        TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        }
    }

    fn sample_file_id() -> FileId {
        FileId::generate("src/test.ts")
    }

    fn sample_symbol(file_id: FileId, name: &str, qualified: &str, kind: SymbolKind) -> SymbolDef {
        let id = SymbolId::generate(&file_id, "typescript", qualified, kind.as_str(), None);
        SymbolDef {
            id,
            kind,
            name: name.to_string(),
            qualified_name: qualified.to_string(),
            symbol_path: qualified.split('.').map(String::from).collect(),
            file_id,
            language: Language::TypeScript,
            range: sample_range(),
            name_range: sample_range(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
        }
    }

    fn sample_reference(file_id: FileId, source: SymbolId) -> ReferenceUse {
        let text = "foo".to_string();
        let range = sample_range();
        let id = ReferenceId::generate(&file_id, Some(&source), range.start_byte, range.end_byte, &text);
        ReferenceUse {
            id,
            file_id,
            source_symbol: Some(source),
            scope_id: None,
            kind: ReferenceKind::Call,
            text,
            name: "foo".to_string(),
            receiver: None,
            arity: Some(2),
            range,
            resolved: None,
        }
    }

    #[test]
    fn test_text_range_byte_len() {
        let r = TextRange {
            start_byte: 5,
            end_byte: 15,
            start_line: 1,
            start_column: 6,
            end_line: 1,
            end_column: 16,
        };
        assert_eq!(r.byte_len(), 10);
    }

    #[test]
    fn test_text_range_contains() {
        let r = TextRange {
            start_byte: 10,
            end_byte: 20,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 1,
        };
        assert!(r.contains_byte(10));
        assert!(r.contains_byte(15));
        assert!(!r.contains_byte(20));
        assert!(!r.contains_byte(9));
    }

    #[test]
    fn test_symbol_def_deterministic_id() {
        let fid = sample_file_id();
        let a = sample_symbol(fid, "foo", "Foo.foo", SymbolKind::Method);
        let b = sample_symbol(fid, "foo", "Foo.foo", SymbolKind::Method);
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn test_symbol_def_different_kind_different_id() {
        let fid = sample_file_id();
        let method = sample_symbol(fid, "foo", "Foo.foo", SymbolKind::Method);
        let function = sample_symbol(fid, "foo", "Foo.foo", SymbolKind::Function);
        assert_ne!(method.id, function.id);
    }

    #[test]
    fn test_reference_is_resolved() {
        let fid = sample_file_id();
        let sym = sample_symbol(fid, "bar", "Bar", SymbolKind::Class);
        let mut r = sample_reference(fid, sym.id);
        assert!(!r.is_resolved());
        r.resolved = Some(ResolvedTarget {
            symbol_id: sym.id,
            confidence: Confidence::certain(),
            strategy: ResolutionStrategy::ExactMatch,
            provenance: Provenance::TreeSitter,
        });
        assert!(r.is_resolved());
    }

    #[test]
    fn test_file_facts_stats() {
        let fid = sample_file_id();
        let sym_a = sample_symbol(fid, "A", "A", SymbolKind::Class);
        let sym_b = sample_symbol(fid, "run", "A.run", SymbolKind::Method);
        let mut ref1 = sample_reference(fid, sym_b.id);
        ref1.resolved = Some(ResolvedTarget {
            symbol_id: sym_a.id,
            confidence: Confidence::certain(),
            strategy: ResolutionStrategy::ExactMatch,
            provenance: Provenance::TreeSitter,
        });
        let ref2 = sample_reference(fid, sym_b.id);
        let facts = FileFacts {
            file: FileInfo {
                file_id: fid,
                path: "src/test.ts".into(),
                language: Language::TypeScript,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![sym_a, sym_b],
            references: vec![ref1, ref2],
            ..Default::default()
        };
        assert_eq!(facts.symbol_count(), 2);
        assert_eq!(facts.reference_count(), 2);
        assert_eq!(facts.unresolved_count(), 1);
        assert!(!facts.has_errors());
    }

    #[test]
    fn test_file_facts_has_errors() {
        let facts = FileFacts {
            diagnostics: vec![ExtractDiagnostic {
                level: DiagnosticLevel::Error,
                message: "parse error".into(),
                range: None,
            }],
            ..Default::default()
        };
        assert!(facts.has_errors());
    }

    #[test]
    fn test_edge_id_is_deterministic() {
        let fid = sample_file_id();
        let a = sample_symbol(fid, "A", "A", SymbolKind::Class);
        let b = sample_symbol(fid, "run", "A.run", SymbolKind::Method);
        let edge_id1 = EdgeId::generate(&a.id, &b.id, "contains", None, "tree_sitter");
        let e1 = RawEdge {
            id: edge_id1.clone(),
            source: a.id,
            target: b.id,
            kind: EdgeKind::Contains,
            confidence: Confidence::certain(),
            provenance: Provenance::TreeSitter,
        };
        let edge_id2 = EdgeId::generate(&a.id, &b.id, "contains", None, "tree_sitter");
        let e2 = RawEdge {
            id: edge_id2,
            source: a.id,
            target: b.id,
            kind: EdgeKind::Contains,
            confidence: Confidence::certain(),
            provenance: Provenance::TreeSitter,
        };
        assert_eq!(e1.id, e2.id);
    }
}
