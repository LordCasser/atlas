use std::sync::Arc;

use analysis::trace::virtual_edges::{TraceEdge, TraceEdgeProvider};
use analysis::trace::{Locator, Slicer, TraceEngine};
use db::{Store, TraceStore};
use types::{
    DataFlowEdge, DataFlowEdgeId, DataFlowKind, DataNode, DataNodeId, DataNodeKind,
    DiagnosticLevel, ExtractDiagnostic, FileFacts, FileId, FileInfo, Language, ParseStatus,
    TextRange,
};

fn node(name: &str, kind: DataNodeKind, byte: u32) -> DataNode {
    let file_id = FileId::generate("trace.ts");
    DataNode {
        id: DataNodeId::generate(&file_id, None, kind.as_str(), Some(name), None, byte),
        file_id,
        function_id: None,
        kind,
        binding_id: None,
        callsite_id: None,
        name: Some(name.into()),
        access_path: None,
        arg_index: None,
        range: TextRange {
            start_byte: byte,
            end_byte: byte + 1,
            start_line: 0,
            end_line: 0,
            start_column: byte,
            end_column: byte + 1,
        },
    }
}

fn edge(from: &DataNode, to: &DataNode, kind: DataFlowKind) -> DataFlowEdge {
    DataFlowEdge::new(
        DataFlowEdgeId::generate(&from.id, &to.id, kind.as_str()),
        from.id,
        to.id,
        kind,
        to.range,
        0.9,
    )
}

fn store(
    nodes: Vec<DataNode>,
    edges: Vec<DataFlowEdge>,
    diagnostics: Vec<ExtractDiagnostic>,
) -> Arc<Store> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    store
        .insert_file_facts(&FileFacts {
            file: FileInfo {
                file_id: FileId::generate("trace.ts"),
                path: "trace.ts".into(),
                language: Language::TypeScript,
                content_hash: "fixed".into(),
                status: ParseStatus::Success,
            },
            data_nodes: nodes,
            dataflow_edges: edges,
            diagnostics,
            ..Default::default()
        })
        .unwrap();
    store
}

fn assert_contiguous(path: &types::trace::TracePath) {
    let mut current = path.source.data_node.as_ref().unwrap().id;
    for (index, step) in path.steps.iter().enumerate() {
        assert_eq!(step.index as usize, index);
        assert_eq!(step.from_node_id, current);
        current = step.to_node_id;
    }
    assert_eq!(current, path.sink.data_node.as_ref().unwrap().id);
}

#[test]
fn exact_node_selection_preserves_incident_edges_and_depth_limits() {
    let first = node("1", DataNodeKind::Literal, 0);
    let second = node("2", DataNodeKind::Literal, 10);
    let preferred = node("shared", DataNodeKind::Local, 20);
    let requested = node("shared", DataNodeKind::Expr, 20);
    let sink = node("sink", DataNodeKind::Expr, 30);
    let store = store(
        vec![
            first.clone(),
            second.clone(),
            preferred.clone(),
            requested.clone(),
            sink.clone(),
        ],
        vec![
            edge(&first, &preferred, DataFlowKind::Assign),
            edge(&second, &requested, DataFlowKind::Assign),
            edge(&requested, &sink, DataFlowKind::Assign),
        ],
        vec![],
    );
    let original_nodes = store.find_data_nodes_by_file(&requested.file_id).unwrap();
    let point = Locator::locate(store.as_ref(), &requested.file_id, 1, 21).unwrap();
    assert_eq!(point.data_node.unwrap().id, preferred.id);
    let exact = Locator::locate_node(store.as_ref(), &requested.id)
        .unwrap()
        .unwrap();
    assert_eq!(exact.data_node.unwrap().id, requested.id);
    assert_eq!(
        exact.incoming.iter().map(|n| n.node_id).collect::<Vec<_>>(),
        vec![second.id]
    );
    assert_eq!(
        exact.outgoing.iter().map(|n| n.node_id).collect::<Vec<_>>(),
        vec![sink.id]
    );

    let engine = TraceEngine::new(store.clone());
    for depth in [0, 20] {
        let response = engine.trace_data_node(&requested.id, depth, &[]);
        assert!(response.ok);
        let path = response.result.as_ref().unwrap();
        assert_eq!(path.sink.data_node.as_ref().unwrap().id, requested.id);
        assert_eq!(
            path.source.data_node.as_ref().unwrap().id,
            if depth == 0 { requested.id } else { second.id }
        );
        assert_eq!(response.partial_result, depth == 0);
        assert_eq!(response.partial_result, path.partial_result);
        assert_eq!(
            serde_json::to_value(&response.diagnostics).unwrap(),
            serde_json::to_value(&path.diagnostics).unwrap()
        );
        assert_contiguous(path);
    }
    assert_eq!(
        store.find_data_nodes_by_file(&requested.file_id).unwrap(),
        original_nodes
    );
}

