//! Extractor: orchestrates tree-sitter parsing + slot-based normalization → FileFacts.
//!
//! The extractor:
//! 1. Parses source code with tree-sitter
//! 2. Runs the queries (definitions, references, imports, scopes)
//! 3. Calls `normalize()` on each capture via the frontend slots
//! 4. Assembles FileFacts (structural edges left to resolver phase)

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::Parser;

use types::Language;
use types::bindings::{BindingDef, BindingUse};
use types::ids::{BindingUseId, CallsiteId, FileId, ScopeId};
use types::{
    ArgumentFact, Callsite, DataNodeKind, DiagnosticLevel, ExtractDiagnostic, FileFacts, FileInfo,
    ParseStatus, ReferenceKind, ScopeDef, ScopeKind, SymbolKind, TextRange,
};

use super::callsite_spec::CallsiteParts;
use super::cfg_builder::CfgResult;
use super::dataflow_builder::{DataFlowBuilder, DataFlowResult};
use super::error::{ExtractionFailure, ExtractionFailureKind};
use super::frontend::{Capture, LanguageFrontend, NormalizeCtx};
use super::languages::node_range;
use super::lexical_binder::LexicalBindingResult;
use super::semantic_binder::SemanticBinder;

// ── Per-file extraction context ───────────────────────────────────────────
// Imported from extraction_ctx.rs to avoid reverse-dependency issues.

use crate::extraction_ctx::ExtractionCtx;

// ── P2: Thread-local tree-sitter parser ──────────────────────────────────
//
// Creating a new `tree_sitter::Parser` per file is expensive (alloc + init).
// Thread-local storage reuses one parser per Rayon worker thread.

thread_local! {
    static TL_PARSER: std::cell::RefCell<Option<Parser>> = const { std::cell::RefCell::new(None) };
}

/// Get (or create) a thread-local parser set to the given language.
/// The parser is reset and ready for a new parse.
fn tl_parse(
    ts_lang: &tree_sitter::Language,
    source_bytes: &[u8],
    file_path: &Path,
    language: Language,
) -> Result<tree_sitter::Tree> {
    TL_PARSER.with(|cell| {
        let mut opt = cell.borrow_mut();
        if opt.is_none() {
            *opt = Some(Parser::new());
        }
        let parser = opt.as_mut().expect("TL_PARSER was just initialized above");
        parser.set_language(ts_lang).map_err(|e| {
            anyhow::Error::new(ExtractionFailure {
                kind: ExtractionFailureKind::ParserInit,
                file_path: file_path.to_string_lossy().to_string(),
                language,
                slot: None,
                message: format!("Failed to set tree-sitter language: {e}"),
            })
        })?;
        parser.parse(source_bytes, None).ok_or_else(|| {
            anyhow::Error::new(ExtractionFailure {
                kind: ExtractionFailureKind::ParserInit,
                file_path: file_path.to_string_lossy().to_string(),
                language,
                slot: None,
                message: "Failed to parse source (returned None)".into(),
            })
        })
    })
}

