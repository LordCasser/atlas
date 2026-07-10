//! Analysis runtime — CFG/dataflow ensure + contract-driven analysis dispatcher.
//!
//! # Role
//! Semantic tools (branch_diff, lifecycle, …) need CFG/dataflow facts without
//! running the full Focus control plane (`FocusRuntime::prepare`). This type is
//! that **second door by brand only**: same [`FocusMaterialize`] stack as
//! FocusRuntime / Engine, never a second configuration.
//!
//! # DEBT-8 contract
//! Handlers parse args and render envelopes. This module owns:
//! - capability gates (e.g. lifecycle C/C++ only)
//! - store I/O for dataflow nodes/edges and domain rules
//! - effect composition (`CfgGraph` + `compose_effects`)
//! - engine invocation (`FieldLifecycleEngine` / `BranchDiffEngine`)
//!
//! # Responsibilities
//! - `ensure_dataflow_for_function` / `ensure_cfg_for_function`
//! - `run_lifecycle` / `run_branch_diff` / `run_semantic_impact` full orchestration

use std::collections::BTreeSet;

use atlas_engine::analysis::{
    self, BranchDiff, BranchDiffEngine, CalleeMatcher, CppOwnershipRules, EffectComposition,
    FieldLifecycleEngine, FieldLifecycleResult, OwnershipRules, ResourceOpConfig, ResourceOpKind,
    ResourceOpPattern,
};
use atlas_engine::effects::{PlaceRef, SemanticEffectKind};
use atlas_engine::{
    CfgEdge, CfgNode, FocusMaterialize, Language, LazyDataflowService, LazyWindow, Store, SymbolId,
    SymbolKind,
};
use serde::Serialize;

const SEMANTIC_IMPACT_FUNCTION_LIMIT: usize = 20;

/// Outcome of a full lifecycle orchestration pass.
#[derive(Debug)]
pub struct LifecycleAnalysisOk {
    pub result: FieldLifecycleResult,
    pub has_user_rules: bool,
    pub has_any_rules: bool,
    /// Soft failure while ensuring dataflow (analysis may still use CFG-only).
    pub dataflow_error: Option<String>,
}

/// Failure modes for lifecycle orchestration (handler maps to envelope).
#[derive(Debug)]
pub enum LifecycleAnalysisErr {
    CfgUnavailable(String),
    /// Symbol missing, not C/C++, or language could not be resolved.
    UnsupportedLanguage,
}

/// Outcome of a full branch-diff orchestration pass.
#[derive(Debug)]
pub struct BranchDiffAnalysisOk {
    pub diffs: Vec<BranchDiff>,
    pub qname: String,
    pub semantic_window: Option<LazyWindow>,
    pub dataflow_refinement_failed: bool,
}

/// Fully composed semantic additions for an impact response.
#[derive(Debug, Default, Serialize)]
pub struct SemanticImpactAnalysisOk {
    pub invariants_affected: Vec<SemanticImpactInvariant>,
    pub lifecycle_paths_affected: Vec<SemanticImpactLifecyclePath>,
    /// True only when at least one persisted domain rule was relevant to an
    /// analyzed C/C++ function. Builtin heuristics do not set this flag.
    pub domain_rules_applied: bool,
}

#[derive(Debug, Serialize)]
pub struct SemanticImpactInvariant {
    pub function: String,
    pub field: String,
    pub issue_count: usize,
    pub issues: Vec<SemanticImpactIssue>,
}

#[derive(Debug, Serialize)]
pub struct SemanticImpactIssue {
    pub line: u32,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct SemanticImpactLifecyclePath {
    pub function: String,
    pub field: String,
    pub final_state: String,
    pub transition_count: usize,
}

/// Thin ensure facade + analysis dispatcher over the project Focus materialize stack.
///
/// Not a second materialize configuration. Prefer this over calling dataflow
/// ensure APIs or analysis engines ad hoc from MCP handlers.
pub struct AnalysisRuntime {
    materialize: FocusMaterialize,
}

impl AnalysisRuntime {
    /// Build from the project-wide Focus materialize stack.
    pub fn from_materialize(materialize: FocusMaterialize) -> Self {
        Self { materialize }
    }

    /// On-demand dataflow ensure service (`LazyDataflowService` mechanism type).
    #[allow(dead_code)] // shared-stack wiring tests and diagnostics
    pub fn dataflow(&self) -> &LazyDataflowService {
        self.materialize.dataflow()
    }

    /// Shared Focus materialize stack.
    #[allow(dead_code)]
    pub fn materialize(&self) -> &FocusMaterialize {
        &self.materialize
    }

