//! GraphBuilder — constructs symbol-level edges from resolved references.
//!
//! Separated from ReferenceResolver (P2): the resolver only produces
//! `(ReferenceUse, ResolvedTarget)` pairs; this module converts those
//! resolved facts into `RawEdge` objects and writes them to the store.
//!
//! Edge kinds produced (symbol-level only):
//! - Calls, Instantiates, Implements (from Call references)
//! - Extends (from Inheritance references)
//! - Implements (from Implementation references)
//! - References (from all other reference kinds)
//! - Contains (from container → child relationships discovered during resolution)

use std::sync::Arc;

use db::Store;
use rayon::prelude::*;
use types::*;
use types::enums::{DataFlowKind, DataNodeKind};

/// Builds symbol-level edges from resolved references.
///
/// This is the second half of the resolution pipeline:
/// 1. `ReferenceResolver` resolves references → `Vec<(ReferenceUse, ResolvedTarget)>`
/// 2. `GraphBuilder` converts those resolved targets → `Vec<RawEdge>`
pub struct GraphBuilder {
    store: Arc<Store>,
}

impl GraphBuilder {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Build all symbol-level edges from a batch of resolved references.
    ///
    /// P5: Edge creation is parallelized — each reference produces edges independently
    /// via Rayon. Results are collected and batch-inserted.
    pub fn build_all(&self, resolved: &[(ReferenceUse, ResolvedTarget)]) -> GraphBuilderStats {
        // P5: Parallel edge creation (each reference is independent).
        // Warnings are collected into a Mutex-protected Vec.
        let warnings: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

        let edges: Vec<RawEdge> = resolved
            .par_iter()
            .filter_map(|(reference, target)| {
                match self.create_edges_for_reference(reference, target) {
                    Ok(edges) => Some(edges),
                    Err(e) => {
                        if let Ok(mut w) = warnings.lock() {
                            w.push(format!(
                                "failed to create edges for reference {:?}: {}",
                                reference.id, e
                            ));
                        }
                        None
                    }
                }
            })
            .flatten()
            .collect();

        let edge_count = edges.len();
        let mut warnings: Vec<String> = warnings.into_inner().unwrap_or_default();

        // Write edges to store, tracking actual success
        let edges_written = if !edges.is_empty() {
            match self.store.batch_insert_edges(&edges) {
                Ok(()) => edge_count,
                Err(e) => {
                    warnings.push(format!(
                        "batch edge insert failed ({} edges): {}",
                        edge_count, e
                    ));
                    0 // actual written is 0 on failure
                }
            }
        } else {
            0
        };

        GraphBuilderStats {
            edges_built: edge_count,
            edges_written,
            warnings,
        }
    }

    /// Build edges scoped to specific files (lazy structural).
    ///
    /// Filters the resolved references to only those originating from
    /// `file_ids`, then delegates to the standard edge creation path.
    pub fn build_for_files(
        &self,
        resolved: &[(ReferenceUse, ResolvedTarget)],
        file_ids: &[FileId],
    ) -> GraphBuilderStats {
        let file_set: std::collections::HashSet<FileId> = file_ids.iter().copied().collect();
        let scoped: Vec<&(ReferenceUse, ResolvedTarget)> = resolved
            .iter()
            .filter(|(r, _)| file_set.contains(&r.file_id))
            .collect();

        if scoped.is_empty() {
            return GraphBuilderStats {
                edges_built: 0,
                edges_written: 0,
                warnings: Vec::new(),
            };
        }

        // Delegate to the same parallel edge-creation logic
        let warnings: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let edges: Vec<RawEdge> = scoped
            .par_iter()
            .filter_map(|(reference, target)| {
                match self.create_edges_for_reference(reference, target) {
                    Ok(edges) => Some(edges),
                    Err(e) => {
                        if let Ok(mut w) = warnings.lock() {
                            w.push(format!(
                                "failed to create edges for reference {:?}: {}",
                                reference.id, e
                            ));
                        }
                        None
                    }
                }
            })
            .flatten()
            .collect();

        // Post-process: detect callback registrations and create RegistersCallback edges
        let callback_edges = Self::detect_callback_registrations(&edges, &self.store);
        let all_edges: Vec<RawEdge> = edges
            .into_iter()
            .chain(callback_edges)
            .collect();

        let edge_count = all_edges.len();
        let mut warnings: Vec<String> = warnings.into_inner().unwrap_or_default();

        let edges_written = if !all_edges.is_empty() {
            match self.store.batch_insert_edges(&all_edges) {
                Ok(()) => edge_count,
                Err(e) => {
                    warnings.push(format!(
                        "batch edge insert failed ({} edges): {}",
                        edge_count, e
                    ));
                    0
                }
            }
        } else {
            0
        };

        GraphBuilderStats {
            edges_built: edge_count,
            edges_written,
            warnings,
        }
    }