/// Extract a single file's facts using the given language frontend.
pub fn extract_file(
    frontend: &LanguageFrontend,
    file_id: FileId,
    file_path: &Path,
    source: &str,
    content_hash: &str,
) -> Result<FileFacts> {
    let mut diagnostics = Vec::new();

    // 1. Parse (P2: uses thread-local parser to avoid per-file alloc)
    let ts_lang = frontend.parser.tree_sitter_language();
    let language = frontend.language();
    let source_bytes = source.as_bytes();
    let tree = tl_parse(&ts_lang, source_bytes, file_path, language)?;
    let root = tree.root_node();

    if root.has_error() {
        diagnostics.push(ExtractDiagnostic {
            level: DiagnosticLevel::Warning,
            message: "Parse errors detected (extraction best-effort)".into(),
            range: None,
        });
    }

    // Bundle per-file context so helpers take one struct instead of 6-9 args.
    let ectx = ExtractionCtx {
        ts_lang: &ts_lang,
        root,
        source,
        file_id,
        file_path,
        language,
    };

    // 2. Extract and normalize definitions
    let mut symbols = extract_and_normalize(
        &ectx,
        frontend.symbols.definition_query(),
        &mut diagnostics,
        "symbols",
        |ctx, capture| frontend.symbols.normalize(ctx, capture),
    )?;

    // 3. Extract and normalize references
    let mut references = extract_and_normalize(
        &ectx,
        frontend.references.reference_query(),
        &mut diagnostics,
        "references",
        |ctx, capture| frontend.references.normalize(ctx, capture),
    )?;

    // 4. Extract and normalize imports
    let imports = extract_and_normalize(
        &ectx,
        frontend.imports.import_query(),
        &mut diagnostics,
        "imports",
        |ctx, capture| frontend.imports.normalize(ctx, capture),
    )?;

    // 5. Extract and normalize scopes
    let mut scopes = extract_and_normalize(
        &ectx,
        frontend.scopes.scope_query(),
        &mut diagnostics,
        "scopes",
        |ctx, capture| frontend.scopes.normalize(ctx, capture),
    )?;

    // 6. Raw edges are now populated downstream by GraphBuilder (new P3 path).
    //    Old normalize_dataflow path was removed in favor of DataFlowBuilder.
    let mut raw_edges = Vec::new();

    // 7. Build scope tree and assign containers
    super::build_scope_tree(&mut scopes, &mut symbols);

    // 7z. Expand function-like symbol ranges to match their function scope.
    //     definitions.scm captures only the name identifier (e.g. "compute"),
    //     but resolve_dataflow_function_ids() needs the full function body range
    //     to assign function_id to DataNodes inside the body.
    for sym in symbols.iter_mut() {
        if matches!(
            sym.kind,
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
        ) {
            // Find the innermost function/method scope that contains this symbol.
            let containing = scopes
                .iter()
                .filter(|s| {
                    matches!(s.kind, ScopeKind::Function | ScopeKind::Method)
                        && s.range.start_byte <= sym.range.end_byte
                        && s.range.end_byte >= sym.range.start_byte
                })
                .min_by_key(|s| s.range.end_byte - s.range.start_byte); // tightest scope
            if let Some(scope) = containing {
                sym.range.start_byte = scope.range.start_byte;
                sym.range.end_byte = scope.range.end_byte;
                sym.range.start_line = scope.range.start_line;
                sym.range.end_line = scope.range.end_line;
                sym.range.start_column = scope.range.start_column;
                sym.range.end_column = scope.range.end_column;
            }
        }
    }

    // 7a. Extract lexical bindings (P7: skip if unsupported)
    let (bindings, binding_uses) = if frontend.lexical.capability().is_supported() {
        let lexical_result = super::lexical_binder::LexicalBinder::extract(
            frontend.lexical.as_ref(),
            &ectx,
            &scopes,
            &symbols,
        )
        .unwrap_or_else(|e| {
            diagnostics.push(ExtractDiagnostic {
                level: DiagnosticLevel::Warning,
                message: format!("Lexical binding extraction failed: {e}"),
                range: None,
            });
            LexicalBindingResult {
                bindings: vec![],
                uses: vec![],
            }
        });
        (lexical_result.bindings, lexical_result.uses)
    } else {
        (vec![], vec![])
    };

    // 7b. Build dataflow graph (P7: skip if unsupported)
    let (mut data_nodes, dataflow_edges) = if frontend.dataflow.capability().is_supported() {
        let dataflow_result = super::dataflow_builder::DataFlowBuilder::extract(
            frontend.dataflow.as_ref(),
            &ectx,
            &bindings,
            &scopes,
        )
        .unwrap_or_else(|e| {
            diagnostics.push(ExtractDiagnostic {
                level: DiagnosticLevel::Warning,
                message: format!("DataFlow builder failed: {e}"),
                range: None,
            });
            DataFlowResult::default()
        });
        let mut nodes = dataflow_result.nodes;
        let edges = dataflow_result.edges;

        // 7d. Resolve DataNode function_ids BEFORE use-def so
        //     UseDefKey(function_id, binding_id?, name) groups correctly.
        super::dataflow_builder::resolve_dataflow_function_ids(&mut nodes, &symbols);

        // 7c. Build use-def edges (only if dataflow succeeded)
        let use_def_edges = DataFlowBuilder::resolve_use_def(&nodes);
        let mut all_edges = edges;
        all_edges.extend(use_def_edges);

        (nodes, all_edges)
    } else {
        (vec![], vec![])
    };

    // 7e. Build per-function control-flow graphs (P7: skip if CFG unsupported)
    let (cfg_nodes, cfg_edges) = if frontend
        .capability
        .supported_features
        .contains(&"cfg".to_string())
    {
        let cfg_result = super::cfg_builder::build_cfg_for_functions(root, &symbols, source_bytes)
            .unwrap_or_else(|e| {
                diagnostics.push(ExtractDiagnostic {
                    level: DiagnosticLevel::Warning,
                    message: format!("CFG builder failed: {e}"),
                    range: None,
                });
                CfgResult::default()
            });
        (cfg_result.nodes, cfg_result.edges)
    } else {
        (vec![], vec![])
    };

    // 8. Bind source ownership and scope through the semantic binder.
    // This is the single source of truth for references/dataflow/callsites:
    // adapters may produce best-effort source IDs, but only IDs present in
    // `symbols` are allowed to survive extraction.
    let binder = SemanticBinder::new(&symbols, &scopes);
    binder.bind_all(file_id, &mut references, &mut raw_edges);

    // 8a. Build identifier-use BindingUse records from the AST.
    //
    // The LexicalBinder (step 7a) only creates BindingUse records at
    // binding *declaration* sites.  This step scans all `(identifier)`
    // nodes and creates additional BindingUse records for usage sites,
    // resolved against the lexical binding table via scope-chain-aware
    // name lookup.  Declaration sites are skipped to avoid duplicates.
    let reference_binding_uses: Vec<BindingUse> =
        build_reference_binding_uses(&ectx, &bindings, &scopes).unwrap_or_else(|e| {
            diagnostics.push(ExtractDiagnostic {
                level: DiagnosticLevel::Warning,
                message: format!("Identifier-use binding scan failed: {e}"),
                range: None,
            });
            vec![]
        });

    // Merge declaration-site uses with identifier-use uses.
    let binding_uses: Vec<BindingUse> = {
        let mut all = binding_uses;
        all.extend(reference_binding_uses);
        all
    };

    // 9. Derive callsites from Call references
    //
    //    The reference range only covers the callee name (e.g. `inner`),
    //    but the callsite range should span the entire call expression
    //    (e.g. `inner(doubled)`) so that positions on arguments still
    //    match the callsite during fallback lookup.
    //
    //    We use the `frontend.callsites` slot, which delegates to the
    //    language-specific `CallsiteExtractorSpec`.  The spec handles
    //    all AST walking internally (including universal fallback for
    //    edge cases).  If the spec returns None, we fall back to using
    //    the reference range as the callsite range.
    let mut callsites: Vec<Callsite> = references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Call && r.source_symbol.is_some())
        .map(|r| {
            let caller = r.source_symbol.unwrap(); // safe: filter ensures Some
            let cs_id = CallsiteId::generate(&r.id, Some(&caller), r.range.start_byte);

            let parts = frontend.callsites.extract_callsite(
                root,
                r.range.start_byte as usize,
                r.range.end_byte as usize,
                source,
            );

            let (callsite_range, callee_range, receiver_fallback, argument_ranges, _call_kind) =
                if let Some(CallsiteParts {
                    call_range,
                    callee_range,
                    receiver_text,
                    argument_ranges,
                    call_kind,
                    ..
                }) = parts
                {
                    (
                        call_range,
                        Some(callee_range),
                        receiver_text,
                        argument_ranges,
                        Some(call_kind),
                    )
                } else {
                    // Spec couldn't find a call expression — fall back to reference range.
                    // This is a safety net; the spec's built-in universal fallback should
                    // handle all real-world call-expression node kinds.
                    let callee_range = Some(r.range);
                    (r.range, callee_range, None, Vec::new(), None)
                };

            let receiver = r.receiver.clone().or(receiver_fallback);
            let args: Vec<ArgumentFact> = argument_ranges
                .into_iter()
                .enumerate()
                .map(|(i, arg_range)| {
                    let value = source[arg_range.start_byte as usize..arg_range.end_byte as usize]
                        .to_string();
                    ArgumentFact {
                        index: i as u32,
                        name: None,
                        value,
                        range: Some(arg_range),
                        data_node_id: None,
                    }
                })
                .collect();

            Callsite {
                id: cs_id,
                reference_id: Some(r.id),
                caller,
                callee: None, // resolved later by the resolution pipeline
                receiver,
                args,
                range: callsite_range,
                callee_range,
            }
        })
        .collect();

    // 9a. Backfill ArgumentFact.data_node_id from DataNodes,
    //     and set DataNode.arg_index from ArgumentFact.index.
    for cs in &mut callsites {
        let provisional_cs_id = CallsiteId::from_file_byte(&file_id, cs.range.start_byte);
        let call_arg_node_indices: Vec<usize> = data_nodes
            .iter()
            .enumerate()
            .filter(|(_, dn)| {
                dn.kind == DataNodeKind::CallArg
                    && dn.callsite_id.as_ref() == Some(&provisional_cs_id)
            })
            .map(|(i, _)| i)
            .collect();
        if call_arg_node_indices.is_empty() {
            continue;
        }
        for arg in &mut cs.args {
            if let Some(arg_range) = arg.range {
                let arg_index = arg.index;
                for &idx in &call_arg_node_indices {
                    if data_nodes[idx].range.start_byte == arg_range.start_byte {
                        arg.data_node_id = Some(data_nodes[idx].id);
                        data_nodes[idx].arg_index = Some(arg_index);
                        break;
                    }
                }
            }
        }
    }

    // After backfill, rewrite all DataNode callsite_ids that used
    // provisional from_file_byte IDs to the real CallsiteId so
    // query-time joins (e.g., return-value bridge) work.
    //
    // Build a map: provisional from_file_byte ID → real Callsite.id
    let cs_id_map: std::collections::HashMap<
        types::ids::CallsiteId,
        types::ids::CallsiteId,
    > = callsites
        .iter()
        .map(|cs| {
            (
                types::ids::CallsiteId::from_file_byte(&file_id, cs.range.start_byte),
                cs.id,
            )
        })
        .collect();

    for dn in data_nodes.iter_mut() {
        if let Some(ref provisional) = dn.callsite_id {
            if let Some(real) = cs_id_map.get(provisional) {
                dn.callsite_id = Some(*real);
            }
        }
    }

    // 10. Collect exported symbol IDs
    let exports: Vec<_> = symbols
        .iter()
        .filter(|s| s.exported)
        .map(|s| s.id)
        .collect();

    // Determine parse status
    let status = if diagnostics
        .iter()
        .any(|d| d.level == DiagnosticLevel::Error)
    {
        ParseStatus::Error
    } else if !diagnostics.is_empty() {
        ParseStatus::Partial
    } else {
        ParseStatus::Success
    };

    let file_path_str = file_path.display().to_string().replace('\\', "/");

    Ok(FileFacts {
        file: FileInfo {
            file_id,
            path: file_path_str,
            language,
            content_hash: content_hash.to_string(),
            status,
        },
        symbols,
        scopes,
        references,
        imports,
        exports,
        raw_edges, // Symbol-level dataflow edges from normalize_dataflow (old path)
        callsites, // Derived from Call references (resolved later)
        diagnostics,
        bindings,       // lexical binding definitions
        binding_uses,   // lexical binding use sites
        data_nodes,     // per-function dataflow nodes
        dataflow_edges, // DataNode→DataNode dataflow edges
        cfg_nodes,      // per-function control-flow graph nodes
        cfg_edges,      // per-function control-flow graph edges
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Scan all `(identifier)` nodes in the AST and create [`BindingUse`] records
/// for usage sites (not declarations).
///
/// The LexicalBinder only creates `BindingUse` records at binding *declaration*
/// sites.  This function fills the gap by capturing every identifier node,
/// resolving it against the lexical binding table via scope-chain-aware name
/// lookup, and creating a `BindingUse` record.  Declaration-site identifiers
/// (those whose range is contained by a `BindingDef` range) are skipped to
/// avoid duplicates.
fn build_reference_binding_uses(
    ctx: &ExtractionCtx<'_>,
    bindings: &[BindingDef],
    scopes: &[ScopeDef],
) -> Result<Vec<BindingUse>> {
    // Capture every identifier node in the tree
    let captures = super::query_helpers::collect_captures(
        ctx.ts_lang,
        "(identifier) @binding.use",
        ctx.root,
        ctx.source_bytes(),
        "binding_uses",
    )
    .map_err(|failure| {
        let filled = ExtractionFailure {
            file_path: ctx.file_path.to_string_lossy().to_string(),
            language: ctx.language,
            ..failure
        };
        anyhow::Error::new(filled)
    })?;

    // Build scope → bindings map
    let mut scope_bindings: HashMap<ScopeId, Vec<&BindingDef>> = HashMap::new();
    for binding in bindings {
        scope_bindings
            .entry(binding.scope_id)
            .or_default()
            .push(binding);
    }

    // Build scope parent map from the scope tree
    let parent_map: HashMap<ScopeId, Option<ScopeId>> =
        scopes.iter().map(|s| (s.id, s.parent_id)).collect();

    // Collect binding declaration ranges (for filtering out decl sites)
    let binding_ranges: Vec<TextRange> = bindings.iter().map(|b| b.range).collect();

    let mut uses: Vec<BindingUse> = Vec::new();

    for (_capture_name, node) in captures {
        let range = node_range(node);

        // Skip declaration sites — these are already handled by LexicalBinder
        if binding_ranges
            .iter()
            .any(|br| br.start_byte <= range.start_byte && br.end_byte >= range.end_byte)
        {
            continue;
        }

        // Extract the identifier text
        let name = match super::languages::node_text(node, ctx.source) {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };

        // Find innermost scope containing this identifier
        let containing_scope: Option<ScopeId> = scopes
            .iter()
            .filter(|s| {
                s.range.start_byte <= range.start_byte && s.range.end_byte >= range.end_byte
            })
            .min_by_key(|s| s.range.byte_len())
            .map(|s| s.id);

        // Resolve the binding by walking the scope chain
        let binding_id = match containing_scope {
            Some(mut sid) => {
                let mut found = None;
                loop {
                    if let Some(bindings_in_scope) = scope_bindings.get(&sid) {
                        if let Some(b) = bindings_in_scope
                            .iter()
                            .find(|b| b.name.as_str() == name.as_str())
                        {
                            found = Some(b.id);
                            break;
                        }
                    }
                    let parent = parent_map.get(&sid).and_then(|&maybe_p| maybe_p);
                    match parent {
                        Some(pid) => sid = pid,
                        None => break,
                    }
                }
                found
            }
            None => None,
        };

        // Use containing scope or fall back to the first (file-level) scope
        let scope_id = match containing_scope
            .or_else(|| scopes.iter().find(|s| s.parent_id.is_none()).map(|s| s.id))
        {
            Some(sid) => sid,
            None => continue,
        };

        let use_id = BindingUseId::generate(
            &ctx.file_id,
            binding_id.as_ref(),
            None::<&types::ids::ReferenceId>,
            &name,
            range.start_byte,
        );

        uses.push(BindingUse {
            id: use_id,
            file_id: ctx.file_id,
            scope_id,
            binding_id,
            reference_id: None,
            name,
            range,
        });
    }

    Ok(uses)
}

/// Run a query and normalize each capture through the provided function.
fn extract_and_normalize<'a, T>(
    ctx: &ExtractionCtx<'a>,
    query_src: &str,
    diagnostics: &mut Vec<ExtractDiagnostic>,
    slot_name: &'static str,
    mut normalize: impl FnMut(NormalizeCtx<'a>, Capture<'a>) -> Option<T>,
) -> Result<Vec<T>> {
    let captures = super::query_helpers::collect_captures(
        ctx.ts_lang,
        query_src,
        ctx.root,
        ctx.source_bytes(),
        slot_name,
    )
    .map_err(|failure| {
        // Fill in file-level context that query_helpers doesn't have.
        let filled = ExtractionFailure {
            file_path: ctx.file_path.to_string_lossy().to_string(),
            language: ctx.language,
            ..failure
        };
        anyhow::Error::new(filled)
    })?;
    let mut results = Vec::new();
    let nctx = ctx.normalize_ctx();

    for (name, node) in captures {
        let capture = Capture {
            name: name.clone(),
            node,
        };
        match normalize(nctx, capture) {
            Some(item) => results.push(item),
            None => {
                let pos = node.start_position();
                diagnostics.push(ExtractDiagnostic {
                    level: DiagnosticLevel::Warning,
                    message: format!(
                        "Failed to normalize capture '{}' at line {}",
                        name,
                        pos.row + 1
                    ),
                    range: Some(TextRange {
                        start_byte: node.start_byte() as u32,
                        end_byte: node.end_byte() as u32,
                        start_line: pos.row as u32,
                        start_column: pos.column as u32,
                        end_line: node.end_position().row as u32,
                        end_column: node.end_position().column as u32,
                    }),
                });
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::LanguageFrontend;
    use crate::languages::create_frontend;
    use types::Language;
    use std::path::PathBuf;

    /// Helper: create a TypeScript LanguageFrontend for tests.
    #[cfg(feature = "typescript")]
    fn ts_frontend() -> LanguageFrontend {
        crate::languages::typescript::typescript_frontend()
    }
    /// Helper: create a Python LanguageFrontend for tests.
    #[cfg(feature = "python")]
    fn py_frontend() -> LanguageFrontend {
        crate::languages::python::python_frontend()
    }

    fn assert_sources_are_known(facts: &FileFacts) {
        let known: std::collections::HashSet<_> = facts.symbols.iter().map(|s| s.id).collect();
        for edge in &facts.raw_edges {
            assert!(
                known.contains(&edge.source),
                "raw edge has ghost source: {edge:?}"
            );
        }
        for callsite in &facts.callsites {
            assert!(
                known.contains(&callsite.caller),
                "callsite has ghost caller: {callsite:?}"
            );
        }
        for reference in &facts.references {
            if let Some(source) = reference.source_symbol {
                assert!(
                    known.contains(&source),
                    "reference has ghost source: {reference:?}"
                );
            }
        }
    }

    #[test]
    fn test_extract_and_insert_ts_arrow_function_registry_guard() {
        use db::Store;
        let source = r#"export const af = (x: number) => {
  const y = x;
  return y;
};
export function f() {
  return af(1);
}
[1].map(n => n + 1);
"#;
        let file_id = FileId::generate("arrow.ts");
        let frontend = ts_frontend();
        let file_path = PathBuf::from("arrow.ts");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert_sources_are_known(&facts);

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "javascript")]
    #[test]
    fn test_extract_and_insert_js_arrow_function_registry_guard() {
        use db::Store;
        let source = r#"export const jf = (x) => {
  const y = x;
  return y;
};
function g() {
  return jf(1);
}
"#;
        let file_id = FileId::generate("arrow.js");
        let frontend = create_frontend(Language::JavaScript).unwrap();
        let file_path = PathBuf::from("arrow.js");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert_eq!(facts.file.language, Language::JavaScript);
        assert_sources_are_known(&facts);

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "arkts")]
    #[test]
    fn test_extract_arkts_smoke() {
        let source = "function sayHello(name: string): void {\n  console.log(`Hello, ${name}`);\n}\nconst greeting: string = \"World\";\nsayHello(greeting);\n";
        let file_id = FileId::generate("test.ets");
        let frontend = create_frontend(Language::ArkTS).unwrap();
        let file_path = PathBuf::from("test.ets");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert_eq!(facts.file.language, Language::ArkTS);
        assert_sources_are_known(&facts);
        assert!(
            !facts.symbols.is_empty(),
            "Should have symbols: {:?}",
            facts.symbols
        );
        assert!(!facts.references.is_empty(), "Should have references");
    }

    #[cfg(feature = "cpp")]
    #[test]
    fn test_extract_and_insert_cpp_out_of_class_method_registry_guard() {
        use db::Store;
        let source = r#"#include <iostream>
namespace N {
class C {
public:
    void m();
    int field;
};
void C::m() {
    int x = 1;
    std::cout << x;
}
}
"#;
        let file_id = FileId::generate("out_of_class.cpp");
        let frontend = create_frontend(Language::Cpp).unwrap();
        let file_path = PathBuf::from("out_of_class.cpp");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert_sources_are_known(&facts);

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[test]
    fn test_extract_ts_simple() {
        let source = "const foo = 1;\nconsole.log(foo);\n";
        let file_id = FileId::generate("test.ts");
        let frontend = ts_frontend();
        let file_path = PathBuf::from("test.ts");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert_eq!(facts.file.path, "test.ts");
        assert_eq!(facts.file.language, Language::TypeScript);
        assert!(
            !facts.symbols.is_empty(),
            "Should have symbols: {:?}",
            facts.symbols
        );
        assert!(!facts.references.is_empty(), "Should have references");
    }

    #[test]
    fn test_extract_python_simple() {
        let source = "def foo():\n    return True\n\nfoo()\n";
        let file_id = FileId::generate("test.py");
        let frontend = py_frontend();
        let file_path = PathBuf::from("test.py");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert_eq!(facts.file.language, Language::Python);
        assert!(!facts.symbols.is_empty(), "Should have symbols");
    }

    #[test]
    fn test_extract_ts_dataflow() {
        // New P3 path: DataFlowBuilder produces DataNodes + DataFlowEdges
        let source =
            "function add(a: number, b: number) {\n  let result = a + b;\n  return result;\n}\n";
        let file_id = FileId::generate("test.ts");
        let frontend = ts_frontend();
        let file_path = PathBuf::from("test.ts");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert!(!facts.data_nodes.is_empty(), "Should have dataflow nodes");
        assert!(
            !facts.dataflow_edges.is_empty(),
            "Should have dataflow edges"
        );
    }

    #[test]
    fn test_extract_python_dataflow() {
        // Python adapter does not yet implement DataFlowBuilder.
        // This test verifies basic extraction still succeeds without the old
        // normalize_dataflow path.
        let source = "def add(a, b):\n    c = a + b\n    return c\n";
        let file_id = FileId::generate("test.py");
        let frontend = py_frontend();
        let file_path = PathBuf::from("test.py");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert!(!facts.symbols.is_empty(), "Should have symbols");
        assert!(facts.raw_edges.is_empty(), "Old dataflow path removed");
    }

    #[test]
    fn test_extract_and_insert_ts() {
        use db::Store;
        let source = "function add(a: number, b: number) {\n  return a + b;\n}\nadd(1, 2);\n";
        let file_id = FileId::generate("test.ts");
        let frontend = ts_frontend();
        let file_path = PathBuf::from("test.ts");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        println!("Symbols: {}", facts.symbols.len());
        for s in &facts.symbols {
            let sid = s.id.to_hex();
            println!(
                "  sym: {} ({}) qname={} id={}",
                s.name,
                s.kind.as_str(),
                s.qualified_name,
                &sid[..8]
            );
        }
        println!("References: {}", facts.references.len());
        println!("Dataflow edges: {}", facts.raw_edges.len());
        for e in &facts.raw_edges {
            let src = e.source.to_hex();
            let tgt = e.target.to_hex();
            println!(
                "  edge: {} -> {} ({})",
                &src[..8],
                &tgt[..8],
                e.kind.as_str()
            );
        }

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[test]
    fn test_extract_and_insert_ts_class() {
        use db::Store;
        let source = r#"export class Calculator {
  add(a: number, b: number): number {
    return a + b;
  }
}
const calc = new Calculator();
calc.add(1, 2);
"#;
        let file_id = FileId::generate("test.ts");
        let frontend = ts_frontend();
        let file_path = PathBuf::from("test.ts");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert!(!facts.symbols.is_empty());

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "java")]
    #[test]
    fn test_extract_and_insert_java() {
        use db::Store;
        let source = r#"import java.util.List;

public class UserService {
    private List<String> users;

    public String findById(String id) {
        return users.get(0);
    }

    public void save(String item) {
        users.add(item);
    }
}
"#;
        let file_id = FileId::generate("test.java");
        let frontend = create_frontend(Language::Java).unwrap();
        let file_path = PathBuf::from("test.java");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        println!("Java Symbols: {}", facts.symbols.len());
        for s in &facts.symbols {
            let sid = s.id.to_hex();
            println!(
                "  sym: {} ({}) qname={} id={}",
                s.name,
                s.kind.as_str(),
                s.qualified_name,
                &sid[..8]
            );
        }
        println!("References: {}", facts.references.len());
        println!("Imports: {}", facts.imports.len());
        println!("Scopes: {}", facts.scopes.len());
        println!("Dataflow edges: {}", facts.raw_edges.len());
        for e in &facts.raw_edges {
            let src = e.source.to_hex();
            let tgt = e.target.to_hex();
            println!(
                "  edge: {} -> {} ({})",
                &src[..8],
                &tgt[..8],
                e.kind.as_str()
            );
        }

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "c")]
    #[test]
    fn test_extract_and_insert_c() {
        use db::Store;
        let source = r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    char* name;
    char* email;
} User;