    /// Trigger on-demand dataflow extraction for a function symbol.
    fn ensure_dataflow_for_function(
        &self,
        symbol_id: &SymbolId,
        query_id: Option<&str>,
    ) -> anyhow::Result<LazyWindow> {
        self.materialize
            .dataflow()
            .ensure_for_function(symbol_id, query_id)
    }

    /// Ensure CFG nodes and edges are available for a function, with Focus materialize fallback.
    fn ensure_cfg_for_function(
        &self,
        store: &Store,
        sid: &SymbolId,
        query_id: &str,
        fn_name: &str,
    ) -> Result<(Vec<CfgNode>, Vec<CfgEdge>), String> {
        let mut cfg_nodes = store
            .find_cfg_nodes_by_function(sid)
            .map_err(|e| format!("Failed to load CFG nodes: {e}"))?;

        if cfg_nodes.is_empty() {
            self.ensure_dataflow_for_function(sid, Some(query_id))
                .map_err(|e| format!("CFG not available for analysis of '{fn_name}': {e:#}"))?;
            cfg_nodes = store
                .find_cfg_nodes_by_function(sid)
                .map_err(|e| format!("Failed to load CFG nodes after Focus materialize: {e}"))?;
        }

        if cfg_nodes.is_empty() {
            return Err(format!(
                "CFG not available for '{fn_name}'. The function may be in a language that does not yet support CFG extraction, or the source file could not be read."
            ));
        }

        let cfg_edges = store.find_cfg_edges_by_function(sid).unwrap_or_default();
        Ok((cfg_nodes, cfg_edges))
    }

    // ── Capability gates ────────────────────────────────────────────────

    /// Lifecycle analysis is C/C++ only. Returns (qname, language) when supported.
    pub fn require_lifecycle_language(
        &self,
        store: &Store,
        sid: &SymbolId,
    ) -> Result<(String, Language), LifecycleAnalysisErr> {
        store
            .find_symbol_by_id(sid)
            .ok()
            .flatten()
            .and_then(|s| match s.language {
                Language::C | Language::Cpp => Some((s.qualified_name, s.language)),
                _ => None,
            })
            .ok_or(LifecycleAnalysisErr::UnsupportedLanguage)
    }

    // ── Store I/O + composition (dispatcher-owned) ──────────────────────

    /// Load dataflow nodes and edges for a function (best-effort empty on error).
    fn load_dataflow_facts(
        &self,
        store: &Store,
        sid: &SymbolId,
    ) -> (Vec<atlas_engine::DataNode>, Vec<atlas_engine::DataFlowEdge>) {
        let data_nodes = store.find_data_nodes_by_function(sid).unwrap_or_default();
        let dataflow_edges = if data_nodes.is_empty() {
            Vec::new()
        } else {
            let ids: Vec<_> = data_nodes.iter().map(|n| n.id).collect();
            store
                .find_dataflow_edges_by_sources(&ids)
                .unwrap_or_default()
        };
        (data_nodes, dataflow_edges)
    }

    /// Build effect composition from CFG + dataflow facts for a language.
    fn compose_effects_for(
        &self,
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        data_nodes: &[atlas_engine::DataNode],
        dataflow_edges: &[atlas_engine::DataFlowEdge],
        language: Language,
        ownership_rules: Option<&CppOwnershipRules>,
    ) -> EffectComposition {
        let contract = resource_contract_for(language, ownership_rules);
        match analysis::cfg_graph::CfgGraph::build(cfg_nodes, cfg_edges) {
            Ok(cfg_graph) => {
                analysis::compose_effects(&cfg_graph, data_nodes, dataflow_edges, &contract)
            }
            Err(_) => EffectComposition::default(),
        }
    }

    /// Ensure dataflow (soft), load facts, compose effects for a function.
    fn semantic_composition_for_function(
        &self,
        store: &Store,
        sid: &SymbolId,
        cfg: (&[CfgNode], &[CfgEdge]),
        language: Language,
        ownership_rules: Option<&CppOwnershipRules>,
        query_id: Option<&str>,
    ) -> (EffectComposition, Option<LazyWindow>, Option<String>) {
        let (cfg_nodes, cfg_edges) = cfg;
        let mut dataflow_error = None;
        let mut semantic_window = None;
        match self.ensure_dataflow_for_function(sid, query_id) {
            Ok(window) => semantic_window = Some(window),
            Err(e) => dataflow_error = Some(format!("{e:#}")),
        }
        let (data_nodes, dataflow_edges) = self.load_dataflow_facts(store, sid);
        let composition = self.compose_effects_for(
            cfg_nodes,
            cfg_edges,
            &data_nodes,
            &dataflow_edges,
            language,
            ownership_rules,
        );
        (composition, semantic_window, dataflow_error)
    }

