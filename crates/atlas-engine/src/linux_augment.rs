//! Linux kernel semantic augmentation.
//!
//! Post-extraction pipeline that detects kernel-specific macro patterns
//! in C source text and enriches [`FileFacts`] with derived edges,
//! export markers, and diagnostics.
//!
//! Detection is regex-based (not tree-sitter) — it operates on raw
//! source text and matches well-known kernel macro invocation patterns.
//!
//! # Detected Patterns
//!
//! | Pattern | Action |
//! |---------|--------|
//! | `EXPORT_SYMBOL(func)` / `EXPORT_SYMBOL_GPL(func)` | Marks function as exported |
//! | `module_init(func)` / `late_initcall(func)` / etc. | Generates RegistersCallback edge |
//! | `SYSCALL_DEFINEn(name, ...)` | Adds syscall diagnostic |

use regex::Regex;

use types::enums::{Confidence, EdgeKind, Provenance};
use types::ids::EdgeId;
use types::structs::{DiagnosticLevel, ExtractDiagnostic, FileFacts, RawEdge};

/// Post-extraction augmentation for Linux kernel C patterns.
pub struct LinuxAugmenter;

/// Outcome of augmenting one file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AugmentResult {
    /// Number of symbols marked as exported (EXPORT_SYMBOL).
    pub symbols_exported: usize,
    /// Number of RegistersCallback edges generated (initcall patterns).
    pub initcall_edges: usize,
    /// Number of syscall diagnostics added (SYSCALL_DEFINE).
    pub syscall_detected: usize,
}

impl LinuxAugmenter {
    /// Augment `facts` with kernel-specific metadata and edges.
    ///
    /// Only processes C files (checks `facts.file.language`).
    /// Operates entirely in-place — no DB access needed.
    pub fn augment(facts: &mut FileFacts, source: &str) -> AugmentResult {
        let mut result = AugmentResult::default();

        // Only process C files
        if facts.file.language.as_str() != "c" {
            return result;
        }

        result.symbols_exported = Self::detect_export_symbol(facts, source);
        result.initcall_edges = Self::detect_initcall(facts, source);
        result.syscall_detected = Self::detect_syscall(facts, source);

        result
    }

    /// Detect `EXPORT_SYMBOL(func)` and `EXPORT_SYMBOL_GPL(func)`.
    ///
    /// Marks the matching function as exported (`sym.exported = true`)
    /// and adds its `SymbolId` to `facts.exports`.
    fn detect_export_symbol(facts: &mut FileFacts, source: &str) -> usize {
        let re = Regex::new(r"EXPORT_SYMBOL(?:_GPL)?\s*\(\s*(\w+)\s*\)")
            .expect("EXPORT_SYMBOL regex");
        let mut count = 0;

        for cap in re.captures_iter(source) {
            let func_name = cap.get(1).unwrap().as_str();
            if let Some(sym) = facts.symbols.iter_mut().find(|s| s.name == func_name) {
                sym.exported = true;
                if !facts.exports.contains(&sym.id) {
                    facts.exports.push(sym.id);
                }
                count += 1;
            }
        }
        count
    }

    /// Detect initcall macros: `module_init(func)`, `late_initcall(func)`,
    /// `postcore_initcall(func)`, `core_initcall(func)`, `arch_initcall(func)`,
    /// `subsys_initcall(func)`, `device_initcall(func)`, etc.
    ///
    /// For each match, generates a `RegistersCallback` edge from the first
    /// symbol in the file (as a proxy for "file-level context") to the
    /// detected init function, with confidence 0.5 and
    /// `Provenance::Heuristic`.
    fn detect_initcall(facts: &mut FileFacts, source: &str) -> usize {
        // Covers: module_init, late_initcall, postcore_initcall, core_initcall,
        // arch_initcall, subsys_initcall, device_initcall, early_initcall,
        // pure_initcall, fs_initcall, rootfs_initcall, __initcall
        let re = Regex::new(
            r"(?:module_init|late_initcall|postcore_initcall|core_initcall|arch_initcall|subsys_initcall|device_initcall|early_initcall|pure_initcall|fs_initcall|rootfs_initcall|__initcall)\s*\(\s*(\w+)\s*\)",
        )
        .expect("initcall regex");
        let mut count = 0;

        // Use first symbol as the file-level "source" for the edge
        let source_id = match facts.symbols.first() {
            Some(s) => s.id,
            None => return 0,
        };

        for cap in re.captures_iter(source) {
            let func_name = cap.get(1).unwrap().as_str();
            if let Some(sym) = facts.symbols.iter().find(|s| s.name == func_name) {
                let edge = RawEdge::new(
                    EdgeId::generate(
                        &source_id,
                        &sym.id,
                        "registers_callback",
                        None,
                        "heuristic",
                    ),
                    source_id,
                    sym.id,
                    EdgeKind::RegistersCallback,
                    Confidence::new(0.5),
                    Provenance::Heuristic,
                );
                facts.raw_edges.push(edge);
                count += 1;
            }
        }
        count
    }

