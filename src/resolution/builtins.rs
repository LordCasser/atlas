//! Built-in/external symbol filter.

use crate::types::Language;

/// Filters out known built-in names that shouldn't be resolved to project symbols.
pub struct BuiltinFilter;

/// Built-in sets per language.
const TS_JS_BUILTINS: &[&str] = &[
    "console", "JSON", "Math", "Array", "Object", "String", "Number",
    "Boolean", "Function", "Date", "RegExp", "Error", "Map", "Set",
    "Promise", "Symbol", "WeakMap", "WeakSet", "Proxy", "Reflect",
    "Intl", "BigInt", "undefined", "NaN", "Infinity", "parseInt",
    "parseFloat", "isNaN", "isFinite", "eval", "encodeURI", "decodeURI",
    "setTimeout", "setInterval", "clearTimeout", "clearInterval",
    "fetch", "Response", "Request", "Headers", "FormData", "URL",
    "URLSearchParams", "Blob", "File", "FileReader", "AbortController",
    "Buffer", "process", "global", "globalThis", "window", "document",
    "navigator", "location", "history", "localStorage", "sessionStorage",
    "alert", "confirm", "prompt", "atob", "btoa",
];

const PYTHON_BUILTINS: &[&str] = &[
    "print", "len", "type", "int", "float", "str", "bool", "list",
    "dict", "tuple", "set", "frozenset", "range", "enumerate", "zip",
    "map", "filter", "sorted", "reversed", "abs", "all", "any",
    "bin", "hex", "oct", "chr", "ord", "divmod", "pow", "round",
    "sum", "min", "max", "isinstance", "issubclass", "hasattr",
    "getattr", "setattr", "delattr", "callable", "iter", "next",
    "open", "input", "repr", "format", "dir", "vars", "globals",
    "locals", "id", "hash", "compile", "exec", "eval", "help",
    "object", "Exception", "ValueError", "TypeError", "KeyError",
    "IndexError", "AttributeError", "ImportError", "RuntimeError",
    "StopIteration", "OSError", "IOError", "super", "self", "cls",
    "None", "True", "False", "NotImplemented", "Ellipsis",
    "__name__", "__main__", "__init__", "__file__", "__doc__",
    "__builtins__", "__package__",
];

impl BuiltinFilter {
    /// Returns `true` if the name is a known built-in for the given language
    /// and should be excluded from resolution.
    pub fn is_builtin(name: &str, lang: Language) -> bool {
        match lang {
            Language::TypeScript | Language::JavaScript | Language::ArkTS => {
                TS_JS_BUILTINS.contains(&name)
            }
            Language::Python => PYTHON_BUILTINS.contains(&name),
            // Other languages will be added in future milestones.
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ts_js_builtins() {
        assert!(BuiltinFilter::is_builtin("console", Language::TypeScript));
        assert!(BuiltinFilter::is_builtin("Promise", Language::JavaScript));
        assert!(BuiltinFilter::is_builtin("window", Language::ArkTS));
        assert!(!BuiltinFilter::is_builtin("myFunction", Language::TypeScript));
    }

    #[test]
    fn test_python_builtins() {
        assert!(BuiltinFilter::is_builtin("print", Language::Python));
        assert!(BuiltinFilter::is_builtin("len", Language::Python));
        assert!(!BuiltinFilter::is_builtin("numpy", Language::Python));
    }
}
