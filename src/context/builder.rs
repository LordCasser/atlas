//! Context builder types: ContextView + ContextSlice.

use crate::types::SymbolDef;

/// Full contextual view of a symbol and its neighborhood.
#[derive(Debug, Clone)]
pub struct ContextView {
    /// The subject symbol.
    pub subject: SymbolDef,
    /// Symbols that call the subject.
    pub callers: Vec<SymbolDef>,
    /// Symbols that the subject calls.
    pub callees: Vec<SymbolDef>,
    /// Peer symbols in the same file.
    pub file_peers: Vec<SymbolDef>,
    /// Files that import the subject's file.
    pub importers: Vec<String>,
    /// Files that the subject's file depends on.
    pub dependencies: Vec<String>,
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
            md.push_str(&format!("- Signature: `{}`\n", sig));
        }
        md.push('\n');

        if !self.callers.is_empty() {
            md.push_str("### Callers\n\n");
            for c in &self.callers {
                md.push_str(&format!("- `{}`\n", c.qualified_name));
            }
            md.push('\n');
        }

        if !self.callees.is_empty() {
            md.push_str("### Callees\n\n");
            for c in &self.callees {
                md.push_str(&format!("- `{}`\n", c.qualified_name));
            }
            md.push('\n');
        }

        if !self.file_peers.is_empty() {
            md.push_str("### File Peers\n\n");
            for p in &self.file_peers {
                if p.id != self.subject.id {
                    md.push_str(&format!("- `{}`\n", p.qualified_name));
                }
            }
            md.push('\n');
        }

        if !self.importers.is_empty() {
            md.push_str("### Importers\n\n");
            for i in &self.importers {
                md.push_str(&format!("- `{}`\n", i));
            }
            md.push('\n');
        }

        if !self.dependencies.is_empty() {
            md.push_str("### Dependencies\n\n");
            for d in &self.dependencies {
                md.push_str(&format!("- `{}`\n", d));
            }
            md.push('\n');
        }

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
