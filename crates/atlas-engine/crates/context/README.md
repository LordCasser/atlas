# context

AI context builder. Produces structured Markdown context for a symbol, suitable for LLM prompt injection.

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
ContextBuilder::build_context_for_symbol(&symbol_id) → ContextView
ContextView::to_markdown() → String
```
