//! `atlas trace` — where does this value come from?
//!
//! Three subcommands:
//! - `trace point` — resolve a source position to its full context (symbol,
//!   data node, scope, bindings).
//! - `trace variable` — trace dataflow backward from a source position to
//!   find how a value reaches that point.
//! - `trace caller-path` — trace the call chain backward from a target
//!   function to its farthest caller.

use atlas_analysis::trace::{TraceEngine, TraceQueryResponse};
use atlas_db::Store;
use atlas_types::ids::SymbolId;
use atlas_workspace::Workspace;
use anyhow::Context;
use std::path::Path;
use std::sync::Arc;

/// Helper: in JSON mode, always output a TraceQueryResponse envelope, even for
/// pre-engine errors like missing file or invalid symbol.  In human-readable
/// mode, just print to stderr.
fn json_or_err<T: serde::Serialize>(
    json: bool,
    resp: &TraceQueryResponse<T>,
    fallback_msg: &str,
) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(resp)?);
        Ok(())
    } else {
        eprintln!("{}", fallback_msg);
        Ok(())
    }
}

/// `atlas trace point`
pub fn run_point(
    project: &str,
    file_path: &str,
    line: u32,
    column: u32,
    json: bool,
) -> anyhow::Result<()> {
    let ws = Workspace::open(Path::new(project))
        .with_context(|| format!("Invalid project path: {}", project))?;
    ws.ensure_atlas_dir()
        .context("Failed to create .atlas directory")?;
    let store = Arc::new(Store::open_db(ws.db_path()).context("Failed to open Atlas database")?);
    let engine = TraceEngine::new_with_root(store.clone(), ws.root().to_path_buf());

    let file_id = match engine.resolve_file_id_with_root(ws.root(), file_path)? {
        Some(fid) => fid,
        None => {
            let resp: TraceQueryResponse<atlas_types::trace::TracePoint> = TraceQueryResponse::err(
                "trace_point",
                &format!("File not found in index: '{}'", file_path),
            );
            return json_or_err(
                json,
                &resp,
                &format!("File not found in index: '{}'", file_path),
            );
        }
    };

    let resp = engine.trace_point(&file_id, line, column);

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    let point = match resp.result {
        Some(p) => p,
        None => {
            eprintln!(
                "Trace point failed: {}",
                resp.diagnostics
                    .first()
                    .map(|d| d.message.as_str())
                    .unwrap_or("unknown error")
            );
            return Ok(());
        }
    };

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
        eprintln!(
            "║ node : {} ({}) access_path={:?}",
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

    eprintln!(
        "║ in   : {} data node(s) flow into this point",
        point.incoming.len()
    );
    for inc in &point.incoming {
        eprintln!("║   ← {} ({})", inc.name, inc.kind);
    }
    eprintln!(
        "║ out  : {} data node(s) flow out of this point",
        point.outgoing.len()
    );
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
    let ws = Workspace::open(Path::new(project))
        .with_context(|| format!("Invalid project path: {}", project))?;
    ws.ensure_atlas_dir()
        .context("Failed to create .atlas directory")?;
    let store = Arc::new(Store::open_db(ws.db_path()).context("Failed to open Atlas database")?);
    let engine = TraceEngine::new_with_root(store.clone(), ws.root().to_path_buf());

    let file_id = match engine.resolve_file_id_with_root(ws.root(), file_path)? {
        Some(fid) => fid,
        None => {
            let resp: TraceQueryResponse<atlas_types::trace::TracePath> = TraceQueryResponse::err(
                "trace_variable",
                &format!("File not found in index: '{}'", file_path),
            );
            return json_or_err(
                json,
                &resp,
                &format!("File not found in index: '{}'", file_path),
            );
        }
    };

    let resp = engine.trace_variable(&file_id, line, column, max_depth);

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    let path = match resp.result {
        Some(p) => p,
        None => {
            for d in &resp.diagnostics {
                eprintln!("{}: {}", d.level.as_str(), d.message);
            }
            return Ok(());
        }
    };

    eprintln!("╔══ Trace Path ════════════════════════════════════");
    eprintln!("║ confidence: {:.2}", path.confidence);
    eprintln!("║ nodes visited: {}", path.nodes_visited);
    eprintln!("║ steps: {}", path.steps.len());
    eprintln!("╠══ Source ════════════════════════════════════════");
    if let Some(ref dn) = path.source.data_node {
        eprintln!(
            "║ {} ({})",
            dn.name.as_deref().unwrap_or("?"),
            dn.kind.as_str()
        );
    }
    eprintln!("╠══ Steps ═════════════════════════════════════════");
    for step in &path.steps {
        eprintln!(
            "║ {}: {} → {} ({})",
            step.index,
            step.from_node_id
                .to_hex()
                .chars()
                .take(8)
                .collect::<String>(),
            step.to_node_id.to_hex().chars().take(8).collect::<String>(),
            step.description,
        );
    }
    eprintln!("╠══ Sink ══════════════════════════════════════════");
    if let Some(ref dn) = path.sink.data_node {
        eprintln!(
            "║ {} ({})",
            dn.name.as_deref().unwrap_or("?"),
            dn.kind.as_str()
        );
    }
    eprintln!("╚══════════════════════════════════════════════════");
    Ok(())
}

/// `atlas trace caller-path`
pub fn run_caller_path(
    project: &str,
    symbol_hex: Option<&str>,
    symbol_name: Option<&str>,
    max_depth: usize,
    json: bool,
) -> anyhow::Result<()> {
    let ws = Workspace::open(Path::new(project))
        .with_context(|| format!("Invalid project path: {}", project))?;
    ws.ensure_atlas_dir()
        .context("Failed to create .atlas directory")?;
    let store = Arc::new(Store::open_db(ws.db_path()).context("Failed to open Atlas database")?);
    let engine = TraceEngine::new_with_root(store.clone(), ws.root().to_path_buf());

    let resp = if let Some(hex) = symbol_hex.filter(|h| !h.is_empty()) {
        let target_id: SymbolId = match hex.parse() {
            Ok(id) => id,
            Err(_) => {
                let resp: TraceQueryResponse<atlas_types::caller_path::CallerChain> =
                    TraceQueryResponse::err(
                        "trace_callers",
                        &format!("Invalid symbol hex ID: {}", hex),
                    );
                return json_or_err(json, &resp, &format!("Invalid symbol hex ID: {}", hex));
            }
        };
        engine.trace_callers(&target_id, max_depth)
    } else if let Some(name) = symbol_name {
        engine.trace_callers_by_name(name, max_depth)
    } else {
        let resp: TraceQueryResponse<atlas_types::caller_path::CallerChain> =
            TraceQueryResponse::err(
                "trace_callers",
                "Must provide either --symbol <hex> or --name <symbol-name>",
            );
        return json_or_err(
            json,
            &resp,
            "Must provide either --symbol <hex> or --name <symbol-name>",
        );
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    let chain = match resp.result {
        Some(c) => c,
        None => {
            for d in &resp.diagnostics {
                eprintln!("{}: {}", d.level.as_str(), d.message);
            }
            return Ok(());
        }
    };

    eprintln!("╔══ Caller Path ═══════════════════════════════════");
    eprintln!("║ nodes visited: {}", chain.nodes_visited);
    eprintln!("║ max depth reached: {}", chain.max_depth_reached);
    eprintln!("║ steps: {}", chain.steps.len());
    eprintln!("╠══ Root (farthest caller) ════════════════════════");
    eprintln!("║ {} ({})", chain.root.name, chain.root.kind.as_str());
    eprintln!("╠══ Steps ═════════════════════════════════════════");
    for step in &chain.steps {
        eprintln!(
            "║ {}: {} → {} ({})",
            step.index,
            step.caller.to_hex().chars().take(8).collect::<String>(),
            step.callee.to_hex().chars().take(8).collect::<String>(),
            step.description,
        );
    }
    eprintln!("╠══ Target ════════════════════════════════════════");
    eprintln!("║ {} ({})", chain.target.name, chain.target.kind.as_str());
    eprintln!("╚══════════════════════════════════════════════════");
    Ok(())
}
