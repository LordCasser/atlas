//! `usages` — find all reference usages of a symbol.

use atlas_engine::InvestigationFocus;

use super::analysis_envelope::AnalysisEnvelope;
use super::{ToolRouter, get_u64};
use crate::tools::symbol_selector::{
    SymbolInput, SymbolResolution, SymbolResolutionPolicy, parse_symbol_input,
};
use serde_json::json;

impl ToolRouter {
    pub(crate) fn handle_usages(&mut self, args: &serde_json::Value) -> (String, bool) {
        let limit = get_u64(args, "limit").unwrap_or(50) as usize;

        // Unified symbol resolution via SymbolInput (string or structured selector).
        let input: SymbolInput = match parse_symbol_input(args, "symbol") {
            Ok(inp) => inp,
            Err(e) => return (e, true),
        };
        let resolution =
            match self.resolve_symbol_input(&input, SymbolResolutionPolicy::BestEffortSingle) {
                Ok(r) => r,
                Err(e) => return (e, true),
            };
        let sid = match resolution {
            SymbolResolution::Single { symbol_id, .. } => symbol_id,
            SymbolResolution::Ambiguous { candidates, .. } => candidates[0].symbol_id,
            SymbolResolution::NotFound { qname, .. } => {
                return (format!("Symbol not found: {qname}"), true);
            }
        };

        self.update_investigation(InvestigationFocus::Symbol(sid));
        let _investigation = self
            .active_mut()
            .job_runtime
            .investigation_state
            .active_investigation
            .clone();

        // Resolve symbol display name early so it's available for both the
        // focus intent and the response "symbol" field.
        let symbol_display = match &input {
            SymbolInput::Name(s) => s.clone(),
            SymbolInput::Selector(sel) => sel.qualified_name.clone(),
        };

        // Prepare focus query to inject coverage / closure provenance.
        // Uses Calls intent with the resolved symbol_id so the focus engine
        // can locate the seed and build structural coverage data.  (P1-F7)
        let (focus_opt, _focus_warnings) =
            self.prepare_focus_query(Some(atlas_engine::QueryIntent::Calls {
                symbol_name: symbol_display.clone(),
                direction: None,
                depth: None,
                file_id: None,
                symbol_id: Some(sid),
            }));

        let mut lr = AnalysisEnvelope::new("symbol", args);

        let refs = match self.active_mut().store.find_references_by_symbol(&sid) {
            Ok(r) => r,
            Err(e) => return (format!("Failed to query usages: {e}"), true),
        };

        let shown = refs.iter().take(limit.min(100));
        let usages: Vec<_> = shown
            .map(|r| {
                let mask = self
                    .active_mut()
                    .store
                    .get_capability_mask(&r.file_id)
                    .unwrap_or_default();
                json!({
                    "text": r.text,
                    "kind": r.kind.as_str(),
                    "file": self.active_mut().store_query_runtime.resolve_file_path(&r.file_id),
                    "line": r.range.start_line + 1,
                    "column": r.range.start_column + 1,
                    "evidence_level": mask.best_capability_name(),
                    "source_capability": mask.bits(),
                })
            })
            .collect();

        let resp = json!({
            "symbol": symbol_display,
            "total_usages": refs.len(),
            "usages": usages,
        });

        // Inject public focus coverage into the response.
        if let Some(ref result) = focus_opt {
            lr = crate::tools::apply_focus_result_to_lr(lr, result);
        }

        let mut stored_args = args.clone();
        if let Some(obj) = stored_args.as_object_mut() {
            obj.insert("view".into(), serde_json::Value::String("usages".into()));
        }

        lr.with_is_error(false)
            .build_with_args(resp, &stored_args, self)
    }
}
