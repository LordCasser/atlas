; TypeScript/JavaScript imports query

; Module source for all import statements
(import_statement
  source: (string) @import.module)

; Named imports
(import_specifier
  name: (identifier) @import.name)

; Aliased import
(import_specifier
  alias: (identifier) @import.alias)

; Namespace import: capture the identifier as a wildcard child
(namespace_import
  (identifier) @import.namespace)

; ── Barrel re-exports ──────────────────────────────────────────────

; Wildcard re-export: `export * from './bar'`
(export_statement
  source: (string) @export.module)

; Named re-export: `export { foo } from './bar'`
; `export_specifier` is nested inside `export_clause` in tree-sitter
(export_clause
  (export_specifier
    name: (identifier) @export.name))

; Aliased re-export: `export { foo as bar } from './bar'`
(export_clause
  (export_specifier
    alias: (identifier) @export.alias))

; Standalone export — marks a symbol as exported but no re-export chain:
; `export const x = 1` / `export function f() {}` / `export default class C {}`
; These are already handled by the `exported` flag on SymbolDef.
; No separate query needed.
