//! Cross-function dataflow bridge for inter-procedural trace.
//!
//! ## Role
//!
//! `CrossFunctionBridge` replaces the runtime BFS logic in `SummaryEdgeProvider`
//! with O(1) query-based lookups against the persisted summary tables
//! (Schema v3).  The caller (`SummaryEdgeProvider`) handles runtime fallback
//! when summary data is absent (old DB or unindexed functions).
//!
//! ## Bridge types
//!
//! | Method | Trigger | Lookup | Virtual edge |
//! |--------|---------|--------|--------------|
//! | `incoming_for_param` | Slicer hits a Parameter node in a callee | Look up callers via callsite, query `summary_call_arg_sources` for the matching arg | `ArgToParam` (caller arg → callee param) |
//! | `incoming_for_call_result` | Slicer hits a CallReturn/Expr with callsite_id | Query `summary_return_sources` for the callee | `ReturnToCall` (callee return source → call result) |
//!
//! ## Confidence model
//!
//! - Direct caller + summary bridge: `row.confidence × 0.92` (strong signal)
//! - Runtime fallback (no summary): `SummaryEdgeProvider` default 0.67 (backward compat)

use db::TraceStore;
use types::enums::{DataFlowKind, DataNodeKind};
use types::ids::DataNodeId;

use crate::trace::virtual_edges::TraceEdge;

/// Bridges inter-procedural dataflow using persisted summaries.
///
/// Returns empty when summary data is absent.  The caller
/// (`SummaryEdgeProvider`) handles runtime fallback via its
/// existing BFS logic for backward compatibility with old DBs.
pub struct CrossFunctionBridge;

impl CrossFunctionBridge {
    /// Find incoming virtual edges **into** a Parameter node.
    ///
    /// For each direct caller, matches `arg_index` → parameter and queries
    /// `summary_call_arg_sources` (via [`TraceStore::query_call_arg_sources`])
    /// to find the call-argument's upstream sources.
    ///
    /// Returns empty when no summary data is available; the caller
    /// (`SummaryEdgeProvider`) falls back to runtime BFS in that case.
    pub fn incoming_for_param(
        param_id: &DataNodeId,
        store: &dyn TraceStore,
    ) -> anyhow::Result<Vec<TraceEdge>> {
        let target_node = match store.get_data_node(param_id)? {
            Some(n) => n,
            None => return Ok(vec![]),
        };
        if target_node.kind != DataNodeKind::Parameter {
            return Ok(vec![]);
        }

        let function_id = match &target_node.function_id {
            Some(fid) => fid.clone(),
            None => return Ok(vec![]),
        };

        let callee_params = store.find_data_nodes_by_function(&function_id)?;
        let param_index = callee_params
            .iter()
            .filter(|dn| dn.kind == DataNodeKind::Parameter)
            .position(|dn| &dn.id == param_id);

        let direct_callers = match store.find_callsites_by_callee(&function_id) {
            Ok(c) => c,
            Err(_) => return Ok(vec![]),
        };

        let mut edges: Vec<TraceEdge> = Vec::new();

        for cs in &direct_callers {
            let caller_function_id = cs.caller;

            if let Some(param_idx) = param_index {
                for (arg_idx, arg) in cs.args.iter().enumerate() {
                    let arg_dn_id = match &arg.data_node_id {
                        Some(dn_id) => dn_id,
                        None => continue,
                    };
                    if arg_idx == param_idx {
                        // Query persisted summary via the TraceStore trait
                        let all_rows = store.query_call_arg_sources(arg_dn_id)?;
                        let arg_sources: Vec<_> = all_rows
                            .into_iter()
                            .filter(|r| &r.function_id == &caller_function_id)
                            .collect();

                        for row in &arg_sources {
                            edges.push(TraceEdge {
                                source_id: row.source_node_id,
                                target_id: param_id.clone(),
                                kind: DataFlowKind::ArgToParam,
                                confidence: row.confidence * 0.92,
                                provenance: format!(
                                    "summary bridge: caller_arg[{}] at callsite {} → callee param[{}]",
                                    arg_idx,
                                    hex::encode(cs.id.as_bytes()),
                                    param_idx,
                                ),
                            });
                        }
                    }
                }
            }
        }

        Ok(edges)
    }

