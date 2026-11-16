//! Re-extract affected C++ files after their actual header facts are available.
//! Runs before relation publication; queries only reuse the recorded token ranges.
use anyhow::Result;
use db::Store;
use extraction::{ExtractionMode, ParseWorkerPool, cpp_annotations, extraction_pool};
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Instant,
};
use types::{
    FileId, ImportKind,
    cpp::{CppFileTypes, CppLookupLimit, CppMacroDefinition},
};

pub(crate) fn prepare(
    store: &Store,
    root: &Path,
    mode: &ExtractionMode,
    canceled: &mut dyn FnMut() -> bool,
) -> Result<usize> {
    let mut file_updates = 0;
    // Repairing an earlier declaration/lambda can expose later annotations.
    // Resolve only after the stored parser inputs and recovered candidates agree.
    // A bounded failure remains unpublished and uses the existing retry path.
    for _ in 0..32 {
        let changed = prepare_pass(store, root, mode, canceled)?;
        file_updates += changed;
        if changed == 0 {
            return Ok(file_updates);
        }
    }
    anyhow::bail!("C++ annotation preparation did not converge within 32 passes")
}

fn prepare_pass(
    store: &Store,
    root: &Path,
    mode: &ExtractionMode,
    canceled: &mut dyn FnMut() -> bool,
) -> Result<usize> {
    anyhow::ensure!(!canceled(), "C++ annotation preparation canceled");
    let facts = store.all_cpp_types()?;
    if !facts.values().any(|f| {
        !f.annotation_candidates.is_empty()
            || !f.normalized_annotations.is_empty()
            || !f.member_macros.is_empty()
            || !f.normalized_member_macros.is_empty()
    }) {
        return Ok(0);
    }
    let include_paths: Vec<String> = serde_json::from_str(
        &store
            .get_metadata(resolution::KEY_INCLUDE_PATHS)?
            .unwrap_or_else(|| "[]".into()),
    )?;
    let mut includes = HashMap::<_, Vec<_>>::new();
    for id in facts.keys() {
        anyhow::ensure!(!canceled(), "C++ annotation preparation canceled");
        for import in store.find_imports_by_file(id)? {
            if import.kind == ImportKind::Include
                && let Some(target) = store.resolve_include_file(&import, &include_paths)?
                && facts.contains_key(&target.file_id)
            {
                includes
                    .entry(*id)
                    .or_default()
                    .push((import.range.end_byte, target.file_id));
            }
        }
    }
    let mut plans = Vec::new();
    let mut ids: Vec<_> = facts.keys().copied().collect();
    ids.sort();
    for id in ids {
        anyhow::ensure!(!canceled(), "C++ annotation preparation canceled");
        let original = &facts[&id];
        let mut ranges = Vec::new();
        // Normalizing a leading token moves the declaration's AST start past
        // it, so the recovered tree need not rediscover that candidate. Retain
        // previously applied sites only after checking their definitions again;
        // changed/removed macros must still retract the old parser input.
        for candidate in original
            .annotation_candidates
            .iter()
            .chain(&original.normalized_annotations)
        {
            let definitions =
                visible_definitions(id, candidate.range.start_byte, &facts, &includes);
            if cpp_annotations::is_annotation(&candidate.text, &definitions, &candidate.position) {
                ranges.push(candidate.clone());
            }
        }
        // A single spelling can have multiple parser roles. Sort those roles
        // as well, so equal old/new observations are adjacent before dedup.
        // Sorting only by byte offset lets interleaved copies grow each pass.
        ranges.sort_by(|a, b| {
            (
                a.range.start_byte,
                a.range.end_byte,
                &a.position,
                &a.name,
                &a.text,
            )
                .cmp(&(
                    b.range.start_byte,
                    b.range.end_byte,
                    &b.position,
                    &b.name,
                    &b.text,
                ))
        });
        ranges.dedup();
        let mut normalized_members = Vec::new();
        for site in &original.member_macros {
            let definitions = visible_definitions(id, site.range.start_byte, &facts, &includes);
            if extraction::cpp_member_macros::introduced_names(site, &definitions).is_some() {
                normalized_members.push(site.clone());
            }
        }
        normalized_members.sort_by_key(|site| site.range.start_byte);
        normalized_members.dedup();
        let members = member_limits(id, original, &facts, &includes);
        let previous_members: Vec<_> = original
            .lookup_limits
            .iter()
            .filter(|limit| {
                original
                    .member_macros
                    .iter()
                    .any(|site| site.range == limit.declaration_range)
            })
            .cloned()
            .collect();
        if ranges == original.normalized_annotations
            && normalized_members == original.normalized_member_macros
            && members == previous_members
        {
            continue;
        }
        let file = store
            .get_file(&id)?
            .ok_or_else(|| anyhow::anyhow!("C++ annotation source metadata missing"))?;
        plans.push((file, ranges, normalized_members));
    }
    if plans.is_empty() {
        return Ok(0);
    }
    let pool = ParseWorkerPool::default_pool();
    let workers = extraction_pool();
    let canonical_root = root.canonicalize()?;
    let batch_size = workers.current_num_threads().max(1);
    tracing::info!(
        files = plans.len(),
        batch_size,
        "preparing C++ declaration annotations"
    );
    let mut changed = 0;
    let mut extraction_ms = 0;
    let mut write_ms = 0;
    // Reuse the existing extraction pool and keep only one batch of facts in
    // memory. Publication remains ordered, with cancellation checked before
    // each batch and its atomic write. No worker writes to the graph store.
    for batch in plans.chunks(batch_size) {
        anyhow::ensure!(!canceled(), "C++ annotation preparation canceled");
        let started = Instant::now();
        let updated: Vec<_> = workers.install(|| {
            batch
                .par_iter()
                .map(|(file, ranges, normalized_members)| -> Result<_> {
                    let canonical = root.join(&file.path).canonicalize()?;
                    anyhow::ensure!(
                        canonical.starts_with(&canonical_root),
                        "C++ annotation source escapes project root"
                    );
                    let source = workspace::read_source(&canonical)?;
                    anyhow::ensure!(
                        source.file_hash == file.content_hash,
                        "C++ annotation source changed during indexing: {}",
                        file.path
                    );
                    let frontend =
                        cpp_annotations::frontend(ranges.clone(), normalized_members.clone())
                            .ok_or_else(|| anyhow::anyhow!("C++ annotation grammar unavailable"))?;
                    let mut updated = pool
                        .extract_one(
                            &frontend,
                            file.file_id,
                            Path::new(&file.path),
                            &source.text,
                            &source.file_hash,
                            mode.clone(),
                        )
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "C++ annotation extraction {}: {}",
                                file.path,
                                e.message
                            )
                        })?;
                    let cpp = updated.cpp_types.as_mut().ok_or_else(|| {
                        anyhow::anyhow!("C++ annotation extraction did not retain type facts")
                    })?;
                    cpp.normalized_annotations = ranges.clone();
                    cpp.normalized_member_macros = normalized_members.clone();
                    // Earlier annotation repair can restore an enclosing class's
                    // namespace. Attach omitted declarations to the current
                    // innermost recorded class rather than retaining an old owner.
                    for site in &mut cpp.normalized_member_macros {
                        if let Some(owner) = updated
                            .symbols
                            .iter()
                            .filter(|symbol| {
                                matches!(
                                    symbol.kind,
                                    types::SymbolKind::Class | types::SymbolKind::Struct
                                ) && symbol.range.start_byte <= site.range.start_byte
                                    && symbol.range.end_byte >= site.range.end_byte
                            })
                            .min_by_key(|symbol| symbol.range.end_byte - symbol.range.start_byte)
                        {
                            site.scope = owner.qualified_name.clone();
                        }
                    }
                    // Omitted invocations no longer appear in the parsed tree.
                    // Keep their original source for name restrictions and for
                    // revalidating changed/removed macro definitions.
                    cpp.member_macros
                        .extend(cpp.normalized_member_macros.iter().cloned());
                    cpp.member_macros.sort_by_key(|site| site.range.start_byte);
                    cpp.member_macros.dedup();
                    let members = member_limits(file.file_id, cpp, &facts, &includes);
                    cpp.lookup_limits.retain(|limit| {
                        !cpp.member_macros
                            .iter()
                            .any(|site| site.range == limit.declaration_range)
                    });
                    cpp.lookup_limits.extend(members);
                    cpp.lookup_limits
                        .sort_by_key(|limit| limit.declaration_range.start_byte);
                    Ok(updated)
                })
                .collect::<Result<Vec<_>>>()
        })?;
        extraction_ms += started.elapsed().as_millis();
        let started = Instant::now();
        anyhow::ensure!(!canceled(), "C++ annotation preparation canceled");
        if changed == 0 {
            store.create_file_replacement_indexes()?;
            // Invalidate before the first replacement, so cancellation/retry
            // cannot retain old lookups after a new class became visible.
            store.set_metadata(db::KEY_RESOLUTION_CONFIG_HASH, "")?;
            store.invalidate_all_references()?;
            store.delete_all_edges()?;
        }
        store.replace_file_facts_batch_with_invalidation(&updated)?;
        changed += updated.len();
        anyhow::ensure!(!canceled(), "C++ annotation preparation canceled");
        write_ms += started.elapsed().as_millis();
    }
    tracing::info!(
        files = changed,
        extraction_ms,
        write_ms,
        "C++ annotation preparation complete"
    );
    Ok(changed)
}

