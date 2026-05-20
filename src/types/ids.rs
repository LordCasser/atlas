//! Atlas typed IDs: FileId, SymbolId, ScopeId, ReferenceId, EdgeId, CallsiteId, ImportId.
//!
//! All IDs are blake3([u8; 32]) newtypes. Stored as BLOB in SQLite.
//! Deterministic: same inputs always produce the same ID.
//! Collision-resistant: blake3 256-bit.

use blake3::Hasher;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use std::fmt;

/// Macro to define a typed blake3 ID newtype with shared trait impls.
/// Reduces boilerplate across 7 ID types while maintaining type safety.
macro_rules! define_id {
    (
        $(#[$meta:meta])*
        $name:ident
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Generate a new ID by hashing the given input bytes with blake3.
            #[allow(dead_code)]
            fn from_hash(input: &[u8]) -> Self {
                Self(blake3::hash(input).into())
            }

            /// Chain multiple byte slices with null separators and hash.
            #[allow(dead_code)]
            fn from_parts(parts: &[&[u8]]) -> Self {
                let mut hasher = Hasher::new();
                for (i, part) in parts.iter().enumerate() {
                    if i > 0 {
                        hasher.update(b"\0"); // separator prevents collisions
                    }
                    hasher.update(part);
                }
                Self(hasher.finalize().into())
            }

            /// Get the raw 32-byte hash.
            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// Hex representation for display/debug.
            pub fn to_hex(&self) -> String {
                hex::encode(self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // Show first 16 hex chars (8 bytes) for readability
                let hex = self.to_hex();
                write!(f, "{}..{}", &hex[..8], &hex[56..])
            }
        }

        impl std::str::FromStr for $name {
            type Err = anyhow::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                if s.len() != 64 {
                    anyhow::bail!(
                        "Invalid {}: expected 64 hex characters, got {} chars",
                        stringify!($name),
                        s.len()
                    );
                }
                let bytes = hex::decode(s)
                    .map_err(|e| anyhow::anyhow!("Invalid hex in {}: {}", stringify!($name), e))?;
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Wrong length for {}", stringify!($name)))?;
                Ok(Self(arr))
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_hex())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.0.as_slice()))
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                match value {
                    ValueRef::Blob(blob) => {
                        if blob.len() != 32 {
                            return Err(FromSqlError::InvalidBlobSize {
                                expected_size: 32,
                                blob_size: blob.len(),
                            });
                        }
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(blob);
                        Ok(Self(arr))
                    }
                    _ => Err(FromSqlError::InvalidType),
                }
            }
        }
    };
}

// ── FileId ──────────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic file identifier: blake3(project_relative_path).
    FileId
);

impl FileId {
    /// Generate a FileId from a project-relative file path.
    pub fn generate(path: &str) -> Self {
        Self::from_hash(path.as_bytes())
    }
}

// ── SymbolId ────────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic symbol identifier: blake3(file_id + language + symbol_path + kind + discriminator).
    SymbolId
);

impl SymbolId {
    /// Generate a SymbolId from its constituent parts.
    ///
    /// - `file_id`: parent file's ID bytes
    /// - `language`: language name (e.g., "typescript")
    /// - `symbol_path`: dot-separated path (e.g., "App.UserService.login")
    /// - `kind`: symbol kind name (e.g., "method")
    /// - `discriminator`: optional signature or overload discriminator
    pub fn generate(
        file_id: &FileId,
        language: &str,
        symbol_path: &str,
        kind: &str,
        discriminator: Option<&str>,
    ) -> Self {
        let mut parts: Vec<&[u8]> = vec![
            file_id.as_bytes(),
            language.as_bytes(),
            symbol_path.as_bytes(),
            kind.as_bytes(),
        ];
        if let Some(disc) = discriminator {
            parts.push(disc.as_bytes());
        }
        Self::from_parts(&parts)
    }
}

// ── ScopeId ─────────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic scope identifier: blake3(file_id + parent_scope_id + scope_kind + start_byte).
    ScopeId
);

