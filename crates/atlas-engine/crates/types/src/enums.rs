//! Atlas core enums: Language, SymbolKind, EdgeKind, ReferenceKind, ImportKind,
//! ScopeKind, Visibility, ResolutionStrategy, Provenance, ResolutionStatus, ParseStatus.
//!
//! Severely trimmed from 22+ languages / 12 edge kinds to the MVP language set
//! plus feature-gated post-MVP and experimental languages.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Language — MVP languages plus feature-gated post-MVP/experimental languages
// ---------------------------------------------------------------------------

/// Languages known to Atlas.
///
/// Cangjie is available behind `#[cfg(feature = "cangjie")]`;
/// it is included in the `all-languages` feature set.
///
/// Go, C#, Rust, PHP, Ruby, and Kotlin are post-MVP languages at Symbolic
/// capability level and are included by the `all-languages` feature.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "lowercase")]
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
    Go,
    CSharp,
    Rust,
    Php,
    Ruby,
    Kotlin,
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
            Self::Go => "go",
            Self::CSharp => "csharp",
            Self::Rust => "rust",
            Self::Php => "php",
            Self::Ruby => "ruby",
            Self::Kotlin => "kotlin",
        }
    }

    /// Parse from the same lowercase string returned by [`as_str`].
    #[allow(clippy::should_implement_trait)]
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
            "go" => Some(Self::Go),
            "csharp" => Some(Self::CSharp),
            "rust" => Some(Self::Rust),
            "php" => Some(Self::Php),
            "ruby" => Some(Self::Ruby),
            "kotlin" => Some(Self::Kotlin),
            _ => None,
        }
    }

    /// Detect language from file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "ts" | "mts" | "cts" | "tsx" => Some(Self::TypeScript),
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "py" | "pyi" | "pyx" => Some(Self::Python),
            #[cfg(feature = "java")]
            "java" => Some(Self::Java),
            #[cfg(feature = "c")]
            "c" | "h" => Some(Self::C),
            #[cfg(feature = "cpp")]
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            #[cfg(feature = "arkts")]
            "ets" | "sts" => Some(Self::ArkTS),
            #[cfg(feature = "cangjie")]
            "cj" | "cangjie" => Some(Self::Cangjie),
            #[cfg(feature = "go")]
            "go" => Some(Self::Go),
            #[cfg(feature = "csharp")]
            "cs" => Some(Self::CSharp),
            #[cfg(feature = "rust")]
            "rs" => Some(Self::Rust),
            #[cfg(feature = "php")]
            "php" => Some(Self::Php),
            #[cfg(feature = "ruby")]
            "rb" => Some(Self::Ruby),
            #[cfg(feature = "kotlin")]
            "kt" | "kts" => Some(Self::Kotlin),
            _ => None,
        }
    }

    /// Detect language from a file path.
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(Self::from_extension)
    }

    /// All file extensions (without dot) for enabled discovery languages.
    pub fn all_extensions() -> Vec<&'static str> {
        #[allow(unused_mut)]
        let mut extensions = vec![
            "ts", "mts", "cts", "tsx", "js", "mjs", "cjs", "jsx", "py", "pyi", "pyx",
        ];
        #[cfg(feature = "java")]
        extensions.extend(["java"]);
        #[cfg(feature = "c")]
        extensions.extend(["c", "h"]);
        #[cfg(feature = "cpp")]
        extensions.extend(["cpp", "cc", "cxx", "hpp", "hh", "hxx"]);
        #[cfg(feature = "arkts")]
        extensions.extend(["ets", "sts"]);
        #[cfg(feature = "cangjie")]
        extensions.extend(["cj", "cangjie"]);
        #[cfg(feature = "go")]
        extensions.extend(["go"]);
        #[cfg(feature = "csharp")]
        extensions.extend(["cs"]);
        #[cfg(feature = "rust")]
        extensions.extend(["rs"]);
        #[cfg(feature = "php")]
        extensions.extend(["php"]);
        #[cfg(feature = "ruby")]
        extensions.extend(["rb"]);
        #[cfg(feature = "kotlin")]
        extensions.extend(["kt", "kts"]);
        extensions
    }

    /// File patterns / globs used by this language (for file discovery).
    pub fn globs(self) -> &'static [&'static str] {
        match self {
            Self::TypeScript => &["**/*.ts", "**/*.mts", "**/*.cts", "**/*.tsx"],
            Self::JavaScript => &["**/*.js", "**/*.mjs", "**/*.cjs", "**/*.jsx"],
            Self::Python => &["**/*.py", "**/*.pyi", "**/*.pyx"],
            Self::Java => &["**/*.java"],
            Self::C => &["**/*.c", "**/*.h"],
            Self::Cpp => &[
                "**/*.cpp", "**/*.cc", "**/*.cxx", "**/*.hpp", "**/*.hh", "**/*.hxx",
            ],
            Self::ArkTS => &["**/*.ets", "**/*.sts"],
            Self::Cangjie => &["**/*.cj", "**/*.cangjie"],
            Self::Go => &["**/*.go"],
            Self::CSharp => &["**/*.cs"],
            Self::Rust => &["**/*.rs"],
            Self::Php => &["**/*.php"],
            Self::Ruby => &["**/*.rb"],
            Self::Kotlin => &["**/*.kt", "**/*.kts"],
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

    #[allow(clippy::should_implement_trait)]
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

/// 22 semantic edge kinds.
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
    /// Callback registration: registrant function → callback target
    /// e.g. `nghttp2_session_callbacks_set_on_frame_recv_callback → on_frame_recv`
    RegistersCallback,
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
            Self::RegistersCallback => "registers_callback",
        }
    }

    #[allow(clippy::should_implement_trait)]
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
            "registers_callback" => Some(Self::RegistersCallback),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// CallContext — call-site context annotation set by CFG builder
