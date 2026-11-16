#![cfg(feature = "cpp")]

use std::{collections::HashSet, path::Path};

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use types::{FileId, Language, ReferenceKind, SymbolKind};

#[test]
fn separate_declarations_preserve_their_own_body_and_call_ownership() {
    // Source identities must not select a preprocessor branch or collapse
    // template overloads whose function declarators happen to be identical.
    for source in [
        "void first(); void second();\n#if ENABLED\nvoid run() { first(); }\n#else\nvoid run() { second(); }\n#endif\n",
        "void first(); void second(); struct Box { template<class T> T get() { first(); return {}; } template<class T, class D> T get() { second(); return {}; } };",
        "void first(); void second(); void run(); void run() { first(); } void run(); void entry() { second(); run(); }",
        "void first(); void second(); template<class T> requires (sizeof(T) == 4) void run() { first(); } template<class T> requires (sizeof(T) == 8) void run() { second(); }",
    ] {
        let mut identities = None;
        for mode in [
            ExtractionMode::Structural,
            ExtractionMode::Full,
            ExtractionMode::ResolutionSymbols,
            ExtractionMode::Manifest,
        ] {
            let facts = extract_file_with_mode(
                &create_frontend(Language::Cpp).unwrap(),
                FileId::generate("occurrences.cpp"),
                Path::new("occurrences.cpp"),
                source,
                "fixture",
                mode.clone(),
                &(),
            )
            .unwrap();
            let callables: Vec<_> = facts
                .symbols
                .iter()
                .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
                .collect();
            let ids: HashSet<_> = callables.iter().map(|s| s.id).collect();
            assert_eq!(
                ids.len(),
                callables.len(),
                "distinct source declarations must survive storage: {source}"
            );
            let located: HashSet<_> = callables
                .iter()
                .map(|s| (s.name_range.start_byte, s.id))
                .collect();
            if let Some(expected) = &identities {
                assert!(
                    located.is_subset(expected),
                    "all extraction modes share identity at the same source occurrence: {source}"
                );
            } else {
                identities = Some(located);
            }
            if matches!(
                mode,
                ExtractionMode::Manifest | ExtractionMode::ResolutionSymbols
            ) {
                continue;
            }
            for name in ["first", "second"] {
                let calls: Vec<_> = facts
                    .references
                    .iter()
                    .filter(|r| r.kind == ReferenceKind::Call && r.name == name)
                    .collect();
                assert_eq!(calls.len(), 1, "{source}: {name}");
                let call = calls[0];
                let owner = callables
                    .iter()
                    .find(|s| Some(s.id) == call.source_symbol)
                    .unwrap();
                assert!(
                    owner.range.start_byte <= call.range.start_byte
                        && call.range.end_byte <= owner.range.end_byte,
                    "{source}: {name} is outside {owner:?}"
                );
            }
        }
    }
}
