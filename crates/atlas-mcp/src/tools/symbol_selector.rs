//! MCP wrappers for the engine-layer [`atlas_engine::symbol_selector`] module.
//!
//! This file provides:
//! 1. Re-exports of all engine types for MCP tool handlers.
//! 2. Convenience methods on [`ToolRouter`] that delegate to engine functions.
//!
//! The core resolution logic lives in [`atlas_engine::symbol_selector`].

// Re-export engine types for internal MCP use
pub(crate) use atlas_engine::symbol_selector::{
    MAX_AGGREGATION_CANDIDATES, ResolvedSymbol, ScoredCandidate, SymbolInput, SymbolResolution,
    SymbolResolutionPolicy, SymbolSelector,
};
use atlas_engine::{FileId, Language};

use super::ToolRouter;
use std::path::Path;

impl ToolRouter {
    /// Unified symbol resolution — delegates to engine.
    pub(crate) fn resolve_symbol_input(
        &self,
        input: &SymbolInput,
        policy: SymbolResolutionPolicy,
    ) -> Result<SymbolResolution, String> {
        atlas_engine::symbol_selector::resolve_symbol_input(&self.project().store, input, policy)
    }

    /// Resolve a SymbolSelector file_path before the file exists in `files`.
    ///
    /// Fresh focus sessions only have `file_inventory`, so precise selectors
    /// from MCP clients must be able to seed extraction from that table.
    pub(crate) fn resolve_selector_file_id(&self, input: &SymbolInput) -> Option<FileId> {
        let SymbolInput::Selector(sel) = input else {
            return None;
        };
        let file_path = sel.file_path.as_deref()?;
        let clean = file_path
            .trim()
            .trim_start_matches("./")
            .trim_start_matches('/');
        if clean.is_empty() {
            return None;
        }

        if let Ok(Some(file_id)) = self.project()
            .store
            .resolve_file_id(&self.project().root, clean)
        {
            return Some(file_id);
        }

        self.project()
            .store
            .find_file_inventory_by_path(clean)
            .ok()
            .flatten()
            .and_then(|row| {
                let arr: [u8; 32] = row.file_id.as_slice().try_into().ok()?;
                Some(FileId::from_bytes(arr))
            })
            .or_else(|| self.register_selector_file_inventory(clean))
    }

    fn register_selector_file_inventory(&self, clean: &str) -> Option<FileId> {
        if clean.contains("..") || Path::new(clean).is_absolute() {
            return None;
        }

        let active = self.project();
        let abs_path = active.root.join(clean);
        let metadata = std::fs::metadata(&abs_path).ok()?;
        if !metadata.is_file() {
            return None;
        }

        let file_id = FileId::generate(clean);
        let language = Language::from_path(Path::new(clean)).unwrap_or(Language::TypeScript);
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        #[cfg(unix)]
        let (inode, dev) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.ino() as i64, metadata.dev() as i64)
        };
        #[cfg(not(unix))]
        let (inode, dev) = (0i64, 0i64);

        active
            .store
            .insert_file_inventory(
                &file_id,
                clean,
                language.as_str(),
                mtime,
                metadata.len() as i64,
                inode,
                dev,
            )
            .ok()?;

        if let Ok(content) = std::fs::read(&abs_path) {
            let hash = blake3::hash(&content).to_hex().to_string();
            let _ = active.store.set_file_fingerprint(&file_id, &hash);
        }

        Some(file_id)
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
            let sel: SymbolSelector =
                serde_json::from_value(val).map_err(|e| format!("Invalid {field}: {e}"))?;
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
