//! Alias table: resolves local-to-local and local-to-field chains to
//! canonicalize field paths in SemanticEffect entries.
//!
//! For example, when `d = data` and then `d->state.aptr.cookiehost` is
//! observed, the alias table resolves the canonical path as
//! `data.state.aptr.cookiehost`.
//!
//! # Algorithm
//!
//! 1. Scan DataFlow `Assign` and `FieldLoad` edges to build a
//!    `Local → AliasTarget` map.
//! 2. Compute transitive closure via fixpoint iteration (bounded by
//!    `MAX_ITERATIONS`).
//! 3. Given a field path `"d.state.aptr.cookiehost"`, split at `.`/`->`,
//!    resolve the first segment against the alias table, and
//!    re-assemble.
//!
//! # Limitations (known, deferred)
//!
//! - Only intra-procedural (per-function).
//! - Does not handle field-to-field aliasing (`x.a = y.b`).
//! - Does not handle pointer arithmetic (`p + offset`).

use std::collections::{HashMap, HashSet};

use types::dataflow::DataFlowEdge;
use types::enums::DataFlowKind;

/// Canonical target of a local alias.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AliasTarget {
    /// Aliased directly (or transitively) to another local.
    Local { name: String },
    /// Aliased to a field access on another local or struct.
    Field { path: String },
}

/// Per-function alias table.
///
/// Maps local-variable names to their canonical alias targets,
/// resolved transitively.
#[derive(Debug, Clone, Default)]
pub struct AliasTable {
    /// `local_name → AliasTarget` after transitive closure.
    mapping: HashMap<String, AliasTarget>,
}

impl AliasTable {
    /// Maximum fixpoint iterations (safety bound).
    const MAX_ITERATIONS: usize = 20;

    /// Build an alias table from DataFlow edges for a single function.
    ///
    /// Consumes the `Assign` (local ← local) and `FieldLoad`
    /// (local ← field) edges.
    pub fn build(edges: &[DataFlowEdge]) -> Self {
        let raw: HashMap<String, HashSet<AliasTarget>> = HashMap::new();

        for edge in edges {
            match edge.kind {
                // local = other_local   (DataFlow: Assign from local to local)
                DataFlowKind::Assign => {
                    // The target of an Assign edge is the LHS; the source is
                    // the RHS.  We need both source and target names.
                    // DataNode IDs don't carry names directly, so we rely
                    // on the caller to provide name lookups.
                    //
                    // This function receives edges only; the name resolution
                    // is done at the call site in `build_with_names`.
                }
                // local = field_expr   (DataFlow: FieldLoad produces a local)
                DataFlowKind::FieldLoad => {
                    // Similar: target is the local, source is the field path.
                }
                _ => {}
            }
        }

        // Fixpoint transitive closure
        let mut table = AliasTable {
            mapping: HashMap::new(),
        };

        for (local, targets) in &raw {
            if let Some(first) = targets.iter().next() {
                table.mapping.insert(local.clone(), first.clone());
            }
        }

        // Transitive closure: if a → b and b → c, then a → c
        let mut changed = true;
        let mut iterations = 0;
        while changed && iterations < Self::MAX_ITERATIONS {
            changed = false;
            iterations += 1;

            let snapshot: Vec<(String, AliasTarget)> = table
                .mapping
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();

            for (local, target) in &snapshot {
                match target {
                    AliasTarget::Local {
                        name: aliased_local,
                    } => {
                        if let Some(next) = table.mapping.get(aliased_local) {
                            if next != target {
                                table.mapping.insert(local.clone(), next.clone());
                                changed = true;
                            }
                        }
                    }
                    AliasTarget::Field { .. } => {}
                }
            }
        }

        table
    }

    /// Build an alias table using DataFlow edges and name-resolution closures.
    ///
    /// `edge_source_name(id)` returns the variable/field name for a `DataNodeId`.
    /// `edge_target_name(id)` returns the variable name for the target node.
    pub fn build_with_names<F, G>(edges: &[DataFlowEdge], source_name: F, target_name: G) -> Self
    where
        F: Fn(types::ids::DataNodeId) -> Option<String>,
        G: Fn(types::ids::DataNodeId) -> Option<String>,
    {
        let mut raw: HashMap<String, Vec<AliasTarget>> = HashMap::new();

        for edge in edges {
            match edge.kind {
                DataFlowKind::Assign => {
                    // target (LHS) = source (RHS)
                    let lhs = match target_name(edge.target) {
                        Some(n) => n,
                        None => continue,
                    };
                    let rhs_name = match source_name(edge.source) {
                        Some(n) => n,
                        None => continue,
                    };
                    raw.entry(lhs)
                        .or_default()
                        .push(AliasTarget::Local { name: rhs_name });
                }
                DataFlowKind::FieldLoad => {
                    // local = field.path
                    let lhs = match target_name(edge.target) {
                        Some(n) => n,
                        None => continue,
                    };
                    let field_path = match source_name(edge.source) {
                        Some(p) => p,
                        None => continue,
                    };
                    raw.entry(lhs)
                        .or_default()
                        .push(AliasTarget::Field { path: field_path });
                }
                _ => {}
            }
        }

        let mut table = AliasTable {
            mapping: HashMap::new(),
        };
        for (local, targets) in &raw {
            if let Some(first) = targets.first() {
                table.mapping.insert(local.clone(), first.clone());
            }
        }

        // Transitive closure
        let mut changed = true;
        let mut iterations = 0;
        while changed && iterations < Self::MAX_ITERATIONS {
            changed = false;
            iterations += 1;
            let snapshot: Vec<(String, AliasTarget)> = table
                .mapping
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (local, target) in &snapshot {
                if let AliasTarget::Local {
                    name: aliased_local,
                } = target
                {
                    if let Some(next) = table.mapping.get(aliased_local) {
                        if next != target {
                            table.mapping.insert(local.clone(), next.clone());
                            changed = true;
                        }
                    }
                }
            }
        }

        table
    }

