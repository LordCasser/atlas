use atlas_engine::Store;
use atlas_mcp::tools::{ToolCallContext, ToolRouter};
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use serde_json::json;
use std::sync::Arc;

fn bench_call_tool_dispatch(c: &mut Criterion) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let router = ToolRouter::new_empty(store.clone(), "/tmp/bench".into());
    let ctx = ToolCallContext::empty();

    c.bench_function("dispatch_project_status", |b| {
        b.iter(|| {
            router.call_tool(
                black_box(&ctx),
                "project",
                black_box(&json!({"action": "status"})),
            )
        })
    });

    c.bench_function("dispatch_domain_rules_list", |b| {
        b.iter(|| {
            router.call_tool(
                black_box(&ctx),
                "domain_rules",
                black_box(&json!({"action": "list"})),
            )
        })
    });

    c.bench_function("dispatch_tasks", |b| {
        b.iter(|| router.call_tool(black_box(&ctx), "tasks", black_box(&json!({}))))
    });
}

criterion_group!(benches, bench_call_tool_dispatch);
criterion_main!(benches);
