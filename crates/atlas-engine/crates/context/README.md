# context

Agent context builder. Produces bounded structured Markdown context for a symbol.

## Output format

```markdown
## `MyClass.doSomething` (method)

File: `src/services/MyClass.ts` (line 42-78)

### Callers (3)
- `App.run` (src/app.ts:120)
- `RouteHandler.dispatch` (src/routes.ts:55)
...

### Callees (5)
- `Logger.info` (src/utils/logger.ts:15)
- `Database.query` (src/db/client.ts:89)
...

### Definition
```typescript
doSomething(id: number): Promise<Result> {
  // ...
}
```

### File peers
- `MyClass.validate` (method)
- `MyClass.transform` (method)
```

## Public API

```rust
ContextBuilder::new(store: Arc<Store>, graph: Arc<GraphEngine>)
ContextBuilder::build_context_for_symbol(&symbol_id, include_file_peers: bool) → ContextView
ContextView::to_markdown() → String
```

When `include_file_peers` is `false`, the `file_peers` field is set to an empty vector and no DB query for file peers is performed, producing smaller, faster responses. The default is `true`.