User* user_create(const char* name, const char* email) {
    User* u = (User*)malloc(sizeof(User));
    u->name = strdup(name);
    u->email = strdup(email);
    return u;
}

void user_free(User* u) {
    free(u->name);
    free(u->email);
    free(u);
}

char* user_greet(const User* u) {
    char* buf = (char*)malloc(256);
    snprintf(buf, 256, "Hello, %s!", u->name);
    return buf;
}
"#;
        let file_id = FileId::generate("test.c");
        let frontend = create_frontend(Language::C).unwrap();
        let file_path = PathBuf::from("test.c");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        println!("C Symbols: {}", facts.symbols.len());
        for s in &facts.symbols {
            let sid = s.id.to_hex();
            println!(
                "  sym: {} ({}) qname={} id={}",
                s.name,
                s.kind.as_str(),
                s.qualified_name,
                &sid[..8]
            );
        }
        println!("Dataflow edges: {}", facts.raw_edges.len());
        for e in &facts.raw_edges {
            let src = e.source.to_hex();
            let tgt = e.target.to_hex();
            println!(
                "  edge: {} -> {} ({})",
                &src[..8],
                &tgt[..8],
                e.kind.as_str()
            );
        }

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "cpp")]
    #[test]
    fn test_extract_and_insert_cpp() {
        use db::Store;
        let source = r#"#include <iostream>
#include <string>
#include <map>

class UserService {
public:
    std::string findById(const std::string& id) {
        auto it = users_.find(id);
        return it != users_.end() ? it->second : "";
    }

    void save(const std::string& key, const std::string& value) {
        users_[key] = value;
    }

private:
    std::map<std::string, std::string> users_;
};
"#;
        let file_id = FileId::generate("test.cpp");
        let frontend = create_frontend(Language::Cpp).unwrap();
        let file_path = PathBuf::from("test.cpp");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        println!("C++ Symbols: {}", facts.symbols.len());
        for s in &facts.symbols {
            let sid = s.id.to_hex();
            println!(
                "  sym: {} ({}) qname={} id={}",
                s.name,
                s.kind.as_str(),
                s.qualified_name,
                &sid[..8]
            );
        }
        println!("Dataflow edges: {}", facts.raw_edges.len());
        for e in &facts.raw_edges {
            let src = e.source.to_hex();
            let tgt = e.target.to_hex();
            println!(
                "  edge: {} -> {} ({})",
                &src[..8],
                &tgt[..8],
                e.kind.as_str()
            );
        }

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "cpp")]
    #[test]
    fn test_extract_cpp_e2e_fixture() {
        use db::Store;
        let source = r#"#include <iostream>
#include <string>
#include <map>
#include <memory>

class IRepository {
public:
    virtual ~IRepository() = default;
    virtual std::string findById(const std::string& id) = 0;
    virtual void save(const std::string& key, const std::string& value) = 0;
};

class User {
public:
    User(std::string name, std::string email)
        : name_(std::move(name)), email_(std::move(email)) {}

    std::string greet() const {
        return "Hello, " + name_ + "!";
    }

    const std::string& getName() const { return name_; }
    const std::string& getEmail() const { return email_; }

private:
    std::string name_;
    std::string email_;
};

class UserService : public IRepository {
public:
    std::string findById(const std::string& id) override {
        auto it = users_.find(id);
        return it != users_.end() ? it->second : "";
    }

    void save(const std::string& key, const std::string& value) override {
        users_[key] = value;
    }

private:
    std::map<std::string, std::string> users_;
};

int main() {
    auto svc = std::make_unique<UserService>();
    User user("John", "john@example.com");
    svc->save(user.getEmail(), user.getName());
    std::cout << user.greet() << std::endl;
    return 0;
}
"#;
        let file_id = FileId::generate("test.cpp");
        let frontend = create_frontend(Language::Cpp).unwrap();
        let file_path = PathBuf::from("test.cpp");

        let facts = extract_file(&frontend, file_id, &file_path, source, "abc").unwrap();
        println!("C++ E2E Symbols: {}", facts.symbols.len());
        for s in &facts.symbols {
            let sid = s.id.to_hex();
            println!(
                "  sym: {} ({}) qname={} id={}",
                s.name,
                s.kind.as_str(),
                s.qualified_name,
                &sid[..8]
            );
        }
        println!("Dataflow edges: {}", facts.raw_edges.len());
        for e in &facts.raw_edges {
            let src = e.source.to_hex();
            let tgt = e.target.to_hex();
            println!(
                "  edge: {} -> {} ({})",
                &src[..8],
                &tgt[..8],
                e.kind.as_str()
            );
        }

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }
}
