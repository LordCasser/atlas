//! Context builder types: ContextView + ContextSlice.

use types::SymbolDef;
use types::enums::EdgeKind;

/// Full contextual view of a symbol and its neighborhood.
#[derive(Debug, Clone)]
pub struct ContextView {
    /// The subject symbol.
    pub subject: SymbolDef,
    /// Project-relative path of the subject's source file.
    pub subject_file_path: Option<String>,
    /// Source code snippet of the subject (first N lines).
    pub subject_source: Option<SourceSnippet>,
    /// Symbols that call the subject (basic list, backward compatible).
    pub callers: Vec<SymbolDef>,
    /// Symbols that the subject calls (basic list, backward compatible).
    pub callees: Vec<SymbolDef>,
    /// Detailed caller info with call-site snippet and edge kind.
    pub caller_details: Vec<CallerDetail>,
    /// Detailed callee info with call-site snippet and edge kind.
    pub callee_details: Vec<CalleeDetail>,
    /// Peer symbols in the same file.
    pub file_peers: Vec<SymbolDef>,
    /// Files that import the subject's file.
    pub importers: Vec<String>,
    /// Files that the subject's file depends on.
    pub dependencies: Vec<String>,
}

/// Source code snippet for a symbol definition.
#[derive(Debug, Clone)]
pub struct SourceSnippet {
    /// Lines of source code (truncated to max_lines).
    pub lines: Vec<String>,
    /// Starting line number (0-based).
    pub start_line: u32,
    /// Total lines in the original source.
    pub total_lines: u32,
    /// Whether the snippet was truncated to max_lines.
    pub truncated: bool,
}

/// Per-caller detail with call-site context.
#[derive(Debug, Clone)]
pub struct CallerDetail {
    pub symbol: SymbolDef,
    pub callsite_line: u32,
    pub callsite_snippet: String,
    pub edge_kind: EdgeKind,
}

/// Per-callee detail with call-site context.
#[derive(Debug, Clone)]
pub struct CalleeDetail {
    pub symbol: SymbolDef,
    pub callsite_line: u32,
    pub callsite_snippet: String,
    pub edge_kind: EdgeKind,
    /// First line of the callee definition (signature).
    pub callee_signature: Option<String>,
}

impl ContextView {
    /// Total number of related symbols (excluding subject).
    pub fn total_related(&self) -> usize {
        self.callers.len() + self.callees.len() + self.file_peers.len()
    }

    /// Format as a Markdown summary suitable for AI consumption.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str(&format!("## Symbol: `{}`\n", self.subject.qualified_name));
        md.push_str(&format!("- Kind: `{}`\n", self.subject.kind.as_str()));
        md.push_str(&format!(
            "- Language: `{}`\n",
            self.subject.language.as_str()
        ));
        if let Some(ref sig) = self.subject.signature {
            md.push_str(&format!("- Signature: `{sig}`\n"));
        }
        let file_info = self
            .subject_source
            .as_ref()
            .map(|s| {
                format!(
                    " (line {}-{})",
                    s.start_line + 1,
                    s.start_line + s.lines.len() as u32
                )
            })
            .unwrap_or_default();
        let file_label = self
            .subject_file_path
            .clone()
            .unwrap_or_else(|| self.subject.file_id.to_hex());
        md.push_str(&format!("- File: `{file_label}`{file_info}\n"));
        md.push('\n');

        // Subject source snippet
        if let Some(ref src) = self.subject_source {
            md.push_str("```\n");
            for line in &src.lines {
                md.push_str(line);
                md.push('\n');
            }
            if src.truncated {
                md.push_str(&format!(
                    "  ... (truncated — {} total lines)\n",
                    src.total_lines
                ));
            }
            md.push_str("```\n\n");
        }

        /// Maximum callers/callees/peers per context output section.
        const MAX_CONTEXT_ITEMS: usize = 10;

        // Callers with source snippets
        if !self.caller_details.is_empty() {
            md.push_str("### Called By\n\n");
            let shown = if self.caller_details.len() > MAX_CONTEXT_ITEMS {
                &self.caller_details[..MAX_CONTEXT_ITEMS]
            } else {
                &self.caller_details
            };
            for (i, c) in shown.iter().enumerate() {
                md.push_str(&format!(
                    "{}. **`{}`** [{}] @ `{}:{}`\n",
                    i + 1,
                    c.symbol.qualified_name,
                    c.edge_kind.as_str(),
                    c.symbol.file_id.short_hex(),
                    c.callsite_line + 1,
                ));
                md.push_str("   ```\n");
                md.push_str(&format!("   {}\n", c.callsite_snippet.trim()));
                md.push_str("   ```\n\n");
            }
            if self.caller_details.len() > MAX_CONTEXT_ITEMS {
                md.push_str(&format!(
                    "- ... and {} more callers\n",
                    self.caller_details.len() - MAX_CONTEXT_ITEMS
                ));
            }
            md.push('\n');
        } else if !self.callers.is_empty() {
            // Fallback: basic caller list (backward compatible)
            md.push_str("### Callers\n\n");
            let shown = if self.callers.len() > MAX_CONTEXT_ITEMS {
                &self.callers[..MAX_CONTEXT_ITEMS]
            } else {
                &self.callers
            };
            for c in shown {
                md.push_str(&format!("- `{}`\n", c.qualified_name));
            }
            md.push('\n');
        }