    /// Create structural edges from a resolved reference.
    ///
    /// Produces:
    /// - `Calls` when a call reference resolves to a function/method/constructor
    /// - `Instantiates` when a call reference resolves to a class/struct
    /// - `Implements` when a call reference resolves to an interface/trait
    /// - `Extends` when an inheritance reference resolves to a class
    /// - `Implements` when an implementation reference resolves to an interface/trait
    /// - `References` for non-call references
    /// - `Contains` from container → child when the target has a container
    fn create_edges_for_reference(
        &self,
        reference: &ReferenceUse,
        target: &ResolvedTarget,
    ) -> anyhow::Result<Vec<RawEdge>> {
        let mut edges = Vec::new();

        // Look up target symbol from the DB (supports cross-file targets)
        let target_sym = match self.store.find_symbol_by_id(&target.symbol_id)? {
            Some(s) => s,
            None => return Ok(edges),
        };

        // Source is the enclosing function/class that contains the reference
        let source = match reference.source_symbol {
            Some(s) => s,
            None => return Ok(edges),
        };

        let edge_kind = if reference.kind == ReferenceKind::Call {
            match target_sym.kind {
                SymbolKind::Class | SymbolKind::Struct => EdgeKind::Instantiates,
                SymbolKind::Interface | SymbolKind::Trait => EdgeKind::Implements,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => {
                    EdgeKind::Calls
                }
                SymbolKind::Variable => {
                    // Function pointer call: try to resolve the actual callee
                    // via local def-use dataflow chain.
                    // e.g. void (*fp)(int) = &handler; fp(42);
                    match try_resolve_function_pointer(&self.store, reference) {
                        Ok(Some(resolved)) => {
                            let resolved_target = ResolvedTarget {
                                symbol_id: resolved,
                                confidence: Confidence::certain() * 0.9f32, // penalty for indirect resolution
                                strategy: ResolutionStrategy::DataflowPointer,
                                provenance: target.provenance,
                            };
                            let mut pointer_edges = self.create_edges_for_reference(
                                reference,
                                &resolved_target,
                            )?;
                            edges.append(&mut pointer_edges);
                            return Ok(edges);
                        }
                        Ok(None) => return Ok(edges),
                        Err(_) => return Ok(edges),
                    }
                }
                _ => return Ok(edges), // Non-callable target — no structural edge
            }
        } else if reference.kind == ReferenceKind::Inheritance {
            EdgeKind::Extends
        } else if reference.kind == ReferenceKind::Implementation {
            EdgeKind::Implements
        } else {
            EdgeKind::References
        };

        let mut edge = RawEdge::new(
            EdgeId::generate(
                &source,
                &target.symbol_id,
                edge_kind.as_str(),
                Some(&reference.id),
                target.provenance.as_str(),
            ),
            source,
            target.symbol_id,
            edge_kind,
            target.confidence,
            target.provenance,
        );
        edge.ref_id = Some(reference.id);
        edge.resolved_by = Some(target.strategy);

        // For call/instantiation edges, store the reference range as the edge
        // location so that caller-path steps can point to the actual call site.
        if matches!(
            edge_kind,
            EdgeKind::Calls | EdgeKind::Instantiates | EdgeKind::Implements
        ) {
            edge.location = Some(reference.range);

            // Connect the callsite to the resolved callee symbol.
            // The callsite was created during extraction with callee: None;
            // now that resolution has determined the target, update it.
            if let Err(e) = self
                .store
                .update_callsite_callee(&reference.id, &target.symbol_id)
            {
                tracing::warn!(
                    "Failed to update callsite callee for ref {:?}: {:?}",
                    reference.id,
                    e,
                );
            }
        }

        edges.push(edge);

        // Also create Contains edges from container symbols during resolution
        if let Some(container) = target_sym.container {
            if self.store.find_symbol_by_id(&container)?.is_some() {
                let mut contains_edge = RawEdge::new(
                    EdgeId::generate(
                        &container,
                        &target.symbol_id,
                        EdgeKind::Contains.as_str(),
                        Some(&reference.id),
                        Provenance::TreeSitter.as_str(),
                    ),
                    container,
                    target.symbol_id,
                    EdgeKind::Contains,
                    Confidence::certain(),
                    Provenance::TreeSitter,
                );
                contains_edge.ref_id = Some(reference.id);
                contains_edge.resolved_by = Some(target.strategy);
                edges.push(contains_edge);
            }
        }

        Ok(edges)
    }
}

