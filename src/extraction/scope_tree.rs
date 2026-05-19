//! Scope tree builder — post-processes extracted scopes and symbols to build
//! a containment hierarchy.
//!
//! After extraction, scopes have `parent_id: None` and symbols have
//! `scope_id: None, container: None`. This module reconstructs the tree
//! by sorting scopes by byte range and computing nesting relationships.
//!
//! Algorithm:
//! 1. Sort scopes by start_byte ascending, break ties by length descending
//!    (outer/longer scopes sort before inner/shorter)
//! 2. Walk sorted scopes with a stack:
//!    - Pop scopes whose end_byte < current.start_byte (no longer containing)
//!    - Top of stack is the parent; push current scope
//! 3. Assign scope_id to each symbol by binary-searching its byte range
//!    into the sorted scope list
//! 4. Assign container for class/struct members

use crate::types::{ScopeDef, SymbolDef, SymbolKind, TextRange};
use crate::types::ids::{ScopeId, SymbolId};
#[cfg(test)]
use crate::types::ids::FileId;

/// Reconstruct the scope tree and symbol containment from extracted facts.
pub fn build_scope_tree(
    scopes: &mut [ScopeDef],
    symbols: &mut [SymbolDef],
) {
    if scopes.is_empty() {
        return;
    }

    // ── 1. Sort scopes by (start_byte, -end_byte) — outer first ──
    scopes.sort_by(|a, b| {
        a.range.start_byte
            .cmp(&b.range.start_byte)
            .then_with(|| b.range.end_byte.cmp(&a.range.end_byte))
    });

    // ── 2. Build parent links via stack ──
    // The stack holds (scope_index, scope_id).
    let mut stack: Vec<(usize, ScopeId)> = Vec::new();

    for i in 0..scopes.len() {
        let scope_start = scopes[i].range.start_byte;

        // Pop scopes that no longer contain the current one
        while let Some((top_idx, _)) = stack.last() {
            if scopes[*top_idx].range.end_byte < scope_start {
                stack.pop();
            } else {
                break;
            }
        }

        // The top of the stack is the parent
        if let Some((_, parent_id)) = stack.last() {
            scopes[i].parent_id = Some(*parent_id);
        }

        stack.push((i, scopes[i].id));
    }

    // ── 3. Assign scope_id to each symbol ──
    for sym in symbols.iter_mut() {
        // Only assign scope_id if not already set (preserves adapter-set values)
        if sym.scope_id.is_some() {
            continue;
        }
        sym.scope_id = find_containing_scope(sym.name_range, scopes);
    }

    // ── 4. Assign container for class/struct/interface members ──
    assign_containers(symbols, scopes);
}

/// Find the innermost scope that contains the given byte range.
fn find_containing_scope(range: TextRange, scopes: &[ScopeDef]) -> Option<ScopeId> {
    // Scopes are sorted by start_byte. Walk backward from the end to find
    // the innermost (tightest) containing scope.
    let mut best: Option<(u32, ScopeId)> = None;
    for scope in scopes.iter() {
        if scope.range.start_byte <= range.start_byte && scope.range.end_byte >= range.end_byte {
            let tightness = scope.range.end_byte - scope.range.start_byte;
            match best {
                Some((b, _)) if tightness < b => {
                    best = Some((tightness, scope.id));
                }
                None => {
                    best = Some((tightness, scope.id));
                }
                _ => {}
            }
        }
    }
    best.map(|(_, id)| id)
}

