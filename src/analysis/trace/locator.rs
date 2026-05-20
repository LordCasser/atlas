//! Position → [`TracePoint`] resolution.
//!
//! The locator takes a source position `(file_id, line, column)` and returns
//! everything Atlas knows about that position: the enclosing reference, its
//! resolved symbol, the data node, incident dataflow edges, the lexical
//! binding, and the enclosing scope.

use crate::db::Store;
use crate::types::bindings::{BindingDef, BindingUse};
use crate::types::enums::ReferenceKind;
use crate::types::ids::{DataNodeId, FileId};
use crate::types::structs::{Callsite, ReferenceUse, TextRange};
use crate::types::trace::{TraceDataNodeRef, TracePoint};

/// Resolves a source position to a [`TracePoint`].
///
/// # Usage
///
/// ```ignore
/// let point = Locator::locate(&store, &file_id, 12, 5)?;
/// println!("{:?}", point.data_node);
/// ```
pub struct Locator;

impl Locator {
    /// Locate the full context at a source position.
    ///
    /// `line` and `column` are 1-based (matching editor convention).
    /// The locator finds the innermost reference, data node, scope, and binding
    /// whose byte range contains the given position.
    pub fn locate(
        store: &Store,
        file_id: &FileId,
        line: u32,
        column: u32,
    ) -> anyhow::Result<TracePoint> {
        // Convert 1-based editor coordinates to 0-based internal representation.
        // tree-sitter produces 0-based line/column, and TextRange stores those
        // directly.  CLI and MCP accept 1-based input (editor convention).
        let line0 = line.saturating_sub(1);
        let col0 = column.saturating_sub(1);

        // 1. Find the reference at this position
        let refs = store.find_references_by_file(file_id)?;
        let reference = find_innermost_at_position(&refs, extract_ref_range, line0, col0);

        // 2. Resolve the symbol this reference points to
        let resolved_symbol = match &reference {
            Some(r) => match &r.resolved {
                Some(rt) => store.find_symbol_by_id(&rt.symbol_id)?,
                None => None,
            },
            None => None,
        };

        // 3. Find the enclosing scope
        let scopes = store.find_scopes_by_file(file_id)?;
        let scope = find_innermost_at_position(&scopes, |s| &s.range, line0, col0).cloned();

        // 4. Find the data node at this position (prefer the most specific,
        //    i.e. the one with the smallest byte range)
        let data_nodes = store.find_data_nodes_by_file(file_id)?;
        let data_node = find_innermost_at_position(&data_nodes, |dn| &dn.range, line0, col0)
            .cloned();

        // 5. Collect incoming and outgoing dataflow edges
        let (incoming, outgoing) = if let Some(ref dn) = data_node {
            let inc_edges = store
                .find_dataflow_edges_by_target(&dn.id)
                .unwrap_or_default();
            let out_edges = store
                .find_dataflow_edges_by_source(&dn.id)
                .unwrap_or_default();
            let incoming = resolve_data_node_refs(store, &inc_edges, |e| &e.source)?;
            let outgoing = resolve_data_node_refs(store, &out_edges, |e| &e.target)?;
            (incoming, outgoing)
        } else {
            (vec![], vec![])
        };

        // 6. Check for callsite
        let callsite = find_callsite_for_position(store, &reference, file_id, line0, col0)?;

        // 7. Check for binding
        let (binding, binding_use) = find_binding_at_position(store, file_id, line0, col0)?;

        Ok(TracePoint {
            reference: reference.cloned(),
            resolved_symbol,
            data_node,
            incoming,
            outgoing,
            binding,
            binding_use,
            scope,
            callsite,
            file_id: file_id.clone(),
            line,
            column,
            capability: None,
            partial_result: false,
            diagnostics: vec![],
        })
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Find the one item in `items` whose range most tightly contains `(line,
/// column)`.  When multiple items contain the same position, the one with
/// the smallest byte span wins (innermost match).
fn find_innermost_at_position<T, F>(
    items: &[T],
    range_fn: F,
    line: u32,
    column: u32,
) -> Option<&T>
where
    F: Fn(&T) -> &TextRange,
{
    let mut best: Option<&T> = None;
    let mut best_span: u32 = u32::MAX;

    for item in items {
        let range = range_fn(item);
        if range_contains(range, line, column) {
            let span = range.end_byte.saturating_sub(range.start_byte);
            if span < best_span {
                best_span = span;
                best = Some(item);
            }
        }
    }
    best
}

/// Check if `(line, column)` falls inside `range`.  Line/column are 0-based
/// (matching tree-sitter and internal storage convention).
fn range_contains(range: &TextRange, line: u32, column: u32) -> bool {
    if line < range.start_line || line > range.end_line {
        return false;
    }
    if line == range.start_line && column < range.start_column {
        return false;
    }
    if line == range.end_line && column > range.end_column {
        return false;
    }
    true
}

/// Extract range from a `ReferenceUse`.
fn extract_ref_range(r: &ReferenceUse) -> &TextRange {
    &r.range
}

/// Resolve a set of dataflow edges to `TraceDataNodeRef`s, looking up each
/// node ID in the Store.
fn resolve_data_node_refs<F>(
    store: &Store,
    edges: &[crate::types::dataflow::DataFlowEdge],
    id_fn: F,
) -> anyhow::Result<Vec<TraceDataNodeRef>>
where
    F: Fn(&crate::types::dataflow::DataFlowEdge) -> &DataNodeId,
{
    let mut refs = Vec::new();
    for edge in edges {
        if let Some(node) = store.get_data_node(id_fn(edge))? {
            refs.push(TraceDataNodeRef::from_data_node(&node));
        }
    }
    Ok(refs)
}

/// Find the callsite at the given position.
///
/// Strategy 1: If the reference at this position is a call expression, look up
/// the corresponding callsite by reference ID.
///
/// Strategy 2 (fallback): If the innermost reference is NOT a call (e.g. user
/// clicked on an argument `x` inside `foo(x)`), scan all callsites in the file
/// and find one whose range contains the position.  This handles the common
/// case where the user clicks on a call argument, not the call target itself.
fn find_callsite_for_position(
    store: &Store,
    reference: &Option<&ReferenceUse>,
    file_id: &FileId,
    line: u32,
    column: u32,
) -> anyhow::Result<Option<Callsite>> {
    // Strategy 1: direct lookup via reference
    if let Some(r) = reference {
        if r.kind == ReferenceKind::Call {
            if let Some(cs) = store.find_callsite_by_reference_id(&r.id)? {
                return Ok(Some(cs));
            }
        }
    }

    // Strategy 2: fallback — find any callsite in this file whose range
    // contains the position (handles clicking on arguments inside a call)
    let callsites = store.find_callsites_by_file(file_id)?;
    Ok(find_innermost_at_position(&callsites, |cs| &cs.range, line, column).cloned())
}

/// Find the binding or binding-use at the given position.
///
/// Looks through all bindings and binding uses in the file, returning the
/// one whose byte range most tightly contains the position.
fn find_binding_at_position(
    store: &Store,
    file_id: &FileId,
    line: u32,
    column: u32,
) -> anyhow::Result<(Option<BindingDef>, Option<BindingUse>)> {
    let bindings = store.find_bindings_by_file(file_id)?;
    let binding = find_innermost_at_position(&bindings, |b| &b.range, line, column).cloned();

    let uses = store.find_binding_uses_by_file(file_id)?;
    let binding_use = find_innermost_at_position(&uses, |u| &u.range, line, column).cloned();

    Ok((binding, binding_use))
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::enums::{ReferenceKind, ScopeKind};
    use crate::types::ids::ScopeId;
    use crate::types::structs::TextRange;

    #[test]
    fn range_contains_exact_position() {
        let range = TextRange {
            start_byte: 10,
            end_byte: 20,
            start_line: 5,
            start_column: 2,
            end_line: 5,
            end_column: 12,
        };
        assert!(range_contains(&range, 5, 2)); // start
        assert!(range_contains(&range, 5, 11)); // inside
        assert!(!range_contains(&range, 4, 10)); // before
        assert!(!range_contains(&range, 6, 1)); // after
        assert!(!range_contains(&range, 5, 1)); // before column
        assert!(!range_contains(&range, 5, 13)); // after column
    }

    #[test]
    fn find_innermost_selects_smallest_range() {
        let outer = TestItem {
            id: 1,
            range: TextRange {
                start_byte: 0,
                end_byte: 100,
                start_line: 1,
                start_column: 1,
                end_line: 10,
                end_column: 1,
            },
        };
        let inner = TestItem {
            id: 2,
            range: TextRange {
                start_byte: 40,
                end_byte: 60,
                start_line: 4,
                start_column: 2,
                end_line: 6,
                end_column: 3,
            },
        };
        let items = vec![outer, inner];
        let found = find_innermost_at_position(&items, |i| &i.range, 5, 5);
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, 2); // inner selected
    }

    #[derive(Debug)]
    struct TestItem {
        id: u32,
        range: TextRange,
    }
}