// ---------------------------------------------------------------------------

/// Call-site context annotation set by the CFG builder.
/// Language-agnostic; each language's CFG builder sets appropriate values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum CallContext {
    /// No special call-site context.
    #[default]
    None,
    /// Go `go` keyword — call executes in a goroutine.
    GoGoroutine,
    /// Go `defer` keyword — call is deferred to function exit.
    GoDefer,
    /// Python `with` statement — call executes in a context-managed block.
    PythonWith,
    /// Java `try-with-resources` — call executes in a context-managed block.
    JavaTryWith,
    /// C# `using` statement — call executes in a context-managed block.
    CSharpUsing,
    /// React useEffect cleanup return body — frees inside are deferred.
    ReactEffectCleanup,
    /// Ruby block-managed resource — allocs inside get auto-free at block exit.
    RubyBlock,
    /// Kotlin `.use {}` block — allocs inside get auto-free at block exit.
    KotlinUse,
}

impl CallContext {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::GoGoroutine => "go_goroutine",
            Self::GoDefer => "go_defer",
            Self::PythonWith => "python_with",
            Self::JavaTryWith => "java_try_with",
            Self::CSharpUsing => "csharp_using",
            Self::ReactEffectCleanup => "react_effect_cleanup",
            Self::RubyBlock => "ruby_block",
            Self::KotlinUse => "kotlin_use",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "go_goroutine" => Some(Self::GoGoroutine),
            "go_defer" => Some(Self::GoDefer),
            "python_with" => Some(Self::PythonWith),
            "java_try_with" => Some(Self::JavaTryWith),
            "csharp_using" => Some(Self::CSharpUsing),
            "react_effect_cleanup" => Some(Self::ReactEffectCleanup),
            "ruby_block" => Some(Self::RubyBlock),
            "kotlin_use" => Some(Self::KotlinUse),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// EffectKind — CFG node side-effect annotation
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

    #[allow(clippy::should_implement_trait)]
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

/// 6 import kinds matching common language patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    Include,
    #[default]
    Import,
    FromImport,
    /// Re-export from another module (`export * from './foo'` or `export { X } from './foo'`)
    ExportFrom,
    Package,
    Use,
}

