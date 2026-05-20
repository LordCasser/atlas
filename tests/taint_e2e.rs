//! Taint analysis end-to-end tests.
//!
//! Each test simulates a realistic source-to-sink dataflow for a language
//! and verifies that the taint engine correctly detects the vulnerability,
//! including severity assignment, path tracing, and edge cases.
//!
//! These tests use canned DataNodes + DataFlowEdges to exercise the full
//! analysis pipeline (rules → engine → path tracer) independently from
//! extraction quality.  Extraction quality is covered by golden tests.

use atlas::analysis::taint::{TaintEngine, TaintPathTracer, TaintRuleLoader};
use atlas::types::dataflow::{DataFlowEdge, DataNode};
use atlas::types::enums::{DataFlowKind, DataNodeKind, Language};
use atlas::types::ids::{DataFlowEdgeId, DataNodeId, FileId};
use atlas::types::structs::TextRange;
use atlas::types::taint::{Severity, TaintRuleKind};

// ── Helpers ─────────────────────────────────────────────────────────────────

fn make_node(
    file_id: FileId, name: &str, kind: DataNodeKind, access_path: Option<&str>,
) -> DataNode {
    DataNode {
        id: DataNodeId::generate(
            &file_id, None, kind.as_str(), Some(name), access_path, 0,
        ),
        file_id,
        function_id: None,
        kind,
        binding_id: None,
        callsite_id: None,
        name: Some(name.to_string()),
        access_path: access_path.map(|s| s.to_string()),
        range: TextRange::default(),
    }
}

fn simple_edge(source: DataNodeId, target: DataNodeId, kind: DataFlowKind) -> DataFlowEdge {
    DataFlowEdge {
        id: DataFlowEdgeId::generate(&source, &target, kind.as_str()),
        source,
        target,
        kind,
        location: TextRange::default(),
        confidence: 1.0,
    }
}

// ── TypeScript: Express req.query → child_process.exec ───────────────────────

/// Simulates:
/// ```ts
/// const { exec } = require('child_process');
/// app.get('/search', (req, res) => {
///     const cmd = req.query.q;   // source: user-controlled
///     exec(cmd);                  // sink: RCE via child_process.exec
/// });
/// ```
#[test]
fn ts_req_query_to_exec() {
    let file_id = FileId::generate("src/handler.ts");

    // DataNodes with access_path matching the rule callee patterns
    let query = make_node(file_id, "query", DataNodeKind::Field,    Some("request"));
    let q     = make_node(file_id, "q",     DataNodeKind::Field,    Some("request.query"));
    let cmd   = make_node(file_id, "cmd",   DataNodeKind::Local,    None);
    let exec_sym = make_node(file_id, "exec", DataNodeKind::CallArg, Some("child_process"));

    let nodes = vec![query.clone(), q.clone(), cmd.clone(), exec_sym.clone()];

    // Edges: query → q → cmd → exec
    let edges = vec![
        simple_edge(query.id, q.id,        DataFlowKind::FieldLoad),
        simple_edge(q.id, cmd.id,          DataFlowKind::Assign),
        simple_edge(cmd.id, exec_sym.id,   DataFlowKind::ArgToParam),
    ];

    // Load default TS rules
    let rules = TaintRuleLoader::load_defaults(&[Language::TypeScript]);
    let engine = TaintEngine::new(rules);
    let result = engine.analyze(&nodes, &edges);

    // Verify findings
    assert!(!result.findings.is_empty(),
        "Should detect req.query → exec flow");
    assert!(result.sources_matched >= 1, "Should match at least 1 source");
    assert!(result.sinks_matched >= 1, "Should match at least 1 sink");

    let finding = &result.findings[0];
    assert_eq!(finding.source_node, query.id,
        "Source should be the 'query' DataNode (matched ts.req.query)");
    assert_eq!(finding.sink_node, exec_sym.id,
        "Sink should be the 'exec' DataNode (matched ts.child_process.exec)");
    assert!(finding.severity >= Severity::High,
        "Command injection is at least High severity");
    assert!(!finding.id.to_hex().is_empty(), "FindingId should be non-empty");

    // Path trace
    let tracer = TaintPathTracer::new();
    let path = tracer.trace_one(finding, &nodes, &edges);
    assert!(path.complete, "Path should reach source");
    assert_eq!(path.steps.len(), 4, "Path should have 4 steps: query→q→cmd→exec");
    assert_eq!(path.steps[0].data_node, query.id, "Step 0 is source");
    assert_eq!(path.steps[path.steps.len() - 1].data_node, exec_sym.id, "Last step is sink");
}

// ── TypeScript: sanitizer blocks taint (req.body → sanitize → innerHTML) ─────

/// Sanitizer (ts.sanitize) blocks taint from reaching a sink.
#[test]
fn ts_sanitizer_blocks_taint() {
    let file_id = FileId::generate("src/sanitized.ts");

    let body = make_node(file_id, "body",      DataNodeKind::Field,   Some("request"));
    let raw  = make_node(file_id, "raw",       DataNodeKind::Field,   Some("request.body"));
    let sanitized = make_node(file_id, "sanitize",  DataNodeKind::Expr, None);
    let inner = make_node(file_id, "innerHTML", DataNodeKind::Field,  None);

    let nodes = vec![body.clone(), raw.clone(), sanitized.clone(), inner.clone()];
    let edges = vec![
        simple_edge(body.id, raw.id,        DataFlowKind::FieldLoad),
        simple_edge(raw.id, sanitized.id,   DataFlowKind::Assign),
        simple_edge(sanitized.id, inner.id, DataFlowKind::Assign),
    ];

    let rules = TaintRuleLoader::load_defaults(&[Language::TypeScript]);
    let engine = TaintEngine::new(rules);
    let result = engine.analyze(&nodes, &edges);

    assert!(result.findings.is_empty(),
        "Sanitizer should block taint: body → sanitize → innerHTML");
    assert!(result.sources_matched >= 1, "Source should still be matched");
}