impl ScopeId {
    /// Generate a ScopeId from its constituent parts.
    ///
    /// - `file_id`: parent file's ID bytes
    /// - `parent`: optional parent scope ID bytes
    /// - `kind`: scope kind name (e.g., "block", "function")
    /// - `start_byte`: start byte offset of the scope in the file
    pub fn generate(
        file_id: &FileId,
        parent: Option<&ScopeId>,
        kind: &str,
        start_byte: u32,
    ) -> Self {
        let start_bytes = start_byte.to_le_bytes();
        let mut parts: Vec<&[u8]> = vec![file_id.as_bytes(), kind.as_bytes(), &start_bytes];
        if let Some(p) = parent {
            parts.insert(1, p.as_bytes());
        }
        Self::from_parts(&parts)
    }
}

// ── ReferenceId ─────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic reference identifier: blake3(file_id + source_symbol + kind + byte_range + text).
    ///
    /// `ReferenceKind` is included in the hash to prevent semantic collisions:
    /// e.g., `obj.method()` produces both `Call` and `FieldAccess` references
    /// at the same byte range — they must have distinct IDs.
    ReferenceId
);

impl ReferenceId {
    /// Generate a ReferenceId from its constituent parts.
    ///
    /// - `file_id`: parent file's ID bytes
    /// - `source_symbol`: optional source symbol ID bytes
    /// - `start_byte`, `end_byte`: byte range of the reference
    /// - `text`: the reference text (e.g., identifier name)
    /// - `kind`: the semantic kind of this reference (MUST be included to prevent collision)
    pub fn generate(
        file_id: &FileId,
        source_symbol: Option<&SymbolId>,
        start_byte: u32,
        end_byte: u32,
        text: &str,
        kind: crate::types::ReferenceKind,
    ) -> Self {
        let sb = start_byte.to_le_bytes();
        let eb = end_byte.to_le_bytes();
        let mut parts: Vec<&[u8]> = vec![
            file_id.as_bytes(),
            kind.as_str().as_bytes(),
            &sb,
            &eb,
            text.as_bytes(),
        ];
        // source_symbol inserted after file_id, before kind
        if let Some(src) = source_symbol {
            parts.insert(1, src.as_bytes());
        }
        Self::from_parts(&parts)
    }
}

// ── EdgeId ──────────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic edge identifier: blake3(source + target + kind + ref_id/provenance).
    EdgeId
);

impl EdgeId {
    /// Generate an EdgeId from its constituent parts.
    ///
    /// - `source`: source symbol ID
    /// - `target`: target symbol ID
    /// - `kind`: edge kind name
    /// - `ref_id`: optional reference ID that produced this edge
    /// - `provenance`: provenance name
    pub fn generate(
        source: &SymbolId,
        target: &SymbolId,
        kind: &str,
        ref_id: Option<&ReferenceId>,
        provenance: &str,
    ) -> Self {
        let mut parts: Vec<&[u8]> = vec![
            source.as_bytes(),
            target.as_bytes(),
            kind.as_bytes(),
            provenance.as_bytes(),
        ];
        if let Some(rid) = ref_id {
            parts.insert(3, rid.as_bytes());
        }
        Self::from_parts(&parts)
    }
}

// ── CallsiteId ──────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic callsite identifier: blake3(ref_id + caller + start_byte).
    CallsiteId
);

impl CallsiteId {
    /// Generate a CallsiteId from its constituent parts.
    ///
    /// - `ref_id`: the reference this callsite is derived from
    /// - `caller`: optional caller symbol ID
    /// - `start_byte`: start byte offset of the call expression
    pub fn generate(ref_id: &ReferenceId, caller: Option<&SymbolId>, start_byte: u32) -> Self {
        let sb = start_byte.to_le_bytes();
        let mut parts: Vec<&[u8]> = vec![ref_id.as_bytes(), &sb];
        if let Some(c) = caller {
            parts.insert(1, c.as_bytes());
        }
        Self::from_parts(&parts)
    }
}

// ── ImportId ────────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic import identifier: blake3(file_id + kind + module + imported_name + start_byte).
    ImportId
);

impl ImportId {
    /// Generate an ImportId from its constituent parts.
    ///
    /// - `file_id`: parent file's ID bytes
    /// - `kind`: import kind name (e.g., "import", "include")
    /// - `module`: the module/path being imported
    /// - `imported_name`: optional specific name being imported
    /// - `start_byte`: start byte offset of the import statement
    pub fn generate(
        file_id: &FileId,
        kind: &str,
        module: &str,
        imported_name: Option<&str>,
        start_byte: u32,
    ) -> Self {
        let sb = start_byte.to_le_bytes();
        let mut parts: Vec<&[u8]> = vec![
            file_id.as_bytes(),
            kind.as_bytes(),
            module.as_bytes(),
            &sb,
        ];
        if let Some(name) = imported_name {
            parts.insert(3, name.as_bytes());
        }
        Self::from_parts(&parts)
    }
}