#[test]
fn absent_node_identity_does_not_fall_back_to_an_existing_position() {
    let existing = node("existing", DataNodeKind::Literal, 0);
    let absent = node("absent", DataNodeKind::Literal, 0);
    let store = store(vec![existing], vec![], vec![]);
    assert!(
        Locator::locate_node(store.as_ref(), &absent.id)
            .unwrap()
            .is_none()
    );
    let response = TraceEngine::new(store).trace_data_node(&absent.id, 10, &[]);
    assert!(response.ok && response.partial_result && response.result.is_none());
    assert!(
        response
            .diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("no_data_node"))
    );
}

#[test]
fn branch_frontier_identity_disambiguates_an_overlapping_location() {
    let first = node("first", DataNodeKind::Parameter, 0);
    let second = node("second", DataNodeKind::Parameter, 10);
    let decoy = node("another value", DataNodeKind::Local, 0);
    let sink = node("sink", DataNodeKind::Expr, 20);
    let store = store(
        vec![first.clone(), second.clone(), decoy.clone(), sink.clone()],
        vec![
            edge(&first, &sink, DataFlowKind::Phi),
            edge(&second, &sink, DataFlowKind::Phi),
        ],
        vec![],
    );
    let engine = TraceEngine::new(store);
    let response = engine.trace_data_node(&sink.id, 20, &[]);
    let displayed_source = response
        .result
        .as_ref()
        .unwrap()
        .source
        .data_node
        .as_ref()
        .unwrap()
        .id;
    assert_eq!(displayed_source, second.id);
    let diagnostic = response
        .diagnostics
        .iter()
        .find(|d| d.code.as_deref() == Some("trace_alternatives_unexpanded"))
        .unwrap();
    let detail: serde_json::Value =
        serde_json::from_str(diagnostic.detail.as_ref().unwrap()).unwrap();
    let expected: DataNodeId =
        serde_json::from_value(detail["edges"][0]["source_id"].clone()).unwrap();
    assert_eq!(expected, first.id);
    let positional = engine.trace_point(&first.file_id, 1, 1);
    assert_eq!(positional.result.unwrap().data_node.unwrap().id, decoy.id);
    let continued = engine.trace_data_node(&expected, 20, &[]);
    assert!(!continued.partial_result);
    assert_eq!(
        continued.result.unwrap().source.data_node.unwrap().id,
        first.id
    );
}

#[test]
fn branch_alternative_is_located_and_can_be_traced_separately() {
    let first = node("first", DataNodeKind::Parameter, 0);
    let second = node("second", DataNodeKind::Parameter, 10);
    let merged = node("merged", DataNodeKind::Expr, 20);
    let sink = node("sink", DataNodeKind::Local, 30);
    let store = store(
        vec![first.clone(), second.clone(), merged.clone(), sink.clone()],
        vec![
            edge(&first, &merged, DataFlowKind::Phi),
            edge(&second, &merged, DataFlowKind::Phi),
            edge(&merged, &sink, DataFlowKind::Assign),
        ],
        vec![],
    );
    let engine = TraceEngine::new(store);
    let response = engine.trace_variable(&sink.file_id, 1, 31, 20);
    assert!(response.ok && response.partial_result);
    let path = response.result.unwrap();
    assert!(path.partial_result);
    assert_contiguous(&path);
    let diagnostic = response
        .diagnostics
        .iter()
        .find(|d| d.code.as_deref() == Some("trace_alternatives_unexpanded"))
        .unwrap();
    let detail: serde_json::Value =
        serde_json::from_str(diagnostic.detail.as_ref().unwrap()).unwrap();
    assert_eq!(detail["total_edges"], 1);
    let position = &detail["edges"][0]["position"];
    let next = engine.trace_variable(
        &sink.file_id,
        position["line"].as_u64().unwrap() as u32,
        position["column"].as_u64().unwrap() as u32,
        20,
    );
    assert!(next.ok && !next.partial_result);
    let mut sources = vec![
        path.source.data_node.unwrap().id,
        next.result.unwrap().source.data_node.unwrap().id,
    ];
    sources.sort();
    let mut expected = vec![first.id, second.id];
    expected.sort();
    assert_eq!(sources, expected);
}

