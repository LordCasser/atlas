//! Dossier return types for the Symbol Dossier redesign.
//!
//! These types form the output of `atlas_explore` after redesign:
//! a comprehensive symbol "dossier" containing source excerpts,
//! call evidence, non-call relation groups, file context, and
//! recommended next queries.
//!
//! All public types derive `Debug, Clone, Serialize, Deserialize`
//! and use `camelCase` JSON naming.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// SubjectInfo + SubjectRange
// ---------------------------------------------------------------------------

/// Core identity information about the dossier subject symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectInfo {
    /// SymbolId as hex string.
    pub id: String,
    /// SymbolKind as string, e.g. "function".
    pub kind: String,
    /// Simple (unqualified) name.
    pub name: String,
    /// Fully qualified name.
    pub qualified_name: String,
    /// Compact declaration signature, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Programming language as string, e.g. "typescript".
    pub language: String,
    /// Project-relative file path.
    pub file: String,
    /// Symbol body range (1-based line numbers).
    pub range: SubjectRange,
}

/// Line-only range (1-based, inclusive) for the dossier subject.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectRange {
    pub start_line: u32,
    pub end_line: u32,
}

// ---------------------------------------------------------------------------
// SourceExcerpt + SourceMode
// ---------------------------------------------------------------------------

/// Source code excerpt for the subject symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceExcerpt {
    /// Source delivery mode.
    pub mode: SourceMode,
    /// 1-based start line of the excerpt.
    pub start_line: u32,
    /// 1-based end line of the excerpt.
    pub end_line: u32,
    /// Whether the excerpt was hard-truncated by `max_source_bytes`.
    pub truncated: bool,
    /// The source text content.
    pub text: String,
}

/// Controls how source code is delivered in the dossier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceMode {
    /// Trimmed excerpt around the symbol definition.
    Excerpt,
    /// Full source text (subject to `max_source_bytes` hard cap).
    Full,
    /// No source text returned.
    #[serde(rename = "none")]
    None_,
}

// ---------------------------------------------------------------------------
// CallEvidence
// ---------------------------------------------------------------------------

/// Call graph evidence: incoming and outgoing call information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallEvidence {
    /// Who calls the subject.
    pub incoming: CallEvidenceGroup,
    /// Who the subject calls.
    pub outgoing: CallEvidenceGroup,
}

/// A group of call evidence entries with a total count.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallEvidenceGroup {
    /// Total number of relations of this direction (may exceed `examples.len()`).
    pub total: usize,
    /// Representative examples.
    pub examples: Vec<CallEvidenceEntry>,
}

/// A single call evidence entry with call-site context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallEvidenceEntry {
    /// The caller or callee symbol.
    pub symbol: PeerSymbol,
    /// Location and snippet of the call site.
    pub callsite: CallSite,
    /// Edge kind string, e.g. "calls".
    pub edge_kind: String,
    /// Confidence label: "exact" | "high" | "medium" | "inferred".
    pub confidence: String,
}

/// Location of a call site with a source snippet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSite {
    /// Project-relative file path.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number.
    pub column: u32,
    /// Source snippet at the call site.
    pub snippet: String,
}

/// Lightweight peer symbol used in relation entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerSymbol {
    /// Simple name.
    pub name: String,
    /// Fully qualified name.
    pub qualified_name: String,
    /// SymbolKind as string, e.g. "function".
    pub kind: String,
    /// Project-relative file path.
    pub file: String,
    /// 1-based line number of the symbol definition.
    pub line: u32,
    /// Compact declaration signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

// ---------------------------------------------------------------------------
// RelationGroups
// ---------------------------------------------------------------------------

/// Non-call relations grouped by relation category.
/// Only groups with results are serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationGroups {
    /// Interface/trait implementation relations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implements: Option<RelationGroup>,
    /// Class/interface extension relations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extends: Option<RelationGroup>,
    /// Instantiation relations (e.g. `new Foo()`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instantiates: Option<RelationGroup>,
    /// Merged FieldRead + FieldWrite access relations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_access: Option<FieldAccessGroup>,
    /// Read access relations (global / non-field reads).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reads: Option<RelationGroup>,
    /// Write access relations (global / non-field writes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writes: Option<RelationGroup>,
    /// Decoration/annotation relations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decorates: Option<RelationGroup>,
    /// Callback registration relations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registers_callback: Option<RelationGroup>,
}

