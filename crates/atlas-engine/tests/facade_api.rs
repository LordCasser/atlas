use atlas_engine::{
    CallGraphView, CalleeDetail, CallerDetail, CompositePathScore, ContextView, ForwardFrontier,
    FrontierNode, GraphBuilderStats, IndexPipelineStats, LanguageFrontend, ParsedQuery,
    PathAliasResolver, PathBreakpoint, PathBreakpointKind, PathEdge, PathEdgeDirection,
    PipelinePhaseTiming, ProjectRoot, RankedPath, SearchOptions, SourcePath, Subgraph, SyncStats,
    TraversalConfig,
};

fn assert_nameable<T>() {}

#[test]
fn supported_facade_signature_types_are_nameable() {
    assert_nameable::<LanguageFrontend>();
    assert_nameable::<IndexPipelineStats>();
    assert_nameable::<PipelinePhaseTiming>();
    assert_nameable::<SyncStats>();
    assert_nameable::<CallGraphView>();
    assert_nameable::<CompositePathScore>();
    assert_nameable::<ForwardFrontier>();
    assert_nameable::<FrontierNode>();
    assert_nameable::<GraphBuilderStats>();
    assert_nameable::<PathBreakpoint>();
    assert_nameable::<PathBreakpointKind>();
    assert_nameable::<PathEdge>();
    assert_nameable::<PathEdgeDirection>();
    assert_nameable::<RankedPath>();
    assert_nameable::<Subgraph>();
    assert_nameable::<TraversalConfig>();
    assert_nameable::<PathAliasResolver>();
    assert_nameable::<CalleeDetail>();
    assert_nameable::<CallerDetail>();
    assert_nameable::<ContextView>();
    assert_nameable::<SearchOptions>();
    assert_nameable::<ParsedQuery>();
    assert_nameable::<ProjectRoot>();
    assert_nameable::<SourcePath>();
}