#[test]
fn zero_and_exact_depth_limits_propagate_to_the_response() {
    let origin = node("7", DataNodeKind::Literal, 0);
    let middle = node("middle", DataNodeKind::Local, 10);
    let sink = node("sink", DataNodeKind::Expr, 20);
    let store = store(
        vec![origin.clone(), middle.clone(), sink.clone()],
        vec![
            edge(&origin, &middle, DataFlowKind::Assign),
            edge(&middle, &sink, DataFlowKind::Assign),
        ],
        vec![],
    );
    let engine = TraceEngine::new(store);
    for depth in 0..=2 {
        let response = engine.trace_variable(&sink.file_id, 1, 21, depth);
        assert!(response.ok);
        let path = response.result.unwrap();
        assert_eq!(path.steps.len(), depth);
        assert_eq!(path.max_depth_reached, depth);
        assert_eq!(path.nodes_visited, depth + 1);
        assert_eq!(response.partial_result, depth < 2);
        assert_eq!(path.partial_result, response.partial_result);
        assert_contiguous(&path);
        assert_eq!(
            response
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some("max_depth_truncated")),
            depth < 2
        );
    }
}

struct VirtualEdge(TraceEdge);
impl TraceEdgeProvider for VirtualEdge {
    fn virtual_incoming(
        &self,
        target: &DataNodeId,
        _: &dyn TraceStore,
    ) -> anyhow::Result<Vec<TraceEdge>> {
        Ok(if *target == self.0.target_id {
            vec![self.0.clone()]
        } else {
            vec![]
        })
    }
}

#[test]
fn virtual_only_predecessor_at_depth_limit_is_not_reported_as_complete() {
    let origin = node("input", DataNodeKind::Parameter, 0);
    let sink = node("result", DataNodeKind::Expr, 10);
    let store = store(vec![origin.clone(), sink.clone()], vec![], vec![]);
    let provider = VirtualEdge(TraceEdge {
        source_id: origin.id,
        target_id: sink.id,
        kind: DataFlowKind::ReturnToCall,
        callsite_id: None,
        confidence: 0.9,
        provenance: "test boundary".into(),
    });
    let point = Locator::locate(store.as_ref(), &sink.file_id, 1, 11).unwrap();
    for depth in [0, 10] {
        let path = Slicer::slice(
            store.as_ref(),
            &point,
            depth,
            Some(&provider),
            &Default::default(),
        )
        .unwrap()
        .unwrap();
        assert!(path.partial_result && path.steps.is_empty());
        assert_eq!(
            path.diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some("max_depth_truncated")),
            depth == 0
        );
        let unavailable = path
            .diagnostics
            .iter()
            .find(|d| d.code.as_deref() == Some("trace_call_context_unavailable"))
            .unwrap();
        let detail: serde_json::Value =
            serde_json::from_str(unavailable.detail.as_ref().unwrap()).unwrap();
        assert_eq!(
            detail["edges"][0]["position"]["data_node_id"],
            serde_json::json!(origin.id)
        );
        assert!(detail["edges"][0]["position"]["call_context"].is_null());
        assert_contiguous(&path);
    }
}

#[test]
fn a_cycle_does_not_hide_an_unvisited_input_or_break_the_displayed_chain() {
    let origin = node("input", DataNodeKind::Parameter, 0);
    let update = node("value", DataNodeKind::Local, 10);
    let sink = node("sink", DataNodeKind::Expr, 20);
    let store = store(
        vec![origin.clone(), update.clone(), sink.clone()],
        vec![
            edge(&origin, &update, DataFlowKind::Assign),
            edge(&update, &update, DataFlowKind::Assign),
            edge(&update, &sink, DataFlowKind::Assign),
        ],
        vec![],
    );
    let response = TraceEngine::new(store).trace_variable(&sink.file_id, 1, 21, 20);
    let path = response.result.unwrap();
    assert!(response.partial_result && path.partial_result);
    assert_eq!(path.source.data_node.as_ref().unwrap().id, origin.id);
    assert!(
        response
            .diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("trace_cycle_unexpanded"))
    );
    assert_contiguous(&path);
}

