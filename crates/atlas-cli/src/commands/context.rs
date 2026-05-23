//! `atlas context` command — build AI context around a symbol or query.

use crate::runtime::{CommandContext, DbMode};
use anyhow::Context;
use atlas_context::ContextBuilder;
use atlas_engine::Store;
use atlas_engine::GraphEngine;
use atlas_engine::SymbolDef;
use std::sync::Arc;

pub fn run(query: &str, project: &str) -> anyhow::Result<()> {
    let ctx = CommandContext::open(project, DbMode::ExistingReadOnly)?;
    let store = ctx.store;
    let root = &ctx.root;

    let graph = GraphEngine::from_store(&store, 0.3).context("Failed to load graph snapshot")?;
    let graph = Arc::new(graph);

    let builder = ContextBuilder::new(Arc::clone(&store), Arc::clone(&graph));

    match builder.build_context_for_query(query)? {
        Some(view) => {
            // Print the structured context (callers, callees, peers, etc.)
            println!("{}", view.to_markdown());

            // Append source code excerpts for the subject and its callers/callees
            print_source_excerpt(root, store.as_ref(), &view.subject);
            for sym in view.callers.iter().take(3) {
                print_source_excerpt(root, store.as_ref(), sym);
            }
            for sym in view.callees.iter().take(3) {
                print_source_excerpt(root, store.as_ref(), sym);
            }
        }
        None => {
            println!("No symbol found for '{}'. Try a different query.", query);
        }
    }

    Ok(())
}

/// Print a source code excerpt for a symbol (file path + line range).
fn print_source_excerpt(project_root: &std::path::Path, store: &Store, sym: &SymbolDef) {
    if let Ok(Some(file_info)) = store.get_file(&sym.file_id) {
        let full_path = project_root.join(&file_info.path);
        // Canonicalize to prevent path traversal from tampered DB
        // Canonicalize both sides — macOS symlinks /var→/private/var
        if let (Ok(canonical), Ok(canonical_root)) =
            (full_path.canonicalize(), project_root.canonicalize())
        {
            if canonical.starts_with(&canonical_root) {
                if let Ok(content) = std::fs::read_to_string(&canonical) {
                    let lines: Vec<&str> = content.lines().collect();
                    let start = sym.range.start_line as usize; // tree-sitter rows are 0-indexed
                    let end = (sym.range.end_line as usize + 1).min(lines.len()); // end_line inclusive → exclusive
                    if start < end {
                        println!(
                            "#### {} ({}:{}-{})",
                            sym.qualified_name,
                            file_info.path,
                            start + 1,
                            end
                        );
                        println!("```{}", language_tag(&sym.language.as_str()));
                        for line in &lines[start..end] {
                            println!("{}", line);
                        }
                        println!("```");
                        println!();
                    }
                }
            }
        }
    }
}

/// Map Atlas language name to a common markdown code fence tag.
fn language_tag(lang: &str) -> &'static str {
    match lang {
        "python" => "python",
        "typescript" => "typescript",
        "javascript" => "javascript",
        "java" => "java",
        "c" => "c",
        "cpp" => "cpp",
        "arkts" => "typescript",
        "cangjie" => "",
        _ => "",
    }
}
