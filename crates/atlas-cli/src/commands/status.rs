//! `atlas status` — display project indexing status and language capability summary.

use crate::runtime::{CommandContext, DbMode};
use anyhow::Context;
use atlas_engine::{FeatureSupport, Language, LanguageCapabilityProfile};

pub fn run(project: &str) -> anyhow::Result<()> {
    let ctx = CommandContext::open(project, DbMode::ExistingReadOnly)?;
    let stats = ctx
        .store
        .get_stats()
        .context("Failed to read database stats")?;
    let ws = &ctx.workspace;

    println!("Atlas Project Status");
    println!("====================");
    println!("  Project root:    {}", ws.root().display());
    println!("  Database:        {}/atlas.db", ws.atlas_dir().display());
    println!("  SQLite version:  {}", stats.sqlite_version);
    println!("  Atlas version:   {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("  Files indexed:   {}", stats.total_files);
    println!("  Symbols:         {}", stats.total_symbols);
    println!("  References:      {}", stats.total_references);
    println!("    - unresolved:  {}", stats.unresolved_references);
    println!("  Edges:           {}", stats.total_edges);

    // Show language breakdown
    if !stats.files_by_language.is_empty() {
        println!();
        println!("  By Language:");
        for (lang, count) in &stats.files_by_language {
            println!("    {lang:<14} {count} files");
        }
    }

    // Show symbol kind breakdown
    if !stats.symbols_by_kind.is_empty() {
        println!();
        println!("  By Symbol Kind:");
        for (kind, count) in &stats.symbols_by_kind {
            println!("    {kind:<14} {count}");
        }
    }

    // Show capability summary
    print_capability_summary(&stats.files_by_language);

    // Show indexed scope when set
    print_indexed_scope(&ctx);

    // Show file index layers summary
    print_layer_summary(&ctx);

    // List indexed files if any
    if stats.total_files > 0 && stats.total_files <= 20 {
        let files = ctx.store.list_files().context("Failed to list files")?;
        println!();
        println!("  Indexed files:");
        for f in &files {
            let lang = f.language.as_str();
            println!("    [{}] {}", lang, f.path);
        }
    } else if stats.total_files > 20 {
        println!();
        println!(
            "  ({} files indexed. Use `atlas files` to list them.)",
            stats.total_files
        );
    } else {
        println!();
        println!("  (No files indexed yet. Run `atlas index` to index your codebase.)");
    }

    Ok(())
}

/// Print per-language capability levels for languages that appear in the project.
fn print_capability_summary(files_by_language: &[(String, i64)]) {
    let mut lang_names: Vec<&str> = files_by_language.iter().map(|(k, _)| k.as_str()).collect();
    lang_names.sort();

    if lang_names.is_empty() {
        return;
    }

    println!();
    println!("  Capability Summary:");
    println!(
        "  {:<14} {:<20} Confidence Floor",
        "Language", "Level"
    );
    println!("  {:-<14} {:-<20} {:-<16}", "", "", "");

    for name in lang_names {
        if let Some(lang) = Language::from_str(name) {
            let profile = LanguageCapabilityProfile::for_language(lang);
            println!(
                "  {:<14} {:<20} {:.0}%",
                name,
                profile.capability_level.as_str(),
                profile.confidence_floor * 100.0
            );
            // If FeatureMatrix is available, show fine-grained capabilities
            if let Some(ref features) = profile.features {
                println!("    Features:");
                print_feature("symbols", &features.symbols);
                print_feature("references", &features.references);
                print_feature("imports", &features.imports);
                print_feature("scopes", &features.scopes);
                print_feature("call_graph", &features.call_graph);
                print_feature("lexical_bindings", &features.lexical_bindings);
                print_feature("local_dataflow", &features.local_dataflow);
                print_feature("use_def", &features.use_def);
                print_feature("field_access", &features.field_access);
                print_feature("call_arguments", &features.call_arguments);
                print_feature("returns_flow", &features.returns_flow);
                print_feature("cfg", &features.cfg);
                print_feature("interprocedural", &features.interprocedural_summaries);
            }
        }
    }
}

fn print_feature(name: &str, fs: &FeatureSupport) {
    let status = if fs.is_supported() {
        "supported"
    } else {
        "unsupported"
    };
    let detail = match fs {
        FeatureSupport::Supported { limitations, .. } => {
            if limitations.is_empty() {
                String::new()
            } else {
                format!(" (limitations: {})", limitations.join(", "))
            }
        }
        FeatureSupport::Unsupported { reason } => {
            format!(" ({reason})")
        }
    };
    println!("      {name:<20} {status}{detail}");
}

/// Show the indexed scope when set in project metadata.
fn print_indexed_scope(ctx: &crate::runtime::CommandContext) {
    if let Ok(Some(json)) = ctx.store.get_metadata("indexed_scope") {
        if json == "[]" {
            return;
        }
        // Try to parse as a JSON array of strings
        if let Ok(patterns) = serde_json::from_str::<Vec<String>>(&json) {
            if !patterns.is_empty() {
                println!();
                println!("  Index scope:");
                for p in &patterns {
                    println!("    - {p}");
                }
            }
        }
    }
}

/// Show file extraction state counts (manifest/structural/dataflow).
fn print_layer_summary(ctx: &crate::runtime::CommandContext) {
    if let Ok(layers) = ctx.store.count_file_extraction_state() {
        if layers.is_empty() {
            return;
        }
        println!();
        println!("  Extraction state:");
        for (layer, status, count) in &layers {
            println!("    {layer:<14} {status}={count}");
        }
    }
}