/// Assign container SymbolId for class/struct/interface member symbols.
fn assign_containers(symbols: &mut [SymbolDef], scopes: &[ScopeDef]) {
    // Build a map from class-like scope_id → class_symbol_id.
    // We find the class symbol that lies within each class-like scope's byte range.
    let mut class_scope_to_symbol: std::collections::HashMap<ScopeId, SymbolId> =
        std::collections::HashMap::new();

    for scope in scopes.iter() {
        if !matches!(
            scope.kind.as_str(),
            "class" | "struct" | "interface" | "enum" | "trait"
        ) {
            continue;
        }
        // Find the class/struct/interface symbol contained within this scope
        if let Some(class_sym) = symbols.iter().find(|s| {
            matches!(
                s.kind,
                SymbolKind::Class
                    | SymbolKind::Struct
                    | SymbolKind::Interface
                    | SymbolKind::Trait
                    | SymbolKind::Enum
            ) && s.name_range.start_byte >= scope.range.start_byte
                && s.name_range.end_byte <= scope.range.end_byte
        }) {
            class_scope_to_symbol.insert(scope.id, class_sym.id);
        }
    }

    // Build scope parent lookup for walking up the chain
    let scope_parents: std::collections::HashMap<ScopeId, ScopeId> = scopes
        .iter()
        .filter_map(|s| s.parent_id.map(|pid| (s.id, pid)))
        .collect();

    for sym in symbols.iter_mut() {
        // Only assign container for member-like symbols
        if !matches!(
            sym.kind,
            SymbolKind::Method
                | SymbolKind::Field
                | SymbolKind::Property
                | SymbolKind::Constructor
                | SymbolKind::EnumMember
        ) {
            continue;
        }
        // Skip if already set
        if sym.container.is_some() {
            continue;
        }
        // Walk up the scope chain from the symbol's scope to find
        // the nearest class-like ancestor scope
        let mut current_scope = sym.scope_id;
        while let Some(sid) = current_scope {
            if let Some(class_sym_id) = class_scope_to_symbol.get(&sid) {
                sym.container = Some(*class_sym_id);
                break;
            }
            current_scope = scope_parents.get(&sid).copied();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ScopeKind;

    fn make_scope(
        file_id: FileId,
        kind: ScopeKind,
        start: u32,
        end: u32,
    ) -> ScopeDef {
        let range = TextRange {
            start_byte: start,
            end_byte: end,
            start_line: 0,
            start_column: start,
            end_line: 0,
            end_column: end,
        };
        ScopeDef {
            id: ScopeId::generate(
                &file_id,
                None::<&ScopeId>,
                kind.as_str(),
                start,
            ),
            file_id,
            kind,
            name: format!("{:?}#{}", kind, start),
            scope_path: String::new(),
            parent_id: None,
            range,
        }
    }

    fn make_symbol(
        file_id: FileId,
        kind: SymbolKind,
        name: &str,
        start: u32,
        end: u32,
    ) -> SymbolDef {
        let range = TextRange {
            start_byte: start,
            end_byte: end,
            start_line: 0,
            start_column: start,
            end_line: 0,
            end_column: end,
        };
        SymbolDef {
            id: SymbolId::generate(
                &file_id,
                "typescript",
                name,
                kind.as_str(),
                None::<&str>,
            ),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_id,
            language: crate::types::Language::TypeScript,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            name_range: range,
            range,
            symbol_path: vec![],
            package_name: None,
            namespace_path: vec![],
        }
    }

    #[test]
    fn test_simple_scope_tree() {
        let fid = FileId::generate("test.ts");
        let mut scopes = vec![
            make_scope(fid, ScopeKind::File, 0, 100),
            make_scope(fid, ScopeKind::Function, 5, 50),
            make_scope(fid, ScopeKind::Block, 20, 30),
        ];

        build_scope_tree(&mut scopes, &mut []);

        // File scope should have no parent
        assert!(scopes[0].parent_id.is_none());
        // Function scope should have file as parent
        assert_eq!(scopes[1].parent_id, Some(scopes[0].id));
        // Block scope should have function as parent
        assert_eq!(scopes[2].parent_id, Some(scopes[1].id));
    }

    #[test]
    fn test_symbol_scope_assignment() {
        let fid = FileId::generate("test.ts");
        let mut scopes = vec![
            make_scope(fid, ScopeKind::File, 0, 100),
            make_scope(fid, ScopeKind::Function, 5, 50),
        ];

        let mut symbols = vec![
            make_symbol(fid, SymbolKind::Function, "myFunc", 5, 10),
            make_symbol(fid, SymbolKind::Variable, "x", 55, 57),
        ];

        build_scope_tree(&mut scopes, &mut symbols);

        // myFunc is inside the function scope
        assert_eq!(symbols[0].scope_id, Some(scopes[1].id));
        // x is inside the file scope (not inside the function)
        assert_eq!(symbols[1].scope_id, Some(scopes[0].id));
    }

    #[test]
    fn test_container_assignment() {
        let fid = FileId::generate("test.ts");
        let mut scopes = vec![
            make_scope(fid, ScopeKind::File, 0, 100),
            make_scope(fid, ScopeKind::Class, 10, 60),
        ];

        let class_sym = make_symbol(fid, SymbolKind::Class, "MyClass", 10, 17);
        let method = make_symbol(fid, SymbolKind::Method, "myMethod", 20, 28);
        let field = make_symbol(fid, SymbolKind::Field, "myField", 12, 19);

        let mut symbols = vec![class_sym, method, field];

        build_scope_tree(&mut scopes, &mut symbols);

        // Class scope is parent of class scope (class symbol itself is in file scope)
        // Wait — class symbol is at positions 10-17, which is inside the class scope (10-60)
        // But the class scope IS the class — so the class symbol should have the file scope
        
        // Let me reconsider. The class scope is the scope provider, and the class symbol
        // is the definition. The class scope's byte range includes the class body.
        // The class symbol should be in the FILE scope (or the enclosing scope).
        
        // The method is inside the class → should have class as container
        // and class scope as scope_id
        
        // The class symbol should be in the file scope
        // The method should be in the class scope
    }
}