impl ImportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Include => "include",
            Self::Import => "import",
            Self::FromImport => "from_import",
            Self::ExportFrom => "export_from",
            Self::Package => "package",
            Self::Use => "use",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "include" => Some(Self::Include),
            "import" => Some(Self::Import),
            "from_import" => Some(Self::FromImport),
            "export_from" => Some(Self::ExportFrom),
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

    #[allow(clippy::should_implement_trait)]
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

    #[allow(clippy::should_implement_trait)]
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

/// 7 strategies for resolving a reference to a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStrategy {
    ExactMatch,
    NameOnly,
    FuzzyMatch,
    Heuristic,
    ImportResolved,
    Builtin,
    /// Resolved via local dataflow def-use chain (e.g. function pointer call).
    DataflowPointer,
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
            Self::DataflowPointer => "dataflow_pointer",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "exact_match" => Some(Self::ExactMatch),
            "name_only" => Some(Self::NameOnly),
            "fuzzy_match" => Some(Self::FuzzyMatch),
            "heuristic" => Some(Self::Heuristic),
            "import_resolved" => Some(Self::ImportResolved),
            "builtin" => Some(Self::Builtin),
            "dataflow_pointer" => Some(Self::DataflowPointer),
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
    /// Detected via callback registration pattern match.
    CallbackPattern,
    /// User-declared annotation (e.g., function-pointer dispatch).
    UserAnnotation,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TreeSitter => "tree_sitter",
            Self::Scip => "scip",
            Self::Heuristic => "heuristic",
            Self::CallbackPattern => "callback_pattern",
            Self::UserAnnotation => "user_annotation",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "tree_sitter" => Some(Self::TreeSitter),
            "scip" => Some(Self::Scip),
            "heuristic" => Some(Self::Heuristic),
            "callback_pattern" => Some(Self::CallbackPattern),
            "user_annotation" => Some(Self::UserAnnotation),
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

    #[allow(clippy::should_implement_trait)]
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

    #[allow(clippy::should_implement_trait)]
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
        if v.is_nan() {
            return Self(0.5); // silent default for NaN
        }
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

    #[allow(clippy::should_implement_trait)]
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

/// 13 data-node kinds.  Used by [`super::dataflow::DataNode`].
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
    /// Identifier use (variable reference, not a declaration).
    VariableUse,
    /// Argument passed at a call-site.
    CallArg,
    /// Function/method being called (callee identifier).
    CallTarget,
    /// Value returned from a call-site.
    CallReturn,
    /// Receiver object (`this` / `self`).
    Receiver,
    /// Global / module-scoped variable.
    Global,
    /// Unknown / opaque node (low confidence).
    Unknown,
    /// React useEffect cleanup return: `return () => cleanup();`
    CleanupReturn,
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
            Self::VariableUse => "variable_use",
            Self::CallArg => "call_arg",
            Self::CallTarget => "call_target",
            Self::CallReturn => "call_return",
            Self::Receiver => "receiver",
            Self::Global => "global",
            Self::Unknown => "unknown",
            Self::CleanupReturn => "cleanup_return",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "parameter" => Some(Self::Parameter),
            "local" => Some(Self::Local),
            "field" => Some(Self::Field),
            "return" => Some(Self::Return),
            "literal" => Some(Self::Literal),
            "expr" => Some(Self::Expr),
            "variable_use" => Some(Self::VariableUse),
            "call_arg" => Some(Self::CallArg),
            "call_target" => Some(Self::CallTarget),
            "call_return" => Some(Self::CallReturn),
            "receiver" => Some(Self::Receiver),
            "global" => Some(Self::Global),
            "unknown" => Some(Self::Unknown),
            "cleanup_return" => Some(Self::CleanupReturn),
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
    /// Call argument flows to call target (intra-procedural).
    ArgToCall,
    /// Actual argument flows to formal parameter (inter-procedural).
    ArgToParam,
    /// Return expression flows to return node (intra-procedural).
    ReturnValue,
    /// Return value flows to call-site result (inter-procedural).
    ReturnToCall,
    /// Receiver flows to `this` / `self` inside callee.
    ReceiverToThis,
    /// Phi node (SSA merge point, e.g. after if-else).
    Phi,
}