// ── Python: Flask request.args → os.system ──────────────────────────────────

/// Simulates:
/// ```python
/// from flask import request
/// import os
/// @app.route('/ping')
/// def ping():
///     host = request.args.get('host')  # source
///     os.system('ping ' + host)        # sink: command injection
/// ```
#[test]
fn py_request_args_to_os_system() {
    let file_id = FileId::generate("app.py");

    let args    = make_node(file_id, "args",   DataNodeKind::Field,     Some("request"));
    let get     = make_node(file_id, "get",    DataNodeKind::CallReturn, Some("request.args"));
    let host    = make_node(file_id, "host",   DataNodeKind::Local,     None);
    let system  = make_node(file_id, "system", DataNodeKind::CallArg,   Some("os"));

    let nodes = vec![args.clone(), get.clone(), host.clone(), system.clone()];
    let edges = vec![
        simple_edge(args.id, get.id,     DataFlowKind::ReturnToCall),
        simple_edge(get.id, host.id,     DataFlowKind::Assign),
        simple_edge(host.id, system.id,  DataFlowKind::ArgToParam),
    ];

    let rules = TaintRuleLoader::load_defaults(&[Language::Python]);
    let engine = TaintEngine::new(rules);
    let result = engine.analyze(&nodes, &edges);

    assert!(!result.findings.is_empty(), "Should detect request.args → os.system flow");
    assert!(result.sources_matched >= 1);
    assert!(result.sinks_matched >= 1);

    let finding = &result.findings[0];
    assert_eq!(finding.source_node, args.id,
        "Source should be request.args (args DataNode)");
    assert_eq!(finding.sink_node, system.id,
        "Sink should be os.system");
    assert!(finding.severity >= Severity::High,
        "Command injection is at least High severity");

    let tracer = TaintPathTracer::new();
    let path = tracer.trace_one(finding, &nodes, &edges);
    assert!(path.complete);
    assert!(path.steps.len() >= 3, "Path should have at least 3 steps");
}

// ── Python: Django HttpRequest → eval (sanitizer blocks) ────────────────────

/// Python sanitizer (html.escape) should block XSS sink (eval).
#[test]
fn py_html_escape_blocks_eval() {
    let file_id = FileId::generate("views.py");

    let form    = make_node(file_id, "form",   DataNodeKind::Field,  Some("request"));
    let user_input = make_node(file_id, "user_input", DataNodeKind::Local, Some("request.form"));
    let escaped = make_node(file_id, "html.escape", DataNodeKind::Expr, None);
    let eval_sym = make_node(file_id, "eval", DataNodeKind::CallArg, None);

    let nodes = vec![form.clone(), user_input.clone(), escaped.clone(), eval_sym.clone()];
    let edges = vec![
        simple_edge(form.id, user_input.id,     DataFlowKind::FieldLoad),
        simple_edge(user_input.id, escaped.id,  DataFlowKind::Assign),
        simple_edge(escaped.id, eval_sym.id,    DataFlowKind::ArgToParam),
    ];

    let rules = TaintRuleLoader::load_defaults(&[Language::Python]);
    let engine = TaintEngine::new(rules);
    let result = engine.analyze(&nodes, &edges);

    assert!(result.findings.is_empty(),
        "html.escape should sanitize before eval (source→sanitizer→sink)");
}

// ── Edge case: max depth limits propagation ─────────────────────────────────

#[test]
fn ts_max_depth_prevents_infinite_propagation() {
    let file_id = FileId::generate("src/loop.ts");

    // Create a chain of 30 DataNodes (exceeds default max_depth=20)
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut prev_id: Option<DataNodeId>;
    {
        // Source: node matching ts.req.query (name="query", access_path="request")
        let first = make_node(file_id, "query", DataNodeKind::Parameter, Some("request"));
        prev_id = Some(first.id);
        nodes.push(first);
    }

    for i in 1..30 {
        let node = make_node(file_id, &format!("var{}", i), DataNodeKind::Local, None);
        let nid = node.id;
        nodes.push(node);
        if let Some(pid) = prev_id {
            edges.push(simple_edge(pid, nid, DataFlowKind::Assign));
        }
        prev_id = Some(nid);
    }

    // Only source and sink rules; no sanitizer
    use atlas::types::taint::TaintRule;
    let rules = vec![
        TaintRule {
            id: "ts.custom.source".into(),
            language: Some(Language::TypeScript),
            kind: TaintRuleKind::Source,
            callee: Some("request".into()),
            symbol_pattern: Some("query".into()),
            access_path_pattern: None, argument_index: None,
            applies_to_return: false, severity: Severity::Medium,
        },
        TaintRule {
            id: "ts.custom.sink".into(),
            language: Some(Language::TypeScript),
            kind: TaintRuleKind::Sink,
            callee: None,
            symbol_pattern: Some("var29".into()),
            access_path_pattern: None, argument_index: None,
            applies_to_return: false, severity: Severity::Low,
        },
    ];

    let engine = TaintEngine::new(rules);
    let result = engine.analyze(&nodes, &edges);

    // With max_depth=20 and chain length 30, sink should NOT be reached
    assert!(result.findings.is_empty(),
        "Max depth (20) should prevent reaching sink in 30-node chain");
    assert!(result.sources_matched >= 1, "Source should be matched");
    assert!(result.paths_explored > 0, "Some paths should be explored");
}
