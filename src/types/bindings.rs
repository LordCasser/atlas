//! Atlas lexical binding types.
//!
//! A **binding** is a lexical name-to-value association at a particular scope:
//! function parameters, local variables (`let`/`const`/`var`), class fields,
//! import aliases, catch variables, etc.
//!
//! # Relationship with other types
//!
//! - [`BindingDef`] is a **definition** point (where the name is introduced).
//! - [`BindingUse`] is a **usage** site (where the name is read/written).
//! - [`ReferenceUse::binding_id`] links a reference back to its binding.
//! - [`DataNode::binding_id`] links a dataflow node back to its binding.
//!
//! # Invariants
//!
//! - Binding IDs are deterministic (blake3) and include `scope_id` so that
//!   same-named bindings in different scopes produce distinct IDs.
//! - Bindings are per-file (not cross-file — imports create new bindings).
//! - A [`BindingUse`] may have `binding_id = None` if unresolved.

use serde::{Deserialize, Serialize};

use super::enums::BindingKind;
use super::ids::{BindingId, BindingUseId, FileId, ReferenceId, ScopeId, SymbolId};
use super::structs::TextRange;

// ---------------------------------------------------------------------------
// BindingDef — a lexical binding definition point
// ---------------------------------------------------------------------------

/// A definition point where a name is bound to a value / parameter.
///
/// Examples: `function f(x) { ... }` → BindingDef for `x` (kind=Parameter).
///           `const y = expr`       → BindingDef for `y` (kind=Local).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BindingDef {
    /// Deterministic identity.
    pub id: BindingId,

    /// Containing file.
    pub file_id: FileId,

    /// Enclosing function, if this binding is inside a function body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_id: Option<SymbolId>,

    /// Scope where this binding is introduced.
    pub scope_id: ScopeId,

    /// What kind of binding this is.
    pub kind: BindingKind,

    /// The declared name (e.g. "req", "name").
    pub name: String,

    /// If this binding corresponds directly to a project-level symbol
    /// (e.g. a class property or module-level variable), the symbol ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_id: Option<SymbolId>,

    /// Source range of the binding definition.
    pub range: TextRange,
}

// ---------------------------------------------------------------------------
// BindingUse — a lexical binding usage site
// ---------------------------------------------------------------------------

/// A usage site where a binding is read or written.
///
/// Every identifier expression that is not a definition is a binding use.
/// The same use may be both a binding use **and** a reference; the
/// `reference_id` field links back to the Atlas reference when available.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BindingUse {
    /// Deterministic identity.
    pub id: BindingUseId,

    /// Containing file.
    pub file_id: FileId,

    /// Scope where this use occurs.
    pub scope_id: ScopeId,

    /// The binding being used, if resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<BindingId>,

    /// The Atlas reference that corresponds to this use, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<ReferenceId>,

    /// Identifier name at the use site.
    pub name: String,

    /// Source range of the binding use.
    pub range: TextRange,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::enums::Language;
    use crate::types::ids::FileId;
    use crate::types::ids::ScopeId;
    use crate::types::ids::SymbolId;
    use crate::types::structs::TextRange;

    fn make_file_id() -> FileId {
        FileId::generate("test.ts")
    }

    fn make_range(start: u32, end: u32) -> TextRange {
        TextRange {
            start_byte: start,
            end_byte: end,
            start_line: 1,
            start_column: start,
            end_line: 1,
            end_column: end,
        }
    }

    #[test]
    fn test_binding_def_serialization_roundtrip() {
        let file_id = make_file_id();
        let scope_id = ScopeId::generate(&file_id, None, "function", 10);
        let binding = BindingDef {
            id: BindingId::generate(&file_id, &scope_id, "parameter", "req", 42),
            file_id,
            function_id: Some(SymbolId::generate(&file_id, "typescript", "handler", "function", None)),
            scope_id,
            kind: BindingKind::Parameter,
            name: "req".to_string(),
            symbol_id: None,
            range: make_range(42, 45),
        };
        let json = serde_json::to_string(&binding).unwrap();
        let parsed: BindingDef = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "req");
        assert_eq!(parsed.kind, BindingKind::Parameter);
    }

    #[test]
    fn test_binding_use_serialization_roundtrip() {
        let file_id = make_file_id();
        let scope_id = ScopeId::generate(&file_id, None, "function", 10);
        let use_site = BindingUse {
            id: BindingUseId::generate(&file_id, None, None, "req", 100),
            file_id,
            scope_id,
            binding_id: None,
            reference_id: None,
            name: "req".to_string(),
            range: make_range(100, 103),
        };
        let json = serde_json::to_string(&use_site).unwrap();
        let parsed: BindingUse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "req");
    }
}