    /// Find incoming virtual edges **into** a CallReturn/Expr (call result) node.
    ///
    /// Looks up the callee's `summary_return_sources` (via
    /// [`TraceStore::query_return_sources`]) and bridges each source back to
    /// the call-result node.
    ///
    /// Returns empty when no summary data is available; the caller
    /// (`SummaryEdgeProvider`) falls back to runtime BFS in that case.
    pub fn incoming_for_call_result(
        call_result_id: &DataNodeId,
        store: &dyn TraceStore,
    ) -> anyhow::Result<Vec<TraceEdge>> {
        let target_node = match store.get_data_node(call_result_id)? {
            Some(n) => n,
            None => return Ok(vec![]),
        };

        let callsite_id = match &target_node.callsite_id {
            Some(csid) => csid,
            None => return Ok(vec![]),
        };

        let callsites = match store.find_callsites_by_id(callsite_id) {
            Ok(c) => c,
            Err(_) => return Ok(vec![]),
        };
        let callee_sym_id = match callsites.first().and_then(|cs| cs.callee.as_ref()) {
            Some(sid) => sid.clone(),
            None => return Ok(vec![]),
        };

        let callee_nodes = match store.find_data_nodes_by_function(&callee_sym_id) {
            Ok(nodes) => nodes,
            Err(_) => return Ok(vec![]),
        };

        let mut edges: Vec<TraceEdge> = Vec::new();

        for return_node in callee_nodes
            .iter()
            .filter(|n| n.kind == DataNodeKind::Return || n.kind == DataNodeKind::CallReturn)
        {
            let return_sources = store.query_return_sources(&return_node.id)?;

            for row in &return_sources {
                edges.push(TraceEdge {
                    source_id: row.source_node_id,
                    target_id: call_result_id.clone(),
                    kind: DataFlowKind::ReturnToCall,
                    confidence: row.confidence * 0.92,
                    provenance: format!(
                        "summary bridge: callee return {} → call result {}",
                        hex::encode(row.return_id.as_bytes()),
                        hex::encode(callsite_id.as_bytes()),
                    ),
                });
            }
        }

        Ok(edges)
    }
}

// Note: No unsafe downcast needed.  `TraceStore` now includes `SummaryReader`
// (added in Schema v3), so `store.query_call_arg_sources(...)` and
// `store.query_return_sources(...)` are available directly through the trait
// object.

#[cfg(test)]
mod tests {
    use super::*;
    use db::Store;
    use db::summary::SummaryStore;
    use types::enums::SymbolKind;
    use types::ids::{CallsiteId, FileId, SymbolId};
    use types::structs::{ArgumentFact, Callsite, TextRange};

