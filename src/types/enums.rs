//! Atlas core enums: Language, SymbolKind, EdgeKind, ReferenceKind, ImportKind,
//! ScopeKind, Visibility, ResolutionStrategy, Provenance, ResolutionStatus, ParseStatus.
//!
//! Severely trimmed from 22+ languages / 12 edge kinds to MVP 8 languages / 21+ edge kinds.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Language — 8 MVP languages (down from 23)
// ---------------------------------------------------------------------------

/// The 8 languages supported in MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    TypeScript,
    JavaScript,
    Python,
    Java,
    C,
    Cpp,
    ArkTS,
    Cangjie,
}

impl Language {
    /// Human-readable language name (lowercase, no spaces).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Java => "java",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::ArkTS => "arkts",
            Self::Cangjie => "cangjie",
        }
    }

    /// Parse from the same lowercase string returned by [`as_str`].
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "typescript" => Some(Self::TypeScript),
            "javascript" => Some(Self::JavaScript),
            "python" => Some(Self::Python),
            "java" => Some(Self::Java),
            "c" => Some(Self::C),
            "cpp" => Some(Self::Cpp),
            "arkts" => Some(Self::ArkTS),
            "cangjie" => Some(Self::Cangjie),
            _ => None,
        }
    }

    /// Detect language from file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "js" | "mjs" | "cjs" => Some(Self::JavaScript),
            "py" | "pyi" | "pyx" => Some(Self::Python),
            "java" => Some(Self::Java),
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            "ets" => Some(Self::ArkTS),
            "cj" | "cangjie" => Some(Self::Cangjie),
            _ => None,
        }
    }

    /// Detect language from a file path.
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(Self::from_extension)
    }

    /// File patterns / globs used by this language (for file discovery).
    pub fn globs(self) -> &'static [&'static str] {
        match self {
            Self::TypeScript => &["**/*.ts", "**/*.mts", "**/*.cts"],
            Self::JavaScript => &["**/*.js", "**/*.mjs", "**/*.cjs"],
            Self::Python => &["**/*.py", "**/*.pyi"],
            Self::Java => &["**/*.java"],
            Self::C => &["**/*.c", "**/*.h"],
            Self::Cpp => &["**/*.cpp", "**/*.cc", "**/*.cxx", "**/*.hpp", "**/*.hh", "**/*.hxx"],
            Self::ArkTS => &["**/*.ets"],
            Self::Cangjie => &["**/*.cj", "**/*.cangjie"],
        }
    }
}

// ---------------------------------------------------------------------------
// SymbolKind — what kind of symbol a definition is
// ---------------------------------------------------------------------------

/// 20 symbol definition kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    File,
    Module,
    Class,
    Struct,
    Interface,
    Trait,
    Enum,
    EnumMember,
    Function,
    Method,
    Property,
    Field,
    Variable,
    Constant,
    TypeAlias,
    Namespace,
    Parameter,
    Constructor,
    Macro,
    Decorator,
    Package,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Enum => "enum",
            Self::EnumMember => "enum_member",
            Self::Function => "function",
            Self::Method => "method",
            Self::Property => "property",
            Self::Field => "field",
            Self::Variable => "variable",
            Self::Constant => "constant",
            Self::TypeAlias => "type_alias",
            Self::Namespace => "namespace",
            Self::Parameter => "parameter",
            Self::Constructor => "constructor",
            Self::Macro => "macro",
            Self::Decorator => "decorator",
            Self::Package => "package",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "file" => Some(Self::File),
            "module" => Some(Self::Module),
            "class" => Some(Self::Class),
            "struct" => Some(Self::Struct),
            "interface" => Some(Self::Interface),
            "trait" => Some(Self::Trait),
            "enum" => Some(Self::Enum),
            "enum_member" => Some(Self::EnumMember),
            "function" => Some(Self::Function),
            "method" => Some(Self::Method),
            "property" => Some(Self::Property),
            "field" => Some(Self::Field),
            "variable" => Some(Self::Variable),
            "constant" => Some(Self::Constant),
            "type_alias" => Some(Self::TypeAlias),
            "namespace" => Some(Self::Namespace),
            "parameter" => Some(Self::Parameter),
            "constructor" => Some(Self::Constructor),
            "macro" => Some(Self::Macro),
            "decorator" => Some(Self::Decorator),
            "package" => Some(Self::Package),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// EdgeKind — semantic relationships between symbols