    /// Resolve a field path through the alias table.
    ///
    /// Splits the path at `.` and `->`, resolves the first segment,
    /// and reassembles.  Returns the original path if no alias is found.
    ///
    /// # Examples
    /// ```
    /// // If "d" → AliasTarget::Local { name: "data" }:
    /// // "d->state.aptr.cookiehost" → "data->state.aptr.cookiehost"
    /// ```
    pub fn resolve_field_path(&self, path: &str) -> String {
        if self.mapping.is_empty() {
            return path.to_string();
        }

        // Split on the first separator (-> or .)
        let (first, rest) = if let Some(idx) = path.find("->") {
            (&path[..idx], &path[idx..])
        } else if let Some(idx) = path.find('.') {
            (&path[..idx], &path[idx..])
        } else {
            // No separator — this is just a local name
            let resolved = self.resolve_local(path);
            return resolved.unwrap_or_else(|| path.to_string());
        };

        let resolved_first = self
            .resolve_local(first)
            .unwrap_or_else(|| first.to_string());
        format!("{resolved_first}{rest}")
    }

    /// Resolve a single local name to its canonical alias target.
    fn resolve_local(&self, name: &str) -> Option<String> {
        match self.mapping.get(name) {
            Some(AliasTarget::Local { name: aliased }) => Some(aliased.clone()),
            Some(AliasTarget::Field { path }) => Some(path.clone()),
            None => None,
        }
    }

    /// Returns the number of alias entries in the table.
    pub fn len(&self) -> usize {
        self.mapping.len()
    }

    /// Returns whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.mapping.is_empty()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use types::enums::DataFlowKind;
    use types::ids::DataNodeId;
    use types::ids::SymbolId;

    fn make_edge(source: u64, target: u64, kind: DataFlowKind) -> DataFlowEdge {
        let file_id = types::ids::FileId::default();
        let fid = SymbolId::default();
        let src_id = DataNodeId::generate(
            &file_id,
            Some(&fid),
            "test",
            Some(&source.to_string()),
            None,
            source as u32,
        );
        let tgt_id = DataNodeId::generate(
            &file_id,
            Some(&fid),
            "test",
            Some(&target.to_string()),
            None,
            target as u32,
        );
        DataFlowEdge {
            id: types::ids::DataFlowEdgeId::default(),
            source: src_id,
            target: tgt_id,
            kind,
            location: types::structs::TextRange {
                start_byte: 0,
                end_byte: 0,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
            confidence: 0.9,
        }
    }

    /// Helper to generate a deterministic DataNodeId for name lookups.
    fn make_dn_id(seq: u64) -> DataNodeId {
        let file_id = types::ids::FileId::default();
        let fid = SymbolId::default();
        DataNodeId::generate(
            &file_id,
            Some(&fid),
            "test",
            Some(&seq.to_string()),
            None,
            seq as u32,
        )
    }

    #[test]
    fn test_empty_table_resolves_unchanged() {
        let table = AliasTable::default();
        assert_eq!(
            table.resolve_field_path("data.state.aptr.cookiehost"),
            "data.state.aptr.cookiehost"
        );
    }

    #[test]
    fn test_local_to_local_alias() {
        let edges = vec![
            // aptr = data.state.aptr
            make_edge(1, 2, DataFlowKind::FieldLoad),
        ];

        let source_names: HashMap<DataNodeId, String> = {
            let mut m = HashMap::new();
            m.insert(make_dn_id(1), "data.state.aptr".to_string());
            m
        };
        let target_names: HashMap<DataNodeId, String> = {
            let mut m = HashMap::new();
            m.insert(make_dn_id(2), "aptr".to_string());
            m
        };

        let table = AliasTable::build_with_names(
            &edges,
            |id| source_names.get(&id).cloned(),
            |id| target_names.get(&id).cloned(),
        );

        assert_eq!(
            table.resolve_field_path("aptr.cookiehost"),
            "data.state.aptr.cookiehost"
        );
    }

    #[test]
    fn test_transitive_closure() {
        // a = b; b = c  =>  a = c
        let edges = vec![
            make_edge(1, 2, DataFlowKind::Assign), // a = b
            make_edge(3, 4, DataFlowKind::Assign), // b = c
        ];

        let source_names: HashMap<DataNodeId, String> = {
            let mut m = HashMap::new();
            m.insert(make_dn_id(1), "b".to_string());
            m.insert(make_dn_id(3), "c".to_string());
            m
        };
        let target_names: HashMap<DataNodeId, String> = {
            let mut m = HashMap::new();
            m.insert(make_dn_id(2), "a".to_string());
            m.insert(make_dn_id(4), "b".to_string());
            m
        };

        let table = AliasTable::build_with_names(
            &edges,
            |id| source_names.get(&id).cloned(),
            |id| target_names.get(&id).cloned(),
        );

        // a should resolve transitively to c
        assert_eq!(table.resolve_field_path("a->field"), "c->field");
    }
}