    fn test_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.init_schema().unwrap();
        store
    }

    fn insert_test_function(store: &Store, file_id: FileId, name: &str) -> SymbolId {
        let range = TextRange {
            start_byte: 0,
            end_byte: 50,
            start_line: 1,
            start_column: 1,
            end_line: 5,
            end_column: 1,
        };
        let sym = types::structs::SymbolDef {
            id: SymbolId::generate(&file_id, "typescript", name, "function", None),
            kind: SymbolKind::Function,
            name: name.into(),
            qualified_name: name.into(),
            symbol_path: vec![name.into()],
            file_id,
            language: types::enums::Language::TypeScript,
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
            layer: "structural".into(),
        };
        store.insert_symbols(&[sym.clone()]).unwrap();
        sym.id
    }

    #[test]
    fn test_bridge_incoming_for_param_without_summary_returns_empty() -> anyhow::Result<()> {
        let store = test_store();
        let file_id = FileId::generate("test_bridge.ts");
        store.upsert_file(&types::structs::FileInfo {
            file_id,
            path: "test_bridge.ts".into(),
            language: types::enums::Language::TypeScript,
            content_hash: "abc".into(),
            status: types::enums::ParseStatus::Success,
        })?;

        let callee_id = insert_test_function(&store, file_id, "callee");
        let _caller_id = insert_test_function(&store, file_id, "caller");

        let param_id =
            DataNodeId::generate(&file_id, Some(&callee_id), "parameter", Some("x"), None, 0);

        let range = TextRange {
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 2,
        };
        let dn = types::dataflow::DataNode::parameter(
            param_id,
            file_id,
            Some(callee_id),
            None,
            "x",
            range,
        );
        let unit = types::lazy::AnalysisUnit::from_function(file_id, callee_id, range);
        store.replace_dataflow_for_unit(&unit, &[dn], &[], &[], &[], &[], &[])?;

        let edges = CrossFunctionBridge::incoming_for_param(&param_id, &store)?;
        assert!(edges.is_empty(), "no summary → no bridge edges");
        Ok(())
    }

    #[test]
    fn test_bridge_incoming_for_call_result_without_callsite_returns_empty() -> anyhow::Result<()> {
        let store = test_store();
        let file_id = FileId::generate("test_bridge2.ts");
        store.upsert_file(&types::structs::FileInfo {
            file_id,
            path: "test_bridge2.ts".into(),
            language: types::enums::Language::TypeScript,
            content_hash: "abc".into(),
            status: types::enums::ParseStatus::Success,
        })?;

        let _callee_id = insert_test_function(&store, file_id, "callee2");
        let caller_id = insert_test_function(&store, file_id, "caller2");

        let cr_id = DataNodeId::generate(&file_id, Some(&caller_id), "call_return", None, None, 50);

        let range = TextRange {
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 2,
        };
        let dn = types::dataflow::DataNode {
            id: cr_id,
            file_id,
            function_id: Some(caller_id),
            kind: DataNodeKind::CallReturn,
            binding_id: None,
            callsite_id: None,
            name: None,
            access_path: None,
            arg_index: None,
            range,
        };
        let unit = types::lazy::AnalysisUnit::from_function(file_id, caller_id, range);
        store.replace_dataflow_for_unit(&unit, &[dn], &[], &[], &[], &[], &[])?;

        let edges = CrossFunctionBridge::incoming_for_call_result(&cr_id, &store)?;
        assert!(edges.is_empty(), "no callsite → no bridge edges");
        Ok(())
    }

    #[test]
    fn test_bridge_incoming_for_param_with_summary_data() -> anyhow::Result<()> {
        let store = test_store();
        let file_id = FileId::generate("test_bridge3.ts");
        store.upsert_file(&types::structs::FileInfo {
            file_id,
            path: "test_bridge3.ts".into(),
            language: types::enums::Language::TypeScript,
            content_hash: "abc".into(),
            status: types::enums::ParseStatus::Success,
        })?;

        let callee_id = insert_test_function(&store, file_id, "callee3");
        let caller_id = insert_test_function(&store, file_id, "caller3");

        let range = TextRange {
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 1,
            end_line: 10,
            end_column: 1,
        };

        let source_node =
            DataNodeId::generate(&file_id, Some(&caller_id), "parameter", Some("x"), None, 0);
        let arg_node_id = DataNodeId::generate(
            &file_id,
            Some(&caller_id),
            "call_arg",
            Some("arg0"),
            None,
            20,
        );
        let param_id =
            DataNodeId::generate(&file_id, Some(&callee_id), "parameter", Some("y"), None, 10);

        // Insert DataNodes
        let callee_param_dn = types::dataflow::DataNode::parameter(
            param_id,
            file_id,
            Some(callee_id),
            None,
            "y",
            range,
        );
        let caller_arg_dn = types::dataflow::DataNode {
            id: arg_node_id,
            file_id,
            function_id: Some(caller_id),
            kind: DataNodeKind::CallArg,
            binding_id: None,
            callsite_id: None,
            name: Some("arg0".into()),
            access_path: None,
            arg_index: Some(0),
            range,
        };
        let caller_param_dn = types::dataflow::DataNode::parameter(
            source_node,
            file_id,
            Some(caller_id),
            None,
            "x",
            range,
        );

        {
            let unit_callee = types::lazy::AnalysisUnit::from_function(file_id, callee_id, range);
            store.replace_dataflow_for_unit(
                &unit_callee,
                &[callee_param_dn],
                &[],
                &[],
                &[],
                &[],
                &[],
            )?;
            let unit_caller = types::lazy::AnalysisUnit::from_function(file_id, caller_id, range);
            store.replace_dataflow_for_unit(
                &unit_caller,
                &[caller_arg_dn, caller_param_dn],
                &[],
                &[],
                &[],
                &[],
                &[],
            )?;
        }

        // Create callsite via the public insert_callsites API
        let ref_id = types::ids::ReferenceId::generate(
            &file_id,
            Some(&caller_id),
            20,
            25,
            "callee3",
            types::enums::ReferenceKind::Call,
        );
        let cs_id = CallsiteId::generate(&ref_id, Some(&caller_id), 20);
        let callsite = Callsite {
            id: cs_id,
            reference_id: Some(ref_id),
            caller: caller_id,
            callee: Some(callee_id),
            receiver: None,
            args: vec![ArgumentFact {
                index: 0,
                name: None,
                value: "x".into(),
                range: None,
                data_node_id: Some(arg_node_id),
            }],
            range,
            callee_range: None,
        };
        store.insert_callsites(&[callsite])?;

        // Build summary for caller
        #[allow(deprecated)]
        let summary = types::summary::FunctionSummary {
            function_id: caller_id,
            node_count: 3,
            edge_count: 2,
            param_flows: vec![types::summary::ParameterFlow {
                param_id: source_node,
                param_index: 0,
                param_name: "x".into(),
                reaches_call_args: vec![arg_node_id],
                reaches_returns: vec![],
                reaches_fields: vec![],
                confidence: 0.85,
                provenance: "intraprocedural_dataflow".into(),
            }],
            return_flows: vec![],
            call_arg_flows: vec![types::summary::CallArgFlow {
                callsite_id: cs_id,
                arg_index: 0,
                arg_node_id,
                sources: vec![source_node],
                confidence: 1.0,
                provenance: "intraprocedural_dataflow".into(),
            }],
            return_sources: vec![],
        };
        SummaryStore::build_for_function(&store, &caller_id, |_, _fid| Ok(summary.clone()))?;

        // Bridge: param_id (callee's param) → caller arg → source
        let edges = CrossFunctionBridge::incoming_for_param(&param_id, &store)?;
        assert!(
            !edges.is_empty(),
            "should have bridge edges with summary data"
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source_id, source_node);
        assert_eq!(edges[0].target_id, param_id);

        Ok(())
    }
}