impl DataFlowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assign => "assign",
            Self::Read => "read",
            Self::Write => "write",
            Self::FieldLoad => "field_load",
            Self::FieldStore => "field_store",
            Self::ArgToCall => "arg_to_call",
            Self::ArgToParam => "arg_to_param",
            Self::ReturnValue => "return_value",
            Self::ReturnToCall => "return_to_call",
            Self::ReceiverToThis => "receiver_to_this",
            Self::Phi => "phi",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "assign" => Some(Self::Assign),
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "field_load" => Some(Self::FieldLoad),
            "field_store" => Some(Self::FieldStore),
            "arg_to_call" => Some(Self::ArgToCall),
            "arg_to_param" => Some(Self::ArgToParam),
            "return_value" => Some(Self::ReturnValue),
            "return_to_call" => Some(Self::ReturnToCall),
            "receiver_to_this" => Some(Self::ReceiverToThis),
            "phi" => Some(Self::Phi),
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
    /// Block-exit point (Python `with`, Rust block drop, C++ destructor scope).
    BlockExit,
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
            Self::BlockExit => "block_exit",
        }
    }

    #[allow(clippy::should_implement_trait)]
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
            "block_exit" => Some(Self::BlockExit),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// EffectKind — CFG node side-effect annotation
// ─────────────────────────────────────────────────────────────────────────────

/// Effect annotation for CFG nodes — what side effect a statement/branch has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectKind {
    Read,      // Read access to a field/variable
    Write,     // Write/modify a field/variable
    Allocate,  // Memory allocation (malloc, new, calloc, realloc)
    Free,      // Memory deallocation (free, delete)
    Call,      // Function/method call
    Condition, // Branch condition evaluation
    Return,    // Return statement
    Goto,      // Goto statement
    Assign,    // Assignment statement
}

