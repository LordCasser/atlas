//! C++ declaration facts retained by extraction for receiver lookup.
//!
//! These describe written declarations, not inferred runtime object types.
use serde::{Deserialize, Serialize};

use crate::{BindingId, ReferenceId, SymbolId, TextRange};

/// A failed type or callable-name prerequisite actually reached by the resolver. This identifies the
/// missing analysis prerequisite, not proof that source or a runtime target is absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppTypeLookupFailure {
    pub kind: CppTypeLookupFailureKind,
    pub name: String,
    pub scope: String,
    pub file_id: crate::FileId,
    pub range: TextRange,
    /// Original declarations that caused this reached lookup restriction.
    /// The primary file/range remains the type use, call or argument origin. Empty means no separate
    /// declaration location was recorded, not that all other causes are absent.
    pub related_declarations: Vec<(crate::FileId, TextRange)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CppTypeLookupFailureKind {
    DefinitionUnavailable,
    DefinitionAmbiguous,
    LookupRestricted,
    NameLookupRestricted,
    VirtualMemberUnresolved,
    NameHidingUnsupported,
    TemplateUnsupported,
    ArgumentIncompatible,
    AnalysisBudgetExceeded,
}

/// Identity of a reached prerequisite, excluding its declaration inventory.
/// Equal positions can have different inventories in different lookup environments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppTypeLookupContext {
    pub kind: CppTypeLookupFailureKind,
    pub name: String,
    pub scope: String,
    pub file_id: crate::FileId,
    pub range: TextRange,
}

/// Global grouping evidence, not a decision to emit a source region. Consumers
/// must also establish usable primary and related declaration locations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CppTypeLookupSharing {
    pub matching_failures: usize,
    pub same_declarations: bool,
}

impl Default for CppTypeLookupSharing {
    fn default() -> Self {
        Self {
            matching_failures: 0,
            same_declarations: true,
        }
    }
}

impl CppTypeLookupSharing {
    /// Compare the original ordered inventories, including duplicates. A single
    /// disagreement disqualifies the entire context from shared presentation.
    pub fn observe(
        &mut self,
        expected: &[(crate::FileId, TextRange)],
        observed: &[(crate::FileId, TextRange)],
    ) {
        self.matching_failures += 1;
        self.same_declarations &= expected == observed;
    }

    pub fn has_shared_inventory(self) -> bool {
        self.matching_failures > 1 && self.same_declarations
    }
}

impl CppTypeLookupFailure {
    pub fn context(&self) -> CppTypeLookupContext {
        CppTypeLookupContext {
            kind: self.kind,
            name: self.name.clone(),
            scope: self.scope.clone(),
            file_id: self.file_id,
            range: self.range,
        }
    }

