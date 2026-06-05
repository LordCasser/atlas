use crate::ExtractionMode;

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