    /// Detect `SYSCALL_DEFINEn(name, ...)` patterns.
    ///
    /// Looks for the corresponding `sys_NAME` function symbol and adds
    /// an INFO-level diagnostic describing the syscall.
    fn detect_syscall(facts: &mut FileFacts, source: &str) -> usize {
        let re = Regex::new(r"SYSCALL_DEFINE(\d)\s*\(\s*(\w+)\s*,").expect("SYSCALL_DEFINE regex");
        let mut count = 0;

        for cap in re.captures_iter(source) {
            let nargs = cap.get(1).unwrap().as_str();
            let name = cap.get(2).unwrap().as_str();
            // Kernel generates sys_NAME, __x64_sys_NAME, or __ia32_sys_NAME
            let candidates = [
                format!("sys_{name}"),
                format!("__x64_sys_{name}"),
                format!("__ia32_sys_{name}"),
            ];
            if candidates
                .iter()
                .any(|cname| facts.symbols.iter().any(|s| s.name == *cname))
            {
                facts.diagnostics.push(ExtractDiagnostic {
                    level: DiagnosticLevel::Info,
                    message: format!("syscall: {name} (SYSCALL_DEFINE{nargs})"),
                    range: None,
                });
                count += 1;
            }
        }
        count
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use types::enums::{Language, ParseStatus, SymbolKind};
    use types::ids::{FileId, SymbolId};
    use types::structs::{FileFacts, FileInfo, SymbolDef, TextRange};

    fn make_c_file_facts() -> FileFacts {
        FileFacts {
            file: FileInfo {
                file_id: FileId::generate("kernel/foo.c"),
                path: "kernel/foo.c".to_string(),
                language: Language::C,
                content_hash: "abc".to_string(),
                status: ParseStatus::default(),
            },
            symbols: vec![],
            ..Default::default()
        }
    }

    fn make_symbol(name: &str, kind: SymbolKind) -> SymbolDef {
        let file_id = FileId::generate("kernel/foo.c");
        SymbolDef {
            id: SymbolId::generate(&file_id, "c", name, kind.as_str(), None),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            symbol_path: vec![name.to_string()],
            file_id,
            language: Language::C,
            range: TextRange::default(),
            name_range: TextRange::default(),
            signature: None,
            visibility: None,
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        }
    }

    #[test]
    fn detect_export_symbol_sets_exported_flag() {
        let source = r#"
int my_init(void) { return 0; }
EXPORT_SYMBOL(my_init);
"#;
        let mut facts = make_c_file_facts();
        facts
            .symbols
            .push(make_symbol("my_init", SymbolKind::Function));
        let fid = facts.symbols[0].id;

        let result = LinuxAugmenter::detect_export_symbol(&mut facts, source);
        assert_eq!(result, 1);
        assert!(facts.symbols[0].exported);
        assert!(facts.exports.contains(&fid));
    }

    #[test]
    fn detect_export_symbol_gpl_sets_exported_flag() {
        let source = r#"
int helper(void) { return 1; }
EXPORT_SYMBOL_GPL(helper);
"#;
        let mut facts = make_c_file_facts();
        facts
            .symbols
            .push(make_symbol("helper", SymbolKind::Function));

        let result = LinuxAugmenter::detect_export_symbol(&mut facts, source);
        assert_eq!(result, 1);
        assert!(facts.symbols[0].exported);
    }

    #[test]
    fn detect_export_symbol_no_match_for_missing_func() {
        let source = "EXPORT_SYMBOL(nonexistent);";
        let mut facts = make_c_file_facts();
        // No symbols added — func_name won't match anything

        let result = LinuxAugmenter::detect_export_symbol(&mut facts, source);
        assert_eq!(result, 0);
        assert!(facts.exports.is_empty());
    }

    #[test]
    fn detect_initcall_generates_edge() {
        let source = r#"
int my_init(void) { return 0; }
module_init(my_init);
"#;
        let mut facts = make_c_file_facts();
        let init_sym = make_symbol("my_init", SymbolKind::Function);
        let first_sym = make_symbol("some_other", SymbolKind::Function);
        facts.symbols.push(first_sym);
        facts.symbols.push(init_sym);

        let result = LinuxAugmenter::detect_initcall(&mut facts, source);
        assert_eq!(result, 1);
        assert_eq!(facts.raw_edges.len(), 1);
        assert_eq!(facts.raw_edges[0].kind, EdgeKind::RegistersCallback);
        assert_eq!(facts.raw_edges[0].provenance, Provenance::Heuristic);
    }

    #[test]
    fn detect_late_initcall_generates_edge() {
        let source = r#"
int late_setup(void) { return 0; }
late_initcall(late_setup);
"#;
        let mut facts = make_c_file_facts();
        facts
            .symbols
            .push(make_symbol("first", SymbolKind::Function));
        facts
            .symbols
            .push(make_symbol("late_setup", SymbolKind::Function));

        let result = LinuxAugmenter::detect_initcall(&mut facts, source);
        assert_eq!(result, 1);
    }

    #[test]
    fn detect_syscall_adds_diagnostic() {
        let source = r#"
SYSCALL_DEFINE3(read, unsigned int, fd, char __user *, buf, size_t, count)
{
    return ksys_read(fd, buf, count);
}
"#;
        let mut facts = make_c_file_facts();
        facts
            .symbols
            .push(make_symbol("sys_read", SymbolKind::Function));

        let result = LinuxAugmenter::detect_syscall(&mut facts, source);
        assert_eq!(result, 1);
        assert_eq!(facts.diagnostics.len(), 1);
        assert!(facts.diagnostics[0].message.contains("syscall"));
        assert!(facts.diagnostics[0].message.contains("read"));
    }

    #[test]
    fn detect_syscall_x64_variant() {
        let source = "SYSCALL_DEFINE1(close, unsigned int, fd) { return 0; }";
        let mut facts = make_c_file_facts();
        facts
            .symbols
            .push(make_symbol("__x64_sys_close", SymbolKind::Function));

        let result = LinuxAugmenter::detect_syscall(&mut facts, source);
        assert_eq!(result, 1);
    }

    #[test]
    fn augment_non_c_file_returns_zero() {
        let mut facts = FileFacts {
            file: FileInfo {
                file_id: FileId::generate("main.rs"),
                path: "main.rs".to_string(),
                language: Language::Rust,
                content_hash: "abc".to_string(),
                status: ParseStatus::default(),
            },
            ..Default::default()
        };
        let result = LinuxAugmenter::augment(&mut facts, "EXPORT_SYMBOL(foo);");
        assert_eq!(result, AugmentResult::default());
    }

    #[test]
    fn augment_c_file_detects_all_patterns() {
        let source = r#"
#include <linux/module.h>
#include <linux/init.h>

static int __init my_init(void) { return 0; }
static void __exit my_exit(void) {}

module_init(my_init);
EXPORT_SYMBOL(my_init);
"#;
        let mut facts = make_c_file_facts();
        facts
            .symbols
            .push(make_symbol("my_init", SymbolKind::Function));
        facts
            .symbols
            .push(make_symbol("my_exit", SymbolKind::Function));

        let result = LinuxAugmenter::augment(&mut facts, source);
        assert_eq!(result.symbols_exported, 1);
        assert_eq!(result.initcall_edges, 1);
        // No syscall in this source
        assert_eq!(result.syscall_detected, 0);
    }
}
