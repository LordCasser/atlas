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
            fn from_hash(input: &[u8]) -> Self {
                Self(blake3::hash(input).into())
            }

            /// Chain multiple byte slices with null separators and hash.
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
    /// Deterministic reference identifier: blake3(file_id + source_symbol + byte_range + text).
    ReferenceId
);

impl ReferenceId {
    /// Generate a ReferenceId from its constituent parts.
    ///
    /// - `file_id`: parent file's ID bytes
    /// - `source_symbol`: optional source symbol ID bytes
    /// - `start_byte`, `end_byte`: byte range of the reference
    /// - `text`: the reference text (e.g., identifier name)
    pub fn generate(
        file_id: &FileId,
        source_symbol: Option<&SymbolId>,
        start_byte: u32,
        end_byte: u32,
        text: &str,
    ) -> Self {
        let sb = start_byte.to_le_bytes();
        let eb = end_byte.to_le_bytes();
        let mut parts: Vec<&[u8]> = vec![file_id.as_bytes(), &sb, &eb, text.as_bytes()];
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
        let file_id = FileId::generate("src/main.ts");
        let id1 = ReferenceId::generate(&file_id, None, 100, 108, "console");
        let id2 = ReferenceId::generate(&file_id, None, 100, 108, "console");
        assert_eq!(id1, id2);
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
        let file_id = FileId::generate("src/main.ts");
        let ref_id = ReferenceId::generate(&file_id, None, 100, 108, "foo");
        let caller = SymbolId::generate(&file_id, "typescript", "App.run", "method", None);
        let id1 = CallsiteId::generate(&ref_id, Some(&caller), 100);
        let id2 = CallsiteId::generate(&ref_id, Some(&caller), 100);
        assert_eq!(id1, id2);
    }
}
