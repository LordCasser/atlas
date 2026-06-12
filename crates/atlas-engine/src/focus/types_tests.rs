//! Tests for focus-driven analysis types.

use super::types::*;
use types::enums::{Language, SymbolKind};
use types::ids::FileId;
use types::structs::precision::PrecisionTier;
use types::structs::KnownGap;

#[test]
fn test_default_window_budget() {
    let budget = WindowBudget::default();
    assert_eq!(budget.max_files, 30);
    assert_eq!(budget.max_time_ms, 18_000);
    assert_eq!(budget.max_symbols, 0);
    assert_eq!(budget.max_edges, 0);
    assert_eq!(budget.max_fanout_per_name, 20);
    assert_eq!(budget.max_bytes, 0);
    assert_eq!(budget.max_iterations, 3);
}

#[test]
fn test_background_budget() {
    let budget = WindowBudget::background();
    assert_eq!(budget.max_files, 100);
    assert_eq!(budget.max_time_ms, 60_000);
    assert_eq!(budget.max_symbols, 0);
    assert_eq!(budget.max_edges, 0);
    assert_eq!(budget.max_fanout_per_name, 20);
    assert_eq!(budget.max_bytes, 0);
    assert_eq!(budget.max_iterations, 1);
}

#[test]
fn test_focus_window_defaults() {
    let seed = FocusSeed::Symbol {
        name: "main".into(),
        kind: Some(SymbolKind::Function),
        language: Language::TypeScript,
    };
    let window = FocusWindow::new(seed, Language::TypeScript);
    assert_eq!(window.strategies.len(), 1);
    assert!(matches!(
        window.strategies[0],
        ClosureStrategy::ImportNeighborhood { depth: 2 }
    ));
    assert_eq!(window.budget.max_files, 30);
    assert_eq!(window.language, Language::TypeScript);
    assert_eq!(window.max_iterations, 3);
}

#[test]
fn test_focus_closure_new() {
    let seed = FocusSeed::File {
        file_id: FileId::generate("test.rs"),
        language: Language::Rust,
    };
    let closure = FocusClosure::new(&seed);
    assert_eq!(closure.seed, seed);
    assert!(closure.files.is_empty());
    assert!(closure.symbols.is_empty());
    assert!(closure.visited.is_empty());
    assert!(closure.gaps.is_empty());
}

#[test]
fn test_focus_closure_mark_extracted() {
    let seed = FocusSeed::File {
        file_id: FileId::generate("test.rs"),
        language: Language::Rust,
    };
    let mut closure = FocusClosure::new(&seed);
    let fid = FileId::generate("extracted.rs");
    closure.mark_extracted(fid, PrecisionTier::Exact);
    assert!(closure.files.contains(&fid));
    assert!(closure.visited.contains(&fid));
    assert_eq!(closure.files.len(), 1);
}

#[test]
fn test_focus_closure_record_gap() {
    let seed = FocusSeed::File {
        file_id: FileId::generate("test.rs"),
        language: Language::Rust,
    };
    let mut closure = FocusClosure::new(&seed);
    let gap = KnownGap::UnresolvedImport {
        from: "mod.rs".into(),
        import_path: "std::collections".into(),
    };
    closure.record_gap(gap.clone());
    assert_eq!(closure.gaps.len(), 1);
    assert_eq!(closure.gaps[0], gap);
}

#[test]
fn test_precision_tier_adapter_exact() {
    // Tested via compat.rs — here we just verify the From impl is callable
    let p = types::structs::Precision {
        coverage: types::structs::CoverageTier::RepoComplete,
        confidence: types::structs::SemanticConfidence::Certain,
    };
    let tier: PrecisionTier = p.into();
    assert_eq!(tier, PrecisionTier::Exact);
}

#[test]
fn test_precision_tier_adapter_manifest() {
    let p = types::structs::Precision {
        coverage: types::structs::CoverageTier::Manifest,
        confidence: types::structs::SemanticConfidence::Low,
    };
    let tier: PrecisionTier = p.into();
    assert_eq!(tier, PrecisionTier::ManifestOnly);
}

#[test]
fn test_focus_seed_equality() {
    let a = FocusSeed::Symbol {
        name: "main".into(),
        kind: Some(SymbolKind::Function),
        language: Language::TypeScript,
    };
    let b = FocusSeed::Symbol {
        name: "main".into(),
        kind: Some(SymbolKind::Function),
        language: Language::TypeScript,
    };
    assert_eq!(a, b);
}

#[test]
fn test_budget_can_absorb() {
    let budget = WindowBudget::default();
    let additions: Vec<FileId> = (0..10)
        .map(|i| FileId::generate(&format!("file_{i}.rs")))
        .collect();
    assert!(budget.can_absorb(&additions));

    let too_many: Vec<FileId> = (0..31)
        .map(|i| FileId::generate(&format!("file_{i}.rs")))
        .collect();
    assert!(!budget.can_absorb(&too_many));
}
