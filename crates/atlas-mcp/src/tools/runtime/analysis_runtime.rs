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
//! - `run_lifecycle` / `run_branch_diff` full orchestration
//! - Shared helpers for graph impact semantic path

use atlas_engine::analysis::{
    self, BranchDiff, BranchDiffEngine, CppOwnershipRules, EffectComposition, FieldLifecycleEngine,
    FieldLifecycleResult, OwnershipRules, ResourceOpConfig,
};
use atlas_engine::{
    CfgEdge, CfgNode, FocusMaterialize, Language, LazyDataflowService, LazyWindow, Store, SymbolId,
};

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
    pub fn ensure_dataflow_for_function(
        &self,
        symbol_id: &SymbolId,
        query_id: Option<&str>,
    ) -> anyhow::Result<LazyWindow> {
        self.materialize
            .dataflow()
            .ensure_for_function(symbol_id, query_id)
    }

    /// Ensure CFG nodes and edges are available for a function, with Focus materialize fallback.
    pub fn ensure_cfg_for_function(
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
    pub fn load_dataflow_facts(
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
    pub fn compose_effects_for(
        &self,
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        data_nodes: &[atlas_engine::DataNode],
        dataflow_edges: &[atlas_engine::DataFlowEdge],
        language: Language,
    ) -> EffectComposition {
        let contract = ResourceOpConfig::default_for(language);
        match analysis::cfg_graph::CfgGraph::build(cfg_nodes, cfg_edges) {
            Ok(cfg_graph) => analysis::compose_effects(
                &cfg_graph,
                data_nodes,
                dataflow_edges,
                &contract,
            ),
            Err(_) => EffectComposition::default(),
        }
    }

    /// Ensure dataflow (soft), load facts, compose effects for a function.
    pub fn semantic_composition_for_function(
        &self,
        store: &Store,
        sid: &SymbolId,
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        language: Language,
        query_id: Option<&str>,
    ) -> (EffectComposition, Option<LazyWindow>, Option<String>) {
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
        );
        (composition, semantic_window, dataflow_error)
    }

    /// Load domain ownership rules for a language from the store.
    pub fn load_ownership_rules(&self, store: &Store, language: Language) -> CppOwnershipRules {
        CppOwnershipRules::load_for(store, language.as_str())
    }

    /// Field lifecycle with an already-built composition (graph impact path).
    pub fn analyze_lifecycle_with_composition(
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
    pub fn analyze_branch_diff_semantic(
        &self,
        cfg_nodes: &[CfgNode],
        cfg_edges: &[CfgEdge],
        composition: &EffectComposition,
    ) -> Vec<BranchDiff> {
        BranchDiffEngine::diff_branches_semantic(cfg_nodes, cfg_edges, composition)
    }

    /// CFG-only branch-diff.
    pub fn analyze_branch_diff_cfg(
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

        let (composition, _window, dataflow_error) = self.semantic_composition_for_function(
            store,
            sid,
            &cfg_nodes,
            &cfg_edges,
            language,
            Some(query_id),
        );

        let cpp_rules = self.load_ownership_rules(store, language);
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

        let (composition, semantic_window, dataflow_error) = self
            .semantic_composition_for_function(
                store,
                sid,
                &cfg_nodes,
                &cfg_edges,
                language,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_engine::{
        CallContext, CfgEdgeKind, CfgNodeId, CfgNodeKind, SymbolId, TextRange,
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
        let f = fid();
        CfgNode {
            id: CfgNodeId::generate(&f, kind.as_str(), byte),
            function_id: f,
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
        let entry = make_node(CfgNodeKind::Entry, 0, 0);
        let branch = make_node(CfgNodeKind::Branch, 10, 1);
        let t = make_node(CfgNodeKind::Statement, 11, 2);
        let f = make_node(CfgNodeKind::Statement, 12, 3);
        let join = make_node(CfgNodeKind::Join, 13, 4);
        let exit = make_node(CfgNodeKind::Exit, 14, 5);
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
        let composition =
            ar.compose_effects_for(&nodes, &edges, &[], &[], Language::C);
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
        assert_eq!(via_runtime[0].branch_node_line, via_engine[0].branch_node_line);
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
}