    /// Load domain ownership rules for a language from the store.
    fn load_ownership_rules(&self, store: &Store, language: Language) -> CppOwnershipRules {
        CppOwnershipRules::load_for(store, language.as_str())
    }

    /// Field lifecycle with an already-built composition (graph impact path).
    fn analyze_lifecycle_with_composition(
        &self,
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        field_path: &str,
        rules: &CppOwnershipRules,
        composition: &EffectComposition,
    ) -> FieldLifecycleResult {
        let ownership_rules = OwnershipRules::default();
        FieldLifecycleEngine::analyze_with_composition(
            cfg_nodes,
            cfg_edges,
            field_path,
            &ownership_rules,
            rules,
            composition,
        )
    }

    /// Semantic branch-diff given composition (graph impact path).
    fn analyze_branch_diff_semantic(
        &self,
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        composition: &EffectComposition,
    ) -> Vec<BranchDiff> {
        BranchDiffEngine::diff_branches_semantic(cfg_nodes, cfg_edges, composition)
    }

    /// CFG-only branch-diff.
    fn analyze_branch_diff_cfg(
        &self,
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
    ) -> Vec<BranchDiff> {
        BranchDiffEngine::diff_branches(cfg_nodes, cfg_edges)
    }

    // ── Full tool orchestration ─────────────────────────────────────────

    /// Full lifecycle path: language gate → CFG → dataflow/compose → rules → engine.
    ///
    /// Handler supplies resolved `sid` / field; this method owns capability and orchestration.
    pub fn run_lifecycle(
        &self,
        store: &Store,
        sid: &SymbolId,
        field: &str,
        query_id: &str,
        symbol_name: &str,
    ) -> Result<LifecycleAnalysisOk, LifecycleAnalysisErr> {
        let (cfg_nodes, cfg_edges) = self
            .ensure_cfg_for_function(store, sid, query_id, symbol_name)
            .map_err(LifecycleAnalysisErr::CfgUnavailable)?;

        let (qname, language) = self.require_lifecycle_language(store, sid)?;

        let cpp_rules = self.load_ownership_rules(store, language);
        let (composition, _window, dataflow_error) = self.semantic_composition_for_function(
            store,
            sid,
            (&cfg_nodes, &cfg_edges),
            language,
            Some(&cpp_rules),
            Some(query_id),
        );

        let has_any_rules = cpp_rules.has_any_rules();
        let has_user_rules = cpp_rules.has_user_rules();

        let mut result = self.analyze_lifecycle_with_composition(
            &cfg_nodes,
            &cfg_edges,
            field,
            &cpp_rules,
            &composition,
        );
        result.function_qname = qname;

        Ok(LifecycleAnalysisOk {
            result,
            has_user_rules,
            has_any_rules,
            dataflow_error,
        })
    }

    /// Full branch-diff path: CFG → (optional semantic compose) → engine.
    pub fn run_branch_diff(
        &self,
        store: &Store,
        sid: &SymbolId,
        query_id: &str,
        symbol_name: &str,
        use_semantic: bool,
    ) -> Result<BranchDiffAnalysisOk, String> {
        let (cfg_nodes, cfg_edges) =
            self.ensure_cfg_for_function(store, sid, query_id, symbol_name)?;

        let qname = store
            .find_symbol_by_id(sid)
            .ok()
            .flatten()
            .map(|s| s.qualified_name)
            .unwrap_or_else(|| symbol_name.to_string());

        if !use_semantic {
            let diffs = self.analyze_branch_diff_cfg(&cfg_nodes, &cfg_edges);
            return Ok(BranchDiffAnalysisOk {
                diffs,
                qname,
                semantic_window: None,
                dataflow_refinement_failed: false,
            });
        }

        let language = store
            .find_symbol_by_id(sid)
            .ok()
            .flatten()
            .map(|s| s.language)
            .unwrap_or(Language::C);

        let ownership_rules = match language {
            Language::C | Language::Cpp => Some(self.load_ownership_rules(store, language)),
            _ => None,
        };

        let (composition, semantic_window, dataflow_error) = self
            .semantic_composition_for_function(
                store,
                sid,
                (&cfg_nodes, &cfg_edges),
                language,
                ownership_rules.as_ref(),
                Some(query_id),
            );

        let dataflow_refinement_failed = dataflow_error.is_some();
        let diffs = self.analyze_branch_diff_semantic(&cfg_nodes, &cfg_edges, &composition);

        Ok(BranchDiffAnalysisOk {
            diffs,
            qname,
            semantic_window,
            dataflow_refinement_failed,
        })
    }

