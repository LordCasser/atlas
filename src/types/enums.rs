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
            "ts" | "mts" | "cts" | "tsx" => Some(Self::TypeScript),
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "py" | "pyi" | "pyx" => Some(Self::Python),
            "java" => Some(Self::Java),
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            "ets" | "sts" => Some(Self::ArkTS),
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

    /// All file extensions (without dot) for the 8 MVP languages.
    pub fn all_extensions() -> Vec<&'static str> {
        vec![
            "ts", "mts", "cts", "tsx", "js", "mjs", "cjs", "jsx",
            "py", "pyi",
            "java",
            "c", "h",
            "cpp", "cc", "cxx", "hpp", "hh", "hxx",
            "ets",
            "cj", "cangjie",
        ]
    }

    /// File patterns / globs used by this language (for file discovery).
    pub fn globs(self) -> &'static [&'static str] {
        match self {
            Self::TypeScript => &["**/*.ts", "**/*.mts", "**/*.cts", "**/*.tsx"],
            Self::JavaScript => &["**/*.js", "**/*.mjs", "**/*.cjs", "**/*.jsx"],
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

/// 21 symbol definition kinds.
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
    ImportResolved,
    Builtin,
}

impl ResolutionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactMatch => "exact_match",
            Self::NameOnly => "name_only",
            Self::FuzzyMatch => "fuzzy_match",
            Self::Heuristic => "heuristic",
            Self::ImportResolved => "import_resolved",
            Self::Builtin => "builtin",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "exact_match" => Some(Self::ExactMatch),
            "name_only" => Some(Self::NameOnly),
            "fuzzy_match" => Some(Self::FuzzyMatch),
            "heuristic" => Some(Self::Heuristic),
            "import_resolved" => Some(Self::ImportResolved),
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
// BindingKind — lexical binding categories
// ---------------------------------------------------------------------------

/// 7 lexical binding kinds.  Used by [`super::bindings::BindingDef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKind {
    /// Function / method formal parameter.
    Parameter,
    /// Local variable (let / const / var).
    Local,
    /// Class / struct field member.
    Field,
    /// Import alias (e.g. `import { x as y }` → alias "y").
    ImportAlias,
    /// Catch-clause variable.
    CatchVariable,
    /// Lambda / arrow-function parameter.
    LambdaParameter,
    /// Global-scope binding (top-level `var` / module-level).
    Global,
}

impl BindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parameter => "parameter",
            Self::Local => "local",
            Self::Field => "field",
            Self::ImportAlias => "import_alias",
            Self::CatchVariable => "catch_variable",
            Self::LambdaParameter => "lambda_parameter",
            Self::Global => "global",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "parameter" => Some(Self::Parameter),
            "local" => Some(Self::Local),
            "field" => Some(Self::Field),
            "import_alias" => Some(Self::ImportAlias),
            "catch_variable" => Some(Self::CatchVariable),
            "lambda_parameter" => Some(Self::LambdaParameter),
            "global" => Some(Self::Global),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// DataNodeKind — data-flow node categories
// ---------------------------------------------------------------------------

/// 11 data-node kinds.  Used by [`super::dataflow::DataNode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataNodeKind {
    /// Formal parameter of a function.
    Parameter,
    /// Local variable binding.
    Local,
    /// Field / member access node.
    Field,
    /// Function return value.
    Return,
    /// Literal constant (string, number, bool).
    Literal,
    /// Generic expression node (when kind is not more specific).
    Expr,
    /// Argument passed at a call-site.
    CallArg,
    /// Value returned from a call-site.
    CallReturn,
    /// Receiver object (`this` / `self`).
    Receiver,
    /// Global / module-scoped variable.
    Global,
    /// Unknown / opaque node (conservative lower bound for taint).
    Unknown,
}

