//! MCP wrappers for the engine-layer [`atlas_engine::symbol_selector`] module.
//!
//! This file provides:
//! 1. Re-exports of all engine types for MCP tool handlers.
//! 2. Convenience methods on [`ToolRouter`] that delegate to engine functions.
//!
//! The core resolution logic lives in [`atlas_engine::symbol_selector`].

// Re-export engine types for internal MCP use
pub(crate) use atlas_engine::symbol_selector::{
    ResolvedSymbol, ScoredCandidate, SymbolInput, SymbolResolution, SymbolResolutionPolicy,
    SymbolSelector, MAX_AGGREGATION_CANDIDATES,
};

use super::ToolRouter;

impl ToolRouter {
    /// Unified symbol resolution — delegates to engine.
    pub(crate) fn resolve_symbol_input(
        &self,
        input: &SymbolInput,
        policy: SymbolResolutionPolicy,
    ) -> Result<SymbolResolution, String> {
        atlas_engine::symbol_selector::resolve_symbol_input(&self.active().store, input, policy)
    }

}

/// Parse an `args[field]` value into `SymbolInput`.
///
/// Handles four cases in order:
/// 1. `Value::String(s)` that looks like a JSON object (`s.starts_with('{')`)
///    → try `serde_json::from_str::<SymbolSelector>`, fall back to `SymbolInput::Name(s)`
///    (recovers from MCP clients that stringify `oneOf` object arguments).
/// 2. `Value::String(s)` (plain qualified name) → `SymbolInput::Name(s)`.
/// 3. `Value::Object(_)` → deserialize as `SymbolSelector` → `SymbolInput::Selector`.
/// 4. Missing / `Value::Null` / other types → `Err("'{field}' parameter is required")`
///    or `Err("'{field}' must be a string or object")`.
pub(crate) fn parse_symbol_input(
    args: &serde_json::Value,
    field: &str,
) -> Result<SymbolInput, String> {
    let val = match args.get(field) {
        Some(v) if !v.is_null() => v.clone(),
        _ => return Err(format!("'{field}' parameter is required")),
    };

    match &val {
        serde_json::Value::String(s) => {
            let trimmed = s.trim_start();
            if trimmed.starts_with('{') {
                if let Ok(sel) = serde_json::from_str::<SymbolSelector>(s) {
                    return Ok(SymbolInput::Selector(sel));
                }
                // parse failure → treat as a plain name
                // (e.g. a function literally named `{something}`)
            }
            Ok(SymbolInput::Name(s.clone()))
        }
        serde_json::Value::Object(_) => {
            let sel: SymbolSelector = serde_json::from_value(val)
                .map_err(|e| format!("Invalid {field}: {e}"))?;
            Ok(SymbolInput::Selector(sel))
        }
        _ => Err(format!("'{field}' must be a string or object")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_plain_string() {
        let result = parse_symbol_input(&json!({"symbol": "myFunc"}), "symbol");
        match result {
            Ok(SymbolInput::Name(name)) => assert_eq!(name, "myFunc"),
            other => panic!("expected Name(\"myFunc\"), got {other:?}"),
        }
    }

    #[test]
    fn test_parse_object() {
        let result = parse_symbol_input(
            &json!({"symbol": {"qualified_name": "myFunc", "kind": "function"}}),
            "symbol",
        );
        match result {
            Ok(SymbolInput::Selector(sel)) => {
                assert_eq!(sel.qualified_name, "myFunc");
                assert_eq!(sel.kind.as_deref(), Some("function"));
            }
            other => panic!("expected Selector, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_stringified_object() {
        let result = parse_symbol_input(
            &json!({"symbol": "{\"qualified_name\": \"myFunc\", \"kind\": \"function\"}"}),
            "symbol",
        );
        match result {
            Ok(SymbolInput::Selector(sel)) => {
                assert_eq!(sel.qualified_name, "myFunc");
                assert_eq!(sel.kind.as_deref(), Some("function"));
            }
            other => panic!("expected Selector from stringified object, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_missing_field() {
        let result = parse_symbol_input(&json!({}), "symbol");
        match result {
            Err(msg) => assert!(
                msg.contains("required"),
                "expected 'required' in error, got: {msg}"
            ),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_null_field() {
        let result = parse_symbol_input(&json!({"symbol": null}), "symbol");
        match result {
            Err(msg) => assert!(
                msg.contains("required"),
                "expected 'required' in error, got: {msg}"
            ),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_weird_braces_name() {
        let result = parse_symbol_input(&json!({"symbol": "{weirdFunc}"}), "symbol");
        match result {
            Ok(SymbolInput::Name(name)) => assert_eq!(name, "{weirdFunc}"),
            other => panic!("expected Name(\"{{weirdFunc}}\"), got {other:?}"),
        }
    }
}
