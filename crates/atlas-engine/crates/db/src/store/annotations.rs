//! Function-pointer dispatch annotation CRUD.

use rusqlite::params;
use types::*;

use super::Store;

impl Store {
    // ── Read ────────────────────────────────────────────────────────────────

    /// List all function-pointer dispatch annotations.
    pub fn get_all_fp_annotations(&self) -> anyhow::Result<Vec<FpAnnotation>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT annotation_id, source_symbol, field_name, target_symbol, confidence
             FROM function_pointer_annotations ORDER BY annotation_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FpAnnotation {
                annotation_id: row.get(0)?,
                source_symbol: row.get(1)?,
                field_name: row.get(2)?,
                target_symbol: row.get(3)?,
                confidence: Confidence::new(row.get::<_, f64>(4)? as f32),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find a single annotation by its ID.
    pub fn get_fp_annotation(&self, annotation_id: &str) -> anyhow::Result<Option<FpAnnotation>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT annotation_id, source_symbol, field_name, target_symbol, confidence
             FROM function_pointer_annotations WHERE annotation_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![annotation_id], |row| {
            Ok(FpAnnotation {
                annotation_id: row.get(0)?,
                source_symbol: row.get(1)?,
                field_name: row.get(2)?,
                target_symbol: row.get(3)?,
                confidence: Confidence::new(row.get::<_, f64>(4)? as f32),
            })
        })?;
        match rows.next() {
            Some(Ok(a)) => Ok(Some(a)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Find an annotation by source symbol + field name.
    pub fn find_fp_annotation_by_field(
        &self,
        source_symbol: &SymbolId,
        field_name: &str,
    ) -> anyhow::Result<Option<FpAnnotation>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare(
            "SELECT annotation_id, source_symbol, field_name, target_symbol, confidence
             FROM function_pointer_annotations
             WHERE source_symbol = ?1 AND field_name = ?2",
        )?;
        let mut rows = stmt.query_map(params![source_symbol, field_name], |row| {
            Ok(FpAnnotation {
                annotation_id: row.get(0)?,
                source_symbol: row.get(1)?,
                field_name: row.get(2)?,
                target_symbol: row.get(3)?,
                confidence: Confidence::new(row.get::<_, f64>(4)? as f32),
            })
        })?;
        match rows.next() {
            Some(Ok(a)) => Ok(Some(a)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    // ── Write ───────────────────────────────────────────────────────────────

    /// Insert or replace an annotation. Idempotent — calling with the same
    /// source + field_name overwrites the previous target.
    pub fn upsert_fp_annotation(&self, annotation: &FpAnnotation) -> anyhow::Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO function_pointer_annotations
             (annotation_id, source_symbol, field_name, target_symbol, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                annotation.annotation_id,
                annotation.source_symbol,
                annotation.field_name,
                annotation.target_symbol,
                annotation.confidence.as_f32() as f64,
            ],
        )?;
        Ok(())
    }

    // ── Delete ──────────────────────────────────────────────────────────────

    /// Delete an annotation by its ID.
    pub fn delete_fp_annotation(&self, annotation_id: &str) -> anyhow::Result<bool> {
        let conn = self.lock();
        let count = conn.execute(
            "DELETE FROM function_pointer_annotations WHERE annotation_id = ?1",
            params![annotation_id],
        )?;
        Ok(count > 0)
    }

    /// Delete an annotation by source symbol + field name.
    pub fn delete_fp_annotation_by_field(
        &self,
        source_symbol: &SymbolId,
        field_name: &str,
    ) -> anyhow::Result<bool> {
        let conn = self.lock();
        let count = conn.execute(
            "DELETE FROM function_pointer_annotations WHERE source_symbol = ?1 AND field_name = ?2",
            params![source_symbol, field_name],
        )?;
        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::ids::{FileId, SymbolId};
    use types::{FileFacts, FileInfo, Language, ParseStatus, SymbolDef, SymbolKind, TextRange, FpAnnotation};

    fn setup_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
    }

    fn insert_symbol(store: &Store, file_id: FileId, name: &str, qname: &str, kind: SymbolKind) -> SymbolId {
        let range = TextRange {
            start_byte: 0, end_byte: 10,
            start_line: 1, start_column: 1,
            end_line: 1, end_column: 11,
        };
        let id = SymbolId::generate(&file_id, "c", qname, kind.as_str(), None);
        let sym = SymbolDef {
            id,
            kind,
            name: name.to_string(),
            qualified_name: qname.to_string(),
            symbol_path: vec![name.to_string()],
            file_id,
            language: Language::C,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        };
        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: format!("src/{}.c", name),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            },
            symbols: vec![sym],
            ..Default::default()
        };
        store.insert_file_facts(&facts).unwrap();
        id
    }