// ── BindingId ────────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic binding identifier: blake3(file_id + scope_id + kind + name + start_byte).
    ///
    /// Represents a lexical binding definition: parameter, local variable,
    /// import alias, catch variable, etc.
    BindingId
);

impl BindingId {
    /// Generate a BindingId from its constituent parts.
    ///
    /// - `file_id`: parent file's ID bytes
    /// - `scope_id`: scope containing this binding
    /// - `kind`: binding kind name (e.g., "parameter", "local")
    /// - `name`: the binding name (e.g., "req", "name")
    /// - `start_byte`: start byte offset of the binding declaration
    pub fn generate(
        file_id: &FileId,
        scope_id: &ScopeId,
        kind: &str,
        name: &str,
        start_byte: u32,
    ) -> Self {
        let sb = start_byte.to_le_bytes();
        let parts: Vec<&[u8]> = vec![
            file_id.as_bytes(),
            scope_id.as_bytes(),
            kind.as_bytes(),
            name.as_bytes(),
            &sb,
        ];
        Self::from_parts(&parts)
    }
}

// ── BindingUseId ─────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic binding-use identifier: blake3(file_id + binding_id? + reference_id? + name + start_byte).
    ///
    /// Represents a usage site of a lexical binding.
    BindingUseId
);

impl BindingUseId {
    /// Generate a BindingUseId from its constituent parts.
    ///
    /// - `file_id`: parent file's ID bytes
    /// - `binding_id`: optional binding being used (None if unresolved)
    /// - `reference_id`: optional reference associated with this use
    /// - `name`: the identifier name at the use site
    /// - `start_byte`: start byte offset of the use
    pub fn generate(
        file_id: &FileId,
        binding_id: Option<&BindingId>,
        reference_id: Option<&ReferenceId>,
        name: &str,
        start_byte: u32,
    ) -> Self {
        let sb = start_byte.to_le_bytes();
        let mut parts: Vec<&[u8]> = vec![
            file_id.as_bytes(),
            name.as_bytes(),
            &sb,
        ];
        // binding_id inserted at position 1 (after file_id)
        if let Some(bid) = binding_id {
            parts.insert(1, bid.as_bytes());
        }
        // reference_id inserted after binding_id (or at position 1 if no binding)
        if let Some(rid) = reference_id {
            let insert_pos = if binding_id.is_some() { 2 } else { 1 };
            parts.insert(insert_pos, rid.as_bytes());
        }
        Self::from_parts(&parts)
    }
}

// ── DataNodeId ───────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic data-node identifier: blake3(file_id + function_id? + kind + name? + access_path? + start_byte).
    ///
    /// A DataNode represents a data entity in the dataflow graph:
    /// parameter, local variable, field, return value, call argument, etc.
    /// DataNodeId → DataNodeId edges form the dataflow graph (NOT SymbolId).
    DataNodeId
);

impl DataNodeId {
    /// Generate a DataNodeId from its constituent parts.
    ///
    /// - `file_id`: parent file's ID bytes
    /// - `function_id`: optional function symbol ID (None for file-level nodes)
    /// - `kind`: data node kind name (e.g., "parameter", "local", "call_arg")
    /// - `name`: optional name (e.g., "req", "name")
    /// - `access_path`: optional access path (e.g., "req.body.name")
    /// - `start_byte`: start byte offset of the data entity
    pub fn generate(
        file_id: &FileId,
        function_id: Option<&SymbolId>,
        kind: &str,
        name: Option<&str>,
        access_path: Option<&str>,
        start_byte: u32,
    ) -> Self {
        let sb = start_byte.to_le_bytes();
        let mut parts: Vec<&[u8]> = vec![
            file_id.as_bytes(),
            kind.as_bytes(),
            &sb,
        ];
        // function_id inserted at position 1 (after file_id)
        if let Some(fid) = function_id {
            parts.insert(1, fid.as_bytes());
        }
        // name inserted after kind
        if let Some(n) = name {
            parts.push(n.as_bytes());
        }
        // access_path appended last
        if let Some(ap) = access_path {
            parts.push(ap.as_bytes());
        }
        Self::from_parts(&parts)
    }
}