impl DataNodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parameter => "parameter",
            Self::Local => "local",
            Self::Field => "field",
            Self::Return => "return",
            Self::Literal => "literal",
            Self::Expr => "expr",
            Self::CallArg => "call_arg",
            Self::CallReturn => "call_return",
            Self::Receiver => "receiver",
            Self::Global => "global",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "parameter" => Some(Self::Parameter),
            "local" => Some(Self::Local),
            "field" => Some(Self::Field),
            "return" => Some(Self::Return),
            "literal" => Some(Self::Literal),
            "expr" => Some(Self::Expr),
            "call_arg" => Some(Self::CallArg),
            "call_return" => Some(Self::CallReturn),
            "receiver" => Some(Self::Receiver),
            "global" => Some(Self::Global),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// DataFlowKind — data-flow edge kinds
// ---------------------------------------------------------------------------

/// 10 data-flow edge kinds.  Used by [`super::dataflow::DataFlowEdge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataFlowKind {
    /// Assignment: `x = expr`  →  node(expr) → node(x)
    Assign,
    /// Read of a variable / field.
    Read,
    /// Write to a variable / field.
    Write,
    /// Field / member load: `a.b`  →  node(a) → node(a.b)
    FieldLoad,
    /// Field / member store: `a.b = v`  →  node(v) → node(a.b)
    FieldStore,
    /// Actual argument flows to formal parameter (inter-procedural).
    ArgToParam,
    /// Return value flows to call-site result (inter-procedural).
    ReturnToCall,
    /// Receiver flows to `this` / `self` inside callee.
    ReceiverToThis,
    /// Phi node (SSA merge point, e.g. after if-else).
    Phi,
    /// Sanitizer / filter node (breaks taint propagation).
    Sanitized,
}

impl DataFlowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assign => "assign",
            Self::Read => "read",
            Self::Write => "write",
            Self::FieldLoad => "field_load",
            Self::FieldStore => "field_store",
            Self::ArgToParam => "arg_to_param",
            Self::ReturnToCall => "return_to_call",
            Self::ReceiverToThis => "receiver_to_this",
            Self::Phi => "phi",
            Self::Sanitized => "sanitized",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "assign" => Some(Self::Assign),
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "field_load" => Some(Self::FieldLoad),
            "field_store" => Some(Self::FieldStore),
            "arg_to_param" => Some(Self::ArgToParam),
            "return_to_call" => Some(Self::ReturnToCall),
            "receiver_to_this" => Some(Self::ReceiverToThis),
            "phi" => Some(Self::Phi),
            "sanitized" => Some(Self::Sanitized),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CfgNodeKind — CFG node types
// ─────────────────────────────────────────────────────────────────────────────

/// Type of control-flow graph node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CfgNodeKind {
    /// Function entry node (virtual start).
    Entry,
    /// Function exit node (virtual end).
    Exit,
    /// Regular statement node.
    Statement,
    /// Branch point (if/switch/ternary).
    Branch,
    /// Loop header (for/while/do).
    Loop,
    /// Return statement node.
    Return,
    /// Throw statement node.
    Throw,
    /// Join point after branch/loop.
    Join,
}

impl CfgNodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Exit => "exit",
            Self::Statement => "statement",
            Self::Branch => "branch",
            Self::Loop => "loop",
            Self::Return => "return",
            Self::Throw => "throw",
            Self::Join => "join",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "entry" => Some(Self::Entry),
            "exit" => Some(Self::Exit),
            "statement" => Some(Self::Statement),
            "branch" => Some(Self::Branch),
            "loop" => Some(Self::Loop),
            "return" => Some(Self::Return),
            "throw" => Some(Self::Throw),
            "join" => Some(Self::Join),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CfgEdgeKind — CFG edge types
// ─────────────────────────────────────────────────────────────────────────────

/// Type of control-flow graph edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CfgEdgeKind {
    /// Sequential normal flow.
    Normal,
    /// True branch of a condition.
    TrueBranch,
    /// False branch of a condition.
    FalseBranch,
    /// Loop back edge.
    LoopBack,
    /// Exception flow edge.
    Exception,
}

impl CfgEdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::TrueBranch => "true_branch",
            Self::FalseBranch => "false_branch",
            Self::LoopBack => "loop_back",
            Self::Exception => "exception",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(Self::Normal),
            "true_branch" => Some(Self::TrueBranch),
            "false_branch" => Some(Self::FalseBranch),
            "loop_back" => Some(Self::LoopBack),
            "exception" => Some(Self::Exception),
            _ => None,
        }
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