        // Callees with source snippets
        if !self.callee_details.is_empty() {
            md.push_str("### Calls\n\n");
            let shown = if self.callee_details.len() > MAX_CONTEXT_ITEMS {
                &self.callee_details[..MAX_CONTEXT_ITEMS]
            } else {
                &self.callee_details
            };
            for (i, c) in shown.iter().enumerate() {
                let boundary_note = if c.edge_kind == EdgeKind::RegistersCallback {
                    "⚠ **Callback boundary**: this callee is registered as a callback and invoked dynamically.\n"
                } else {
                    ""
                };
                md.push_str(&format!(
                    "{}. **`{}`** [{}]\n",
                    i + 1,
                    c.symbol.qualified_name,
                    c.edge_kind.as_str(),
                ));
                if !boundary_note.is_empty() {
                    md.push_str(boundary_note);
                }
                if let Some(ref sig) = c.callee_signature {
                    md.push_str(&format!("   Signature: `{sig}`\n"));
                }
                md.push_str(&format!(
                    "   @ `{}:{}`\n",
                    c.symbol.file_id.short_hex(),
                    c.callsite_line + 1
                ));
                md.push_str("   ```\n");
                md.push_str(&format!("   {}\n", c.callsite_snippet.trim()));
                md.push_str("   ```\n\n");
            }
            if self.callee_details.len() > MAX_CONTEXT_ITEMS {
                md.push_str(&format!(
                    "- ... and {} more callees\n",
                    self.callee_details.len() - MAX_CONTEXT_ITEMS
                ));
            }
            md.push('\n');
        } else if !self.callees.is_empty() {
            // Fallback: basic callee list
            md.push_str("### Callees\n\n");
            let shown = if self.callees.len() > MAX_CONTEXT_ITEMS {
                &self.callees[..MAX_CONTEXT_ITEMS]
            } else {
                &self.callees
            };
            for c in shown {
                md.push_str(&format!("- `{}`\n", c.qualified_name));
            }
            md.push('\n');
        }

        if !self.file_peers.is_empty() {
            md.push_str("### File Peers\n\n");
            for p in &self.file_peers {
                if p.id != self.subject.id {
                    let sig = p.signature.as_deref().unwrap_or("");
                    md.push_str(&format!(
                        "- `{}` `{}` {}\n",
                        p.qualified_name,
                        p.kind.as_str(),
                        sig
                    ));
                }
            }
            md.push('\n');
        }

        if !self.importers.is_empty() {
            md.push_str("### Importers\n\n");
            for i in &self.importers {
                md.push_str(&format!("- `{i}`\n"));
            }
            md.push('\n');
        }

        if !self.dependencies.is_empty() {
            md.push_str("### Dependencies\n\n");
            for d in &self.dependencies {
                md.push_str(&format!("- `{d}`\n"));
            }
            md.push('\n');
        }

        // ── Trail: actionable next steps for Agent ──
        md.push_str("---\n");
        md.push_str("*Trail — follow these to explore further (no additional lookup needed):*\n");
        if !self.callee_details.is_empty() {
            let first_callee = &self.callee_details[0].symbol.qualified_name;
            md.push_str(&format!(
                "- **Calls** → `symbol` with `view: \"context\"` and `symbol: {{\"qualified_name\": \"{first_callee}\"}}`\n"
            ));
        }
        if !self.caller_details.is_empty() {
            md.push_str(&format!(
                "- **Called by** → `trace` with `kind: \"callers\"` and `symbol: {{\"qualified_name\": \"{}\"}}`\n",
                self.subject.qualified_name
            ));
        }
        md.push_str(&format!("- **Full source** → `explore` with `source_mode: \"full\"` and `symbol: {{\"qualified_name\": \"{}\"}}`\n", self.subject.qualified_name));
        if self.dependencies.len() > 1 {
            md.push_str(&format!(
                "- **Dependencies** → {} imported files\n",
                self.dependencies.len()
            ));
        }
        md.push('\n');

        md
    }
}

/// Lightweight context slice: subject + direct callers/callees only.
#[derive(Debug, Clone)]
pub struct ContextSlice {
    /// The subject symbol.
    pub subject: SymbolDef,
    /// Direct callers.
    pub callers: Vec<SymbolDef>,
    /// Direct callees.
    pub callees: Vec<SymbolDef>,
}

