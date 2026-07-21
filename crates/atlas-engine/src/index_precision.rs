use crate::focus::query::QueryNeed;
use crate::{ExtractionMode, Store};

/// Return true when `requested` would lower the fact coverage of `current` CatalogTier.
pub fn would_downgrade_index_precision(current: &str, requested: &ExtractionMode) -> bool {
    match requested {
        ExtractionMode::Manifest => is_rich_catalog_tier(current),
        ExtractionMode::Structural => current == "full",
        ExtractionMode::Full => false,
        ExtractionMode::ResolutionSymbols | ExtractionMode::LazyDataflow { .. } => false,
    }
}

/// Return true for CatalogTier strings that contain structural or richer facts.
pub fn is_rich_catalog_tier(catalog_tier: &str) -> bool {
    matches!(
        catalog_tier,
        "partial_structural"
            | "partial_structural+lazy"
            | "structural"
            | "structural+lazy"
            | "full"
    )
}

/// Return true when a finalized whole-project Index already satisfies a query.
///
/// A scoped Index is a reusable fact source, but it is not proof of repository
/// coverage. Likewise, a manifest Index followed by a few Focus structural
/// upgrades must remain on the Focus path instead of turning the mixed catalog
/// into a false full-cache signal.
pub fn has_finalized_repo_cache_for(store: &Store, need: QueryNeed) -> bool {
    let finalized = store
        .get_metadata("last_index_time")
        .ok()
        .flatten()
        .is_some();
    let whole_project = store
        .get_metadata("indexed_scope")
        .ok()
        .flatten()
        .and_then(|scope| serde_json::from_str::<serde_json::Value>(&scope).ok())
        .is_some_and(|scope| {
            scope
                .get("include")
                .and_then(serde_json::Value::as_array)
                .is_some_and(Vec::is_empty)
                && scope
                    .get("exclude")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(Vec::is_empty)
        });
    let finalized_grade = store.get_metadata("indexed_pipeline_grade").ok().flatten();
    let grade_satisfies = finalized_grade.as_deref().is_some_and(|grade| match need {
        QueryNeed::Manifest => matches!(grade, "manifest" | "structural" | "full"),
        QueryNeed::Structural | QueryNeed::CallGraph => matches!(grade, "structural" | "full"),
        QueryNeed::Dataflow => grade == "full",
    });
    if !finalized || !whole_project || !grade_satisfies {
        return false;
    }

    let catalog_tier = store
        .read_catalog_tier()
        .unwrap_or_else(|_| "unknown".to_string());
    match need {
        QueryNeed::Manifest => matches!(
            catalog_tier.as_str(),
            "manifest" | "structural" | "structural+lazy" | "full"
        ),
        QueryNeed::Structural | QueryNeed::CallGraph => matches!(
            catalog_tier.as_str(),
            "structural" | "structural+lazy" | "full"
        ),
        QueryNeed::Dataflow => catalog_tier == "full",
    }
}

pub fn extraction_mode_name(mode: &ExtractionMode) -> &'static str {
    match mode {
        ExtractionMode::Manifest => "manifest",
        ExtractionMode::Structural => "structural",
        ExtractionMode::Full => "full",
        ExtractionMode::ResolutionSymbols => "resolution-symbols",
        ExtractionMode::LazyDataflow { .. } => "lazy-dataflow",
    }
}

pub fn recommended_extract_recipe_for(catalog_tier: &str) -> &'static str {
    match catalog_tier {
        "full" => "full",
        _ => "structural",
    }
}

