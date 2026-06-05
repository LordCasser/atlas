use crate::ExtractionMode;
use crate::Store;

/// Return true when `requested` would lower the precision of `current`.
pub fn would_downgrade_index_precision(current: &str, requested: &ExtractionMode) -> bool {
    match requested {
        ExtractionMode::Manifest => is_rich_index_mode(current),
        ExtractionMode::Structural => current == "full",
        ExtractionMode::Full => false,
        ExtractionMode::ResolutionSymbols | ExtractionMode::LazyDataflow { .. } => false,
    }
}

/// Return true for index modes that contain structural or richer facts.
pub fn is_rich_index_mode(index_mode: &str) -> bool {
    matches!(
        index_mode,
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

pub fn recommended_analysis_for(index_mode: &str) -> &'static str {
    match index_mode {
        "full" => "full",
        _ => "structural",
    }
}

/// Prevent accidental precision downgrade of an existing rich index.
///
/// Returns an error if `requested` would lower the precision of the
/// current index mode, unless `force_reindex` is true.
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
        .read_index_mode()
        .unwrap_or_else(|_| "unknown".to_string());
    if !would_downgrade_index_precision(&current, requested) {
        return Ok(());
    }
    anyhow::bail!(
        "Refusing to run {operation} with analysis='{}' because the existing fresh index is {current}. \
         This would discard higher-precision facts for changed files. \
         Use analysis='{}' or pass force_reindex=true to allow the downgrade explicitly.",
        extraction_mode_name(requested),
        recommended_analysis_for(&current),
    );
}