#[test]
fn missing_source_node_keeps_a_real_endpoint_and_reports_the_dangling_edge() {
    let absent = node("absent", DataNodeKind::Parameter, 0);
    let sink = node("sink", DataNodeKind::Expr, 10);
    let store = store(vec![sink.clone()], vec![], vec![]);
    let provider = VirtualEdge(TraceEdge {
        source_id: absent.id,
        target_id: sink.id,
        kind: DataFlowKind::ArgToParam,
        callsite_id: None,
        confidence: 0.9,
        provenance: "missing node".into(),
    });
    let point = Locator::locate(store.as_ref(), &sink.file_id, 1, 11).unwrap();
    let path = Slicer::slice(
        store.as_ref(),
        &point,
        10,
        Some(&provider),
        &Default::default(),
    )
    .unwrap()
    .unwrap();
    assert!(path.partial_result);
    assert_eq!(path.source.data_node.as_ref().unwrap(), &sink);
    let diagnostic = path
        .diagnostics
        .iter()
        .find(|d| d.code.as_deref() == Some("trace_data_node_missing"))
        .unwrap();
    let detail: serde_json::Value =
        serde_json::from_str(diagnostic.detail.as_ref().unwrap()).unwrap();
    assert!(detail["edges"][0]["position"].is_null());
    assert_contiguous(&path);
}

#[test]
fn extraction_limits_only_propagate_when_global_or_touching_the_path() {
    let origin = node("7", DataNodeKind::Literal, 0);
    let sink = node("sink", DataNodeKind::Local, 10);
    for (range, expected) in [
        (Some(sink.range), true),
        (
            Some(node("adjacent_before", DataNodeKind::Expr, 9).range),
            false,
        ),
        (
            Some(node("adjacent_after", DataNodeKind::Expr, 11).range),
            false,
        ),
        (
            Some(TextRange {
                end_byte: 10,
                end_column: 10,
                ..sink.range
            }),
            true,
        ),
        (
            Some(node("unrelated", DataNodeKind::Expr, 100).range),
            false,
        ),
        (None, true),
    ] {
        let store = store(
            vec![origin.clone(), sink.clone()],
            vec![edge(&origin, &sink, DataFlowKind::Assign)],
            vec![ExtractDiagnostic {
                level: DiagnosticLevel::Warning,
                message: "use_def_limit".into(),
                range,
            }],
        );
        let response = TraceEngine::new(store).trace_variable(&sink.file_id, 1, 11, 20);
        assert_eq!(response.partial_result, expected);
        assert_eq!(response.result.as_ref().unwrap().partial_result, expected);
        assert_eq!(
            response
                .diagnostics
                .iter()
                .any(|d| d.code.as_deref() == Some("trace_extraction_limit")),
            expected
        );
    }
}

#[test]
fn empty_non_input_endpoint_is_explicitly_unknown() {
    let sink = node("opaque", DataNodeKind::CallTarget, 0);
    let response = TraceEngine::new(store(vec![sink.clone()], vec![], vec![])).trace_variable(
        &sink.file_id,
        1,
        1,
        20,
    );
    assert!(response.partial_result);
    assert!(
        response
            .diagnostics
            .iter()
            .any(|d| d.code.as_deref() == Some("trace_origin_unestablished"))
    );
    assert_contiguous(response.result.as_ref().unwrap());
}

#[test]
fn alternative_details_are_bounded_without_claiming_the_remaining_edges_were_shown() {
    let sink = node("sink", DataNodeKind::Expr, 1000);
    let mut nodes: Vec<_> = (0..132)
        .map(|i| node(&format!("input_{i}"), DataNodeKind::Parameter, i * 2))
        .collect();
    let edges = nodes
        .iter()
        .map(|n| edge(n, &sink, DataFlowKind::Phi))
        .collect();
    nodes.push(sink.clone());
    let engine = TraceEngine::new(store(nodes, edges, vec![]));
    let response = engine.trace_variable(&sink.file_id, 1, 1001, 20);
    let diagnostic = response
        .diagnostics
        .iter()
        .find(|d| d.code.as_deref() == Some("trace_alternatives_unexpanded"))
        .unwrap();
    let detail: serde_json::Value =
        serde_json::from_str(diagnostic.detail.as_ref().unwrap()).unwrap();
    assert_eq!(detail["total_edges"], 131);
    assert_eq!(detail["edges"].as_array().unwrap().len(), 128);
    assert_eq!(detail["details_truncated"], true);
    assert!(response.partial_result);
}