/// Generic relation group with a total count and examples.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationGroup {
    /// Total number of relations of this kind (may exceed `examples.len()`).
    pub total: usize,
    /// Representative examples.
    pub examples: Vec<RelationEntry>,
}

/// A single relation entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationEntry {
    /// The related symbol.
    pub symbol: PeerSymbol,
    /// Source snippet at the relation site.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Confidence label.
    pub confidence: String,
}

/// Field access group (merged FieldRead + FieldWrite, per Decision #5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldAccessGroup {
    /// Total number of field access relations.
    pub total: usize,
    /// Representative examples.
    pub examples: Vec<FieldAccessEntry>,
}

/// A single field access entry with read/write distinction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldAccessEntry {
    /// Access direction: "read" | "write".
    pub access: String,
    /// The related symbol.
    pub symbol: PeerSymbol,
    /// Source snippet at the access site.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Confidence label.
    pub confidence: String,
}

// ---------------------------------------------------------------------------
// FileContext
// ---------------------------------------------------------------------------

/// File-level context: imports, exports, and peer symbols.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContext {
    /// Project-relative file path.
    pub file: String,
    /// Import statements in this file.
    pub imports: Vec<ImportFact>,
    /// Export declarations from this file.
    pub exports: Vec<ExportFact>,
    /// Peer symbols in the same file.
    pub peers: Vec<PeerSymbol>,
}

/// Condensed import fact for the dossier.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFact {
    /// Module path being imported.
    pub module: String,
    /// Imported symbol names.
    pub symbols: Vec<String>,
    /// 1-based line number of the import statement.
    pub line: u32,
}

/// Export fact for the dossier.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFact {
    /// The name under which the symbol is exported.
    pub exported_name: String,
    /// The local symbol ID being exported (hex string). Absent for re-exports
    /// that do not bind a local symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_symbol_id: Option<String>,
    /// Source module for `export ... from` declarations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// Type of export.
    pub export_kind: ExportKind,
    /// Provenance source of the export fact.
    pub source: ExportSource,
    /// 1-based line number.
    pub line: u32,
}

/// Type of export declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportKind {
    /// Named export: `export { foo }`.
    Named,
    /// Default export: `export default class Foo`.
    #[serde(rename = "default")]
    Default_,
    /// Wildcard re-export: `export * from './foo'`.
    Wildcard,
}

/// Provenance source for export facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportSource {
    /// Explicitly extracted from language syntax (e.g. `export` keyword).
    ExplicitSyntax,
    /// Inferred from graph edges (transitional data).
    GraphEdge,
    /// Synthetic marker for unsupported languages (per Decision #1(C)).
    InferredUnsupported,
}

// ---------------------------------------------------------------------------
// AmbiguousResponse (Decision #4)
// ---------------------------------------------------------------------------

/// Returned when a symbol query matches multiple candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmbiguousResponse {
    /// Always true for this variant.
    pub ambiguous: bool,
    /// The original query string.
    pub query: String,
    /// Matching candidate symbols.
    pub candidates: Vec<SymbolCandidate>,
    /// Recommended next queries to disambiguate.
    pub recommended_next_queries: Vec<RecommendedQuery>,
}

/// A candidate symbol from an ambiguous query.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolCandidate {
    /// Fully qualified name.
    pub qualified_name: String,
    /// Compact declaration signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Project-relative file path.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// SymbolKind as string.
    pub kind: String,
    /// Language as string.
    pub language: String,
}

// ---------------------------------------------------------------------------
// ExploreDossier (main return type)
// ---------------------------------------------------------------------------

