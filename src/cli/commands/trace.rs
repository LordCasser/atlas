//! `atlas trace` — where does this value come from?
//!
//! Two subcommands:
//! - `trace point` — resolve a source position to its full context (symbol,
//!   data node, scope, bindings).
//! - `trace variable` — trace dataflow backward from a source position to
//!   find how a value reaches that point.

use crate::analysis::trace::{Locator, Slicer};
use crate::db::Store;
use crate::types::ids::FileId;
use anyhow::{anyhow, Context};
use std::path::Path;

/// Resolve file_path (relative to project root) to a FileId.
fn file_path_to_id(store: &Store, project_root: &Path, file_path: &str) -> anyhow::Result<FileId> {
    // Normalize: strip leading ./ or /
    let clean = file_path.trim_start_matches("./").trim_start_matches('/');

    let files = store.list_files().context("Failed to list files")?;

    // Try exact match first
    for f in &files {
        if f.path == clean {
            return Ok(f.file_id.clone());
        }
    }
    // Try suffix match (e.g. "src/foo.ts" matches ".../src/foo.ts")
    for f in &files {
        if f.path.ends_with(clean) {
            return Ok(f.file_id.clone());
        }
    }

    // Try resolving against project_root
    let abs = project_root.join(clean);
    let abs_str = abs.to_string_lossy();
    for f in &files {
        let file_abs = project_root.join(&f.path);
        if file_abs.to_string_lossy() == abs_str {
            return Ok(f.file_id.clone());
        }
    }

    Err(anyhow!(
        "File not found in index: '{}'. Index the project first with `atlas index`.",
        file_path
    ))
}

/// `atlas trace point`
pub fn run_point(
    project: &str,
    file_path: &str,
    line: u32,
    column: u32,
    json: bool,
) -> anyhow::Result<()> {
    let root = Path::new(project);
    let store = Store::open(root).context("Failed to open Atlas database")?;
    let file_id = file_path_to_id(&store, root, file_path)?;

    let point = Locator::locate(&store, &file_id, line, column)
        .context("Failed to locate position")?;

    if json {
        let output = serde_json::to_string_pretty(&point)?;
        println!("{}", output);
    } else {
        eprintln!("╔══ Trace Point ═══════════════════════════════════");
        eprintln!("║ file : {}", file_path);
        eprintln!("║ pos  : {}:{}", line, column);

        if let Some(ref r) = point.reference {
            eprintln!("║ ref  : {} ({})", r.text, r.kind.as_str());
        } else {
            eprintln!("║ ref  : (none)");
        }

        if let Some(ref sym) = point.resolved_symbol {
            eprintln!("║ sym  : {} ({})", sym.name, sym.kind.as_str());
        } else {
            eprintln!("║ sym  : (unresolved)");
        }

        if let Some(ref dn) = point.data_node {
            eprintln!("║ node : {} ({}) access_path={:?}",
                dn.name.as_deref().unwrap_or("?"),
                dn.kind.as_str(),
                dn.access_path,
            );
        } else {
            eprintln!("║ node : (none)");
        }

        if let Some(ref sc) = point.scope {
            eprintln!("║ scope: {} ({})", &sc.name, sc.kind.as_str());
        } else {
            eprintln!("║ scope: (none)");
        }

        eprintln!("║ in   : {} data node(s) flow into this point", point.incoming.len());
        for inc in &point.incoming {
            eprintln!("║   ← {} ({})", inc.name, inc.kind);
        }
        eprintln!("║ out  : {} data node(s) flow out of this point", point.outgoing.len());
        for out in &point.outgoing {
            eprintln!("║   → {} ({})", out.name, out.kind);
        }

        if let Some(ref b) = point.binding {
            eprintln!("║ bind : {} ({})", b.name, b.kind.as_str());
        }

        if let Some(ref cs) = point.callsite {
            eprintln!("║ call : {:?}", cs.receiver);
        }
        eprintln!("╚══════════════════════════════════════════════════");
    }

    Ok(())
}

/// `atlas trace variable`
pub fn run_variable(
    project: &str,
    file_path: &str,
    line: u32,
    column: u32,
    max_depth: usize,
    json: bool,
) -> anyhow::Result<()> {
    let root = Path::new(project);
    let store = Store::open(root).context("Failed to open Atlas database")?;
    let file_id = file_path_to_id(&store, root, file_path)?;

    let sink = Locator::locate(&store, &file_id, line, column)
        .context("Failed to locate position")?;

    if sink.data_node.is_none() {
        eprintln!("No data node found at {}:{}:{}.", file_path, line, column);
        eprintln!("Dataflow tracing requires data nodes (only available for TypeScript and Python).");
        return Ok(());
    }

    let trace = Slicer::slice(&store, &sink, max_depth)
        .context("Failed to trace dataflow")?;

    match trace {
        None => {
            eprintln!("No dataflow trace found for this position.");
            eprintln!("The slicer could not walk backward from this data node.");
            eprintln!("This may be normal if the value originates from a function parameter or global.");
        }
        Some(path) => {
            if json {
                let output = serde_json::to_string_pretty(&path)?;
                println!("{}", output);
            } else {
                eprintln!("╔══ Trace Path ════════════════════════════════════");
                eprintln!("║ confidence: {:.2}", path.confidence);
                eprintln!("║ nodes visited: {}", path.nodes_visited);
                eprintln!("║ steps: {}", path.steps.len());
                eprintln!("╠══ Source ════════════════════════════════════════");
                if let Some(ref dn) = path.source.data_node {
                    eprintln!("║ {} ({})", dn.name.as_deref().unwrap_or("?"), dn.kind.as_str());
                }
                eprintln!("╠══ Steps ═════════════════════════════════════════");
                for step in &path.steps {
                    eprintln!("║ {}: {} → {} ({})",
                        step.index,
                        step.from_node_id.to_hex().chars().take(8).collect::<String>(),
                        step.to_node_id.to_hex().chars().take(8).collect::<String>(),
                        step.description,
                    );
                }
                eprintln!("╠══ Sink ══════════════════════════════════════════");
                if let Some(ref dn) = path.sink.data_node {
                    eprintln!("║ {} ({})", dn.name.as_deref().unwrap_or("?"), dn.kind.as_str());
                }
                eprintln!("╚══════════════════════════════════════════════════");
            }
        }
    }

    Ok(())
}
