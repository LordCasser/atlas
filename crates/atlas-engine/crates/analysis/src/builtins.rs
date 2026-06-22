//! Shared builtin C/C++ function name constants.
//!
//! These constants are the single source of truth for the builtin allocation,
//! deallocation, and ownership-ambiguous function names recognized by both
//! [`CppOwnershipRules`] (ownership-rules layer) and [`ResourceOpConfig`]
//! (resource-operation layer).

/// C/C++ allocation functions that return owned resources.
pub(crate) const C_ALLOC_FUNCTIONS: &[&str] = &[
    "malloc",
    "calloc",
    "strdup",
    "strndup",
    "fopen",
    "kmalloc",
    "kzalloc",
    "kcalloc",
    "kmalloc_array",
    "kvcalloc",
    "vmalloc",
    "vzalloc",
    "kzalloc_obj",
    "operator new",
    "operator new[]",
];

/// C/C++ deallocation/free functions that consume resources.
pub(crate) const C_FREE_FUNCTIONS: &[&str] = &[
    "free",
    "kfree",
    "kvfree",
    "vfree",
    "operator delete",
    "operator delete[]",
    "std::free",
];

/// Functions that may or may not transfer ownership (e.g., realloc).
pub(crate) const C_MAYBE_OWNED: &[&str] = &["realloc"];