/// Attempt to resolve a function pointer call through local def-use dataflow.
///
/// For code like:
/// ```c
/// void (*fp)(int) = &handler;
/// fp(42);
/// ```
/// the reference `fp` resolves to a `Variable` symbol (the function pointer),
/// not a `Function`.  This function follows the intra-procedural dataflow
/// graph from the CallTarget node backwards to find the actual function.
///
/// Algorithm: BFS up to depth 3 following incoming Assign/Read edges.
/// Returns `Some(SymbolId)` if a Function symbol is found, `None` if
/// the chain cannot be resolved.
fn try_resolve_function_pointer(
    store: &Arc<Store>,
    reference: &ReferenceUse,
) -> anyhow::Result<Option<SymbolId>> {
    // 1. Find the CallTarget DataNode at this reference position
    let file_nodes = store.find_data_nodes_by_file(&reference.file_id)?;
    let call_target = match file_nodes.iter().find(|n| {
        n.kind == DataNodeKind::CallTarget
            && n.range.start_byte == reference.range.start_byte
            && n.range.end_byte == reference.range.end_byte
    }) {
        Some(node) => node,
        None => return Ok(None),
    };

    // 2. BFS over incoming dataflow edges (up to depth 3)
    let mut visited: std::collections::HashSet<types::DataNodeId> =
        std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<(types::DataNodeId, usize)> =
        std::collections::VecDeque::new();

    queue.push_back((call_target.id, 0));
    visited.insert(call_target.id);

    while let Some((current_id, depth)) = queue.pop_front() {
        if depth > 3 {
            continue;
        }

        // Check all incoming edges to the current node
        let incoming = store.find_dataflow_edges_by_target(&current_id)?;
        for edge in &incoming {
            // Only follow edges that represent data flow (not call/return bridges)
            match edge.kind {
                DataFlowKind::Assign
                | DataFlowKind::Read
                | DataFlowKind::FieldLoad
                | DataFlowKind::Phi => {}
                _ => continue,
            }

            if visited.contains(&edge.source) {
                continue;
            }
            visited.insert(edge.source);

            // Look up the source node
            let source_node = match store.get_data_node(&edge.source)? {
                Some(node) => node,
                None => continue,
            };

            // Check if this source node's name corresponds to a Function symbol.
            // The source could be:
            // - An Expr node: `&handler` → name = "handler" (from pointer_expression capture)
            // - A VariableUse node: `handler` read from a variable → name = "handler"
            if let Some(ref name) = source_node.name {
                // Search for a Function symbol with this name
                if let Ok(candidates) = store.find_symbols_by_name(name) {
                    for sym in &candidates {
                        if sym.kind == SymbolKind::Function
                            && sym.file_id == reference.file_id
                        {
                            // Found a function match in the same file
                            return Ok(Some(sym.id));
                        }
                    }
                }
            }

            // Continue BFS from this source node
            queue.push_back((edge.source, depth + 1));
        }
    }

    Ok(None)
}

/// Statistics from a GraphBuilder run.
#[derive(Debug, Clone, Default)]
pub struct GraphBuilderStats {
    /// Number of edges built (before write to store).
    pub edges_built: usize,
    /// Number of edges actually written (0 on batch insert failure).
    pub edges_written: usize,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use extraction::LanguageFrontend;
    use extraction::create_frontend;
    use extraction::extract_file;
    use resolution::ReferenceResolver;
    use std::path::PathBuf;
    use types::Language;

    fn ts_frontend() -> LanguageFrontend {
        create_frontend(Language::TypeScript).unwrap()
    }

    /// Test that GraphBuilder produces edges from resolved references.
    #[test]
    fn test_graph_builder_basic() {
        let lib_src = r#"export function greet(name: string): string {
    return `Hello, ${name}!`;
}
"#;
        let lib_id = FileId::generate("lib.ts");
        let frontend = ts_frontend();
        let lib_facts = extract_file(&frontend, lib_id, &PathBuf::from("lib.ts"), lib_src, "abc")
            .expect("lib.ts extraction failed");

        let main_src = r#"import { greet } from './lib';

function main() {
    const msg = greet("World");
}
main();
"#;
        let main_id = FileId::generate("main.ts");
        let main_facts = extract_file(
            &frontend,
            main_id,
            &PathBuf::from("main.ts"),
            main_src,
            "abc",
        )
        .expect("main.ts extraction failed");

        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        store.insert_file_facts(&lib_facts).expect("insert lib.ts");
        store
            .insert_file_facts(&main_facts)
            .expect("insert main.ts");