impl EffectKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Allocate => "allocate",
            Self::Free => "free",
            Self::Call => "call",
            Self::Condition => "condition",
            Self::Return => "return",
            Self::Goto => "goto",
            Self::Assign => "assign",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "allocate" => Some(Self::Allocate),
            "free" => Some(Self::Free),
            "call" => Some(Self::Call),
            "condition" => Some(Self::Condition),
            "return" => Some(Self::Return),
            "goto" => Some(Self::Goto),
            "assign" => Some(Self::Assign),
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

    #[allow(clippy::should_implement_trait)]
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
            Language::Go,
            Language::CSharp,
            Language::Rust,
            Language::Php,
            Language::Ruby,
            Language::Kotlin,
        ] {
            assert_eq!(Language::from_str(lang.as_str()), Some(lang));
        }
    }

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        #[cfg(feature = "arkts")]
        assert_eq!(Language::from_extension("ets"), Some(Language::ArkTS));
        #[cfg(not(feature = "arkts"))]
        assert_eq!(Language::from_extension("ets"), None);
        #[cfg(feature = "cangjie")]
        assert_eq!(Language::from_extension("cj"), Some(Language::Cangjie));
        #[cfg(not(feature = "cangjie"))]
        assert_eq!(Language::from_extension("cj"), None);
        #[cfg(feature = "go")]
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
        #[cfg(not(feature = "go"))]
        assert_eq!(Language::from_extension("go"), None);
        #[cfg(feature = "csharp")]
        assert_eq!(Language::from_extension("cs"), Some(Language::CSharp));
        #[cfg(not(feature = "csharp"))]
        assert_eq!(Language::from_extension("cs"), None);
        #[cfg(feature = "rust")]
        assert_eq!(Language::from_extension("rs"), Some(Language::Rust));
        #[cfg(not(feature = "rust"))]
        assert_eq!(Language::from_extension("rs"), None);
        #[cfg(feature = "php")]
        assert_eq!(Language::from_extension("php"), Some(Language::Php));
        #[cfg(not(feature = "php"))]
        assert_eq!(Language::from_extension("php"), None);
        #[cfg(feature = "ruby")]
        assert_eq!(Language::from_extension("rb"), Some(Language::Ruby));
        #[cfg(not(feature = "ruby"))]
        assert_eq!(Language::from_extension("rb"), None);
        #[cfg(feature = "kotlin")]
        assert_eq!(Language::from_extension("kt"), Some(Language::Kotlin));
        #[cfg(not(feature = "kotlin"))]
        assert_eq!(Language::from_extension("kt"), None);
        #[cfg(feature = "java")]
        assert_eq!(Language::from_extension("java"), Some(Language::Java));
        #[cfg(not(feature = "java"))]
        assert_eq!(Language::from_extension("java"), None);
        assert_eq!(Language::from_extension("unknown"), None);
    }

    #[test]
    fn test_language_mvp_only() {
        // Non-MVP languages MUST NOT be recognized when feature is off
        #[cfg(not(feature = "go"))]
        assert_eq!(Language::from_extension("go"), None);
        #[cfg(not(feature = "csharp"))]
        assert_eq!(Language::from_extension("cs"), None);
        #[cfg(not(feature = "rust"))]
        assert_eq!(Language::from_extension("rs"), None);
        #[cfg(not(feature = "php"))]
        assert_eq!(Language::from_extension("php"), None);
        #[cfg(not(feature = "ruby"))]
        assert_eq!(Language::from_extension("rb"), None);
        #[cfg(not(feature = "kotlin"))]
        assert_eq!(Language::from_extension("kt"), None);
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
        assert!(((a + b).as_f32() - 0.7).abs() < f32::EPSILON);
    }

    // ── EffectKind tests ──────────────────────────────────────────────────

    #[test]
    fn test_effect_kind_as_str_roundtrip() {
        let kinds = [
            EffectKind::Read,
            EffectKind::Write,
            EffectKind::Allocate,
            EffectKind::Free,
            EffectKind::Call,
            EffectKind::Condition,
            EffectKind::Return,
            EffectKind::Goto,
            EffectKind::Assign,
        ];
        for kind in &kinds {
            let s = kind.as_str();
            let back = EffectKind::from_str(s);
            assert_eq!(back, Some(*kind), "Roundtrip failed for {kind:?}");
        }
    }

    #[test]
    fn test_effect_kind_from_str_invalid() {
        assert_eq!(EffectKind::from_str("invalid"), None);
        assert_eq!(EffectKind::from_str(""), None);
        assert_eq!(EffectKind::from_str("READ"), None); // case sensitive
    }

    #[test]
    fn test_effect_kind_serde() {
        let kind = EffectKind::Free;
        let json = serde_json::to_string(&kind).unwrap();
        let parsed: EffectKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, parsed);
    }

    #[test]
    fn test_effect_kind_all_variants_have_unique_str() {
        use std::collections::HashSet;
        let kinds = [
            EffectKind::Read,
            EffectKind::Write,
            EffectKind::Allocate,
            EffectKind::Free,
            EffectKind::Call,
            EffectKind::Condition,
            EffectKind::Return,
            EffectKind::Goto,
            EffectKind::Assign,
        ];
        let mut seen = HashSet::new();
        for k in &kinds {
            assert!(seen.insert(k.as_str()), "Duplicate as_str: {}", k.as_str());
        }
        assert_eq!(seen.len(), 9);
    }

    // ── CfgNodeKind as_str roundtrip ──────────────────────────────────────

    #[test]
    fn test_cfg_node_kind_as_str_roundtrip() {
        let kinds = [
            CfgNodeKind::Entry,
            CfgNodeKind::Exit,
            CfgNodeKind::Statement,
            CfgNodeKind::Branch,
            CfgNodeKind::Loop,
            CfgNodeKind::Return,
            CfgNodeKind::Throw,
            CfgNodeKind::Join,
        ];
        for kind in &kinds {
            let s = kind.as_str();
            let back = CfgNodeKind::from_str(s);
            assert_eq!(back, Some(*kind));
        }
    }

    #[test]
    fn test_cfg_node_kind_from_str_invalid() {
        assert_eq!(CfgNodeKind::from_str("invalid"), None);
        assert_eq!(CfgNodeKind::from_str(""), None);
    }
}
