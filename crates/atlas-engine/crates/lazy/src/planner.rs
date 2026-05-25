//! LazyDataflowPlanner: from a query seed, produce a [`LazyWindow`] of
//! AnalysisUnits whose dataflow should be built on demand.
//!
//! The planner reads only structural index data from the store:
//! symbols, scopes, references, callsites, and symbol_edges.
//! It does NOT perform extraction or write to the database.

use std::collections::HashSet;

use anyhow::Result;
use db::Store;
use types::ids::{FileId, SymbolId};
use types::lazy::{AnalysisUnit, LazyWindow, VariableFocus};
use types::structs::{ReferenceUse, ScopeDef, SymbolDef, TextRange};
use types::enums::{ScopeKind, SymbolKind};

use crate::constants::{
    LAZY_DATAFLOW_MAX_DEPTH, LAZY_DATAFLOW_MAX_UNITS,
};

/// Entry point for all lazy-window planning.
pub(crate) struct LazyDataflowPlanner;

impl LazyDataflowPlanner {
    /// Plan a window for a `trace_variable` query starting at `(file_id, line, column)`.
    ///
    /// Steps:
    /// 1. Locate the innermost reference → resolved symbol
    /// 2. Find enclosing scope → enclosing function symbol → seed_unit
    /// 3. Expand callers/callees up to [`LAZY_DATAFLOW_MAX_DEPTH`]
    /// 4. Truncate if unit count exceeds [`LAZY_DATAFLOW_MAX_UNITS`]
    pub fn plan_for_position(
        store: &Store,
        file_id: &FileId,
        line: u32,
        column: u32,
    ) -> Result<LazyWindow> {
        let line0 = line.saturating_sub(1);
        let col0 = column.saturating_sub(1);

        // 1. Find innermost reference
        let refs = store.find_references_by_file(file_id)?;
        let reference = find_innermost_reference(&refs, line0, col0);

        // 2. Find resolved symbol and enclosing function
        let resolved_symbol = reference
            .as_ref()
            .and_then(|r| r.resolved.as_ref())
            .and_then(|rt| store.find_symbol_by_id(&rt.symbol_id).ok().flatten());

        // 3. Find enclosing scope → enclosing function
        let scopes = store.find_scopes_by_file(file_id)?;
        let scope = find_innermost_scope(&scopes, line0, col0);
        let enclosing_function = scope
            .as_ref()
            .and_then(|s| find_enclosing_function(&scopes, s, store));

        // 4. Determine seed unit
        let seed_unit = if let Some(ref func) = enclosing_function {
            AnalysisUnit::from_function(*file_id, func.id, func.range)
        } else {
            // Fall back to top-level unit
            // Use the file's byte range (0..max) or scope range
            let range = scopes
                .iter()
                .find(|s| s.parent_id.is_none())
                .map(|s| s.range)
                .unwrap_or(TextRange {
                    start_byte: 0,
                    end_byte: u32::MAX,
                    start_line: 0,
                    start_column: 0,
                    end_line: u32::MAX,
                    end_column: 0,
                });
            AnalysisUnit::from_top_level(*file_id, range)
        };

        // 5. Variable focus (stored in window for downstream use)
        let variable_focus = reference.as_ref().map(|r| VariableFocus {
            name: r.name.clone(),
            reference_range: r.range,
            reference_id: Some(r.id),
            resolved_symbol_id: resolved_symbol.as_ref().map(|s| s.id),
        });

        // 6. Expand window
        let mut units: Vec<AnalysisUnit> = vec![seed_unit.clone()];
        let mut seen: HashSet<[u8; 16]> = HashSet::new();
        seen.insert(seed_unit.unit_id);
        let mut frontier: Vec<AnalysisUnit> = vec![seed_unit.clone()];

        for depth in 0..LAZY_DATAFLOW_MAX_DEPTH {
            if depth > 0 && frontier.is_empty() {
                break;
            }
            if depth > 0 {
                let mut next_frontier: Vec<AnalysisUnit> = Vec::new();
                for unit in &frontier {
                    if let Some(sid) = unit.symbol_id {
                        // Callees: functions called by this unit
                        if let Ok(callsites) = store.find_callsites_by_file(&unit.file_id) {
                            for cs in callsites {
                                if cs.caller == sid {
                                    if let Some(callee) = cs.callee {
                                        if let Ok(Some(callee_sym)) = store.find_symbol_by_id(&callee) {
                                            add_if_new(
                                                &callee_sym, &mut units, &mut seen, &mut next_frontier,
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        // Callers: functions that call this unit
                        if let Ok(edges) = store.find_edges_by_target(&sid) {
                            for edge in edges {
                                add_if_new_by_id(
                                    store, edge.source, &mut units, &mut seen, &mut next_frontier,
                                );
                            }
                        }
                    }
                }
                frontier = next_frontier;
            }

            if units.len() >= LAZY_DATAFLOW_MAX_UNITS {
                break;
            }
        }

        let truncated = units.len() > LAZY_DATAFLOW_MAX_UNITS;
        if truncated {
            units.truncate(LAZY_DATAFLOW_MAX_UNITS);
        }

        Ok(LazyWindow {
            seed_unit,
            units,
            variable_focus,
            truncated,
        })
    }

    /// Plan a window for a `trace_function` query starting at a known symbol.
    pub fn plan_for_function(
        store: &Store,
        symbol_id: &SymbolId,
    ) -> Result<LazyWindow> {
        let sym = match store.find_symbol_by_id(symbol_id)? {
            Some(s) => s,
            None => anyhow::bail!("symbol not found: {:?}", symbol_id),
        };
        let seed_unit = AnalysisUnit::from_function(sym.file_id, sym.id, sym.range);

        let mut units: Vec<AnalysisUnit> = vec![seed_unit.clone()];
        let mut seen: HashSet<[u8; 16]> = HashSet::new();
        seen.insert(seed_unit.unit_id);
        let mut frontier: Vec<AnalysisUnit> = vec![seed_unit.clone()];

        for depth in 0..LAZY_DATAFLOW_MAX_DEPTH {
            if depth > 0 {
                let mut next_frontier: Vec<AnalysisUnit> = Vec::new();
                for unit in &frontier {
                    if let Some(sid) = unit.symbol_id {
                        if let Ok(callsites) = store.find_callsites_by_file(&unit.file_id) {
                            for cs in callsites {
                                if cs.caller == sid {
                                    if let Some(callee) = cs.callee {
                                        if let Ok(Some(sym)) = store.find_symbol_by_id(&callee) {
                                            add_if_new(&sym, &mut units, &mut seen, &mut next_frontier);
                                        }
                                    }
                                }
                            }
                        }
                        if let Ok(edges) = store.find_edges_by_target(&sid) {
                            for edge in edges {
                                add_if_new_by_id(store, edge.source, &mut units, &mut seen, &mut next_frontier);
                            }
                        }
                    }
                }
                frontier = next_frontier;
            }
            if units.len() >= LAZY_DATAFLOW_MAX_UNITS {
                break;
            }
        }

        let truncated = units.len() > LAZY_DATAFLOW_MAX_UNITS;
        if truncated {
            units.truncate(LAZY_DATAFLOW_MAX_UNITS);
        }

        Ok(LazyWindow {
            seed_unit,
            units,
            variable_focus: None,
            truncated,
        })
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn range_contains(r: &TextRange, line: u32, column: u32) -> bool {
    if line < r.start_line || line > r.end_line {
        return false;
    }
    if line == r.start_line && column < r.start_column {
        return false;
    }
    if line == r.end_line && column > r.end_column {
        return false;
    }
    true
}

fn find_innermost_reference(refs: &[ReferenceUse], line: u32, col: u32) -> Option<&ReferenceUse> {
    let mut best: Option<&ReferenceUse> = None;
    let mut best_span = u32::MAX;
    for r in refs {
        if range_contains(&r.range, line, col) {
            let span = r.range.end_byte.saturating_sub(r.range.start_byte);
            if span < best_span {
                best_span = span;
                best = Some(r);
            }
        }
    }
    best
}

fn find_innermost_scope(scopes: &[ScopeDef], line: u32, col: u32) -> Option<&ScopeDef> {
    let mut best: Option<&ScopeDef> = None;
    let mut best_span = u32::MAX;
    for s in scopes {
        if range_contains(&s.range, line, col) {
            let span = s.range.end_byte.saturating_sub(s.range.start_byte);
            if span < best_span {
                best_span = span;
                best = Some(s);
            }
        }
    }
    best
}

fn find_enclosing_function(
    scopes: &[ScopeDef],
    start: &ScopeDef,
    store: &Store,
) -> Option<SymbolDef> {
    // Walk scope parent chain looking for a function/method scope.
    let mut current = Some(start.id);
    let parent_map: std::collections::HashMap<_, _> =
        scopes.iter().map(|s| (s.id, s.parent_id)).collect();

    while let Some(sid) = current {
        let scope = scopes.iter().find(|s| s.id == sid)?;
        if matches!(scope.kind, ScopeKind::Function | ScopeKind::Method) {
            // Find the symbol whose range matches this scope
            if let Ok(symbols) = store.find_symbols_by_file(&scope.file_id) {
                let func_sym = symbols.iter().find(|sym| {
                    matches!(
                        sym.kind,
                        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
                    ) && sym.range.start_byte == scope.range.start_byte
                        && sym.range.end_byte == scope.range.end_byte
                });
                if let Some(sym) = func_sym {
                    return Some(sym.clone());
                }
            }
            // Fallback: scope itself might not have an exact symbol match —
            // return None and let caller use top-level.
            return None;
        }
        current = parent_map.get(&sid).and_then(|&p| p);
    }
    None
}

fn add_if_new(
    sym: &SymbolDef,
    units: &mut Vec<AnalysisUnit>,
    seen: &mut HashSet<[u8; 16]>,
    frontier: &mut Vec<AnalysisUnit>,
) {
    let unit = AnalysisUnit::from_function(sym.file_id, sym.id, sym.range);
    if seen.insert(unit.unit_id) {
        units.push(unit.clone());
        frontier.push(unit);
    }
}

fn add_if_new_by_id(
    store: &Store,
    source_id: SymbolId,
    units: &mut Vec<AnalysisUnit>,
    seen: &mut HashSet<[u8; 16]>,
    frontier: &mut Vec<AnalysisUnit>,
) {
    if let Ok(Some(sym)) = store.find_symbol_by_id(&source_id) {
        add_if_new(&sym, units, seen, frontier);
    }
}