// ── DataFlowEdgeId ───────────────────────────────────────────────────────────

define_id!(
    /// Deterministic dataflow-edge identifier: blake3(source_node + target_node + kind).
    ///
    /// Represents a data flow between two DataNodes.
    DataFlowEdgeId
);

impl DataFlowEdgeId {
    /// Generate a DataFlowEdgeId from its constituent parts.
    ///
    /// - `source`: source DataNodeId
    /// - `target`: target DataNodeId
    /// - `kind`: dataflow kind name (e.g., "assign", "field_load", "arg_to_param")
    pub fn generate(
        source: &DataNodeId,
        target: &DataNodeId,
        kind: &str,
    ) -> Self {
        let parts: Vec<&[u8]> = vec![
            source.as_bytes(),
            target.as_bytes(),
            kind.as_bytes(),
        ];
        Self::from_parts(&parts)
    }
}

// ── CfgNodeId ────────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic CFG-node identifier: blake3(function_id + kind + start_byte).
    ///
    /// Represents a control-flow graph node within a function.
    CfgNodeId
);

impl CfgNodeId {
    /// Generate a CfgNodeId from its constituent parts.
    ///
    /// - `function_id`: the function symbol this node belongs to
    /// - `kind`: node kind name (e.g., "entry", "statement", "branch")
    /// - `start_byte`: start byte offset of the corresponding AST node
    pub fn generate(function_id: &SymbolId, kind: &str, start_byte: u32) -> Self {
        let sb = start_byte.to_le_bytes();
        let parts: Vec<&[u8]> = vec![
            function_id.as_bytes(),
            kind.as_bytes(),
            &sb,
        ];
        Self::from_parts(&parts)
    }
}

// ── CfgEdgeId ────────────────────────────────────────────────────────────────

define_id!(
    /// Deterministic CFG-edge identifier: blake3(source_node + target_node + kind).
    ///
    /// Represents a control-flow edge between two CFG nodes.
    CfgEdgeId
);