        // Resolve
        let mut resolver = ReferenceResolver::new(store.clone());
        let (resolved, _res_stats) = resolver.resolve_all().expect("resolution failed");

        // Build edges
        let builder = GraphBuilder::new(store.clone());
        let stats = builder.build_all(&resolved);

        assert!(
            stats.edges_built > 0,
            "Expected >0 edges, got {}",
            stats.edges_built
        );
    }

    /// Test that `try_resolve_function_pointer` follows a local def-use
    /// chain from a CallTarget through an Assign edge to find the actual
    /// function.  Simulates `void (*fp)(void) = &handler; fp();`.
    #[test]
    fn test_resolve_function_pointer_via_dataflow() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();

        let file_id = FileId::generate("src/example.c");
        let range = TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };

        // Insert file
        store
            .upsert_file(&FileInfo {
                file_id,
                path: "src/example.c".into(),
                language: Language::C,
                content_hash: "abc".into(),
                status: ParseStatus::Success,
            })
            .unwrap();

        // Create function symbol "handler" and variable symbol "fp"
        let handler_id = SymbolId::generate(&file_id, "c", "handler", "function", None);
        let handler_sym = SymbolDef {
            id: handler_id,
            kind: SymbolKind::Function,
            name: "handler".to_string(),
            qualified_name: "handler".to_string(),
            symbol_path: vec!["handler".to_string()],
            file_id,
            language: Language::C,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        };

        let fp_id = SymbolId::generate(&file_id, "c", "fp", "variable", None);
        let fp_sym = SymbolDef {
            id: fp_id,
            kind: SymbolKind::Variable,
            name: "fp".to_string(),
            qualified_name: "fp".to_string(),
            symbol_path: vec!["fp".to_string()],
            file_id,
            language: Language::C,
            range,
            name_range: range,
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        };

        store
            .insert_file_facts(&FileFacts {
                file: FileInfo {
                    file_id,
                    path: "src/example.c".into(),
                    language: Language::C,
                    content_hash: "abc".into(),
                    status: ParseStatus::Success,
                },
                symbols: vec![handler_sym, fp_sym.clone()],
                ..Default::default()
            })
            .unwrap();

        // Create data nodes: CallTarget "fp" and Expr "&handler"
        // Ranges must match the reference position for lookup.
        let call_range = TextRange {
            start_byte: 100,
            end_byte: 102,
            start_line: 5,
            start_column: 1,
            end_line: 5,
            end_column: 3,
        };
        let expr_range = range; // reuse the default range

        let call_target_id = DataNodeId::generate(
            &file_id,
            Some(&fp_id),
            "call_target",
            Some("fp"),
            Some("fp"),
            call_range.start_byte,
        );
        let expr_id = DataNodeId::generate(
            &file_id,
            Some(&fp_id),
            "expr",
            Some("handler"),
            Some("handler"),
            50,
        );

        let call_target = DataNode::call_target(
            call_target_id,
            file_id,
            Some(fp_id),
            None,
            "fp",
            "fp",
            call_range,
        );
        let expr_node = DataNode {
            id: expr_id,
            file_id,
            function_id: Some(fp_id),
            kind: DataNodeKind::Expr,
            binding_id: None,
            callsite_id: None,
            name: Some("handler".to_string()),
            access_path: Some("handler".to_string()),
            arg_index: None,
            range: expr_range,
        };

        // Create an Assign edge: Expr("handler") → CallTarget("fp")
        let edge_id = DataFlowEdgeId::generate(&expr_id, &call_target_id, "assign");
        let assign_edge = DataFlowEdge {
            id: edge_id,
            source: expr_id,
            target: call_target_id,
            kind: DataFlowKind::Assign,
            location: range,
            confidence: 1.0,
        };

        // Write data nodes + edge via lazy build path
        store
            .insert_data_nodes(&[call_target, expr_node])
            .unwrap();
        store
            .insert_dataflow_edges(&[assign_edge])
            .unwrap();

        // Create a reference at the CallTarget position resolving to `fp`
        let ref_id = ReferenceId::generate(
            &file_id,
            Some(&fp_id), // source_symbol = enclosing function
            100,          // start_byte of call_target
            102,          // end_byte
            "fp",
            ReferenceKind::Call,
        );
        let reference = ReferenceUse {
            id: ref_id,
            file_id,
            source_symbol: Some(fp_id),
            scope_id: None,
            kind: ReferenceKind::Call,
            text: "fp".to_string(),
            name: "fp".to_string(),
            receiver: None,
            arity: Some(0),
            range: TextRange {
                start_byte: 100,
                end_byte: 102,
                start_line: 5,
                start_column: 1,
                end_line: 5,
                end_column: 3,
            },
            binding_id: None,
            resolved: None,
        };

        // Try to resolve via dataflow
        let result =
            try_resolve_function_pointer(&store, &reference).expect("resolution should not error");
        assert_eq!(
            result,
            Some(handler_id),
            "function pointer should resolve to handler"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Callback registration detection
// ───────────────────────────────────────────────────────────────────────────

/// Known callback registration patterns: (callee name contains pattern, callback arg index).
const CALLBACK_PATTERNS: &[(&str, usize)] = &[
    ("_set_", 1),                 // nghttp2_session_callbacks_set_*
    ("_callback", 1),             // set_callback(..., handler)
    ("pthread_create", 2),        // pthread_create(_, _, thread_fn, _)
    ("signal", 1),                // signal(SIGINT, handler)
    ("atexit", 0),               // atexit(cleanup)
    ("qsort", 3),                // qsort(base, n, sz, cmp)
    ("on_", 0),                  // on_click(handler), on_frame_recv(session, ...)
    ("add_listener", 1),          // add_listener(event, handler)
    ("register", 0),             // register_handler(handler)
];

impl GraphBuilder {
    /// After building standard call edges, scan for callback registration
    /// patterns and create `RegistersCallback` edges.
    fn detect_callback_registrations(
        edges: &[RawEdge],
        store: &Arc<Store>,
    ) -> Vec<RawEdge> {
        let mut result = Vec::new();

        for edge in edges {
            if edge.kind != EdgeKind::Calls {
                continue;
            }

            // Look up callee symbol name
            let callee_name = match store
                .find_symbol_by_id(&edge.target)
                .ok()
                .flatten()
                .map(|s| s.name)
            {
                Some(name) => name,
                None => continue,
            };

            // Check if callee matches any callback pattern
            let matching_pattern = CALLBACK_PATTERNS
                .iter()
                .find(|(pattern, _)| callee_name.contains(pattern));

            let (_pattern, arg_index) = match matching_pattern {
                Some(p) => p,
                None => continue,
            };

            // Try to resolve the callback argument to a function symbol.
            // Skip if callsite or data nodes are not available.
            let ref_id = match &edge.ref_id {
                Some(rid) => rid,
                None => continue,
            };

            let callsite = match store
                .find_callsite_by_reference_id(ref_id)
                .ok()
                .flatten()
            {
                Some(cs) => cs,
                None => continue,
            };

            // The callback is at args[arg_index]; look up its data_node_id
            let callback_dn = match callsite.args.get(*arg_index) {
                Some(arg) => arg,
                None => continue,
            };

            let callback_dn_id = match &callback_dn.data_node_id {
                Some(dn_id) => dn_id,
                None => continue,
            };

            let callback_node = match store.get_data_node(callback_dn_id).ok().flatten() {
                Some(dn) => dn,
                None => continue,
            };

            let callback_name = match &callback_node.name {
                Some(name) => name.clone(),
                None => continue,
            };

            // Attempt to find a function symbol matching the callback name.
            // Prefer symbols in the same file as the registrant.
            let candidates = match store.find_symbols_by_name(&callback_name) {
                Ok(syms) if !syms.is_empty() => syms,
                _ => continue,
            };

            // Get the file_id of the edge source (registrant function)
            let registrant_file = match store
                .find_symbol_by_id(&edge.source)
                .ok()
                .flatten()
                .map(|s| s.file_id)
            {
                Some(fid) => fid,
                None => continue,
            };

            let callback_sym = match candidates
                .iter()
                .find(|s| s.file_id == registrant_file)
                .or_else(|| candidates.first())
            {
                Some(sym) => sym,
                None => continue,
            };

            // Create the RegistersCallback edge: registrant → callback
            let rcb_edge = RawEdge::new(
                types::ids::EdgeId::generate(
                    &edge.source,
                    &callback_sym.id,
                    "registers_callback",
                    Some(ref_id),
                    "callback_pattern",
                ),
                edge.source.clone(),
                callback_sym.id,
                EdgeKind::RegistersCallback,
                types::Confidence::new(0.65),
                types::Provenance::CallbackPattern,
            );

            result.push(rcb_edge);
        }

        result
    }
}
