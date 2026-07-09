use crate::ExtractionMode;
use crate::Store;

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