/// The complete Symbol Dossier returned by `atlas_explore`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExploreDossier {
    /// Identity information about the subject symbol.
    pub subject: SubjectInfo,
    /// Source code excerpt (None if source_mode is None_).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_excerpt: Option<SourceExcerpt>,
    /// Call graph evidence (incoming + outgoing).
    pub call_evidence: CallEvidence,
    /// Non-call relations grouped by category.
    pub relation_groups: RelationGroups,
    /// File-level context (imports, exports, peers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_context: Option<FileContext>,
    /// Recommended next queries for further exploration.
    pub recommended_next_queries: Vec<RecommendedQuery>,
    /// AnswerQuality tier of the underlying data.
    pub precision_tier: String,
    /// Non-critical warnings (e.g. truncated results).
    pub warnings: Vec<String>,
}

/// A recommended follow-up query to continue exploring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecommendedQuery {
    /// Target tool name: "atlas_calls" | "atlas_explore" | "atlas_symbol".
    pub tool: String,
    /// Recommended arguments as a JSON object.
    pub args: serde_json::Value,
    /// Human-readable reason for the recommendation.
    pub reason: String,
}

// ---------------------------------------------------------------------------
// ExploreRequest (input params)
// ---------------------------------------------------------------------------

/// Parameters for building a Symbol Dossier.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExploreRequest {
    /// Symbol query string (qualified name or simple name).
    pub symbol: String,

    /// Source delivery mode.
    #[serde(default = "default_source_mode")]
    pub source_mode: SourceMode,

    /// Number of context lines for excerpt mode.
    #[serde(default = "default_source_lines")]
    pub source_lines: u32,

    /// Max call evidence examples per direction.
    #[serde(default = "default_evidence_limit")]
    pub evidence_limit: usize,

    /// Max relation examples per group.
    #[serde(default = "default_relation_limit")]
    pub relation_limit: usize,

    /// Max peer symbols to include.
    #[serde(default = "default_peer_limit")]
    pub peer_limit: usize,

    /// Whether to include file-level context (imports, exports, peers).
    #[serde(default = "default_true")]
    pub include_file_context: bool,

    /// Whether to generate recommended next queries.
    #[serde(default = "default_true")]
    pub include_recommendations: bool,

    /// Hard byte cap for source text (applies to both excerpt and full mode).
    #[serde(default = "default_max_source_bytes")]
    pub max_source_bytes: usize,
}

fn default_source_mode() -> SourceMode {
    SourceMode::Excerpt
}
fn default_source_lines() -> u32 {
    40
}
fn default_evidence_limit() -> usize {
    5
}
fn default_relation_limit() -> usize {
    20
}
fn default_peer_limit() -> usize {
    12
}
fn default_true() -> bool {
    true
}
fn default_max_source_bytes() -> usize {
    65536
}

// ---------------------------------------------------------------------------
// InternalRelationKind (internal use, not serialized)
// ---------------------------------------------------------------------------

/// Internal relation kind with full semantic precision.
///
/// Used by `RelationRepository`. The output layer maps these to
/// dossier `RelationGroups` (e.g., FieldRead + FieldWrite → field_access).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InternalRelationKind {
    Calls,
    References,
    Implements,
    Extends,
    Instantiates,
    Reads,
    Writes,
    FieldRead,
    FieldWrite,
    Decorates,
    RegistersCallback,
}

impl InternalRelationKind {
    /// Map from existing `EdgeKind` to `InternalRelationKind`.
    /// Returns `None` for edge kinds not relevant to the dossier
    /// (e.g., Contains, Defines, Includes, Imports, Exports).
    pub fn from_edge_kind(ek: types::enums::EdgeKind) -> Option<Self> {
        use types::enums::EdgeKind;
        Some(match ek {
            EdgeKind::Calls => Self::Calls,
            EdgeKind::References => Self::References,
            EdgeKind::Implements => Self::Implements,
            EdgeKind::Extends => Self::Extends,
            EdgeKind::Instantiates => Self::Instantiates,
            EdgeKind::Reads => Self::Reads,
            EdgeKind::Writes => Self::Writes,
            EdgeKind::FieldRead => Self::FieldRead,
            EdgeKind::FieldWrite => Self::FieldWrite,
            EdgeKind::Decorates => Self::Decorates,
            EdgeKind::RegistersCallback => Self::RegistersCallback,
            // Imports, Exports → handled by FileFactsRepository
            // Contains, Defines, Includes, TypeOf, Returns, Overrides,
            // Argument, Parameter, Assigns → not relevant for dossier
            _ => return None,
        })
    }
}