fn member_limits(
    file: FileId,
    original: &CppFileTypes,
    facts: &HashMap<FileId, CppFileTypes>,
    includes: &HashMap<FileId, Vec<(u32, FileId)>>,
) -> Vec<CppLookupLimit> {
    let mut limits = Vec::new();
    for site in &original.member_macros {
        let definitions = visible_definitions(file, site.range.start_byte, facts, includes);
        let names = match extraction::cpp_member_macros::introduced_names(site, &definitions) {
            Some(names) => names.into_iter().map(Some).collect(),
            None if definitions.contains_key(site.name.as_str())
                || original
                    .lookup_limits
                    .iter()
                    .any(|limit| limit.declaration_range == site.range) =>
            {
                vec![None]
            }
            // An unconfirmed prefix may be an ordinary parenthesized C++
            // declarator. Do not turn it into a new unknown macro restriction.
            None => continue,
        };
        limits.extend(names.into_iter().map(|name| CppLookupLimit {
            scope: site.scope.clone(),
            name,
            declaration_range: site.range,
            block_range: None,
        }));
    }
    limits
}

fn visible_definitions<'a>(
    file: FileId,
    before: u32,
    facts: &'a HashMap<FileId, CppFileTypes>,
    includes: &HashMap<FileId, Vec<(u32, FileId)>>,
) -> HashMap<&'a str, Vec<&'a CppMacroDefinition>> {
    let mut definitions = HashMap::<_, Vec<_>>::new();
    let mut pending = vec![(file, before)];
    let mut visited = HashSet::new();
    while let Some((id, cutoff)) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        if let Some(facts) = facts.get(&id) {
            for definition in &facts.macros {
                if definition.range.end_byte <= cutoff {
                    definitions
                        .entry(definition.name.as_str())
                        .or_default()
                        .push(definition);
                }
            }
        }
        for (end, target) in includes.get(&id).into_iter().flatten() {
            if *end <= cutoff {
                pending.push((*target, u32::MAX));
            }
        }
    }
    definitions
}
