// Golden test fixture: ArkTS struct (simple)
// Covers: struct recovery via source-scan, symbol + scope creation
// The `struct` keyword is not valid TS — tree-sitter produces ERROR nodes.
// Recovery must locate the ERROR node, extract the struct name, brace-match
// the full declaration range, and emit SymbolKind::Struct + ScopeKind::Struct.

struct SimpleWidget {
  build() {
    console.info("built");
  }
}