// ---------------------------------------------------------------------------

/// 21 semantic edge kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Contains,
    Calls,
    Imports,
    Includes,
    Exports,
    Extends,
    Implements,
    References,
    TypeOf,
    Returns,
    Instantiates,
    Overrides,
    Decorates,
    Defines,
    Argument,
    Parameter,
    Assigns,
    Reads,
    Writes,
    FieldRead,
    FieldWrite,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Includes => "includes",
            Self::Exports => "exports",
            Self::Extends => "extends",
            Self::Implements => "implements",
            Self::References => "references",
            Self::TypeOf => "type_of",
            Self::Returns => "returns",
            Self::Instantiates => "instantiates",
            Self::Overrides => "overrides",
            Self::Decorates => "decorates",
            Self::Defines => "defines",
            Self::Argument => "argument",
            Self::Parameter => "parameter",
            Self::Assigns => "assigns",
            Self::Reads => "reads",
            Self::Writes => "writes",
            Self::FieldRead => "field_read",
            Self::FieldWrite => "field_write",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "contains" => Some(Self::Contains),
            "calls" => Some(Self::Calls),
            "imports" => Some(Self::Imports),
            "includes" => Some(Self::Includes),
            "exports" => Some(Self::Exports),
            "extends" => Some(Self::Extends),
            "implements" => Some(Self::Implements),
            "references" => Some(Self::References),
            "type_of" => Some(Self::TypeOf),
            "returns" => Some(Self::Returns),
            "instantiates" => Some(Self::Instantiates),
            "overrides" => Some(Self::Overrides),
            "decorates" => Some(Self::Decorates),
            "defines" => Some(Self::Defines),
            "argument" => Some(Self::Argument),
            "parameter" => Some(Self::Parameter),
            "assigns" => Some(Self::Assigns),
            "reads" => Some(Self::Reads),
            "writes" => Some(Self::Writes),
            "field_read" => Some(Self::FieldRead),
            "field_write" => Some(Self::FieldWrite),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ReferenceKind — what kind of reference a usage is
// ---------------------------------------------------------------------------

/// 12 reference kinds describing how a symbol is referenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    Usage,
    TypeReference,
    Call,
    Import,
    FieldAccess,
    Inheritance,
    Implementation,
    Override,
    Decoration,
    Read,
    Write,
    Instantiation,
}

impl ReferenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::TypeReference => "type_reference",
            Self::Call => "call",
            Self::Import => "import",
            Self::FieldAccess => "field_access",
            Self::Inheritance => "inheritance",
            Self::Implementation => "implementation",
            Self::Override => "override",
            Self::Decoration => "decoration",
            Self::Read => "read",
            Self::Write => "write",
            Self::Instantiation => "instantiation",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "usage" => Some(Self::Usage),
            "type_reference" => Some(Self::TypeReference),
            "call" => Some(Self::Call),
            "import" => Some(Self::Import),
            "field_access" => Some(Self::FieldAccess),
            "inheritance" => Some(Self::Inheritance),
            "implementation" => Some(Self::Implementation),
            "override" => Some(Self::Override),
            "decoration" => Some(Self::Decoration),
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "instantiation" => Some(Self::Instantiation),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ImportKind — type of import statement
// ---------------------------------------------------------------------------

/// 5 import kinds matching common language patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    Include,
    #[default]
    Import,
    FromImport,
    Package,
    Use,
}

impl ImportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Include => "include",
            Self::Import => "import",
            Self::FromImport => "from_import",
            Self::Package => "package",
            Self::Use => "use",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "include" => Some(Self::Include),
            "import" => Some(Self::Import),
            "from_import" => Some(Self::FromImport),
            "package" => Some(Self::Package),
            "use" => Some(Self::Use),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ScopeKind — type of scope (containment context)
