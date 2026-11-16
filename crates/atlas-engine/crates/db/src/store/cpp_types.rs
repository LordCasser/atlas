use std::collections::{HashMap, HashSet};

use rusqlite::OptionalExtension;
use types::{
    FileId, ReferenceId, TextRange,
    cpp::{
        CppFileTypes, CppLookupLimit, CppTypeLookupContext, CppTypeLookupFailure,
        CppTypeLookupFailureKind, CppTypeLookupSharing,
    },
};

use super::Store;

/// File-local recorded regions needed to project calls, without deserializing
/// unrelated receiver, argument, macro or declaration facts.
#[derive(Debug, Default)]
pub struct CppCallRegions {
    pub lambda_captures: Vec<types::cpp::CppLambdaCapture>,
    pub allocation_sites: Vec<types::cpp::CppAllocationSite>,
    pub lookup_limits: Vec<types::cpp::CppLookupLimit>,
    pub unverified_callable_scopes: Vec<types::SymbolId>,
}

/// Exact selected failures and global grouping evidence for each returned reference.
#[derive(Debug, Default)]
pub struct CppTypeLookupSelection {
    pub failures: HashMap<ReferenceId, CppTypeLookupFailure>,
    pub sharing: HashMap<ReferenceId, CppTypeLookupSharing>,
    pub work: CppTypeLookupReadWork,
}

/// Logical work, not SQLite page reads or a byte/memory bound. SQLite still
/// parses stored JSON to extract each global context; unrelated inventories are
/// not deserialized into Rust values.
#[derive(Debug, Default)]
pub struct CppTypeLookupReadWork {
    pub unique_references_requested: usize,
    pub failures_decoded: usize,
    pub global_contexts_examined: usize,
    pub matching_inventories_examined: usize,
    pub declaration_lists_decoded: usize,
}

#[derive(Default)]
struct FailureDecoder {
    lists: HashMap<String, Vec<(FileId, TextRange)>>,
    memo_bytes: usize,
    lists_decoded: usize,
}

impl FailureDecoder {
    fn declarations(&mut self, related: String) -> anyhow::Result<Vec<(FileId, TextRange)>> {
        if let Some(list) = self.lists.get(&related) {
            return Ok(list.clone());
        }
        let list: Vec<(FileId, TextRange)> = serde_json::from_str(&related)?;
        self.lists_decoded += 1;
        // Bound cached payload, not map overhead or the returned inventories.
        const MEMO_BYTES: usize = 8 * 1024 * 1024;
        let bytes = related.capacity().saturating_add(
            list.capacity()
                .saturating_mul(std::mem::size_of::<(FileId, TextRange)>()),
        );
        if bytes <= MEMO_BYTES {
            if self.memo_bytes + bytes > MEMO_BYTES {
                self.lists.clear();
                self.memo_bytes = 0;
            }
            self.memo_bytes += bytes;
            self.lists.insert(related, list.clone());
        }
        Ok(list)
    }

    fn failure(&mut self, json: &str, related: String) -> anyhow::Result<CppTypeLookupFailure> {
        let mut failure: CppTypeLookupFailure = serde_json::from_str(json)?;
        failure.related_declarations = self.declarations(related)?;
        Ok(failure)
    }
}

