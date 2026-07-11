// ArkTS struct golden fixture.
// The `struct` keyword is byte-stable normalized to `class ` pre-parse
// (normalize_struct_keywords in arkts.rs), so the TS grammar produces a
// normal class_declaration. The normalizer then upgrades Class->Struct
// and ScopeKind::Class->ScopeKind::Struct based on source-byte inspection.

struct SimpleWidget {
  build() {
    console.info("built");
  }
}