    fn test_annotation_id(source: &SymbolId, field_name: &str) -> String {
        let hex = blake3::hash(source.as_bytes()).to_hex();
        format!("fpa:{}:{}", &hex[..16], field_name)
    }

    #[test]
    fn test_upsert_and_get() {
        let store = setup_store();
        let file_a = FileId::generate("src/field.c");
        let file_b = FileId::generate("src/target.c");

        let source = insert_symbol(&store, file_a, "do_it", "Curl_handler.do_it", SymbolKind::Field);
        let target = insert_symbol(&store, file_b, "Curl_http", "Curl_http", SymbolKind::Function);

        let ann = FpAnnotation {
            annotation_id: test_annotation_id(&source, "do_it"),
            source_symbol: source,
            field_name: "do_it".into(),
            target_symbol: target,
            confidence: Confidence::new(1.0),
        };

        store.upsert_fp_annotation(&ann).unwrap();

        let all = store.get_all_fp_annotations().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].field_name, "do_it");

        let found = store.find_fp_annotation_by_field(&source, "do_it").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().target_symbol, target);
    }

    #[test]
    fn test_upsert_overwrites() {
        let store = setup_store();
        let fa = FileId::generate("src/f.c");
        let fb = FileId::generate("src/impl_a.c");
        let fc = FileId::generate("src/impl_b.c");

        let source = insert_symbol(&store, fa, "handler", "Struct.handler", SymbolKind::Field);
        let target1 = insert_symbol(&store, fb, "impl_a", "impl_a", SymbolKind::Function);
        let target2 = insert_symbol(&store, fc, "impl_b", "impl_b", SymbolKind::Function);

        let ann1 = FpAnnotation {
            annotation_id: test_annotation_id(&source, "handler"),
            source_symbol: source,
            field_name: "handler".into(),
            target_symbol: target1,
            confidence: Confidence::new(1.0),
        };
        store.upsert_fp_annotation(&ann1).unwrap();

        let ann2 = FpAnnotation {
            annotation_id: test_annotation_id(&source, "handler"),
            source_symbol: source,
            field_name: "handler".into(),
            target_symbol: target2,
            confidence: Confidence::new(0.8),
        };
        store.upsert_fp_annotation(&ann2).unwrap();

        let all = store.get_all_fp_annotations().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].target_symbol, target2);
        assert!((all[0].confidence.as_f32() - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn test_delete_by_id() {
        let store = setup_store();
        let fa = FileId::generate("src/cb.c");
        let fb = FileId::generate("src/tgt.c");

        let source = insert_symbol(&store, fa, "cb", "Struct.cb", SymbolKind::Field);
        let target = insert_symbol(&store, fb, "target_fn", "target_fn", SymbolKind::Function);
        let aid = test_annotation_id(&source, "cb");

        let ann = FpAnnotation {
            annotation_id: aid.clone(),
            source_symbol: source,
            field_name: "cb".into(),
            target_symbol: target,
            confidence: Confidence::new(1.0),
        };
        store.upsert_fp_annotation(&ann).unwrap();

        assert!(store.delete_fp_annotation(&aid).unwrap());
        assert!(store.get_all_fp_annotations().unwrap().is_empty());
    }

    #[test]
    fn test_delete_by_field() {
        let store = setup_store();
        let fa = FileId::generate("src/ff.c");
        let fb = FileId::generate("src/gg.c");

        let source = insert_symbol(&store, fa, "f", "S.f", SymbolKind::Field);
        let target = insert_symbol(&store, fb, "g", "g", SymbolKind::Function);
        let aid = test_annotation_id(&source, "f");

        let ann = FpAnnotation {
            annotation_id: aid,
            source_symbol: source,
            field_name: "f".into(),
            target_symbol: target,
            confidence: Confidence::new(1.0),
        };
        store.upsert_fp_annotation(&ann).unwrap();

        assert!(store.delete_fp_annotation_by_field(&source, "f").unwrap());
        assert!(store.get_all_fp_annotations().unwrap().is_empty());
    }

    #[test]
    fn test_delete_nonexistent() {
        let store = setup_store();
        assert!(!store.delete_fp_annotation("nonexistent").unwrap());
    }
}
