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

; ── CommonJS require imports ───────────────────────────────────────

; const / let / var x = require('./foo')
(variable_declarator
  name: (identifier) @import.require_name
  value: (call_expression
    function: (identifier) @_require_fn
    arguments: (arguments (string) @import.require_module))
  (#eq? @_require_fn "require"))

; Bare require('./foo') without assignment (side-effect import)
(expression_statement
  (call_expression
    function: (identifier) @_require_fn2
    arguments: (arguments (string) @import.require_module))
  (#eq? @_require_fn2 "require"))

; ── CommonJS module.exports / exports.foo ──────────────────────────

; module.exports = expr
(expression_statement
  (assignment_expression
    left: (member_expression
      object: (identifier) @_cjs_mod
      property: (property_identifier) @_cjs_mod_prop)
    right: (identifier) @export.cjs_default)
  (#eq? @_cjs_mod "module")
  (#eq? @_cjs_mod_prop "exports"))

; exports.foo = expr
(expression_statement
  (assignment_expression
    left: (member_expression
      object: (identifier) @_cjs_exports
      property: (property_identifier) @export.cjs_name))
  (#eq? @_cjs_exports "exports"))

; ── Standalone export ───────────────────────────────────────────────
; `export const x = 1` / `export function f() {}` / `export default class C {}`
; These are already handled by the `exported` flag on SymbolDef.
; No separate query needed.