    /// Analyze the callable portion of an impact subgraph using existing CFG
    /// facts and Focus-owned dataflow refinement.
    ///
    /// Graph traversal remains the handler's concern. Once the target IDs are
    /// known, this method owns capability gates, store reads, composition,
    /// engine calls, and deterministic result aggregation.
    pub fn run_semantic_impact(
        &self,
        store: &Store,
        target_ids: &[SymbolId],
    ) -> SemanticImpactAnalysisOk {
        let mut output = SemanticImpactAnalysisOk::default();
        let mut c_rules: Option<CppOwnershipRules> = None;
        let mut cpp_rules: Option<CppOwnershipRules> = None;

        for sid in target_ids.iter().take(SEMANTIC_IMPACT_FUNCTION_LIMIT) {
            let Some(symbol) = store.find_symbol_by_id(sid).ok().flatten() else {
                continue;
            };
            if !matches!(
                symbol.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            ) {
                continue;
            }

            let Ok(cfg_nodes) = store.find_cfg_nodes_by_function(sid) else {
                continue;
            };
            if cfg_nodes.is_empty() {
                continue;
            }
            let ownership_rules: Option<&CppOwnershipRules> = match symbol.language {
                Language::C => Some(
                    &*c_rules.get_or_insert_with(|| self.load_ownership_rules(store, Language::C)),
                ),
                Language::Cpp => Some(
                    &*cpp_rules
                        .get_or_insert_with(|| self.load_ownership_rules(store, Language::Cpp)),
                ),
                _ => None,
            };
            let cfg_edges = store.find_cfg_edges_by_function(sid).unwrap_or_default();
            let (composition, _window, _dataflow_error) = self.semantic_composition_for_function(
                store,
                sid,
                (&cfg_nodes, &cfg_edges),
                symbol.language,
                ownership_rules,
                None,
            );
            if ownership_rules.is_some_and(has_composition_rules) {
                output.domain_rules_applied = true;
            }

            self.collect_semantic_impact_for_function(
                &mut output,
                &symbol.qualified_name,
                &cfg_nodes,
                &cfg_edges,
                &composition,
                ownership_rules,
            );
        }

        output
    }