impl CfgEdgeId {
    /// Generate a CfgEdgeId from its constituent parts.
    ///
    /// - `source`: source CfgNodeId
    /// - `target`: target CfgNodeId
    /// - `kind`: edge kind name (e.g., "normal", "true_branch")
    pub fn generate(source: &CfgNodeId, target: &CfgNodeId, kind: &str) -> Self {
        let parts: Vec<&[u8]> = vec![
            source.as_bytes(),
            target.as_bytes(),
            kind.as_bytes(),
        ];
        Self::from_parts(&parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_id_deterministic() {
        let id1 = FileId::generate("src/main.ts");
        let id2 = FileId::generate("src/main.ts");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_file_id_different_paths() {
        let id1 = FileId::generate("src/main.ts");
        let id2 = FileId::generate("src/app.ts");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_symbol_id_deterministic() {
        let file_id = FileId::generate("src/main.ts");
        let id1 = SymbolId::generate(&file_id, "typescript", "App.run", "method", None);
        let id2 = SymbolId::generate(&file_id, "typescript", "App.run", "method", None);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_symbol_id_different_kinds() {
        let file_id = FileId::generate("src/main.ts");
        let id1 = SymbolId::generate(&file_id, "typescript", "App.run", "method", None);
        let id2 = SymbolId::generate(&file_id, "typescript", "App.run", "function", None);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_symbol_id_discriminator() {
        let file_id = FileId::generate("src/main.ts");
        let id1 = SymbolId::generate(&file_id, "typescript", "App.run", "method", None);
        let id2 = SymbolId::generate(
            &file_id,
            "typescript",
            "App.run",
            "method",
            Some("(int)"),
        );
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_reference_id_deterministic() {
        use crate::types::ReferenceKind;
        let file_id = FileId::generate("src/main.ts");
        let id1 = ReferenceId::generate(&file_id, None, 100, 108, "console", ReferenceKind::Usage);
        let id2 = ReferenceId::generate(&file_id, None, 100, 108, "console", ReferenceKind::Usage);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_reference_id_kind_prevents_collision() {
        use crate::types::ReferenceKind;
        let file_id = FileId::generate("src/main.ts");
        let call_id = ReferenceId::generate(&file_id, None, 100, 108, "method", ReferenceKind::Call);
        let field_id = ReferenceId::generate(&file_id, None, 100, 108, "method", ReferenceKind::FieldAccess);
        // Same range + text, different kind → different IDs (fixes obj.method() collision)
        assert_ne!(call_id, field_id);
    }

    #[test]
    fn test_edge_id_deterministic() {
        let file_id = FileId::generate("src/main.ts");
        let src = SymbolId::generate(&file_id, "typescript", "A.foo", "method", None);
        let tgt = SymbolId::generate(&file_id, "typescript", "B.bar", "method", None);
        let id1 = EdgeId::generate(&src, &tgt, "calls", None, "tree_sitter");
        let id2 = EdgeId::generate(&src, &tgt, "calls", None, "tree_sitter");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_id_display_short() {
        let id = FileId::generate("src/main.ts");
        let displayed = id.to_string();
        // Format: "first8..last8" = 19 chars
        assert!(displayed.contains(".."));
    }

    #[test]
    fn test_id_hex_roundtrip() {
        let id = FileId::generate("src/main.ts");
        let hex = id.to_hex();
        assert_eq!(hex.len(), 64);
        let parsed: FileId = hex.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_id_from_str_invalid() {
        assert!("invalid".parse::<FileId>().is_err());
        assert!("zzzz".parse::<FileId>().is_err());
        assert!("a".parse::<SymbolId>().is_err());
    }

    #[test]
    fn test_scope_id_with_parent() {
        let file_id = FileId::generate("src/main.ts");
        let parent = ScopeId::generate(&file_id, None, "module", 0);
        let child = ScopeId::generate(&file_id, Some(&parent), "function", 42);
        let orphan = ScopeId::generate(&file_id, None, "function", 42);
        assert_ne!(child, orphan);
    }

    #[test]
    fn test_id_serde_roundtrip() {
        let file_id = FileId::generate("src/main.ts");
        let json = serde_json::to_string(&file_id).unwrap();
        let parsed: FileId = serde_json::from_str(&json).unwrap();
        assert_eq!(file_id, parsed);
    }

    #[test]
    fn test_id_rusqlite_blob_roundtrip() {
        let id = FileId::generate("src/main.ts");
        let sql_output = id.to_sql().unwrap();
        // Verify it produces a BLOB (Borrowed or Owned)
        match sql_output {
            rusqlite::types::ToSqlOutput::Borrowed(rusqlite::types::ValueRef::Blob(ref blob)) => {
                assert_eq!(blob.len(), 32);
            }
            rusqlite::types::ToSqlOutput::Owned(rusqlite::types::Value::Blob(ref blob)) => {
                assert_eq!(blob.len(), 32);
            }
            other => panic!("Expected BLOB output, got {:?}", other),
        }
    }

    #[test]
    fn test_import_id_with_name() {
        let file_id = FileId::generate("src/main.ts");
        let id1 = ImportId::generate(&file_id, "from_import", "react", Some("useState"), 10);
        let id2 = ImportId::generate(&file_id, "from_import", "react", None, 10);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_callsite_id_deterministic() {
        use crate::types::ReferenceKind;
        let file_id = FileId::generate("src/main.ts");
        let ref_id = ReferenceId::generate(&file_id, None, 100, 108, "foo", ReferenceKind::Call);
        let caller = SymbolId::generate(&file_id, "typescript", "App.run", "method", None);
        let id1 = CallsiteId::generate(&ref_id, Some(&caller), 100);
        let id2 = CallsiteId::generate(&ref_id, Some(&caller), 100);
        assert_eq!(id1, id2);
    }

    // -- BindingId --------------------------------------------------------

    #[test]
    fn test_binding_id_deterministic() {
        let file_id = FileId::generate("src/main.ts");
        let scope_id = ScopeId::generate(&file_id, None, "function", 42);
        let id1 = BindingId::generate(&file_id, &scope_id, "parameter", "req", 50);
        let id2 = BindingId::generate(&file_id, &scope_id, "parameter", "req", 50);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_binding_id_different_scope() {
        let file_id = FileId::generate("src/main.ts");
        let s1 = ScopeId::generate(&file_id, None, "function", 42);
        let s2 = ScopeId::generate(&file_id, None, "function", 100);
        let id1 = BindingId::generate(&file_id, &s1, "parameter", "req", 50);
        let id2 = BindingId::generate(&file_id, &s2, "parameter", "req", 50);
        assert_ne!(id1, id2);
    }

    // -- BindingUseId -----------------------------------------------------

    #[test]
    fn test_binding_use_id_deterministic() {
        let file_id = FileId::generate("src/main.ts");
        let scope_id = ScopeId::generate(&file_id, None, "function", 42);
        let binding_id = BindingId::generate(&file_id, &scope_id, "parameter", "req", 50);
        let id1 = BindingUseId::generate(&file_id, Some(&binding_id), None, "req", 120);
        let id2 = BindingUseId::generate(&file_id, Some(&binding_id), None, "req", 120);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_binding_use_id_no_binding() {
        let file_id = FileId::generate("src/main.ts");
        let id1 = BindingUseId::generate(&file_id, None, None, "x", 80);
        let id2 = BindingUseId::generate(&file_id, None, None, "x", 81);
        assert_ne!(id1, id2);
    }

    // -- DataNodeId -------------------------------------------------------

    #[test]
    fn test_data_node_id_deterministic() {
        let file_id = FileId::generate("src/main.ts");
        let func_id = SymbolId::generate(&file_id, "typescript", "handler", "function", None);
        let id1 = DataNodeId::generate(
            &file_id, Some(&func_id), "parameter", Some("req"), Some("req"), 50,
        );
        let id2 = DataNodeId::generate(
            &file_id, Some(&func_id), "parameter", Some("req"), Some("req"), 50,
        );
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_data_node_id_different_kind() {
        let file_id = FileId::generate("src/main.ts");
        let func_id = SymbolId::generate(&file_id, "typescript", "handler", "function", None);
        let id1 = DataNodeId::generate(
            &file_id, Some(&func_id), "parameter", Some("req"), None, 50,
        );
        let id2 = DataNodeId::generate(
            &file_id, Some(&func_id), "local", Some("req"), None, 50,
        );
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_data_node_id_different_access_path() {
        let file_id = FileId::generate("src/main.ts");
        let func_id = SymbolId::generate(&file_id, "typescript", "handler", "function", None);
        let id1 = DataNodeId::generate(
            &file_id, Some(&func_id), "field", Some("body"), Some("req.body"), 80,
        );
        let id2 = DataNodeId::generate(
            &file_id, Some(&func_id), "field", Some("body"), Some("req.body.name"), 80,
        );
        assert_ne!(id1, id2);
    }

    // -- DataFlowEdgeId ---------------------------------------------------

    #[test]
    fn test_dataflow_edge_id_deterministic() {
        let file_id = FileId::generate("src/main.ts");
        let func_id = SymbolId::generate(&file_id, "typescript", "handler", "function", None);
        let src = DataNodeId::generate(&file_id, Some(&func_id), "parameter", Some("req"), None, 50);
        let tgt = DataNodeId::generate(&file_id, Some(&func_id), "local", Some("name"), None, 100);
        let id1 = DataFlowEdgeId::generate(&src, &tgt, "assign");
        let id2 = DataFlowEdgeId::generate(&src, &tgt, "assign");
        assert_eq!(id1, id2);

        let id3 = DataFlowEdgeId::generate(&tgt, &src, "assign"); // reversed
        assert_ne!(id1, id3);
    }

    // -- CfgNodeId -------------------------------------------------------

    #[test]
    fn test_cfg_node_id_deterministic() {
        let file_id = FileId::generate("src/main.ts");
        let func_id = SymbolId::generate(&file_id, "typescript", "handler", "function", None);
        let id1 = CfgNodeId::generate(&func_id, "statement", 100);
        let id2 = CfgNodeId::generate(&func_id, "statement", 100);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_cfg_node_id_different_kind() {
        let file_id = FileId::generate("src/main.ts");
        let func_id = SymbolId::generate(&file_id, "typescript", "handler", "function", None);
        let id1 = CfgNodeId::generate(&func_id, "entry", 50);
        let id2 = CfgNodeId::generate(&func_id, "exit", 50);
        assert_ne!(id1, id2);
    }

    // -- CfgEdgeId --------------------------------------------------------

    #[test]
    fn test_cfg_edge_id_deterministic() {
        let file_id = FileId::generate("src/main.ts");
        let func_id = SymbolId::generate(&file_id, "typescript", "handler", "function", None);
        let src = CfgNodeId::generate(&func_id, "entry", 10);
        let tgt = CfgNodeId::generate(&func_id, "statement", 100);
        let id1 = CfgEdgeId::generate(&src, &tgt, "normal");
        let id2 = CfgEdgeId::generate(&src, &tgt, "normal");
        assert_eq!(id1, id2);
    }
}
