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
use tracing::info_span;
use tree_sitter::Parser;

use types::Language;
use types::bindings::{BindingDef, BindingUse};
use types::dataflow::{DataFlowEdge, DataNode};
use types::ids::{BindingUseId, CallsiteId, FileId, ScopeId};
use types::{
    ArgumentFact, Callsite, DataNodeKind, DiagnosticLevel, ExtractDiagnostic, FileFacts, FileInfo,
    ParseStatus, ReferenceKind, ScopeDef, ScopeKind, SymbolDef, SymbolKind, TextRange,
};

use super::callsite_spec::CallsiteParts;
use super::cancel::CancelCheck;
use super::cfg_builder::CfgResult;
use super::dataflow_builder::{DataFlowBuilder, DataFlowResult};
use super::error::{ExtractionFailure, ExtractionFailureKind};
use super::frontend::{Capture, LanguageFrontend, NormalizeCtx};
use super::languages::node_range;
use super::lexical_binder::LexicalBindingResult;
use super::mode::ExtractionMode;
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

/// Like [`extract_file_with_mode`] but cancellation-aware.
///
/// Checks `token.is_cancelled()` at strategic checkpoints and returns a typed
/// [`ExtractionFailureKind::Cancelled`] error if the budget is exhausted.
pub fn extract_file_with_mode(
    frontend: &LanguageFrontend,
    file_id: FileId,
    file_path: &Path,
    source: &str,
    content_hash: &str,
    mode: ExtractionMode,
    token: &dyn CancelCheck,
) -> Result<FileFacts> {
    let _span =
        info_span!(target: "atlas_extract", "extract.file", path = %file_path.display()).entered();
    let mut diagnostics = Vec::new();
    let language = frontend.language();

    // CP1: Check cancellation before expensive parse.
    if token.is_cancelled() {
        return Err(cancelled_error(file_path, language));
    }

    // 1. Parse (P2: uses thread-local parser to avoid per-file alloc)
    let ts_lang = frontend.parser.tree_sitter_language();
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
    // Use manifest_query() for Manifest mode (top-level only), definition_query() otherwise.
    let definition_src = if mode.produces_manifest() {
        frontend.symbols.manifest_query()
    } else {
        frontend.symbols.definition_query()
    };
    let mut symbols = extract_and_normalize(
        &ectx,
        definition_src,
        &mut diagnostics,
        "symbols",
        |ctx, capture| frontend.symbols.normalize(ctx, capture),
        Some(token),
    )?;

    // CP2: Check cancellation after symbol extraction.
    if token.is_cancelled() {
        return Err(cancelled_error(file_path, language));
    }

    // Recovery hook: recover tree-sitter parse artifacts (e.g., ArkTS struct).
    // MUST run before Manifest early-return so recovered symbols appear in manifest mode.
    // Pass an empty scopes vec — scope recovery happens after scope extraction.
    let mut recovery_scopes: Vec<ScopeDef> = Vec::new();
    frontend.recovery.recover_definitions(
        source,
        &tree,
        file_id,
        &mut symbols,
        &mut recovery_scopes,
    );

    // Manifest mode: early return — symbols only, no references/scopes/dataflow.
    if mode.produces_manifest() {
        retain_manifest_top_level_symbols(&mut symbols, root);
        set_symbol_layers(&mut symbols, "manifest");
        let file_path_str = file_path.display().to_string().replace('\\', "/");
        let mut facts = FileFacts {
            file: FileInfo {
                file_id,
                path: file_path_str,
                language,
                content_hash: content_hash.to_string(),
                status: if root.has_error() {
                    ParseStatus::Partial
                } else {
                    ParseStatus::Success
                },
            },
            symbols,
            scopes: recovery_scopes,
            references: vec![],
            imports: vec![],
            exports: vec![],
            raw_edges: vec![],
            callsites: vec![],
            bindings: vec![],
            binding_uses: vec![],
            data_nodes: vec![],
            dataflow_edges: vec![],
            cfg_nodes: vec![],
            cfg_edges: vec![],
            diagnostics,
            budget_exceeded: false,
            lexical_failed: false,
            dataflow_failed: false,
            cfg_failed: false,
            layer: "manifest".to_string(),
        };
        crate::post_extract::apply_post_extract_hooks(&mut facts, source);
        return Ok(facts);
    }

    // 3. Extract and normalize references (skip in ResolutionSymbols mode)
    let mut references = if mode.produces_references() {
        extract_and_normalize(
            &ectx,
            frontend.references.reference_query(),
            &mut diagnostics,
            "references",
            |ctx, capture| frontend.references.normalize(ctx, capture),
            Some(token),
        )?
    } else {
        vec![]
    };

    // CP3: Check cancellation after reference extraction.
    if token.is_cancelled() {
        return Err(cancelled_error(file_path, language));
    }

    // 4. Extract and normalize imports
    let imports = extract_and_normalize(
        &ectx,
        frontend.imports.import_query(),
        &mut diagnostics,
        "imports",
        |ctx, capture| frontend.imports.normalize(ctx, capture),
        Some(token),
    )?;

    // 5. Extract and normalize scopes
    let mut scopes = extract_and_normalize(
        &ectx,
        frontend.scopes.scope_query(),
        &mut diagnostics,
        "scopes",
        |ctx, capture| frontend.scopes.normalize(ctx, capture),
        Some(token),
    )?;

    // CP4: Check cancellation after imports + scopes extraction.
    if token.is_cancelled() {
        return Err(cancelled_error(file_path, language));
    }

    // 5a. Merge recovery scopes (from recover_definitions) and run scope recovery.
    //     This runs before build_scope_tree() so recovered scopes participate in
    //     container assignment.
    scopes.extend(recovery_scopes);
    frontend
        .recovery
        .recover_scopes(source, &tree, file_id, &mut symbols, &mut scopes);

    // 6. Raw edges are now populated downstream by GraphBuilder (new P3 path).
    //    Old normalize_dataflow path was removed in favor of DataFlowBuilder.
    let mut raw_edges = Vec::new();

    // 7. Build scope tree and assign containers
    super::build_scope_tree(&mut scopes, &mut symbols);

    // 7z. Expand callable and type symbol ranges to match their defining scope.
    //     definitions.scm captures only the name identifier (e.g. "compute"),
    //     but resolve_dataflow_function_ids() needs the full function body range
    //     to assign function_id to DataNodes inside the body.
    for sym in symbols.iter_mut() {
        let expected_scope = match sym.kind {
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => {
                Some(&[ScopeKind::Function, ScopeKind::Method][..])
            }
            SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Trait => {
                Some(
                    &[
                        ScopeKind::Class,
                        ScopeKind::Struct,
                        ScopeKind::Interface,
                        ScopeKind::Trait,
                    ][..],
                )
            }
            SymbolKind::Enum => Some(&[ScopeKind::Enum][..]),
            _ => None,
        };
        if let Some(expected_scope) = expected_scope {
            // Find the tightest defining scope that contains the captured name.
            let containing = scopes
                .iter()
                .filter(|s| {
                    expected_scope.contains(&s.kind)
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

    // ResolutionSymbols mode: return after symbols + imports + scopes + scope_tree.
    // Dependencies only need to be resolution targets, not full structural extraction.
    if matches!(mode, ExtractionMode::ResolutionSymbols) {
        set_symbol_layers(&mut symbols, "resolution_symbols");
        let file_path_str = file_path.display().to_string().replace('\\', "/");
        let mut facts = FileFacts {
            file: FileInfo {
                file_id,
                path: file_path_str,
                language,
                content_hash: content_hash.to_string(),
                status: if root.has_error() {
                    ParseStatus::Partial
                } else {
                    ParseStatus::Success
                },
            },
            symbols,
            scopes,
            imports,
            references: vec![],
            exports: vec![],
            raw_edges: vec![],
            callsites: vec![],
            bindings: vec![],
            binding_uses: vec![],
            data_nodes: vec![],
            dataflow_edges: vec![],
            cfg_nodes: vec![],
            cfg_edges: vec![],
            diagnostics,
            budget_exceeded: false,
            lexical_failed: false,
            dataflow_failed: false,
            cfg_failed: false,
            layer: "resolution_symbols".to_string(),
        };
        crate::post_extract::apply_post_extract_hooks(&mut facts, source);
        return Ok(facts);
    }

    // 7a. Extract lexical bindings (P7: skip if unsupported)
    let mut lexical_failed = false;
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
            lexical_failed = true;
            LexicalBindingResult {
                bindings: vec![],
                uses: vec![],
            }
        });
        (lexical_result.bindings, lexical_result.uses)
    } else {
        (vec![], vec![])
    };

    // 7b. Build dataflow graph (P7: skip in Structural mode)
    let mut budget_exceeded = false;

    // In LazyDataflow mode: compute capture byte ranges from window
    let capture_ranges: Option<Vec<(u32, u32)>> =
        if let ExtractionMode::LazyDataflow { ref window } = mode {
            Some(
                window
                    .units
                    .iter()
                    .filter(|u| u.file_id == file_id)
                    .map(|u| (u.range.start_byte, u.range.end_byte))
                    .collect(),
            )
        } else {
            None
        };
    let capture_ranges_ref: Option<&[(u32, u32)]> = capture_ranges.as_deref();

    let mut dataflow_failed = false;
    let (mut data_nodes, dataflow_edges) =
        if mode.produces_dataflow() && frontend.dataflow.capability().is_supported() {
            let dataflow_result = super::dataflow_builder::DataFlowBuilder::extract(
                frontend.dataflow.as_ref(),
                &ectx,
                &bindings,
                &scopes,
                &symbols,
                capture_ranges_ref,
            )
            .unwrap_or_else(|e| {
                diagnostics.push(ExtractDiagnostic {
                    level: DiagnosticLevel::Warning,
                    message: format!("DataFlow builder failed: {e}"),
                    range: None,
                });
                dataflow_failed = true;
                DataFlowResult::default()
            });
            let nodes = dataflow_result.nodes;
            let edges = dataflow_result.edges;

            // 7c. Build use-def edges (only if dataflow succeeded)
            // function_ids already resolved inside DataFlowBuilder::extract
            let use_def_edges = DataFlowBuilder::resolve_use_def(&nodes);
            let mut all_edges = edges;
            all_edges.extend(use_def_edges);

            // In LazyDataflow mode: filter nodes and edges to only those
            // whose ranges fall within the window units.
            if let ExtractionMode::LazyDataflow { ref window } = mode {
                let filtered_data = filter_dataflow_to_window(&nodes, &all_edges, window, file_id);
                budget_exceeded = filtered_data.truncated;
                (filtered_data.nodes, filtered_data.edges)
            } else {
                (nodes, all_edges)
            }
        } else {
            (vec![], vec![])
        };

    // 7e. Build per-function control-flow graphs (P7: skip in Structural mode)
    let mut cfg_failed = false;
    let (cfg_nodes, cfg_edges) =
        if mode.produces_cfg() && frontend.capability.features.cfg.is_supported() {
            let cfg_result =
                super::cfg_builder::build_cfg_for_functions(language, root, &symbols, source_bytes)
                    .unwrap_or_else(|e| {
                        diagnostics.push(ExtractDiagnostic {
                            level: DiagnosticLevel::Warning,
                            message: format!("CFG builder failed: {e}"),
                            range: None,
                        });
                        cfg_failed = true;
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
    //
    // In Structural mode this step is skipped entirely — there is no
    // dataflow context to justify a full AST identifier scan.  Callers
    // receive binding_uses from declaration sites only (from step 7a).
    let reference_binding_uses: Vec<BindingUse> = if mode.produces_reference_binding_uses() {
        build_reference_binding_uses(&ectx, &bindings, &scopes).unwrap_or_else(|e| {
            diagnostics.push(ExtractDiagnostic {
                level: DiagnosticLevel::Warning,
                message: format!("Identifier-use binding scan failed: {e}"),
                range: None,
            });
            vec![]
        })
    } else {
        vec![]
    };

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
        .filter_map(|r| {
            let caller = match r.source_symbol {
                Some(s) => s,
                None => return None, // filter ensures Some, but be defensive
            };
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
                    let value = source
                        .get(arg_range.start_byte as usize..arg_range.end_byte as usize)
                        .unwrap_or("")
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

            Some(Callsite {
                id: cs_id,
                reference_id: Some(r.id),
                caller,
                receiver,
                args,
                range: callsite_range,
                callee_range,
            })
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
    let cs_id_map: std::collections::HashMap<types::ids::CallsiteId, types::ids::CallsiteId> =
        callsites
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

    // In LazyDataflow mode, filter bindings, cfg, and dataflow to the window.
    // (data_nodes/dataflow_edges are already filtered in step 7b/7c above.)
    let (bindings, binding_uses, cfg_nodes, cfg_edges) =
        if let ExtractionMode::LazyDataflow { ref window } = mode {
            let file_units: Vec<&types::lazy::AnalysisUnit> = window
                .units
                .iter()
                .filter(|u| u.file_id == file_id)
                .collect();
            let is_inside =
                |r: &TextRange| -> bool { file_units.iter().any(|u| range_inside(r, &u.range)) };

            // Filter bindings to window
            let bindings: Vec<_> = bindings
                .into_iter()
                .filter(|b| is_inside(&b.range))
                .collect();
            let binding_ids: std::collections::HashSet<_> = bindings.iter().map(|b| b.id).collect();
            let binding_uses: Vec<_> = binding_uses
                .into_iter()
                .filter(|u| {
                    is_inside(&u.range)
                        && u.binding_id
                            .map(|bid| binding_ids.contains(&bid))
                            .unwrap_or(true)
                })
                .collect();

            // Filter CFG to window
            let cfg_nodes: Vec<_> = cfg_nodes
                .into_iter()
                .filter(|n| {
                    file_units
                        .iter()
                        .any(|u| u.symbol_id.map(|sid| n.function_id == sid).unwrap_or(false))
                })
                .collect();
            let cfg_node_ids: std::collections::HashSet<_> =
                cfg_nodes.iter().map(|n| n.id).collect();
            let cfg_edges: Vec<_> = cfg_edges
                .into_iter()
                .filter(|e| cfg_node_ids.contains(&e.source) && cfg_node_ids.contains(&e.target))
                .collect();

            (bindings, binding_uses, cfg_nodes, cfg_edges)
        } else {
            (bindings, binding_uses, cfg_nodes, cfg_edges)
        };

    // In LazyDataflow mode, the caller already has structural facts in DB.
    // We only build dataflow for the window — clear structural fields so
    // the caller does not accidentally overwrite existing DB rows.
    let output_layer = if mode.produces_dataflow() {
        "dataflow"
    } else {
        "structural"
    };

    let (
        mut symbols_out,
        scopes_out,
        references_out,
        imports_out,
        exports_out,
        raw_edges_out,
        callsites_out,
    ) = if matches!(mode, ExtractionMode::LazyDataflow { .. }) {
        (vec![], vec![], vec![], vec![], vec![], vec![], vec![])
    } else {
        (
            symbols, scopes, references, imports, exports, raw_edges, callsites,
        )
    };
    set_symbol_layers(&mut symbols_out, output_layer);

    // Log extraction degradation flags — callers can inspect FileFacts fields
    // directly, but a warn-level trace ensures operators see these in logs.
    if lexical_failed {
        tracing::warn!(file = %file_path_str, "lexical binding extraction failed for this file");
    }
    if dataflow_failed {
        tracing::warn!(file = %file_path_str, "dataflow extraction failed for this file");
    }
    if cfg_failed {
        tracing::warn!(file = %file_path_str, "CFG extraction failed for this file");
    }

    let mut facts = FileFacts {
        file: FileInfo {
            file_id,
            path: file_path_str,
            language,
            content_hash: content_hash.to_string(),
            status,
        },
        symbols: symbols_out,
        scopes: scopes_out,
        references: references_out,
        imports: imports_out,
        exports: exports_out,
        raw_edges: raw_edges_out,
        callsites: callsites_out,
        diagnostics,
        bindings,
        binding_uses,
        data_nodes,
        dataflow_edges,
        cfg_nodes,
        cfg_edges,
        budget_exceeded,
        lexical_failed,
        dataflow_failed,
        cfg_failed,
        layer: output_layer.to_string(),
    };
    // Shared index/lazy post-extract (EXPORT_SYMBOL, initcall, …).
    crate::post_extract::apply_post_extract_hooks(&mut facts, source);
    Ok(facts)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn set_symbol_layers(symbols: &mut [SymbolDef], layer: &str) {
    for symbol in symbols {
        symbol.layer = layer.to_string();
    }
}

fn retain_manifest_top_level_symbols(symbols: &mut Vec<SymbolDef>, root: tree_sitter::Node<'_>) {
    symbols.retain(|symbol| is_manifest_top_level_symbol(root, symbol));
}

fn is_manifest_top_level_symbol(root: tree_sitter::Node<'_>, symbol: &SymbolDef) -> bool {
    let Some(node) = root.descendant_for_byte_range(
        symbol.name_range.start_byte as usize,
        symbol.name_range.end_byte as usize,
    ) else {
        return true;
    };

    let mut current = Some(node);
    while let Some(node) = current {
        if node == root {
            return true;
        }
        if is_manifest_nested_barrier(node.kind()) {
            return false;
        }
        current = node.parent();
    }
    true
}

fn is_manifest_nested_barrier(kind: &str) -> bool {
    matches!(
        kind,
        "block"
            | "body"
            | "class_body"
            | "compound_statement"
            | "declaration_list"
            | "enum_body"
            | "field_declaration_list"
            | "interface_body"
            | "method_declaration"
            | "statement_block"
            | "struct_body"
    )
}

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
        None,
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

fn cancelled_error(file_path: &Path, language: Language) -> anyhow::Error {
    anyhow::Error::new(
        ExtractionFailure::new(
            ExtractionFailureKind::Cancelled,
            file_path.to_string_lossy().to_string(),
            language,
        )
        .with_message("cancelled"),
    )
}

/// Run a query and normalize each capture through the provided function.
///
/// `token` is an optional [`CancelCheck`] — when `Some`, the capture loop
/// checks cancellation every 100 captures and returns a typed cancellation
/// error instead of partial facts.
/// Pass `None` only for internal phases that do not have a cancellation budget.
fn extract_and_normalize<'a, T>(
    ctx: &ExtractionCtx<'a>,
    query_src: &str,
    diagnostics: &mut Vec<ExtractDiagnostic>,
    slot_name: &'static str,
    mut normalize: impl FnMut(NormalizeCtx<'a>, Capture<'a>) -> Option<T>,
    token: Option<&dyn CancelCheck>,
) -> Result<Vec<T>> {
    let captures = super::query_helpers::collect_captures(
        ctx.ts_lang,
        query_src,
        ctx.root,
        ctx.source_bytes(),
        slot_name,
        token,
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

// ---------------------------------------------------------------------------
// Lazy window helpers
// ---------------------------------------------------------------------------

/// Check if range `inner` is fully contained within `outer`.
fn range_inside(inner: &TextRange, outer: &TextRange) -> bool {
    inner.start_byte >= outer.start_byte && inner.end_byte <= outer.end_byte
}

/// Filter data nodes and dataflow edges to a lazy window.
///
/// Only nodes whose byte range falls within one of the window units
/// for the given file are kept.  Edges are kept only if both source
/// and target nodes survive the filter.
fn filter_dataflow_to_window(
    nodes: &[DataNode],
    edges: &[DataFlowEdge],
    window: &types::lazy::LazyWindow,
    file_id: FileId,
) -> FilteredDataflow {
    use super::mode::{LAZY_MAX_EDGES_PER_UNIT, LAZY_MAX_NODES_PER_UNIT};

    let file_units: Vec<&types::lazy::AnalysisUnit> = window
        .units
        .iter()
        .filter(|u| u.file_id == file_id)
        .collect();

    if file_units.is_empty() {
        return FilteredDataflow {
            nodes: vec![],
            edges: vec![],
            truncated: false,
        };
    }

    let max_nodes = file_units.len() * LAZY_MAX_NODES_PER_UNIT;
    let max_edges = file_units.len() * LAZY_MAX_EDGES_PER_UNIT;

    let mut filtered_nodes: Vec<DataNode> = nodes
        .iter()
        .filter(|n| file_units.iter().any(|u| range_inside(&n.range, &u.range)))
        .cloned()
        .collect();

    let mut filtered_truncated = false;
    if filtered_nodes.len() > max_nodes {
        filtered_nodes.truncate(max_nodes);
        filtered_truncated = true;
    }

    let kept_ids: std::collections::HashSet<types::ids::DataNodeId> =
        filtered_nodes.iter().map(|n| n.id).collect();

    let mut filtered_edges: Vec<DataFlowEdge> = edges
        .iter()
        .filter(|e| kept_ids.contains(&e.source) && kept_ids.contains(&e.target))
        .cloned()
        .collect();

    if filtered_edges.len() > max_edges {
        filtered_edges.truncate(max_edges);
        filtered_truncated = true;
    }

    FilteredDataflow {
        nodes: filtered_nodes,
        edges: filtered_edges,
        truncated: filtered_truncated,
    }
}

struct FilteredDataflow {
    nodes: Vec<DataNode>,
    edges: Vec<DataFlowEdge>,
    truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::LanguageFrontend;
    use crate::languages::{available_languages, create_frontend};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use types::{Language, ReferenceKind};

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

    fn extract_full(
        frontend: &LanguageFrontend,
        file_id: FileId,
        file_path: &std::path::Path,
        source: &str,
        content_hash: &str,
    ) -> Result<FileFacts> {
        extract_file_with_mode(
            frontend,
            file_id,
            file_path,
            source,
            content_hash,
            ExtractionMode::Full,
            &(),
        )
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

    struct ManifestBoundaryCase {
        language: Language,
        path: &'static str,
        source: &'static str,
        expected: &'static [&'static str],
        rejected: &'static [&'static str],
    }

    fn manifest_boundary_cases() -> Vec<ManifestBoundaryCase> {
        vec![
            ManifestBoundaryCase {
                language: Language::TypeScript,
                path: "manifest.ts",
                source: concat!(
                    "function topLevel() { const localValue = 1; return localValue; }\n",
                    "class TopClass { method() { return 1; } }\n",
                    "const TOP_CONST = 1;\n",
                ),
                expected: &["topLevel", "TopClass", "TOP_CONST"],
                rejected: &["localValue", "method"],
            },
            ManifestBoundaryCase {
                language: Language::JavaScript,
                path: "manifest.js",
                source: concat!(
                    "function topLevel() { const localValue = 1; return localValue; }\n",
                    "class TopClass { method() { return 1; } }\n",
                    "const TOP_CONST = 1;\n",
                ),
                expected: &["topLevel", "TopClass", "TOP_CONST"],
                rejected: &["localValue", "method"],
            },
            ManifestBoundaryCase {
                language: Language::Python,
                path: "manifest.py",
                source: concat!(
                    "def top_level():\n",
                    "    local_value = 1\n",
                    "    return local_value\n",
                    "\n",
                    "class TopClass:\n",
                    "    def method(self):\n",
                    "        return 1\n",
                ),
                expected: &["top_level", "TopClass"],
                rejected: &["local_value", "method"],
            },
            ManifestBoundaryCase {
                language: Language::Java,
                path: "Manifest.java",
                source: concat!(
                    "class TopClass { void method() {} class NestedClass {} }\n",
                    "interface TopIface {}\n",
                    "enum TopEnum { A }\n",
                ),
                expected: &["TopClass", "TopIface", "TopEnum"],
                rejected: &["method", "NestedClass"],
            },
            ManifestBoundaryCase {
                language: Language::C,
                path: "manifest.c",
                source: concat!(
                    "struct TopStruct { int field; };\n",
                    "enum TopEnum { TOP_A };\n",
                    "typedef int TopAlias;\n",
                    "int top_global;\n",
                    "int top_fn(void) { int local_var = 1; return local_var; }\n",
                ),
                expected: &["TopStruct", "TopEnum", "TopAlias", "top_global", "top_fn"],
                rejected: &["field", "local_var"],
            },
            ManifestBoundaryCase {
                language: Language::Cpp,
                path: "manifest.cpp",
                source: concat!(
                    "namespace TopNs { void nested_fn() {} }\n",
                    "class TopClass { void method() {} };\n",
                    "int top_global;\n",
                    "int top_fn() { int local_var = 1; return local_var; }\n",
                ),
                expected: &["TopNs", "TopClass", "top_global", "top_fn"],
                rejected: &["nested_fn", "method", "local_var"],
            },
            ManifestBoundaryCase {
                language: Language::ArkTS,
                path: "manifest.ets",
                source: concat!(
                    "function topLevel() { const localValue = 1; return localValue; }\n",
                    "class TopClass { method() { return 1; } }\n",
                    "const TOP_CONST = 1;\n",
                ),
                expected: &["topLevel", "TopClass", "TOP_CONST"],
                rejected: &["localValue", "method"],
            },
            ManifestBoundaryCase {
                language: Language::Cangjie,
                path: "manifest.cj",
                source: concat!(
                    "func topLevel(): Int64 {\n",
                    "    let localValue = 1\n",
                    "    return localValue\n",
                    "}\n",
                    "let topValue = 1\n",
                ),
                expected: &["topLevel", "topValue"],
                rejected: &["localValue"],
            },
            ManifestBoundaryCase {
                language: Language::Go,
                path: "manifest.go",
                source: concat!(
                    "package main\n",
                    "type TopStruct struct { Field int }\n",
                    "type TopIface interface { M() }\n",
                    "type TopAlias int\n",
                    "func topFn() int { localVar := 1; return localVar }\n",
                    "func (t TopStruct) method() {}\n",
                ),
                expected: &["TopStruct", "TopIface", "TopAlias", "topFn"],
                rejected: &["Field", "localVar", "method"],
            },
            ManifestBoundaryCase {
                language: Language::CSharp,
                path: "Manifest.cs",
                source: concat!(
                    "namespace TopNs { class NestedInNamespace {} }\n",
                    "class TopClass { void Method() {} class NestedClass {} }\n",
                    "interface TopIface {}\n",
                    "enum TopEnum { A }\n",
                    "delegate void TopDelegate();\n",
                ),
                expected: &["TopNs", "TopClass", "TopIface", "TopEnum", "TopDelegate"],
                rejected: &["NestedInNamespace", "Method", "NestedClass"],
            },
            ManifestBoundaryCase {
                language: Language::Rust,
                path: "manifest.rs",
                source: concat!(
                    "fn top_fn() { let local_var = 1; }\n",
                    "struct TopStruct { field: i32 }\n",
                    "enum TopEnum { A }\n",
                    "trait TopTrait { fn method(&self); }\n",
                    "mod top_mod { pub fn nested_fn() {} }\n",
                    "const TOP_CONST: i32 = 1;\n",
                    "static TOP_STATIC: i32 = 1;\n",
                    "type TopAlias = i32;\n",
                    "macro_rules! top_macro { () => {}; }\n",
                ),
                expected: &[
                    "top_fn",
                    "TopStruct",
                    "TopEnum",
                    "TopTrait",
                    "top_mod",
                    "TOP_CONST",
                    "TOP_STATIC",
                    "TopAlias",
                    "top_macro",
                ],
                rejected: &["local_var", "field", "method", "nested_fn"],
            },
            ManifestBoundaryCase {
                language: Language::Php,
                path: "manifest.php",
                source: concat!(
                    "<?php\n",
                    "class TopClass { function innerMethod() {} const INNER_CONST = 1; }\n",
                    "interface TopIface {}\n",
                    "trait TopTrait {}\n",
                    "function top_func() { function nested_func() {} }\n",
                ),
                expected: &["TopClass", "TopIface", "TopTrait", "top_func"],
                rejected: &["innerMethod", "INNER_CONST", "nested_func"],
            },
            ManifestBoundaryCase {
                language: Language::Ruby,
                path: "manifest.rb",
                source: concat!(
                    "class TopClass\n",
                    "  INNER_CONST = 1\n",
                    "  def inner_method\n",
                    "  end\n",
                    "end\n",
                    "module TopModule\n",
                    "end\n",
                    "def top_method\n",
                    "end\n",
                    "TOP_CONST = 1\n",
                ),
                expected: &["TopClass", "TopModule", "top_method", "TOP_CONST"],
                rejected: &["INNER_CONST", "inner_method"],
            },
            ManifestBoundaryCase {
                language: Language::Kotlin,
                path: "Manifest.kt",
                source: concat!(
                    "class TopClass { fun innerFun() {} val innerProp = 1 }\n",
                    "object TopObject\n",
                    "fun topFun(): Int { val localVar = 1; return localVar }\n",
                    "val topProp = 1\n",
                ),
                expected: &["TopClass", "TopObject", "topFun", "topProp"],
                rejected: &["innerFun", "innerProp", "localVar"],
            },
        ]
    }

    #[test]
    fn manifest_mode_keeps_top_level_boundary_for_all_available_languages() {
        let cases = manifest_boundary_cases();
        let case_languages: HashSet<_> = cases.iter().map(|case| case.language).collect();
        for language in available_languages() {
            assert!(
                case_languages.contains(&language),
                "missing manifest boundary fixture for {}",
                language.as_str()
            );
        }

        let mut checked = 0usize;
        for case in cases {
            let Some(frontend) = create_frontend(case.language) else {
                continue;
            };
            checked += 1;
            let facts = extract_file_with_mode(
                &frontend,
                FileId::generate(case.path),
                &PathBuf::from(case.path),
                case.source,
                "manifest-boundary",
                ExtractionMode::Manifest,
                &(),
            )
            .unwrap_or_else(|err| {
                panic!(
                    "manifest extraction failed for {}: {err}",
                    case.language.as_str()
                )
            });

            assert_eq!(facts.layer, "manifest", "{}", case.language.as_str());
            assert!(
                facts.references.is_empty()
                    && facts.imports.is_empty()
                    && facts.exports.is_empty()
                    && facts.raw_edges.is_empty()
                    && facts.callsites.is_empty()
                    && facts.bindings.is_empty()
                    && facts.binding_uses.is_empty()
                    && facts.data_nodes.is_empty()
                    && facts.dataflow_edges.is_empty()
                    && facts.cfg_nodes.is_empty()
                    && facts.cfg_edges.is_empty(),
                "manifest mode must only emit file/symbol facts for {}",
                case.language.as_str()
            );

            let names: HashSet<_> = facts
                .symbols
                .iter()
                .map(|symbol| symbol.name.as_str())
                .collect();
            for expected in case.expected {
                assert!(
                    names.contains(expected),
                    "{} manifest missing top-level symbol {expected}; got {names:?}",
                    case.language.as_str()
                );
            }
            for rejected in case.rejected {
                assert!(
                    !names.contains(rejected),
                    "{} manifest leaked nested/local symbol {rejected}; got {names:?}",
                    case.language.as_str()
                );
            }
            for symbol in &facts.symbols {
                assert_eq!(
                    symbol.layer,
                    "manifest",
                    "{} symbol {} used non-manifest layer",
                    case.language.as_str(),
                    symbol.name
                );
            }
        }
        assert!(
            checked > 0,
            "expected at least one available language fixture"
        );
    }

    #[cfg(feature = "c")]
    #[test]
    fn test_extract_c_enum_type_use_does_not_own_function_body_calls() {
        let source = r#"enum tcp_tw_status { TCP_TW_OK };

int dev_net_rcu(void) {
    return 0;
}

int tcp_v4_rcv(void) {
    enum tcp_tw_status tw_status;
    return dev_net_rcu();
}
"#;
        let file_id = FileId::generate("test_enum_owner.c");
        let frontend = create_frontend(Language::C).unwrap();
        let file_path = PathBuf::from("test_enum_owner.c");

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
        let caller = facts
            .symbols
            .iter()
            .find(|s| s.name == "tcp_v4_rcv")
            .expect("tcp_v4_rcv symbol")
            .id;
        let enum_defs = facts
            .symbols
            .iter()
            .filter(|s| s.name == "tcp_tw_status")
            .count();
        assert_eq!(
            enum_defs, 1,
            "plain enum-typed variables are not definitions"
        );

        let call = facts
            .references
            .iter()
            .find(|r| r.kind == ReferenceKind::Call && r.name == "dev_net_rcu")
            .expect("dev_net_rcu call reference");
        assert_eq!(call.source_symbol, Some(caller));
    }

    #[cfg(feature = "c")]
    #[test]
    fn c_type_symbol_range_covers_the_complete_definition() {
        let source = "struct ioam6_lwt {\n    int state;\n    long reset_ts;\n};\n";
        let file_id = FileId::generate("type_range.c");
        let frontend = create_frontend(Language::C).unwrap();
        let file_path = PathBuf::from("type_range.c");

        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            &file_path,
            source,
            "type-range",
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();
        let symbol = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "ioam6_lwt")
            .expect("struct symbol");

        assert_eq!(symbol.name_range.start_line, 0);
        assert_eq!(symbol.range.start_line, 0);
        assert_eq!(symbol.range.end_line, 3);
        assert_eq!(
            &source[symbol.range.start_byte as usize..symbol.range.end_byte as usize],
            "struct ioam6_lwt {\n    int state;\n    long reset_ts;\n}"
        );
    }

    #[cfg(feature = "c")]
    #[test]
    fn c_enum_symbol_range_covers_the_complete_definition() {
        let source = "enum stripe_result {\n    STRIPE_SUCCESS,\n    STRIPE_RETRY,\n};\n";
        let file_id = FileId::generate("enum_range.c");
        let frontend = create_frontend(Language::C).unwrap();
        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            &PathBuf::from("enum_range.c"),
            source,
            "enum-range",
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();
        let symbol = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "stripe_result")
            .expect("enum symbol");

        assert_eq!(symbol.range.start_line, 0);
        assert_eq!(symbol.range.end_line, 3);
    }

    #[cfg(feature = "c")]
    #[test]
    fn c_function_pointer_field_is_indexed_as_field_symbol() {
        let source = "struct dispatch_ops {\n    int (*do_it)(int);\n};\n";
        let file_id = FileId::generate("fp_field.c");
        let frontend = create_frontend(Language::C).unwrap();
        let facts = extract_full(
            &frontend,
            file_id,
            &PathBuf::from("fp_field.c"),
            source,
            "fp-field",
        )
        .unwrap();

        let field = facts
            .symbols
            .iter()
            .find(|symbol| {
                symbol.name == "do_it"
                    && symbol.qualified_name == "dispatch_ops.do_it"
                    && symbol.kind == types::SymbolKind::Field
            })
            .expect("function-pointer struct field symbol");
        assert_eq!(
            &source[field.name_range.start_byte as usize..field.name_range.end_byte as usize],
            "do_it"
        );
    }

    #[cfg(feature = "rust")]
    #[test]
    fn rust_type_symbol_range_covers_the_complete_definition() {
        let source = "pub struct JobManager {\n    current: usize,\n}\n";
        let file_id = FileId::generate("type_range.rs");
        let frontend = create_frontend(Language::Rust).unwrap();
        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            &PathBuf::from("type_range.rs"),
            source,
            "type-range",
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();
        let symbol = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "JobManager")
            .expect("struct symbol");

        assert_eq!(symbol.range.start_line, 0);
        assert_eq!(symbol.range.end_line, 2);
        assert_eq!(
            &source[symbol.range.start_byte as usize..symbol.range.end_byte as usize],
            "pub struct JobManager {\n    current: usize,\n}"
        );
    }

    #[cfg(feature = "typescript")]
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

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
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

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
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

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
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

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert_sources_are_known(&facts);

        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let result = store.insert_file_facts(&facts);
        assert!(result.is_ok(), "Insert failed: {:?}", result.err());
    }

    #[cfg(feature = "cpp")]
    #[test]
    fn cpp_function_pointer_field_is_indexed_as_field_symbol() {
        let source = "struct DispatchOps {\n    int (*do_it)(int);\n};\n";
        let file_id = FileId::generate("fp_field.cpp");
        let frontend = create_frontend(Language::Cpp).unwrap();
        let facts = extract_full(
            &frontend,
            file_id,
            &PathBuf::from("fp_field.cpp"),
            source,
            "fp-field",
        )
        .unwrap();

        let field = facts
            .symbols
            .iter()
            .find(|symbol| {
                symbol.name == "do_it"
                    && symbol.qualified_name == "DispatchOps::do_it"
                    && symbol.kind == types::SymbolKind::Field
            })
            .expect("function-pointer struct field symbol");
        assert_eq!(
            &source[field.name_range.start_byte as usize..field.name_range.end_byte as usize],
            "do_it"
        );
    }
    #[cfg(feature = "typescript")]
    #[test]
    fn test_extract_ts_simple() {
        let source = "const foo = 1;\nconsole.log(foo);\n";
        let file_id = FileId::generate("test.ts");
        let frontend = ts_frontend();
        let file_path = PathBuf::from("test.ts");

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert_eq!(facts.file.path, "test.ts");
        assert_eq!(facts.file.language, Language::TypeScript);
        assert!(
            !facts.symbols.is_empty(),
            "Should have symbols: {:?}",
            facts.symbols
        );
        assert!(!facts.references.is_empty(), "Should have references");
    }
    #[cfg(feature = "python")]
    #[test]
    fn test_extract_python_simple() {
        let source = "def foo():\n    return True\n\nfoo()\n";
        let file_id = FileId::generate("test.py");
        let frontend = py_frontend();
        let file_path = PathBuf::from("test.py");

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert_eq!(facts.file.language, Language::Python);
        assert!(!facts.symbols.is_empty(), "Should have symbols");
    }
    #[cfg(feature = "typescript")]
    #[test]
    fn test_extract_ts_dataflow() {
        // New P3 path: DataFlowBuilder produces DataNodes + DataFlowEdges
        let source =
            "function add(a: number, b: number) {\n  let result = a + b;\n  return result;\n}\n";
        let file_id = FileId::generate("test.ts");
        let frontend = ts_frontend();
        let file_path = PathBuf::from("test.ts");

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert!(!facts.data_nodes.is_empty(), "Should have dataflow nodes");
        assert!(
            !facts.dataflow_edges.is_empty(),
            "Should have dataflow edges"
        );
    }
    #[cfg(feature = "python")]
    #[test]
    fn test_extract_python_dataflow() {
        // Python adapter does not yet implement DataFlowBuilder.
        // This test verifies basic extraction still succeeds without the old
        // normalize_dataflow path.
        let source = "def add(a, b):\n    c = a + b\n    return c\n";
        let file_id = FileId::generate("test.py");
        let frontend = py_frontend();
        let file_path = PathBuf::from("test.py");

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
        assert!(!facts.symbols.is_empty(), "Should have symbols");
        assert!(facts.raw_edges.is_empty(), "Old dataflow path removed");
    }
    #[cfg(feature = "typescript")]
    #[test]
    fn test_extract_and_insert_ts() {
        use db::Store;
        let source = "function add(a: number, b: number) {\n  return a + b;\n}\nadd(1, 2);\n";
        let file_id = FileId::generate("test.ts");
        let frontend = ts_frontend();
        let file_path = PathBuf::from("test.ts");

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
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
    #[cfg(feature = "typescript")]
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

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
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

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
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

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
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

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
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

        let facts = extract_full(&frontend, file_id, &file_path, source, "abc").unwrap();
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

    /// C++ qualified call `CertUtils::GetDev()` must capture simple name + full text.
    /// (Resolution layer keys off `name`; diagnostics use `text` / `receiver`.)
    #[cfg(feature = "cpp")]
    #[test]
    fn test_cpp_qualified_call_ref_simple_name_and_full_text() {
        let source = r#"
class CertUtils {
public:
    static int GetDev();
};

int CertUtils::GetDev() {
    return 1;
}

int use_dev() {
    return CertUtils::GetDev();
}
"#;
        let file_id = FileId::generate("cert_utils.cpp");
        let frontend = create_frontend(Language::Cpp).unwrap();
        let facts = extract_full(
            &frontend,
            file_id,
            &PathBuf::from("cert_utils.cpp"),
            source,
            "abc",
        )
        .expect("extract cpp");

        assert!(
            facts
                .symbols
                .iter()
                .any(|s| s.name == "GetDev" && s.qualified_name == "CertUtils::GetDev"),
            "expected method symbol CertUtils::GetDev, got {:?}",
            facts
                .symbols
                .iter()
                .map(|s| (&s.name, &s.qualified_name))
                .collect::<Vec<_>>()
        );

        let call = facts
            .references
            .iter()
            .find(|r| r.kind == ReferenceKind::Call && r.name == "GetDev")
            .expect("expected call ref with simple name GetDev");
        assert_eq!(
            call.text, "CertUtils::GetDev",
            "text should be full qualified call span"
        );
        assert_eq!(
            call.receiver.as_deref(),
            Some("CertUtils"),
            "receiver should be scope prefix"
        );

        // Nested A::B::method — outermost text / receiver prefix.
        let nested = r#"
namespace A {
namespace B {
int method();
}
}
int caller() { return A::B::method(); }
"#;
        let nid = FileId::generate("nested.cpp");
        let nf = extract_full(
            &create_frontend(Language::Cpp).unwrap(),
            nid,
            &PathBuf::from("nested.cpp"),
            nested,
            "abc",
        )
        .expect("extract nested");
        let ncall = nf
            .references
            .iter()
            .find(|r| r.kind == ReferenceKind::Call && r.name == "method")
            .expect("nested qualified call");
        assert_eq!(ncall.text, "A::B::method");
        assert_eq!(ncall.receiver.as_deref(), Some("A::B"));
    }

    /// PHP `\Foo\bar()` must capture last segment as name and full path as text.
    #[cfg(feature = "php")]
    #[test]
    fn test_php_qualified_call_ref_simple_name_and_full_text() {
        let source = r#"<?php
namespace Foo {
    function bar() { return 1; }
}
namespace {
    function use_bar() { return \Foo\bar(); }
}
"#;
        let file_id = FileId::generate("qualified.php");
        let frontend = create_frontend(Language::Php).unwrap();
        let facts = extract_full(
            &frontend,
            file_id,
            &PathBuf::from("qualified.php"),
            source,
            "abc",
        )
        .expect("extract php");

        let call = facts
            .references
            .iter()
            .find(|r| r.kind == ReferenceKind::Call && r.name == "bar")
            .expect("expected call ref name=bar for \\Foo\\bar()");
        assert!(
            call.text.contains("Foo") && call.text.contains("bar"),
            "text should preserve qualified path, got {:?}",
            call.text
        );
        // Prefix may be "\Foo" or "Foo" depending on leading slash in source span.
        assert!(
            call.receiver
                .as_ref()
                .is_some_and(|r| r.contains("Foo")),
            "receiver should carry namespace prefix, got {:?}",
            call.receiver
        );
    }

    // ── Lazy dataflow integration tests ─────────────────────────────────

    // ── ResolutionSymbols tests ─────────────────────────────────────

    /// Phase 4: ResolutionSymbols mode produces symbols + scopes + imports,
    /// but no references, dataflow, callsites, or lexical bindings.
    #[test]
    #[cfg(feature = "typescript")]
    fn resolution_symbols_mode_output_shape() {
        let frontend = ts_frontend();
        let source = "const x = 1;\nimport { y } from './dep';\nfunction f() { return x + y; }\n";
        let file_id = FileId::generate("test_res_sym.ts");
        let path = std::path::Path::new("test_res_sym.ts");

        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            path,
            source,
            "abc",
            ExtractionMode::ResolutionSymbols,
            &(),
        )
        .unwrap();

        // Should produce symbols (all, not just top-level)
        assert!(
            !facts.symbols.is_empty(),
            "ResolutionSymbols: should have symbols"
        );
        // Should produce scopes
        assert!(
            !facts.scopes.is_empty(),
            "ResolutionSymbols: should have scopes"
        );
        // Should produce imports
        assert!(
            !facts.imports.is_empty(),
            "ResolutionSymbols: should have imports"
        );
        // Should NOT produce references
        assert!(
            facts.references.is_empty(),
            "ResolutionSymbols: references must be empty"
        );
        // Should NOT produce dataflow
        assert!(
            facts.data_nodes.is_empty(),
            "ResolutionSymbols: data_nodes must be empty"
        );
        assert!(
            facts.dataflow_edges.is_empty(),
            "ResolutionSymbols: dataflow_edges must be empty"
        );
        // Should NOT produce callsites
        assert!(
            facts.callsites.is_empty(),
            "ResolutionSymbols: callsites must be empty"
        );
        // Should NOT produce lexical bindings
        assert!(
            facts.bindings.is_empty(),
            "ResolutionSymbols: bindings must be empty"
        );
        // Should have the correct layer name
        assert_eq!(facts.layer, "resolution_symbols");
    }

    /// 7a: Structural mode produces no dataflow/CFG, but keeps bindings.
    #[test]
    #[cfg(feature = "typescript")]
    fn structural_mode_produces_no_dataflow() {
        let frontend = ts_frontend();
        let source =
            "function add(a: number, b: number): number {\n  let sum = a + b;\n  return sum;\n}\n";
        let file_id = FileId::generate("test_7a.ts");
        let path = std::path::Path::new("test_7a.ts");

        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            path,
            source,
            "abc",
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();

        assert!(!facts.symbols.is_empty(), "Structural: should have symbols");
        assert!(!facts.scopes.is_empty(), "Structural: should have scopes");
        assert!(
            facts.data_nodes.is_empty(),
            "Structural: data_nodes must be empty"
        );
        assert!(
            facts.dataflow_edges.is_empty(),
            "Structural: dataflow_edges must be empty"
        );
        assert!(
            facts.cfg_nodes.is_empty(),
            "Structural: cfg_nodes must be empty"
        );
        assert!(
            facts.cfg_edges.is_empty(),
            "Structural: cfg_edges must be empty"
        );
        // LexicalBinder should still produce bindings (declaration sites)
        assert!(
            !facts.bindings.is_empty(),
            "Structural: bindings should exist (LexicalBinder runs)"
        );
        // Step 8a (identifier-use binding scan) is skipped in Structural
        // so binding_uses only has declaration-site uses
    }

    /// 7d: Full mode produces the same data as the explicit Full-mode extraction.
    #[test]
    #[cfg(feature = "typescript")]
    fn full_mode_matches_test_helper() {
        let frontend = ts_frontend();
        let source = "const x = 1;\nfunction f() { return x + 2; }\nf();\n";
        let file_id = FileId::generate("test_7d.ts");
        let path = std::path::Path::new("test_7d.ts");

        let facts_full_helper = extract_full(&frontend, file_id, path, source, "abc").unwrap();
        let facts_full = extract_file_with_mode(
            &frontend,
            file_id,
            path,
            source,
            "abc",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();

        assert_eq!(facts_full_helper.symbols.len(), facts_full.symbols.len());
        assert_eq!(
            facts_full_helper.data_nodes.len(),
            facts_full.data_nodes.len()
        );
        assert_eq!(
            facts_full_helper.dataflow_edges.len(),
            facts_full.dataflow_edges.len()
        );
        assert_eq!(facts_full_helper.bindings.len(), facts_full.bindings.len());
    }

    /// 7e: Budget truncation — a file with many nodes triggers budget_exceeded.
    #[test]
    #[cfg(feature = "typescript")]
    fn lazy_dataflow_budget_truncation() {
        use types::lazy::{AnalysisUnit, LazyWindow};

        let frontend = ts_frontend();
        let mut source = String::from("function big() {\n");
        for i in 0..3000 {
            source.push_str(&format!("  let v{i} = {i};\n"));
        }
        source.push_str("  return v0;\n}\n");
        let file_id = FileId::generate("test_7e.ts");
        let path = std::path::Path::new("test_7e.ts");

        // Build a minimal window that covers this file's function
        let symbols = {
            let facts = extract_file_with_mode(
                &frontend,
                file_id,
                path,
                &source,
                "abc",
                ExtractionMode::Full, // need symbols for unit construction
                &(),
            )
            .unwrap();
            facts.symbols
        };
        let func_sym = symbols
            .iter()
            .find(|s| s.name == "big")
            .expect("function 'big' not found");

        let window = LazyWindow {
            seed_unit: AnalysisUnit::from_function(file_id, func_sym.id, func_sym.range),
            units: vec![AnalysisUnit::from_function(
                file_id,
                func_sym.id,
                func_sym.range,
            )],
            variable_focus: None,
            truncated: false,
            units_built: 0,
            units_cached: 0,
            units_pending: 0,
            pending_job_ids: Vec::new(),
            quality: None,
            capability_mask: types::structs::FactCoverage::default(),
        };

        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            path,
            &source,
            "abc",
            ExtractionMode::LazyDataflow { window },
            &(),
        )
        .unwrap();

        // With 3000 variable declarations, we should exceed LAZY_MAX_NODES_PER_UNIT (2000)
        // and the filter should have set budget_exceeded
        assert!(
            facts.budget_exceeded,
            "7e: budget_exceeded should be true for 3000-variable function"
        );
        assert!(
            facts.data_nodes.len() <= crate::mode::LAZY_MAX_NODES_PER_UNIT,
            "7e: data_nodes should be capped at LAZY_MAX_NODES_PER_UNIT"
        );
    }

    /// Gap-2: TopLevel unit — dataflow can be built for file-scope code
    /// (variables/functions not inside any enclosing function).
    #[test]
    #[cfg(feature = "typescript")]
    fn toplevel_unit_produces_dataflow() {
        use types::lazy::{AnalysisUnit, LazyWindow};

        let frontend = ts_frontend();
        let source =
            "const GLOBAL = 42;\nlet count = GLOBAL + 1;\nfunction f() { return count; }\n";
        let file_id = FileId::generate("test_toplevel.ts");
        let path = std::path::Path::new("test_toplevel.ts");

        // First extract structurally to get symbol info
        let facts_full = extract_file_with_mode(
            &frontend,
            file_id,
            path,
            source,
            "abc",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();

        // Build a window covering the top-level scope
        // Top-level range is the whole file (approximate via largest scope)
        let file_range = facts_full
            .scopes
            .iter()
            .find(|s| s.parent_id.is_none())
            .map(|s| s.range)
            .unwrap_or(TextRange {
                start_byte: 0,
                end_byte: source.len() as u32,
                start_line: 0,
                start_column: 0,
                end_line: 10,
                end_column: 0,
            });

        let window = LazyWindow {
            seed_unit: AnalysisUnit::from_top_level(file_id, file_range),
            units: vec![AnalysisUnit::from_top_level(file_id, file_range)],
            variable_focus: None,
            truncated: false,
            units_built: 0,
            units_cached: 0,
            units_pending: 0,
            pending_job_ids: Vec::new(),
            quality: None,
            capability_mask: types::structs::FactCoverage::default(),
        };

        let facts_lazy = extract_file_with_mode(
            &frontend,
            file_id,
            path,
            source,
            "abc",
            ExtractionMode::LazyDataflow { window },
            &(),
        )
        .unwrap();

        // Top-level dataflow should produce data nodes (e.g., for GLOBAL, count)
        assert!(
            !facts_lazy.data_nodes.is_empty(),
            "top-level scope should produce dataflow nodes"
        );
        // Structural fields should be empty in LazyDataflow mode
        assert!(
            facts_lazy.symbols.is_empty(),
            "LazyDataflow mode should clear structural fields"
        );
    }

    /// Regression: Full mode produces layer "dataflow" (not "structural").
    ///
    /// Bug: the layer decision only matched `LazyDataflow` before the fix.
    /// After using `produces_dataflow()`, Full mode correctly returns "dataflow".
    #[test]
    #[cfg(feature = "typescript")]
    fn full_mode_layer_is_dataflow() {
        let frontend = ts_frontend();
        let source = "let x = 1;\n";
        let file_id = FileId::generate("test_full_layer.ts");
        let path = std::path::Path::new("test_full_layer.ts");

        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            path,
            source,
            "abc",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();

        assert_eq!(
            facts.layer, "dataflow",
            "Full mode must have layer 'dataflow'"
        );
    }

    /// Regression: Structural mode produces layer "structural".
    ///
    /// Complements `full_mode_layer_is_dataflow` to verify the non-dataflow path.
    #[test]
    #[cfg(feature = "typescript")]
    fn structural_mode_layer_is_structural() {
        let frontend = ts_frontend();
        let source = "let x = 1;\n";
        let file_id = FileId::generate("test_structural_layer.ts");
        let path = std::path::Path::new("test_structural_layer.ts");

        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            path,
            source,
            "abc",
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();

        assert_eq!(
            facts.layer, "structural",
            "Structural mode must have layer 'structural'"
        );
    }
}
