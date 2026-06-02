//! Domain rules — re-exports from the language-agnostic domain_rules crate.
//! C/C++ ownership rules are in `ownership_rules`.

pub use domain_rules::*;
pub use super::ownership_rules::CppOwnershipRules;
