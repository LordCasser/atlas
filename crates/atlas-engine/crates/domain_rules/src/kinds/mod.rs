//! Language-specific rule kind registries.
//!
//! Each language module defines a `LanguageRuleKinds` implementation
//! with its own set of rule kinds, builtin rules, and validation logic.

pub mod c;
pub mod csharp;
pub mod go;
pub mod java;
pub mod kotlin;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod typescript;