impl ContextSlice {
    /// Format as a compact Markdown snippet.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str(&format!(
            "`{}` ({})",
            self.subject.qualified_name,
            self.subject.kind.as_str()
        ));
        if !self.callers.is_empty() {
            let names: Vec<&str> = self.callers.iter().map(|s| s.name.as_str()).collect();
            md.push_str(&format!(" ← called by [{}]", names.join(", ")));
        }
        if !self.callees.is_empty() {
            let names: Vec<&str> = self.callees.iter().map(|s| s.name.as_str()).collect();
            md.push_str(&format!(" → calls [{}]", names.join(", ")));
        }
        md
    }
}

// ───────────────────────────────────────────────────────────────────────────
// tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use types::SymbolDef;
    use types::enums::{EdgeKind, Language, SymbolKind, Visibility};
    use types::ids::SymbolId;
    use types::structs::TextRange;

    fn make_sym(name: &str) -> SymbolDef {
        let fid = types::ids::FileId::generate("test.c");
        SymbolDef {
            id: SymbolId::generate(&fid, "c", name, "Function", None),
            file_id: fid,
            kind: SymbolKind::Function,
            name: name.into(),
            qualified_name: name.into(),
            symbol_path: vec![name.into()],
            language: Language::C,
            range: TextRange::default(),
            name_range: TextRange::default(),
            signature: Some("void fn()".into()),
            visibility: Some(Visibility::Public),
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".into(),
        }
    }

    #[test]
    fn context_view_to_markdown_basic() {
        let subject = make_sym("do_work");
        let view = ContextView {
            subject: subject.clone(),
            subject_file_path: Some("src/work.rs".into()),
            subject_source: None,
            callers: vec![],
            callees: vec![],
            caller_details: vec![],
            callee_details: vec![],
            file_peers: vec![],
            importers: vec![],
            dependencies: vec![],
        };
        let md = view.to_markdown();
        assert!(md.contains("do_work"), "must include symbol name");
        assert!(md.contains("function"), "must include kind (lowercase)");
        assert!(md.contains("void fn()"), "must include signature");
        assert!(md.contains("Language"), "must include language");
    }

    #[test]
    fn context_view_callback_boundary_note() {
        let subject = make_sym("register_handlers");
        let handler = make_sym("on_frame");
        let callee_detail = CalleeDetail {
            symbol: handler,
            callsite_line: 42,
            callsite_snippet: "  set_callback(session, on_frame);".into(),
            edge_kind: EdgeKind::RegistersCallback,
            callee_signature: Some("void on_frame()".into()),
        };
        let view = ContextView {
            subject,
            subject_file_path: Some("src/callbacks.c".into()),
            subject_source: None,
            callers: vec![],
            callees: vec![],
            caller_details: vec![],
            callee_details: vec![callee_detail],
            file_peers: vec![],
            importers: vec![],
            dependencies: vec![],
        };
        let md = view.to_markdown();
        assert!(
            md.contains("⚠"),
            "must show boundary warning for RegistersCallback"
        );
        assert!(
            md.contains("Callback boundary"),
            "must explain callback boundary"
        );
        assert!(md.contains("on_frame"), "must include callee name");
        assert!(md.contains("registers_callback"), "must show edge kind");
    }

    #[test]
    fn context_view_normal_callee_no_warning() {
        let subject = make_sym("main");
        let helper = make_sym("helper");
        let callee_detail = CalleeDetail {
            symbol: helper,
            callsite_line: 10,
            callsite_snippet: "  helper();".into(),
            edge_kind: EdgeKind::Calls,
            callee_signature: None,
        };
        let view = ContextView {
            subject,
            subject_file_path: Some("src/main.c".into()),
            subject_source: None,
            callers: vec![],
            callees: vec![],
            caller_details: vec![],
            callee_details: vec![callee_detail],
            file_peers: vec![],
            importers: vec![],
            dependencies: vec![],
        };
        let md = view.to_markdown();
        assert!(!md.contains("⚠"), "normal Calls edge must not show warning");
        assert!(
            md.contains("calls"),
            "must show edge kind as section header"
        );
        assert!(md.contains("`symbol` with `view: \"context\"`"));
        assert!(md.contains("`explore` with `source_mode: \"full\"`"));
        assert!(!md.contains("trace_caller_path"));
        assert!(!md.contains("codegraph_node"));
    }

    #[test]
    fn context_slice_format() {
        let subject = make_sym("helper");
        let caller = make_sym("main");
        let slice = ContextSlice {
            subject,
            callers: vec![caller],
            callees: vec![],
        };
        let md = slice.to_markdown();
        assert!(md.contains("helper"), "must include subject");
        assert!(md.contains("main"), "must include caller name");
        assert!(md.contains("called by"), "must show call direction");
    }
}