/// Prevent accidental fact-coverage downgrade of an existing rich catalog.
///
/// Returns an error if `requested` would lower the CatalogTier of the
/// current store, unless `force_reindex` is true.
pub fn guard_against_precision_downgrade(
    store: &Store,
    requested: &ExtractionMode,
    force_reindex: bool,
    operation: &str,
) -> anyhow::Result<()> {
    if force_reindex {
        return Ok(());
    }
    let current = store
        .read_catalog_tier()
        .unwrap_or_else(|_| "unknown".to_string());
    if !would_downgrade_index_precision(&current, requested) {
        return Ok(());
    }
    anyhow::bail!(
        "Refusing to run {operation} with analysis='{}' because the existing fresh catalog is {current}. \
         This would discard higher-coverage facts for changed files. \
         Use analysis='{}' or pass force_reindex=true to allow the downgrade explicitly.",
        extraction_mode_name(requested),
        recommended_extract_recipe_for(&current),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{FactCoverage, FileId, FileInfo, Language, ParseStatus};

    fn store_with_complete_layer(layer: &str) -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let file_id = FileId::generate("src/main.ts");
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/main.ts".into(),
                language: Language::TypeScript,
                content_hash: "hash".into(),
                status: ParseStatus::Success,
            })
            .unwrap();
        store
            .upsert_file_extraction_state(
                &file_id,
                layer,
                "hash",
                "complete",
                FactCoverage::default(),
            )
            .unwrap();
        store
    }

    fn mark_finalized(
        store: &Store,
        grade: &str,
        include_patterns: &[&str],
        exclude_patterns: &[&str],
    ) {
        store.set_metadata("last_index_time", "1").unwrap();
        store
            .set_metadata(
                "indexed_scope",
                &serde_json::json!({
                    "include": include_patterns,
                    "exclude": exclude_patterns,
                })
                .to_string(),
            )
            .unwrap();
        store.set_metadata("indexed_pipeline_grade", grade).unwrap();
    }

    #[test]
    fn finalized_pipeline_grade_bounds_query_reuse() {
        let cases = [
            (
                "manifest",
                [
                    (QueryNeed::Manifest, true),
                    (QueryNeed::Structural, false),
                    (QueryNeed::CallGraph, false),
                    (QueryNeed::Dataflow, false),
                ],
            ),
            (
                "structural",
                [
                    (QueryNeed::Manifest, true),
                    (QueryNeed::Structural, true),
                    (QueryNeed::CallGraph, true),
                    (QueryNeed::Dataflow, false),
                ],
            ),
            (
                "full",
                [
                    (QueryNeed::Manifest, true),
                    (QueryNeed::Structural, true),
                    (QueryNeed::CallGraph, true),
                    (QueryNeed::Dataflow, true),
                ],
            ),
        ];

        for (grade, expectations) in cases {
            // A full current catalog isolates the authority granted by the
            // last manual Index from the facts Focus may have added later.
            let store = store_with_complete_layer("dataflow");
            mark_finalized(&store, grade, &[], &[]);
            for (need, expected) in expectations {
                assert_eq!(
                    has_finalized_repo_cache_for(&store, need),
                    expected,
                    "grade={grade}, need={need:?}"
                );
            }
        }
    }

    #[test]
    fn current_catalog_must_still_satisfy_the_query() {
        let cases = [
            ("manifest", [true, false, false, false]),
            ("structural", [true, true, true, false]),
            ("dataflow", [true, true, true, true]),
        ];
        let needs = [
            QueryNeed::Manifest,
            QueryNeed::Structural,
            QueryNeed::CallGraph,
            QueryNeed::Dataflow,
        ];

        for (layer, expected) in cases {
            let store = store_with_complete_layer(layer);
            mark_finalized(&store, "full", &[], &[]);
            for (index, need) in needs.into_iter().enumerate() {
                assert_eq!(
                    has_finalized_repo_cache_for(&store, need),
                    expected[index],
                    "layer={layer}, need={need:?}"
                );
            }
        }
    }

    #[test]
    fn any_manual_scope_keeps_focus_active() {
        for (include_patterns, exclude_patterns) in
            [(&["src/**"][..], &[][..]), (&[][..], &["vendor/**"][..])]
        {
            let store = store_with_complete_layer("dataflow");
            mark_finalized(&store, "full", include_patterns, exclude_patterns);
            for need in [
                QueryNeed::Manifest,
                QueryNeed::Structural,
                QueryNeed::CallGraph,
                QueryNeed::Dataflow,
            ] {
                assert!(
                    !has_finalized_repo_cache_for(&store, need),
                    "scoped Index must remain a reusable Focus base: need={need:?}"
                );
            }
        }
    }

    #[test]
    fn legacy_or_incomplete_finalization_metadata_grants_no_authority() {
        let store = store_with_complete_layer("dataflow");
        store.set_metadata("last_index_time", "1").unwrap();
        store.set_metadata("indexed_scope", "[]").unwrap();
        store
            .set_metadata("indexed_pipeline_grade", "full")
            .unwrap();

        assert!(!has_finalized_repo_cache_for(&store, QueryNeed::CallGraph));
    }
}