impl Store {
    pub fn cpp_allocation_site_count(&self) -> anyhow::Result<usize> {
        let count: i64 = self.lock_read().query_row(
            "SELECT coalesce(sum(json_array_length(facts_json, '$.allocation_sites')), 0) FROM cpp_type_facts",
            [],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count)?)
    }

    /// Source regions for unmodeled allocation/initialization invocations.
    pub fn cpp_allocation_sites(
        &self,
        canceled: &dyn Fn() -> bool,
    ) -> anyhow::Result<Vec<(FileId, types::cpp::CppAllocationSite)>> {
        anyhow::ensure!(!canceled(), "allocation region read canceled");
        let conn = self.lock_read();
        let mut statement = conn.prepare("SELECT file_id, item.value FROM cpp_type_facts, json_each(facts_json, '$.allocation_sites') AS item")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, FileId>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            anyhow::ensure!(!canceled(), "allocation region read canceled");
            let (file, json) = row?;
            Ok((file, serde_json::from_str(&json)?))
        })
        .collect()
    }

    /// Anonymous body locations and containment for query continuation. These
    /// facts do not establish invocation and do not load all receiver types.
    pub fn cpp_lambda_captures(
        &self,
    ) -> anyhow::Result<Vec<(FileId, types::cpp::CppLambdaCapture)>> {
        let conn = self.lock_read();
        let mut statement = conn.prepare("SELECT file_id, item.value FROM cpp_type_facts, json_each(facts_json, '$.lambda_captures') AS item")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, FileId>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (file, json) = row?;
            Ok((file, serde_json::from_str(&json)?))
        })
        .collect()
    }
    /// Precise failed type lookups from the project resolution pass. The
    /// enclosing index Artifact controls publication; query readers never write.
    pub fn cpp_type_lookup_failures(
        &self,
        canceled: &dyn Fn() -> bool,
    ) -> anyhow::Result<HashMap<types::ReferenceId, types::cpp::CppTypeLookupFailure>> {
        anyhow::ensure!(!canceled(), "type lookup diagnostic read canceled");
        let conn = self.lock_read();
        let mut statement = conn.prepare(
            "SELECT reference_id,
                    json_set(failure_json, '$.related_declarations', json('[]')),
                    json_extract(failure_json, '$.related_declarations')
             FROM \"references\"
             WHERE resolved_symbol_id IS NULL AND failure_json IS NOT NULL",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, types::ReferenceId>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut decoder = FailureDecoder::default();
        let mut failures = HashMap::new();
        for row in rows {
            anyhow::ensure!(!canceled(), "type lookup diagnostic read canceled");
            let (reference, json, related) = row?;
            failures.insert(reference, decoder.failure(&json, related)?);
        }
        anyhow::ensure!(!canceled(), "type lookup diagnostic read canceled");
        Ok(failures)
    }

    /// Read exact failures for deduplicated reference IDs, with grouping evidence
    /// from all unresolved failure records in this immutable published Artifact.
    /// Unknown, resolved and failure-free references are omitted. An empty request
    /// or a selection with no failures performs no global scan. Cancellation is
    /// checked at entry, between records and before returning (including empty).
    ///
    /// This does not select call owners or project source locations. A shared
    /// inventory is not sufficient to emit a region if its locations are absent.
    /// No failure outside the selection is decoded as a failure object; global
    /// context metadata and matching declaration inventories are still read.
    pub fn cpp_type_lookup_failures_for_references(
        &self,
        references: &[ReferenceId],
        canceled: &dyn Fn() -> bool,
    ) -> anyhow::Result<CppTypeLookupSelection> {
        anyhow::ensure!(!canceled(), "type lookup diagnostic read canceled");
        let ids: HashSet<_> = references.iter().copied().collect();
        let mut result = CppTypeLookupSelection::default();
        result.work.unique_references_requested = ids.len();
        let conn = self.lock_read();
        let mut selected = conn.prepare(
            "SELECT json_set(failure_json, '$.related_declarations', json('[]')),
                    json_extract(failure_json, '$.related_declarations')
             FROM \"references\" WHERE reference_id = ?1
             AND resolved_symbol_id IS NULL AND failure_json IS NOT NULL",
        )?;
        let mut decoder = FailureDecoder::default();
        for id in ids {
            anyhow::ensure!(!canceled(), "type lookup diagnostic read canceled");
            let row: Option<(String, String)> = selected
                .query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            if let Some((json, related)) = row {
                result.failures.insert(id, decoder.failure(&json, related)?);
                result.work.failures_decoded += 1;
            }
        }
        if !result.failures.is_empty() {
            let mut groups = HashMap::new();
            for failure in result.failures.values() {
                groups.entry(failure.context()).or_insert((
                    failure.related_declarations.as_slice(),
                    CppTypeLookupSharing::default(),
                ));
            }
            let mut contexts = conn.prepare(
                "SELECT reference_id,
                        json_extract(failure_json, '$.kind', '$.name', '$.scope', '$.file_id', '$.range')
                 FROM \"references\" WHERE resolved_symbol_id IS NULL AND failure_json IS NOT NULL",
            )?;
            let rows = contexts.query_map([], |row| {
                Ok((row.get::<_, ReferenceId>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut inventory = conn.prepare(
                "SELECT json_extract(failure_json, '$.related_declarations')
                 FROM \"references\" WHERE reference_id = ?1",
            )?;
            for row in rows {
                anyhow::ensure!(!canceled(), "type lookup diagnostic read canceled");
                let (id, json) = row?;
                let (kind, name, scope, file_id, range): (
                    CppTypeLookupFailureKind,
                    String,
                    String,
                    FileId,
                    TextRange,
                ) = serde_json::from_str(&json)?;
                result.work.global_contexts_examined += 1;
                let key = CppTypeLookupContext {
                    kind,
                    name,
                    scope,
                    file_id,
                    range,
                };
                if let Some((expected, sharing)) = groups.get_mut(&key) {
                    result.work.matching_inventories_examined += 1;
                    if let Some(failure) = result.failures.get(&id) {
                        sharing.observe(expected, &failure.related_declarations);
                    } else {
                        let json = inventory.query_row([id], |row| row.get::<_, String>(0))?;
                        let declarations = decoder.declarations(json)?;
                        sharing.observe(expected, &declarations);
                    }
                }
            }
            for (id, failure) in &result.failures {
                result.sharing.insert(*id, groups[&failure.context()].1);
            }
        }
        result.work.declaration_lists_decoded = decoder.lists_decoded;
        anyhow::ensure!(!canceled(), "type lookup diagnostic read canceled");
        Ok(result)
    }

    /// Replace the previous failed attempt, including clearing a formerly
    /// precise cause when the current attempt cannot establish it.
    pub fn batch_update_cpp_type_lookup_failures(
        &self,
        failures: &[(types::ReferenceId, Option<types::cpp::CppTypeLookupFailure>)],
    ) -> anyhow::Result<()> {
        if failures.is_empty() {
            return Ok(());
        }
        self.with_transaction(|tx| {
            let mut statement = tx.prepare(
                "UPDATE \"references\" SET failure_json = ?2
                 WHERE reference_id = ?1 AND resolved_symbol_id IS NULL
                   AND (failure_json IS NOT NULL OR ?2 IS NOT NULL)",
            )?;
            for (reference, failure) in failures {
                let json = failure.as_ref().map(serde_json::to_string).transpose()?;
                statement.execute(rusqlite::params![reference, json])?;
            }
            Ok(())
        })
    }

    /// Source-located local lookup limits for read-only query diagnostics.
    /// Avoid loading unrelated receiver and callable facts into the graph cache.
    pub fn cpp_local_lookup_limits(&self) -> anyhow::Result<HashMap<FileId, Vec<CppLookupLimit>>> {
        let conn = self.lock_read();
        let mut statement = conn.prepare("SELECT file_id, item.value FROM cpp_type_facts, json_each(facts_json, '$.lookup_limits') AS item WHERE json_extract(item.value, '$.block_range') IS NOT NULL")?;
        let values = statement.query_map([], |row| {
            Ok((row.get::<_, FileId>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut limits = HashMap::<_, Vec<_>>::new();
        for value in values {
            let (file_id, json) = value?;
            limits
                .entry(file_id)
                .or_default()
                .push(serde_json::from_str(&json)?);
        }
        Ok(limits)
    }

    /// Small public projection for concrete call-owner gaps, without loading
    /// every receiver/parameter fact into a graph-query cache.
    pub fn cpp_unverified_callable_scopes(&self) -> anyhow::Result<HashSet<types::SymbolId>> {
        let conn = self.lock_read();
        let mut statement = conn.prepare("SELECT item.value FROM cpp_type_facts, json_each(facts_json, '$.unverified_callable_scopes') AS item")?;
        let values = statement.query_map([], |row| row.get::<_, String>(0))?;
        values
            .map(|value| value?.parse::<types::SymbolId>())
            .collect()
    }
    pub fn cpp_call_regions_for_file(
        &self,
        file: &FileId,
    ) -> anyhow::Result<Option<CppCallRegions>> {
        let json: Option<String> = self.lock_read().query_row(
            "SELECT json_extract(facts_json, '$.lambda_captures', '$.allocation_sites',
                '$.lookup_limits', '$.unverified_callable_scopes') FROM cpp_type_facts WHERE file_id = ?1",
            [file], |row|row.get(0),
        ).optional()?;
        json.map(|json| {
            let (lambda_captures, allocation_sites, lookup_limits, unverified_callable_scopes) =
                serde_json::from_str(&json)?;
            Ok(CppCallRegions {
                lambda_captures,
                allocation_sites,
                lookup_limits,
                unverified_callable_scopes,
            })
        })
        .transpose()
    }

    pub fn cpp_types_for_file(&self, file_id: &FileId) -> anyhow::Result<Option<CppFileTypes>> {
        let json: Option<String> = self
            .lock_read()
            .query_row(
                "SELECT facts_json FROM cpp_type_facts WHERE file_id = ?1",
                [file_id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(Into::into)
    }

    /// Loaded once for a resolution session, never per candidate/call.
    pub fn all_cpp_types(&self) -> anyhow::Result<HashMap<FileId, CppFileTypes>> {
        let conn = self.lock_read();
        let mut stmt = conn.prepare("SELECT file_id, facts_json FROM cpp_type_facts")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, FileId>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (file, json) = row?;
            Ok((file, serde_json::from_str(&json)?))
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{
        FileFacts, FileInfo, Language, SymbolId, TextRange,
        cpp::{CppBaseClass, CppCallableDeclaration, CppDeclaredType, CppRecordType, CppValueType},
    };

    #[test]
    fn lookup_diagnostic_reads_preserve_repeated_lists_and_cancellation() {
        use types::cpp::{CppTypeLookupFailure, CppTypeLookupFailureKind};
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let file_id = FileId::generate("diagnostics.cpp");
        let mut facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "diagnostics.cpp".into(),
                language: Language::Cpp,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut expected = HashMap::new();
        for index in 1..=6u8 {
            let id = types::ReferenceId::from_bytes([index; 32]);
            let range = TextRange {
                start_byte: u32::from(index) * 10,
                end_byte: u32::from(index) * 10 + 4,
                ..Default::default()
            };
            facts.references.push(types::ReferenceUse {
                id,
                file_id,
                source_symbol: None,
                scope_id: None,
                kind: types::ReferenceKind::Call,
                text: "call".into(),
                name: "call".into(),
                receiver: None,
                arity: Some(0),
                range,
                binding_id: None,
                resolved: None,
            });
            let first = (
                FileId::generate("first.h"),
                TextRange {
                    start_byte: 2,
                    end_byte: 6,
                    ..Default::default()
                },
            );
            let second = (
                FileId::generate("second.h"),
                TextRange {
                    start_byte: 7,
                    end_byte: 11,
                    ..Default::default()
                },
            );
            expected.insert(
                id,
                CppTypeLookupFailure {
                    kind: CppTypeLookupFailureKind::NameLookupRestricted,
                    name: format!("call_{index}"),
                    scope: "scope".into(),
                    file_id,
                    range,
                    related_declarations: if index % 2 == 0 {
                        vec![first, second, first]
                    } else {
                        vec![second]
                    },
                },
            );
        }
        store.insert_file_facts(&facts).unwrap();
        let updates: Vec<_> = expected
            .iter()
            .map(|(id, failure)| (*id, Some(failure.clone())))
            .collect();
        store
            .batch_update_cpp_type_lookup_failures(&updates)
            .unwrap();
        assert_eq!(store.cpp_type_lookup_failures(&|| false).unwrap(), expected);
        assert!(store.cpp_type_lookup_failures(&|| true).is_err());
        let checks = std::cell::Cell::new(0);
        assert!(
            store
                .cpp_type_lookup_failures(&|| {
                    checks.set(checks.get() + 1);
                    checks.get() >= 4
                })
                .is_err()
        );
        assert_eq!(store.cpp_type_lookup_failures(&|| false).unwrap(), expected);
        let removed = facts.references[0].id;
        store
            .batch_update_cpp_type_lookup_failures(&[(removed, None)])
            .unwrap();
        expected.remove(&removed);
        assert_eq!(store.cpp_type_lookup_failures(&|| false).unwrap(), expected);
        store.lock().execute("UPDATE \"references\" SET failure_json = json_remove(failure_json, '$.related_declarations') WHERE failure_json IS NOT NULL", []).unwrap();
        assert!(
            store.cpp_type_lookup_failures(&|| false).is_err(),
            "missing required source context must not become an empty list"
        );
    }

    #[test]
    fn selected_lookup_failures_preserve_global_evidence_and_read_only_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("facts.db");
        let file_id = FileId::generate("calls.cpp");
        let primary = TextRange {
            start_byte: 10,
            end_byte: 14,
            ..Default::default()
        };
        let declaration = (
            file_id,
            TextRange {
                start_byte: 20,
                end_byte: 24,
                ..Default::default()
            },
        );
        let id = |n| ReferenceId::from_bytes([n; 32]);
        let store = Store::open_db(&path).unwrap();
        store.init_schema().unwrap();
        let facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "calls.cpp".into(),
                language: Language::Cpp,
                ..Default::default()
            },
            references: (1..=8)
                .map(|n| types::ReferenceUse {
                    id: id(n),
                    file_id,
                    source_symbol: None,
                    scope_id: None,
                    kind: types::ReferenceKind::Call,
                    text: "invoke()".into(),
                    name: "invoke".into(),
                    receiver: None,
                    arity: Some(0),
                    range: primary,
                    binding_id: None,
                    resolved: None,
                })
                .collect(),
            ..Default::default()
        };
        store.insert_file_facts(&facts).unwrap();
        let common = CppTypeLookupFailure {
            kind: CppTypeLookupFailureKind::LookupRestricted,
            name: "Base".into(),
            scope: "api".into(),
            file_id,
            range: primary,
            related_declarations: vec![declaration, declaration],
        };
        let updates: Vec<_> = (1..=7)
            .map(|n| {
                let mut failure = common.clone();
                if n == 3 || n == 4 {
                    failure.name = "Conflicting".into();
                    if n == 4 {
                        failure.related_declarations.pop();
                    }
                }
                if n == 5 || n == 6 {
                    failure.name = "Empty".into();
                    failure.related_declarations.clear();
                }
                if n == 7 {
                    failure.file_id = FileId::generate("absent.h");
                }
                (id(n), Some(failure))
            })
            .collect();
        store
            .batch_update_cpp_type_lookup_failures(&updates)
            .unwrap();
        drop(store);
        let before = std::fs::read(&path).unwrap();
        let store = Store::open_db_read_only(&path).unwrap();
        let global = store.cpp_type_lookup_failures(&|| false).unwrap();
        for requested in [
            vec![],
            vec![id(8), id(99)],
            vec![id(1), id(1), id(3), id(5), id(7), id(8), id(99)],
        ] {
            let result = store
                .cpp_type_lookup_failures_for_references(&requested, &|| false)
                .unwrap();
            let expected: HashMap<_, _> = global
                .iter()
                .filter(|(id, _)| requested.contains(id))
                .map(|(id, f)| (*id, f.clone()))
                .collect();
            assert_eq!(result.failures, expected);
            assert_eq!(result.work.failures_decoded, expected.len());
            assert_eq!(
                result.work.unique_references_requested,
                requested.iter().collect::<HashSet<_>>().len()
            );
            assert_eq!(
                result.work.global_contexts_examined,
                if expected.is_empty() { 0 } else { 7 }
            );
            for (reference, failure) in &result.failures {
                // Independent oracle: preserve the original projection key and inventory comparison.
                let matching: Vec<_> = global
                    .values()
                    .filter(|other| {
                        other.code() == failure.code()
                            && other.name == failure.name
                            && other.scope == failure.scope
                            && other.file_id == failure.file_id
                            && other.range == failure.range
                    })
                    .collect();
                let evidence = result.sharing[reference];
                assert_eq!(evidence.matching_failures, matching.len());
                assert_eq!(
                    evidence.same_declarations,
                    matching
                        .iter()
                        .all(|f| f.related_declarations == failure.related_declarations)
                );
            }
        }
        let result = store
            .cpp_type_lookup_failures_for_references(&[id(1), id(3), id(5), id(7)], &|| false)
            .unwrap();
        assert!(result.sharing[&id(1)].has_shared_inventory());
        assert!(!result.sharing[&id(3)].has_shared_inventory());
        assert!(
            result.sharing[&id(5)].has_shared_inventory(),
            "empty inventory is evidence, not a projected region"
        );
        assert!(
            store
                .get_file(&result.failures[&id(7)].file_id)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .cpp_type_lookup_failures_for_references(&[], &|| true)
                .is_err()
        );
        // Cancel during selection and during the global scan; no partial success.
        for stop in [2, 5] {
            let checks = std::cell::Cell::new(0);
            assert!(
                store
                    .cpp_type_lookup_failures_for_references(&[id(1)], &|| {
                        checks.set(checks.get() + 1);
                        checks.get() >= stop
                    })
                    .is_err()
            );
        }
        assert!(
            store
                .batch_update_cpp_type_lookup_failures(&[(id(1), None)])
                .is_err()
        );
        drop(store);
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn cpp_type_facts_follow_atomic_file_replacement_and_deletion() {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let file_id = FileId::generate("types.cpp");
        let unverified_owner =
            SymbolId::generate(&file_id, "cpp", "api::recovered", "function", None);
        let cpp = CppFileTypes {
            allocation_sites: vec![types::cpp::CppAllocationSite {
                range: TextRange {
                    start_byte: 10,
                    end_byte: 20,
                    ..TextRange::default()
                },
                source_symbol: Some(unverified_owner),
            }],
            lookup_limits: vec![types::cpp::CppLookupLimit {
                scope: "api".into(),
                name: Some("Imported".into()),
                declaration_range: TextRange::default(),
                block_range: Some(TextRange {
                    end_byte: 40,
                    ..TextRange::default()
                }),
            }],
            unverified_callable_scopes: vec![unverified_owner],
            records: vec![CppRecordType {
                symbol_id: SymbolId::generate(&file_id, "cpp", "api::Device", "class", None),
                is_definition: true,
                bases: Some(vec![CppBaseClass {
                    declared_type: CppDeclaredType {
                        name: "Base".into(),
                        template_arguments: vec![],
                        pointer: false,
                        reference: false,
                        const_: false,
                        volatile: false,
                    },
                    virtual_: false,
                    range: TextRange::default(),
                }]),
                identity_supported: true,
                lookup_supported: true,
                template_parameters: None,
            }],
            callables: vec![CppCallableDeclaration {
                symbol_id: SymbolId::generate(&file_id, "cpp", "api::Device::run", "method", None),
                template_parameters: None,
                parameter_types: vec!["const api::Value&".into()],
                parameter_declared_types: vec![Some(CppDeclaredType {
                    name: "api::Value".into(),
                    template_arguments: vec![],
                    pointer: false,
                    reference: true,
                    const_: true,
                    volatile: false,
                })],
                minimum_arity: 1,
                qualifiers: "const".into(),
                is_virtual: false,
                internal_linkage: false,
                return_type: None,
            }],
            values: vec![CppValueType {
                binding_id: None,
                symbol_id: None,
                capture_required: true,
                mutable_: false,
                bit_field: false,
                declared_type: Some(CppDeclaredType {
                    name: "api::Device".into(),
                    template_arguments: Vec::new(),
                    pointer: true,
                    reference: false,
                    const_: true,
                    volatile: false,
                }),
                initializer_call: None,
                lookup_scope: "app".into(),
                declaration_range: TextRange::default(),
            }],
            aliases: vec![types::cpp::CppTypeAlias {
                symbol_id: unverified_owner,
                target: Some(types::cpp::CppDeclaredType {
                    name: "int".into(),
                    template_arguments: vec![],
                    pointer: false,
                    reference: false,
                    const_: false,
                    volatile: false,
                }),
                target_range: TextRange::default(),
            }],
            lambda_captures: vec![types::cpp::CppLambdaCapture {
                symbol_id: Some(unverified_owner),
                enclosing_symbol: None,
                binding_id: None,
                binding_const: false,
                parameter_count: Some(0),
                range: TextRange::default(),
                body_range: TextRange::default(),
                default: Some(types::cpp::CppCaptureKind::Reference),
                captures: Some(vec![("value".into(), types::cpp::CppCaptureKind::Copy)]),
                mutable_: false,
            }],
            ..Default::default()
        };
        let mut facts = FileFacts {
            file: FileInfo {
                file_id,
                path: "types.cpp".into(),
                language: Language::Cpp,
                ..Default::default()
            },
            cpp_types: Some(cpp.clone()),
            ..Default::default()
        };
        store.insert_file_facts(&facts).unwrap();
        assert_eq!(store.cpp_allocation_site_count().unwrap(), 1);
        assert_eq!(
            store.cpp_allocation_sites(&|| false).unwrap(),
            vec![(file_id, cpp.allocation_sites[0].clone())]
        );
        assert!(store.cpp_allocation_sites(&|| true).is_err());
        assert_eq!(
            store.cpp_lambda_captures().unwrap(),
            vec![(file_id, cpp.lambda_captures[0].clone())]
        );
        assert_eq!(
            store.cpp_types_for_file(&file_id).unwrap(),
            Some(cpp.clone())
        );
        assert_eq!(store.all_cpp_types().unwrap()[&file_id], cpp);
        let regions = store.cpp_call_regions_for_file(&file_id).unwrap().unwrap();
        assert_eq!(regions.lambda_captures, cpp.lambda_captures);
        assert_eq!(regions.allocation_sites, cpp.allocation_sites);
        assert_eq!(regions.lookup_limits, cpp.lookup_limits);
        assert_eq!(
            regions.unverified_callable_scopes,
            cpp.unverified_callable_scopes
        );
        assert!(
            store
                .cpp_call_regions_for_file(&FileId::generate("absent.cpp"))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.cpp_local_lookup_limits().unwrap()[&file_id],
            cpp.lookup_limits
        );
        assert_eq!(
            store.cpp_unverified_callable_scopes().unwrap(),
            HashSet::from([unverified_owner])
        );
        facts.cpp_types = Some(CppFileTypes::default());
        store.replace_file_facts(&file_id, &facts).unwrap();
        assert_eq!(store.cpp_allocation_site_count().unwrap(), 0);
        assert!(store.cpp_allocation_sites(&|| false).unwrap().is_empty());
        assert_eq!(store.cpp_types_for_file(&file_id).unwrap(), facts.cpp_types);
        assert!(store.cpp_unverified_callable_scopes().unwrap().is_empty());
        assert!(store.cpp_local_lookup_limits().unwrap().is_empty());
        store.delete_file_data(&file_id).unwrap();
        assert!(store.all_cpp_types().unwrap().is_empty());
    }
}