    fn collect_semantic_impact_for_function(
        &self,
        output: &mut SemanticImpactAnalysisOk,
        function_qname: &str,
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        composition: &EffectComposition,
        ownership_rules: Option<&CppOwnershipRules>,
    ) {
        if let Some(rules) = ownership_rules {
            for field_path in field_paths_from_composition(composition) {
                let mut lifecycle = self.analyze_lifecycle_with_composition(
                    cfg_nodes,
                    cfg_edges,
                    &field_path,
                    rules,
                    composition,
                );
                lifecycle.function_qname = function_qname.to_string();

                if !lifecycle.suspicious_points.is_empty() {
                    let issues = lifecycle
                        .suspicious_points
                        .iter()
                        .map(|point| SemanticImpactIssue {
                            line: point.line,
                            kind: format!("{:?}", point.kind),
                            message: point.message.clone(),
                        })
                        .collect::<Vec<_>>();
                    output.invariants_affected.push(SemanticImpactInvariant {
                        function: function_qname.to_string(),
                        field: field_path.clone(),
                        issue_count: issues.len(),
                        issues,
                    });
                }

                if lifecycle.transitions.len() >= 2 {
                    output
                        .lifecycle_paths_affected
                        .push(SemanticImpactLifecyclePath {
                            function: function_qname.to_string(),
                            field: field_path,
                            final_state: lifecycle.final_state.as_str().to_string(),
                            transition_count: lifecycle.transitions.len(),
                        });
                }
            }
        }

        for diff in self.analyze_branch_diff_semantic(cfg_nodes, cfg_edges, composition) {
            let Some(asymmetry) = diff.suspicious_asymmetry else {
                continue;
            };
            output.invariants_affected.push(SemanticImpactInvariant {
                function: function_qname.to_string(),
                field: diff.common_prefix,
                issue_count: 1,
                issues: vec![SemanticImpactIssue {
                    line: diff.branch_node_line,
                    kind: "BranchAsymmetry".to_string(),
                    message: asymmetry,
                }],
            });
        }
    }
}

fn field_paths_from_composition(composition: &EffectComposition) -> BTreeSet<String> {
    let mut fields = BTreeSet::new();
    for effects in composition.node_effects.values() {
        for effect in effects {
            let path = match &effect.kind {
                SemanticEffectKind::Free {
                    place: PlaceRef::Field { path },
                    ..
                }
                | SemanticEffectKind::Alloc {
                    target: PlaceRef::Field { path },
                    ..
                }
                | SemanticEffectKind::Store {
                    dst: PlaceRef::Field { path },
                    ..
                }
                | SemanticEffectKind::Assign {
                    dst: PlaceRef::Field { path },
                    ..
                }
                | SemanticEffectKind::Nullify {
                    place: PlaceRef::Field { path },
                } => Some(path),
                _ => None,
            };
            if let Some(path) = path.filter(|path| !path.is_empty()) {
                fields.insert(path.clone());
            }
        }
    }
    fields
}

fn resource_contract_for(
    language: Language,
    ownership_rules: Option<&CppOwnershipRules>,
) -> ResourceOpConfig {
    let mut contract = ResourceOpConfig::default_for(language);
    let Some(rules) = ownership_rules else {
        return contract;
    };

    contract
        .producers
        .extend(rules.allocation_functions.iter().map(|(pattern, _)| {
            ResourceOpPattern::new(
                ResourceOpKind::Produce,
                CalleeMatcher::Exact(pattern.clone()),
                0,
            )
            .with_implicit_cleanup(false)
        }));
    contract.consumers.extend(
        rules
            .free_functions
            .iter()
            .chain(rules.cleanup_functions.iter())
            .map(|(pattern, _)| {
                ResourceOpPattern::new(
                    ResourceOpKind::Consume,
                    CalleeMatcher::Exact(pattern.clone()),
                    0,
                )
            }),
    );
    contract
}

fn has_composition_rules(rules: &CppOwnershipRules) -> bool {
    !rules.free_functions.is_empty()
        || !rules.allocation_functions.is_empty()
        || !rules.cleanup_functions.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::effects::{SemanticEffect, ValueSource};
    use atlas_engine::{
        CallContext, CfgEdgeKind, CfgNodeId, CfgNodeKind, DataFlowEdge, DataFlowEdgeId,
        DataFlowKind, DataNode, DataNodeId, EffectId, SymbolId, TextRange,
    };
    use std::sync::Arc;

    fn runtime_with_store() -> (AnalysisRuntime, Arc<Store>) {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        let store = Arc::new(store);
        let ar = AnalysisRuntime::from_materialize(FocusMaterialize::open(store.clone(), None));
        (ar, store)
    }

    fn fid() -> SymbolId {
        SymbolId::from_bytes([0xA1; 32])
    }

    fn make_node(kind: CfgNodeKind, line: u32, byte: u32) -> CfgNode {
        make_node_for(fid(), kind, line, byte)
    }

    fn make_node_for(function_id: SymbolId, kind: CfgNodeKind, line: u32, byte: u32) -> CfgNode {
        CfgNode {
            id: CfgNodeId::generate(&function_id, kind.as_str(), byte),
            function_id,
            kind,
            stmt_range: TextRange {
                start_byte: byte,
                end_byte: byte,
                start_line: line,
                start_column: 0,
                end_line: line,
                end_column: 0,
            },
            call_context: CallContext::None,
            semantic_effects: vec![],
        }
    }

    fn branched_cfg() -> (Vec<CfgNode>, Vec<CfgEdge>) {
        branched_cfg_for(fid())
    }

    fn branched_cfg_for(function_id: SymbolId) -> (Vec<CfgNode>, Vec<CfgEdge>) {
        let entry = make_node_for(function_id, CfgNodeKind::Entry, 0, 0);
        let branch = make_node_for(function_id, CfgNodeKind::Branch, 10, 1);
        let t = make_node_for(function_id, CfgNodeKind::Statement, 11, 2);
        let f = make_node_for(function_id, CfgNodeKind::Statement, 12, 3);
        let join = make_node_for(function_id, CfgNodeKind::Join, 13, 4);
        let exit = make_node_for(function_id, CfgNodeKind::Exit, 14, 5);
        let edges = vec![
            CfgEdge::new(&entry.id, &branch.id, CfgEdgeKind::Normal),
            CfgEdge::new(&branch.id, &t.id, CfgEdgeKind::TrueBranch),
            CfgEdge::new(&branch.id, &f.id, CfgEdgeKind::FalseBranch),
            CfgEdge::new(&t.id, &join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&f.id, &join.id, CfgEdgeKind::Normal),
            CfgEdge::new(&join.id, &exit.id, CfgEdgeKind::Normal),
        ];
        (vec![entry, branch, t, f, join, exit], edges)
    }

    fn semantic_effect(node_id: CfgNodeId, order: u32, kind: SemanticEffectKind) -> SemanticEffect {
        SemanticEffect {
            id: EffectId::generate(&node_id, order, "semantic-impact-test"),
            cfg_node_id: node_id,
            order,
            kind,
            confidence: 1.0,
            consumption_style: None,
            description: None,
            eligible_for_implicit_cleanup: None,
        }
    }

    #[test]
    fn analysis_runtime_uses_materialize_with_rebuilder() {
        let (ar, _) = runtime_with_store();
        assert!(ar.dataflow().has_structural_rebuilder());
    }

    #[test]
    fn capability_gate_rejects_missing_symbol() {
        let (ar, store) = runtime_with_store();
        let sid = SymbolId::from_bytes([0xBB; 32]);
        let err = ar
            .require_lifecycle_language(store.as_ref(), &sid)
            .expect_err("missing symbol is unsupported");
        assert!(matches!(err, LifecycleAnalysisErr::UnsupportedLanguage));
    }

    fn insert_fn(store: &Store, lang: Language, name: &str) -> SymbolId {
        let path = format!("{name}.{}", lang.as_str());
        let file_id = atlas_engine::FileId::generate(&path);
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id,
                path,
                language: lang,
                content_hash: "h".into(),
                status: atlas_engine::ParseStatus::Success,
            })
            .unwrap();
        let sid = SymbolId::generate(&file_id, lang.as_str(), name, "function", None);
        store
            .insert_symbols(&[atlas_engine::SymbolDef {
                id: sid,
                kind: atlas_engine::SymbolKind::Function,
                name: name.into(),
                qualified_name: name.into(),
                symbol_path: vec![name.into()],
                file_id,
                language: lang,
                range: TextRange::default(),
                name_range: TextRange::default(),
                signature: None,
                visibility: None,
                exported: false,
                static_: false,
                async_: false,
                container: None,
                scope_id: None,
                package_name: None,
                namespace_path: vec![],
                layer: "structural".into(),
            }])
            .unwrap();
        sid
    }

    /// Dispatcher owns C/C++ capability gate — TypeScript must fail even if
    /// the symbol resolves (not only "missing symbol").
    #[test]
    fn capability_gate_rejects_non_cpp_language() {
        let (ar, store) = runtime_with_store();
        let sid = insert_fn(store.as_ref(), Language::TypeScript, "ts_fn");
        let err = ar
            .require_lifecycle_language(store.as_ref(), &sid)
            .expect_err("TS is unsupported for lifecycle");
        assert!(matches!(err, LifecycleAnalysisErr::UnsupportedLanguage));
    }

    #[test]
    fn capability_gate_accepts_c_and_cpp() {
        let (ar, store) = runtime_with_store();
        for (lang, name) in [(Language::C, "c_fn"), (Language::Cpp, "cpp_fn")] {
            let sid = insert_fn(store.as_ref(), lang, name);
            let (qname, got) = ar
                .require_lifecycle_language(store.as_ref(), &sid)
                .expect("C/C++ must pass lifecycle gate");
            assert_eq!(qname, name);
            assert_eq!(got, lang);
        }
    }

    #[test]
    fn compose_effects_for_empty_facts_is_default_shaped() {
        let (ar, _) = runtime_with_store();
        let (nodes, edges) = branched_cfg();
        let composition = ar.compose_effects_for(&nodes, &edges, &[], &[], Language::C, None);
        // No dataflow facts → composer yields empty node_effects.
        assert!(
            composition.node_effects.is_empty(),
            "empty dataflow should not invent effects"
        );
        let diffs = ar.analyze_branch_diff_semantic(&nodes, &edges, &composition);
        let via_engine = BranchDiffEngine::diff_branches_semantic(&nodes, &edges, &composition);
        assert_eq!(diffs.len(), via_engine.len());
    }

    #[test]
    fn analyze_branch_diff_cfg_matches_engine() {
        let (ar, _) = runtime_with_store();
        let (nodes, edges) = branched_cfg();
        let via_runtime = ar.analyze_branch_diff_cfg(&nodes, &edges);
        let via_engine = BranchDiffEngine::diff_branches(&nodes, &edges);
        assert_eq!(via_runtime.len(), via_engine.len());
        assert_eq!(via_runtime.len(), 1);
        assert_eq!(via_runtime[0].branch_node_line, 10);
        assert_eq!(
            via_runtime[0].branch_node_line,
            via_engine[0].branch_node_line
        );
    }

    #[test]
    fn compose_effects_uses_persisted_cpp_ownership_rules() {
        let (ar, store) = runtime_with_store();
        store
            .upsert_domain_rule(
                "c",
                "free_fn",
                "release_owned",
                "exact",
                "user",
                "enabled",
                1.0,
                None,
            )
            .unwrap();
        store
            .upsert_domain_rule(
                "c",
                "alloc_fn",
                "acquire_owned",
                "exact",
                "user",
                "enabled",
                1.0,
                None,
            )
            .unwrap();
        let rules = ar.load_ownership_rules(store.as_ref(), Language::C);
        let cpp_contract = resource_contract_for(Language::Cpp, Some(&rules));
        let persisted_alloc = cpp_contract
            .producers
            .iter()
            .find(|pattern| pattern.matcher.matches("acquire_owned"))
            .expect("persisted alloc_fn must extend the default config");
        assert!(
            !persisted_alloc.implicit_cleanup,
            "custom C/C++ allocators require explicit cleanup unless a richer rule says otherwise"
        );

        let (mut nodes, edges) = branched_cfg();
        nodes[2].stmt_range.end_byte = nodes[2].stmt_range.start_byte + 1;
        let statement_id = nodes[2].id;
        let function_id = nodes[2].function_id;
        let range = nodes[2].stmt_range.clone();
        let file_id = atlas_engine::FileId::generate("rule.c");
        let call_target = DataNode::call_target(
            DataNodeId::generate(
                &file_id,
                Some(&function_id),
                "call_target",
                Some("release_owned"),
                Some("release_owned"),
                range.start_byte,
            ),
            file_id,
            Some(function_id),
            None,
            "release_owned",
            "release_owned",
            range,
        );
        let default_target = DataNode::call_target(
            DataNodeId::generate(
                &file_id,
                Some(&function_id),
                "call_target",
                Some("widget_free"),
                Some("widget_free"),
                nodes[2].stmt_range.start_byte,
            ),
            file_id,
            Some(function_id),
            None,
            "widget_free",
            "widget_free",
            nodes[2].stmt_range.clone(),
        );

        let without_rules = ar.compose_effects_for(
            &nodes,
            &edges,
            &[call_target.clone()],
            &[],
            Language::C,
            None,
        );
        assert!(without_rules.node_effects.is_empty());

        let with_rules = ar.compose_effects_for(
            &nodes,
            &edges,
            &[call_target, default_target],
            &[],
            Language::C,
            Some(&rules),
        );
        let effects = with_rules
            .node_effects
            .get(&statement_id)
            .expect("persisted free_fn must produce an effect");
        assert!(effects.iter().any(|effect| matches!(
            &effect.kind,
            SemanticEffectKind::Free { callee, .. } if callee == "release_owned"
        )));
        assert!(
            effects.iter().any(|effect| matches!(
                &effect.kind,
                SemanticEffectKind::Free { callee, .. } if callee == "widget_free"
            )),
            "persisted rules must extend, not replace, the default C suffix matcher"
        );
    }

    #[test]
    fn run_lifecycle_cfg_unavailable_without_symbol_cfg() {
        let (ar, store) = runtime_with_store();
        let sid = SymbolId::from_bytes([0xCC; 32]);
        let err = ar
            .run_lifecycle(store.as_ref(), &sid, "buf", "q1", "missing_fn")
            .expect_err("no CFG → error");
        assert!(matches!(err, LifecycleAnalysisErr::CfgUnavailable(_)));
    }

    #[test]
    fn run_branch_diff_cfg_unavailable_without_symbol_cfg() {
        let (ar, store) = runtime_with_store();
        let sid = SymbolId::from_bytes([0xDD; 32]);
        let err = ar
            .run_branch_diff(store.as_ref(), &sid, "q1", "missing_fn", false)
            .expect_err("no CFG → error");
        assert!(err.contains("CFG not available") || err.contains("Failed to load CFG"));
    }

    #[test]
    fn semantic_impact_dispatcher_reads_persisted_cfg_and_dataflow() {
        let (ar, store) = runtime_with_store();
        let sid = insert_fn(store.as_ref(), Language::C, "worker");
        let symbol = store.find_symbol_by_id(&sid).unwrap().unwrap();
        let (mut nodes, edges) = branched_cfg_for(sid);
        nodes[2].stmt_range.end_byte = nodes[2].stmt_range.start_byte + 1;
        let effect_range = nodes[2].stmt_range.clone();
        store.insert_cfg_nodes(&nodes).unwrap();
        store.insert_cfg_edges(&edges).unwrap();
        store
            .upsert_domain_rule(
                "c",
                "free_fn",
                "release_owned",
                "exact",
                "user",
                "enabled",
                1.0,
                None,
            )
            .unwrap();

        let field = DataNode::field(
            DataNodeId::generate(
                &symbol.file_id,
                Some(&sid),
                "field",
                Some("ptr"),
                Some("state.ptr"),
                effect_range.start_byte,
            ),
            symbol.file_id,
            Some(sid),
            "ptr",
            "state.ptr",
            effect_range.clone(),
        );
        let call_arg = DataNode::call_arg(
            DataNodeId::generate(
                &symbol.file_id,
                Some(&sid),
                "call_arg",
                Some("ptr"),
                Some("state.ptr"),
                effect_range.start_byte,
            ),
            symbol.file_id,
            Some(sid),
            None,
            Some("ptr"),
            effect_range.clone(),
        );
        let call_target = DataNode::call_target(
            DataNodeId::generate(
                &symbol.file_id,
                Some(&sid),
                "call_target",
                Some("release_owned"),
                Some("release_owned"),
                effect_range.start_byte,
            ),
            symbol.file_id,
            Some(sid),
            None,
            "release_owned",
            "release_owned",
            effect_range.clone(),
        );
        let field_to_arg = DataFlowEdge::new(
            DataFlowEdgeId::generate(&field.id, &call_arg.id, DataFlowKind::FieldLoad.as_str()),
            field.id,
            call_arg.id,
            DataFlowKind::FieldLoad,
            effect_range,
            1.0,
        );
        store
            .insert_data_nodes(&[field, call_arg, call_target])
            .unwrap();
        store.insert_dataflow_edges(&[field_to_arg]).unwrap();

        let output = ar.run_semantic_impact(store.as_ref(), &[sid]);
        assert!(output.domain_rules_applied);
        let invariant = output
            .invariants_affected
            .iter()
            .find(|invariant| invariant.field == "state.ptr")
            .expect("persisted dataflow must produce a branch asymmetry");
        assert_eq!(invariant.function, "worker");
        assert_eq!(invariant.field, "state.ptr");
        assert_eq!(invariant.issue_count, 1);
        assert_eq!(invariant.issues[0].kind, "BranchAsymmetry");
        assert_eq!(invariant.issues[0].line, 10);
    }

    #[test]
    fn semantic_impact_field_paths_are_deterministic_and_non_empty() {
        let node = make_node(CfgNodeKind::Statement, 1, 10);
        let mut composition = EffectComposition::default();
        composition.node_effects.insert(
            node.id,
            vec![
                semantic_effect(
                    node.id,
                    0,
                    SemanticEffectKind::Store {
                        dst: PlaceRef::Field {
                            path: "z.last".into(),
                        },
                        src: ValueSource::Unknown,
                    },
                ),
                semantic_effect(
                    node.id,
                    1,
                    SemanticEffectKind::Free {
                        place: PlaceRef::Field {
                            path: "a.first".into(),
                        },
                        callee: "free".into(),
                    },
                ),
                semantic_effect(
                    node.id,
                    2,
                    SemanticEffectKind::Nullify {
                        place: PlaceRef::Field {
                            path: "m.middle".into(),
                        },
                    },
                ),
                semantic_effect(
                    node.id,
                    3,
                    SemanticEffectKind::Alloc {
                        target: PlaceRef::Field {
                            path: String::new(),
                        },
                        callee: "malloc".into(),
                    },
                ),
            ],
        );

        assert_eq!(
            field_paths_from_composition(&composition)
                .into_iter()
                .collect::<Vec<_>>(),
            vec![
                "a.first".to_string(),
                "m.middle".to_string(),
                "z.last".to_string()
            ]
        );
    }

    #[test]
    fn semantic_impact_reports_only_persisted_rules_actually_in_scope() {
        let (ar, store) = runtime_with_store();
        let c_sid = insert_fn(store.as_ref(), Language::C, "c_worker");
        let ts_sid = insert_fn(store.as_ref(), Language::TypeScript, "ts_worker");
        for sid in [c_sid, ts_sid] {
            let (nodes, edges) = branched_cfg_for(sid);
            store.insert_cfg_nodes(&nodes).unwrap();
            store.insert_cfg_edges(&edges).unwrap();
        }

        let no_rules = ar.run_semantic_impact(store.as_ref(), &[c_sid, ts_sid]);
        assert!(!no_rules.domain_rules_applied);

        store
            .upsert_domain_rule(
                "c",
                "owned_pattern",
                "state.ptr*",
                "glob",
                "user",
                "enabled",
                1.0,
                None,
            )
            .unwrap();
        let owned_pattern_only = ar.run_semantic_impact(store.as_ref(), &[c_sid]);
        assert!(
            !owned_pattern_only.domain_rules_applied,
            "owned_pattern is not an effect-composition rule"
        );

        store
            .upsert_domain_rule(
                "c",
                "free_fn",
                "release_owned",
                "exact",
                "user",
                "enabled",
                1.0,
                None,
            )
            .unwrap();
        let c_and_ts = ar.run_semantic_impact(store.as_ref(), &[c_sid, ts_sid]);
        assert!(c_and_ts.domain_rules_applied);

        let ts_only = ar.run_semantic_impact(store.as_ref(), &[ts_sid]);
        assert!(
            !ts_only.domain_rules_applied,
            "C rules must not be reported as applied to a TypeScript-only impact set"
        );
    }
}