// ---------------------------------------------------------------------------

/// 13 scope kinds defining containment hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    File,
    Module,
    Class,
    Struct,
    Interface,
    Enum,
    Function,
    Method,
    #[default]
    Block,
    Loop,
    Conditional,
    Namespace,
    Trait,
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Interface => "interface",
            Self::Enum => "enum",
            Self::Function => "function",
            Self::Method => "method",
            Self::Block => "block",
            Self::Loop => "loop",
            Self::Conditional => "conditional",
            Self::Namespace => "namespace",
            Self::Trait => "trait",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "file" => Some(Self::File),
            "module" => Some(Self::Module),
            "class" => Some(Self::Class),
            "struct" => Some(Self::Struct),
            "interface" => Some(Self::Interface),
            "enum" => Some(Self::Enum),
            "function" => Some(Self::Function),
            "method" => Some(Self::Method),
            "block" => Some(Self::Block),
            "loop" => Some(Self::Loop),
            "conditional" => Some(Self::Conditional),
            "namespace" => Some(Self::Namespace),
            "trait" => Some(Self::Trait),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Visibility — symbol access level
// ---------------------------------------------------------------------------

/// 5 visibility levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Internal,
    Package,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Protected => "protected",
            Self::Internal => "internal",
            Self::Package => "package",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "public" => Some(Self::Public),
            "private" => Some(Self::Private),
            "protected" => Some(Self::Protected),
            "internal" => Some(Self::Internal),
            "package" => Some(Self::Package),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ResolutionStrategy — how a reference was resolved
// ---------------------------------------------------------------------------

/// 5 strategies for resolving a reference to a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStrategy {
    ExactMatch,
    NameOnly,
    FuzzyMatch,
    Heuristic,
    Builtin,
}

impl ResolutionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactMatch => "exact_match",
            Self::NameOnly => "name_only",
            Self::FuzzyMatch => "fuzzy_match",
            Self::Heuristic => "heuristic",
            Self::Builtin => "builtin",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "exact_match" => Some(Self::ExactMatch),
            "name_only" => Some(Self::NameOnly),
            "fuzzy_match" => Some(Self::FuzzyMatch),
            "heuristic" => Some(Self::Heuristic),
            "builtin" => Some(Self::Builtin),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Provenance — origin of extracted data
// ---------------------------------------------------------------------------

/// How a fact was derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    #[default]
    TreeSitter,
    Scip,
    Heuristic,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TreeSitter => "tree_sitter",
            Self::Scip => "scip",
            Self::Heuristic => "heuristic",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "tree_sitter" => Some(Self::TreeSitter),
            "scip" => Some(Self::Scip),
            "heuristic" => Some(Self::Heuristic),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ResolutionStatus — whether a reference was resolved
// ---------------------------------------------------------------------------

/// Resolution outcome for a reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    #[default]
    Unresolved,
    Resolved,
    Partial,
}

impl ResolutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unresolved => "unresolved",
            Self::Resolved => "resolved",
            Self::Partial => "partial",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "unresolved" => Some(Self::Unresolved),
            "resolved" => Some(Self::Resolved),
            "partial" => Some(Self::Partial),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ParseStatus — whether a file was parsed successfully
// ---------------------------------------------------------------------------

/// Result of parsing a single source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ParseStatus {
    #[default]
    Success,
    Partial,
    Error,
    Skipped,
}

impl ParseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Partial => "partial",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "success" => Some(Self::Success),
            "partial" => Some(Self::Partial),
            "error" => Some(Self::Error),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Confidence — a 0.0–1.0 confidence score
// ---------------------------------------------------------------------------

/// Confidence score clamped to `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Confidence(f32);

impl Confidence {
    /// Create a new Confidence, clamping to [0.0, 1.0].
    pub fn new(v: f32) -> Self {
        Self(v.clamp(0.0, 1.0))
    }