    pub fn code(&self) -> &'static str {
        match self.kind {
            CppTypeLookupFailureKind::DefinitionUnavailable => "cpp_type_definition_unavailable",
            CppTypeLookupFailureKind::DefinitionAmbiguous => "cpp_type_definition_ambiguous",
            CppTypeLookupFailureKind::LookupRestricted => "cpp_type_lookup_restricted",
            CppTypeLookupFailureKind::NameLookupRestricted => "cpp_name_lookup_restricted",
            CppTypeLookupFailureKind::VirtualMemberUnresolved => "cpp_virtual_member_unresolved",
            CppTypeLookupFailureKind::NameHidingUnsupported => "cpp_type_name_hiding_unmodeled",
            CppTypeLookupFailureKind::TemplateUnsupported => "cpp_type_template_unmodeled",
            CppTypeLookupFailureKind::ArgumentIncompatible => "cpp_argument_incompatible",
            CppTypeLookupFailureKind::AnalysisBudgetExceeded => "cpp_type_analysis_budget_exceeded",
        }
    }

    pub fn message(&self) -> String {
        let cause = match self.kind {
            CppTypeLookupFailureKind::DefinitionUnavailable => {
                "No visible recorded definition establishes this type. Inspect the related type use and source/include inputs; missing input and omitted extraction are not distinguished by this lookup result."
            }
            CppTypeLookupFailureKind::DefinitionAmbiguous => {
                "Multiple visible recorded definitions prevent a unique type selection."
            }
            CppTypeLookupFailureKind::LookupRestricted => {
                "Recorded extraction or name-lookup restrictions prevent establishing this type."
            }
            CppTypeLookupFailureKind::NameLookupRestricted => {
                return format!(
                    "Callable name lookup for '{}' stopped in scope '{}': recorded declarations restrict lookup. Inspect the related declarations before selecting an outer or same-named target. This is a reached prerequisite failure, not an enumeration of all remaining blockers.",
                    self.name, self.scope
                );
            }
            CppTypeLookupFailureKind::NameHidingUnsupported => {
                "A visible alias or non-record declaration affects the name; this lookup cannot select an outer same-named type."
            }
            CppTypeLookupFailureKind::VirtualMemberUnresolved => {
                return format!(
                    "Member lookup for '{}' reached a same-named virtual declaration in '{}'; this resolver has not established a direct body target. The related declaration is a lookup fact, not proof of overload selection, the runtime implementation or invocation. Other prerequisites may remain.",
                    self.name, self.scope
                );
            }
            CppTypeLookupFailureKind::TemplateUnsupported => {
                "Template arguments or visible specializations exceed supported type lookup."
            }
            CppTypeLookupFailureKind::ArgumentIncompatible => {
                return format!(
                    "Argument type '{}' from scope '{}': a nonconstant arithmetic value or nonzero integer literal cannot bind to the selected pointer parameter. Inspect the argument origin and related callable declaration. Other argument conversions are not established by this check.",
                    self.name, self.scope
                );
            }
            CppTypeLookupFailureKind::AnalysisBudgetExceeded => {
                "Type identity analysis exhausted its bounded budget before examining all required type arguments. This is an unexamined remainder, not a missing type or a demonstrated conflict."
            }
        };
        format!(
            "Type lookup for '{}' from scope '{}': {cause} This is a reached prerequisite failure, not an enumeration of all remaining blockers.",
            self.name, self.scope
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppDeclaredType {
    /// A possibly qualified class/template name. Aliases need separate lookup.
    pub name: String,
    pub template_arguments: Vec<CppDeclaredType>,
    pub pointer: bool,
    pub reference: bool,
    /// Qualifiers on the object (or pointee), not on a pointer variable.
    pub const_: bool,
    pub volatile: bool,
}

/// A written alias declaration. The supported target subset introduces no
/// additional declarator/cv layers; unsupported aliases still hide their name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppTypeAlias {
    pub symbol_id: SymbolId,
    pub target: Option<CppDeclaredType>,
    pub target_range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppValueType {
    pub binding_id: Option<BindingId>,
    pub symbol_id: Option<SymbolId>,
    /// An automatic local/parameter needs capture when used across a lambda.
    pub capture_required: bool,
    /// Field properties needed by member-expression typing. False for locals.
    pub mutable_: bool,
    pub bit_field: bool,
    /// None means the declaration exists but its type syntax is unsupported.
    pub declared_type: Option<CppDeclaredType>,
    /// Plain auto initialized by this call; its declared return type may be used.
    pub initializer_call: Option<ReferenceId>,
    pub lookup_scope: String,
    pub declaration_range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppRecordType {
    pub symbol_id: SymbolId,
    pub is_definition: bool,
    /// None retains an unsupported base clause; an empty list means no bases.
    pub bases: Option<Vec<CppBaseClass>>,
    /// The class head and enclosing scope establish a supported type identity.
    /// This does not establish completeness of members or their function bodies.
    pub identity_supported: bool,
    /// Member declarations are inspectable for lookup; function-body contents
    /// have independent extraction limits and cannot introduce class members.
    pub lookup_supported: bool,
    /// Written type parameter names of a supported primary class template.
    /// None denotes an ordinary record, or unsupported syntax gated above.
    pub template_parameters: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppBaseClass {
    /// Written base type, including supported primary-template arguments.
    pub declared_type: CppDeclaredType,
    pub virtual_: bool,
    pub range: TextRange,
}

/// Supported written callable types, extracted from declarator AST nodes.
/// Parameter names/default expressions are not part of a function's identity.
/// This does not expand aliases or establish compiler-level type equivalence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppCallableDeclaration {
    pub symbol_id: SymbolId,
    /// Ordered names in a supported primary function-template declaration.
    /// None denotes an ordinary callable; unsupported templates have no callable
    /// entry. These names do not establish argument substitution or applicability.
    pub template_parameters: Option<Vec<String>>,
    pub parameter_types: Vec<String>,
    /// Declaration types used for argument matching, in parameter order.
    /// Unsupported declarators retain their written identity above and None here.
    pub parameter_declared_types: Vec<Option<CppDeclaredType>>,
    pub minimum_arity: u32,
    /// Member cv/ref qualifiers; noexcept and declaration-only specifiers omitted.
    pub qualifiers: String,
    pub is_virtual: bool,
    /// File-local free functions must not bind to another translation unit.
    pub internal_linkage: bool,
    /// A supported written return type, not a summary of runtime returned values.
    pub return_type: Option<CppDeclaredType>,
}

/// Syntax needed to derive an argument type without reparsing source text.
/// Unsupported expressions remain present, with their range in Callsite.args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CppArgumentExpression {
    Name(String),
    Literal(String),
    Field {
        object: Box<Self>,
        name: String,
        arrow: bool,
    },
    Binary {
        operator: String,
        left: Box<Self>,
        right: Box<Self>,
    },
    Unknown,
}

/// Syntax of a written function-template callee. The original ReferenceUse
/// retains its identity, expression and source range. These fields provide the
/// ordinary-name lookup form without losing the explicit template arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppTemplateCall {
    pub reference_id: super::ReferenceId,
    pub name: String,
    pub text: String,
    pub receiver: Option<String>,
    pub name_range: TextRange,
    pub arguments_range: TextRange,
    /// None retains unsupported type/value/declarator syntax. Empty lists are
    /// distinct: deduction/defaults would be required before selection.
    pub arguments: Option<Vec<CppDeclaredType>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CppCaptureKind {
    Copy,
    Reference,
    /// Init/pack capture introduces a name whose type is not inferred here.
    Unknown,
}

/// Anonymous callable identity and lexical facts. Containment is not invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppLambdaCapture {
    pub symbol_id: Option<super::SymbolId>,
    pub enclosing_symbol: Option<super::SymbolId>,
    /// A plain auto variable initialized directly by this closure expression.
    pub binding_id: Option<super::BindingId>,
    pub binding_const: bool,
    /// Written ordinary parameter count; dependent/unsupported declarators are unknown.
    pub parameter_count: Option<u32>,
    pub range: TextRange,
    pub body_range: TextRange,
    pub default: Option<CppCaptureKind>,
    /// None preserves an unsupported capture/declarator syntax boundary.
    pub captures: Option<Vec<(String, CppCaptureKind)>>,
    pub mutable_: bool,
}

/// Written allocation syntax whose allocator/initialization invocations are
/// not represented by the ordinary call-reference extractor. This records a
/// source region, not a selected constructor or proof of evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppAllocationSite {
    pub range: TextRange,
    pub source_symbol: Option<SymbolId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppFileTypes {
    pub aliases: Vec<CppTypeAlias>,
    pub lambda_captures: Vec<CppLambdaCapture>,
    pub allocation_sites: Vec<CppAllocationSite>,
    /// Reuses the reference identity and argument ordering of existing callsites.
    pub arguments: Vec<(super::ReferenceId, Vec<CppArgumentExpression>)>,
    pub template_calls: Vec<CppTemplateCall>,
    /// Written definitions, including unsupported/undefined macro names.
    pub macros: Vec<CppMacroDefinition>,
    /// Possible annotation tokens at the recorded declaration position.
    pub annotation_candidates: Vec<CppAnnotationToken>,
    /// Validated annotation tokens omitted from the parser input only.
    pub normalized_annotations: Vec<CppAnnotationToken>,
    /// Direct class-member macro-shaped declarations. Their name inventory is
    /// unknown until actual visible definitions have been checked at indexing.
    pub member_macros: Vec<CppMemberMacro>,
    /// Member invocations omitted from parser input after their complete
    /// declaration inventories were checked. Their name limits remain recorded.
    pub normalized_member_macros: Vec<CppMemberMacro>,
    /// Source owners with unverified callable scopes, including recovered records.
    pub unverified_callable_scopes: Vec<SymbolId>,
    /// Unexpanded declarations with their actual source and lexical extent.
    pub lookup_limits: Vec<CppLookupLimit>,
    /// Friend/inline-namespace declarations requiring additional ADL semantics.
    /// These restrict associated lookup only, not ordinary member lookup.
    pub adl_limits: Vec<CppLookupLimit>,
    /// Class template names with specializations not yet selected semantically.
    pub specialized_templates: Vec<String>,
    pub values: Vec<CppValueType>,
    pub records: Vec<CppRecordType>,
    /// Missing entries mean the callable declaration syntax is unsupported.
    pub callables: Vec<CppCallableDeclaration>,
    /// Calls where intervening closures do not establish an explicit captured
    /// this pointer. This limits access to the enclosing object, not locals.
    pub this_capture_unavailable: Vec<ReferenceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppMacroDefinition {
    pub name: String,
    /// Some for supported function-like parameter lists, including zero args.
    pub parameters: Option<Vec<String>>,
    /// Standard trailing ellipsis; named parameters exclude __VA_ARGS__.
    pub variadic: bool,
    /// None for undef directives or unsupported syntax/parameter lists.
    pub replacement: Option<String>,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppMemberMacro {
    pub scope: String,
    pub name: String,
    /// Original invocation text, including a terminating semicolon when written.
    pub text: String,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CppAnnotationPosition {
    ClassPrefix,
    DeclarationPrefix,
    CallableSuffix,
    LambdaSuffix,
    DataSuffix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppAnnotationToken {
    pub name: String,
    /// Original spelling, including arguments for a function-like invocation.
    pub text: String,
    pub range: TextRange,
    pub position: CppAnnotationPosition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppLookupLimit {
    /// Enclosing class/namespace; block ownership is represented separately.
    pub scope: String,
    /// A single introduced name, or None for an unknown set of names.
    pub name: Option<String>,
    pub declaration_range: TextRange,
    /// A verified local block. None keeps the enclosing-scope restriction.
    /// Class/namespace declaration order is not yet fully modeled.
    pub block_range: Option<TextRange>,
}

impl CppLookupLimit {
    /// Whether this unexpanded declaration can affect a written name at a
    /// position in the same file. This does not resolve the imported name.
    pub fn limits_local_lookup(&self, name: &str, start_byte: u32) -> bool {
        !name.starts_with("::")
            && self.block_range.is_some_and(|block| {
                start_byte >= self.declaration_range.end_byte
                    && start_byte >= block.start_byte
                    && start_byte < block.end_byte
            })
            && self
                .name
                .as_deref()
                .is_none_or(|imported| Some(imported) == name.split("::").next())
    }

    /// Direct callee lookup only. Object member names use their receiver's
    /// class scope; a receiver's declared type needs a separate lookup site.
    pub fn limits_local_call_lookup(&self, call: &crate::ReferenceUse) -> bool {
        if call.kind != crate::ReferenceKind::Call
            || call
                .receiver
                .as_deref()
                .is_some_and(|receiver| call.text != format!("{receiver}::{}", call.name))
        {
            return false;
        }
        self.limits_local_lookup(&call.text, call.range.start_byte)
    }
}
