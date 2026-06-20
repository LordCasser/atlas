use atlas_engine::Store;
use atlas_mcp::tools::ToolRouter;
use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::Arc;

fn bench_graph_init(c: &mut Criterion) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();

    c.bench_function("graph_initialize_empty", |b| {
        b.iter(|| {
            let router = ToolRouter::new_empty(store.clone(), "/tmp/bench".into());
            router.ensure_graph_initialized().unwrap();
        })
    });
}

fn bench_maybe_refresh_noop(c: &mut Criterion) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let router = ToolRouter::new_empty(store.clone(), "/tmp/bench".into());
    router.ensure_graph_initialized().unwrap();

    // Ensure last_signature_check is in cooldown window
    // First call sets the timestamp, second call skips signature check

    c.bench_function("maybe_refresh_graph_cooldown", |b| {
        b.iter(|| {
            router.maybe_refresh_graph().unwrap();
        })
    });
}

criterion_group!(benches, bench_graph_init, bench_maybe_refresh_noop);
criterion_main!(benches);