    /// Unchecked (for known-valid internal use).
    pub const fn from_f32_unchecked(v: f32) -> Self {
        Self(v)
    }

    /// Full confidence.
    pub const fn certain() -> Self {
        Self(1.0)
    }

    /// No confidence.
    pub const fn none() -> Self {
        Self(0.0)
    }

    /// Raw f32 value.
    pub fn as_f32(self) -> f32 {
        self.0
    }
}

impl Default for Confidence {
    fn default() -> Self {
        Self(0.5)
    }
}

impl std::ops::Add for Confidence {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.0 + rhs.0)
    }
}

impl std::ops::Mul<f32> for Confidence {
    type Output = Self;
    fn mul(self, rhs: f32) -> Self {
        Self::new(self.0 * rhs)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Language ---

    #[test]
    fn test_language_roundtrip() {
        for lang in [
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::ArkTS,
            Language::Cangjie,
        ] {
            assert_eq!(Language::from_str(lang.as_str()), Some(lang));
        }
    }

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("ets"), Some(Language::ArkTS));
        assert_eq!(Language::from_extension("cj"), Some(Language::Cangjie));
        assert_eq!(Language::from_extension("java"), Some(Language::Java));
        assert_eq!(Language::from_extension("unknown"), None);
    }

    #[test]
    fn test_language_mvp_only() {
        // Non-MVP languages MUST NOT be recognized
        assert_eq!(Language::from_extension("go"), None);
        assert_eq!(Language::from_extension("rs"), None);
        assert_eq!(Language::from_extension("cs"), None);
        assert_eq!(Language::from_extension("php"), None);
        assert_eq!(Language::from_extension("rb"), None);
    }

    #[test]
    fn test_language_default_is_typescript() {
        assert_eq!(Language::default(), Language::TypeScript);
    }

    // --- SymbolKind ---

    #[test]
    fn test_symbol_kind_roundtrip() {
        let all = [
            SymbolKind::File,
            SymbolKind::Module,
            SymbolKind::Class,
            SymbolKind::Struct,
            SymbolKind::Interface,
            SymbolKind::Trait,
            SymbolKind::Enum,
            SymbolKind::EnumMember,
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Property,
            SymbolKind::Field,
            SymbolKind::Variable,
            SymbolKind::Constant,
            SymbolKind::TypeAlias,
            SymbolKind::Namespace,
            SymbolKind::Parameter,
            SymbolKind::Constructor,
            SymbolKind::Macro,
            SymbolKind::Decorator,
            SymbolKind::Package,
        ];
        for k in all {
            assert_eq!(SymbolKind::from_str(k.as_str()), Some(k));
        }
    }

    // --- EdgeKind ---

    #[test]
    fn test_edge_kind_roundtrip() {
        for k in [
            EdgeKind::Contains,
            EdgeKind::Calls,
            EdgeKind::Imports,
            EdgeKind::Exports,
            EdgeKind::Defines,
            EdgeKind::Assigns,
            EdgeKind::Reads,
            EdgeKind::Writes,
        ] {
            assert_eq!(EdgeKind::from_str(k.as_str()), Some(k));
        }
    }

    // --- Serde ---

    #[test]
    fn test_language_serde() {
        let v = Language::Python;
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"python\"");
        let back: Language = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn test_symbol_kind_serde() {
        let v = SymbolKind::Function;
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"function\"");
        let back: SymbolKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn test_edge_kind_serde() {
        let v = EdgeKind::Calls;
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"calls\"");
        let back: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    // --- Confidence ---

    #[test]
    fn test_confidence_clamps() {
        assert_eq!(Confidence::new(1.5).as_f32(), 1.0);
        assert_eq!(Confidence::new(-0.5).as_f32(), 0.0);
    }

    #[test]
    fn test_confidence_add() {
        let a = Confidence::new(0.3);
        let b = Confidence::new(0.4);
        assert!((a + b).as_f32() - 0.7 < f32::EPSILON);
    }
}
