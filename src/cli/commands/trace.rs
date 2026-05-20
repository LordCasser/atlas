//! `atlas trace` — where does this value come from?
//!
//! Two subcommands:
//! - `trace point` — resolve a source position to its full context (symbol,
//!   data node, scope, bindings).
//! - `trace variable` — trace dataflow backward from a source position to
//!   find how a value reaches that point.

use crate::analysis::trace::{CallerPathExplorer, Locator, Slicer};
use crate::db::Store;
use crate::types::capability::LanguageCapabilityProfile;
use crate::types::ids::{FileId, SymbolId};
use crate::types::trace::{TraceDiagnostic, TracePath};
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

    let mut point = Locator::locate(&store, &file_id, line, column)
        .context("Failed to locate position")?;

    // Inject capability from file_id → FileInfo.language (truth)
    if let Some(fi) = store.get_file(&file_id).context("Failed to get file info")? {
        point.capability = Some(LanguageCapabilityProfile::for_language(fi.language));
    }

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

    let mut sink = Locator::locate(&store, &file_id, line, column)
        .context("Failed to locate position")?;

    // Inject capability from file_id → FileInfo.language (truth)
    let cap = if let Some(fi) = store.get_file(&file_id).context("Failed to get file info")? {
        let c = LanguageCapabilityProfile::for_language(fi.language);
        sink.capability = Some(c.clone());
        Some(c)
    } else {
        None
    };

    if sink.data_node.is_none() {
        if json {
            let partial = TracePath {
                source: sink.clone(),
                steps: vec![],
                sink,
                confidence: 0.0,
                nodes_visited: 0,
                capability: cap,
                partial_result: true,
                diagnostics: vec![
                    TraceDiagnostic::warning("No data node at this position — dataflow tracing requires data nodes (only available for some languages)")
                        .with_code("no_data_node"),
                ],
            };
            println!("{}", serde_json::to_string_pretty(&partial)?);
        } else {
            eprintln!("No data node found at {}:{}:{}.", file_path, line, column);
            eprintln!("Dataflow tracing requires data nodes (only available for TypeScript and Python).");
        }
        return Ok(());
    }

    let trace = Slicer::slice(&store, &sink, max_depth)
        .context("Failed to trace dataflow")?;

    match trace {
        None => {
            if json {
                let partial = TracePath {
                    source: sink.clone(),
                    steps: vec![],
                    sink,
                    confidence: 0.0,
                    nodes_visited: 0,
                    capability: cap,
                    partial_result: true,
                    diagnostics: vec![
                        TraceDiagnostic::warning("Slicer could not walk backward from this data node")
                            .with_code("no_trace_path"),
                    ],
                };
                println!("{}", serde_json::to_string_pretty(&partial)?);
            } else {
                eprintln!("No dataflow trace found for this position.");
                eprintln!("The slicer could not walk backward from this data node.");
                eprintln!("This may be normal if the value originates from a function parameter or global.");
            }
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

/// `atlas trace caller-path`
pub fn run_caller_path(
    project: &str,
    symbol_hex: &str,
    max_depth: usize,
    json: bool,
) -> anyhow::Result<()> {
    let root = Path::new(project);
    let store = Store::open(root).context("Failed to open Atlas database")?;
    let target_id: SymbolId = symbol_hex
        .parse()
        .map_err(|_| anyhow!("Invalid symbol hex ID: {}", symbol_hex))?;

    let chain = CallerPathExplorer::explore(&store, &target_id, max_depth)
        .context("Failed to explore caller path")?;

    match chain {
        None => {
            if json {
                let partial = serde_json::json!({
                    "partial_result": true,
                    "diagnostics": [{
                        "level": "warning",
                        "message": format!("No callers found for symbol {} — this is a root/top-level function", symbol_hex),
                        "code": "no_callers",
                    }],
                });
                println!("{}", serde_json::to_string_pretty(&partial)?);
            } else {
                eprintln!("No callers found for symbol {}.", symbol_hex);
                eprintln!("This function is a root/top-level function (no incoming call edges).");
            }
        }
        Some(c) => {
            if json {
                let output = serde_json::to_string_pretty(&c)?;
                println!("{}", output);
            } else {
                eprintln!("╔══ Caller Path ═══════════════════════════════════");
                eprintln!("║ nodes visited: {}", c.nodes_visited);
                eprintln!("║ max depth reached: {}", c.max_depth_reached);
                eprintln!("║ steps: {}", c.steps.len());
                eprintln!("╠══ Root (farthest caller) ════════════════════════");
                eprintln!("║ {} ({})", c.root.name, c.root.kind.as_str());
                eprintln!("╠══ Steps ═════════════════════════════════════════");
                for step in &c.steps {
                    eprintln!(
                        "║ {}: {} → {} ({})",
                        step.index,
                        step.caller.to_hex().chars().take(8).collect::<String>(),
                        step.callee.to_hex().chars().take(8).collect::<String>(),
                        step.description,
                    );
                }
                eprintln!("╠══ Target ════════════════════════════════════════");
                eprintln!("║ {} ({})", c.target.name, c.target.kind.as_str());
                eprintln!("╚══════════════════════════════════════════════════");
            }
        }
    }

    Ok(())
}
