//! CfgBuilder — per-function control-flow graph from tree-sitter AST.
//!
//! # Architecture
//!
//! Walks the tree-sitter AST of a function body and produces:
//! - [`CfgNode`]s: Entry, Statement, Branch, Loop, Return, Throw, Join, Exit
//! - [`CfgEdge`]s: Normal, TrueBranch, FalseBranch, CaseBranch, LoopBack,
//!   Break, Continue, Redo, Retry, Goto, Defer, Exception
//!
//! # Supported constructs
//!
//! - Block statements (sequential Normal edges)
//! - if/else (Branch → TrueBranch/FalseBranch → Join)
//! - for/while/do (Loop → body → LoopBack → exit)
//! - return/throw (→ Exit)
//! - ?: ternary (Branch → Join)
//! - configured switch/match/when/case/select sibling paths, including
//!   supported implicit/explicit fall-through and blocking Go select semantics
//! - common try/catch/except/finally paths, with path-isolated normal and abrupt
//!   continuations (Exception edges and deterministic finally clones)
//! - Ruby method-body and nested begin/rescue/else/ensure regions through the
//!   same path-isolated continuation lowering
//! - Ruby `redo` for lexical loops and modeled block resources, plus
//!   rescue-owned `retry`, including nested ensure/resource cleanup
//! - Java try-with-resources, C# using, Python with, Kotlin use, and Ruby block
//!   resources with owner-matched path-isolated BlockExit nodes
//! - lexically resolved labeled break/continue for Java, JS/TS/ArkTS, Go,
//!   Rust, and Kotlin, including transfer through finally/managed cleanup
//! - direct same-function goto/label edges for C, C++, Go, C#, and PHP;
//!   C# exits execute intervening finally/using cleanup, while PHP exits
//!   execute intervening finally regions and reject loop/switch entry
//! - bounded path-sensitive Go defer registration with LIFO execution on
//!   normal function exits
//! - Rust `?` success and residual-return paths, bounded by nested closure and
//!   async-block function boundaries
//! - Rust `let-else` success vs explicit/unconditional-loop abrupt alternatives
//! - standalone unqualified Rust `panic!`/`unreachable!`/`todo!`/
//!   `unimplemented!` macros as local Throw terminals
//!
//! # NOT supported (deferred)
//! - async/await
//! - computed goto, C# goto case/default, and labels that the selected
//!   tree-sitter grammar does not expose as a lexical control target
//! - resolved/inherited catch-type selection and implicit exceptions from
//!   ordinary statements (Java/C#/PHP only apply an ordered exact-match cutoff
//!   for direct object-created explicit throws)
//! - cleanup exception suppression/replacement and exact exception identity
//! - Ruby ordinary iterator/callback block bodies
//! - Go defer stacks that can grow through a loop, over-budget defer-state
//!   expansion, and panic/recover unwinding
//! - Rust macro shadowing/re-exports, custom never-return macros, panic unwind,
//!   and `catch_unwind` recovery
//!
//! # Invariants
//!
//! - Every function CFG has exactly one Entry and one Exit node.
//! - All nodes belong to the same `function_id`.
//! - Comment AST extras never become executable Statement nodes.
//! - CfgNodeId and CfgEdgeId are deterministic (blake3).

use std::collections::{HashMap, HashSet, VecDeque};

use tree_sitter::Node;
use types::cfg::{CfgEdge, CfgNode};
use types::enums::{CallContext, CfgEdgeKind, CfgNodeKind, Language, SymbolKind};
use types::ids::{CfgNodeId, SymbolId};
use types::structs::{SymbolDef, TextRange};

/// Bound the multiplicative cost of continuation-aware `finally` and managed
/// resource lowering. Larger regions fall back atomically to one opaque
/// statement instead of emitting a partial or path-crossing CFG.
const MAX_PATH_ISOLATED_CLONES_PER_REGION: usize = 64;

// ── CfgLanguageConfig ───────────────────────────────────────────────────────

/// Language-specific tree-sitter node kind names used by the CFG builder.
struct CfgLanguageConfig {
    /// Node kinds that represent function body blocks.
    block_kinds: &'static [&'static str],
    /// Node kinds for if/else branches.
    if_kinds: &'static [&'static str],
    /// Node kinds for loops (for, while, do).
    loop_kinds: &'static [&'static str],
    /// Node kinds for return statements.
    return_kinds: &'static [&'static str],
    /// Node kinds for throw/raise statements.
    throw_kinds: &'static [&'static str],
    /// Node kinds for expression/declaration statements.
    stmt_kinds: &'static [&'static str],
    /// Node kinds for switch statements (switch, switch_expression, etc.).
    switch_kinds: &'static [&'static str],
    /// Node kinds for case/default clauses inside a switch body
    /// (case_statement, switch_case, expression_case, etc.).
    case_kinds: &'static [&'static str],
}

/// Return the language-specific CFG configuration for the given language.
fn cfg_config(lang: Language) -> CfgLanguageConfig {
    match lang {
        Language::TypeScript | Language::JavaScript | Language::ArkTS => CfgLanguageConfig {
            block_kinds: &["statement_block"],
            if_kinds: &["if_statement"],
            loop_kinds: &["for_statement", "while_statement", "do_statement"],
            return_kinds: &["return_statement"],
            throw_kinds: &["throw_statement"],
            stmt_kinds: &[
                "expression_statement",
                "variable_declaration",
                "lexical_declaration",
                "continue_statement",
                "break_statement",
                "debugger_statement",
                "empty_statement",
            ],
            switch_kinds: &["switch_statement"],
            case_kinds: &["switch_case", "switch_default"],
        },
        Language::Java => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_statement"],
            loop_kinds: &[
                "for_statement",
                "while_statement",
                "do_statement",
                "enhanced_for_statement",
            ],
            return_kinds: &["return_statement"],
            throw_kinds: &["throw_statement"],
            stmt_kinds: &[
                "expression_statement",
                "local_variable_declaration",
                "continue_statement",
                "break_statement",
            ],
            switch_kinds: &["switch_expression"],
            case_kinds: &["switch_block_statement_group", "switch_rule"],
        },
        Language::Go => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_statement"],
            loop_kinds: &["for_statement"],
            return_kinds: &["return_statement"],
            throw_kinds: &[], // Go has no throw
            stmt_kinds: &[
                "expression_statement",
                "short_var_declaration",
                "var_declaration",
                "continue_statement",
                "break_statement",
            ],
            switch_kinds: &[
                "expression_switch_statement",
                "type_switch_statement",
                "select_statement",
            ],
            case_kinds: &[
                "expression_case",
                "type_case",
                "communication_case",
                "default_case",
            ],
        },
        Language::Python => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_statement", "elif_clause", "else_clause"],
            loop_kinds: &["for_statement", "while_statement"],
            return_kinds: &["return_statement"],
            throw_kinds: &["raise_statement"],
            stmt_kinds: &[
                "expression_statement",
                "assignment",
                "continue_statement",
                "break_statement",
            ],
            // Match cases have no fall-through. CFG preserves sibling control
            // paths; capture-pattern and guard semantics remain outside dataflow.
            switch_kinds: &["match_statement"],
            case_kinds: &["case_clause"],
        },
        Language::C => CfgLanguageConfig {
            block_kinds: &["compound_statement"],
            if_kinds: &["if_statement"],
            loop_kinds: &["for_statement", "while_statement", "do_statement"],
            return_kinds: &["return_statement"],
            throw_kinds: &[], // C has no throw
            stmt_kinds: &[
                "expression_statement",
                "declaration",
                "continue_statement",
                "break_statement",
            ],
            switch_kinds: &["switch_statement"],
            case_kinds: &["case_statement"],
        },
        Language::Cpp => CfgLanguageConfig {
            block_kinds: &["compound_statement"],
            if_kinds: &["if_statement"],
            loop_kinds: &["for_statement", "while_statement", "do_statement"],
            return_kinds: &["return_statement"],
            throw_kinds: &["throw_statement"],
            stmt_kinds: &[
                "expression_statement",
                "declaration",
                "continue_statement",
                "break_statement",
            ],
            switch_kinds: &["switch_statement"],
            case_kinds: &["case_statement"],
        },
        Language::Rust => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_expression"],
            loop_kinds: &["for_expression", "while_expression", "loop_expression"],
            return_kinds: &["return_expression"],
            throw_kinds: &[], // Rust uses Result, not throw
            stmt_kinds: &["let_declaration", "continue_expression", "break_expression"],
            // Match arms have no fall-through. CFG preserves sibling control
            // paths; pattern/guard/binding semantics remain outside dataflow.
            switch_kinds: &["match_expression"],
            case_kinds: &["match_arm"],
        },
        Language::CSharp => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["if_statement"],
            loop_kinds: &[
                "for_statement",
                "foreach_statement",
                "while_statement",
                "do_statement",
            ],
            return_kinds: &["return_statement"],
            throw_kinds: &["throw_statement"],
            stmt_kinds: &["expression_statement", "local_declaration_statement"],
            switch_kinds: &["switch_statement"],
            case_kinds: &["switch_section"],
        },
        Language::Kotlin => CfgLanguageConfig {
            block_kinds: &["function_body", "control_structure_body", "statements"],
            if_kinds: &["if_expression"],
            loop_kinds: &["for_statement", "while_statement", "do_while_statement"],
            return_kinds: &["jump_expression"],
            throw_kinds: &[],
            stmt_kinds: &[
                "property_declaration",
                "assignment",
                "variable_declaration",
                "call_expression",
            ],
            // `when` entries have no fall-through. CFG preserves sibling
            // control paths; condition/guard/binding semantics stay outside dataflow.
            switch_kinds: &["when_expression"],
            case_kinds: &["when_entry"],
        },
        Language::Cangjie => CfgLanguageConfig {
            block_kinds: &["block"],
            if_kinds: &["ifExpression"],
            loop_kinds: &["whileExpression", "forInExpression", "doWhileExpression"],
            return_kinds: &["jumpExpression"], // jumpExpression covers return/break/continue
            throw_kinds: &[],
            stmt_kinds: &["variableDeclaration", "expressionStatement"],
            // Match arms have no fall-through, so they can use the same
            // sibling-path CFG shape as switch cases. Pattern/guard semantics
            // remain outside the CFG layer.
            switch_kinds: &["matchExpression"],
            case_kinds: &["matchCase", "matchCaseBody"],
        },
        Language::Php => CfgLanguageConfig {
            block_kinds: &["compound_statement", "colon_block"],
            if_kinds: &["if_statement", "else_if_clause"],
            loop_kinds: &[
                "for_statement",
                "foreach_statement",
                "while_statement",
                "do_statement",
            ],
            return_kinds: &["return_statement"],
            // PHP represents `throw` as an expression wrapped by an
            // `expression_statement`; the wrapper dispatch below unwraps it.
            throw_kinds: &["throw_expression"],
            stmt_kinds: &[
                "echo_statement",
                "global_declaration",
                "static_variable_declaration",
                "unset_statement",
                "break_statement",
                "continue_statement",
                "empty_statement",
            ],
            switch_kinds: &["switch_statement"],
            case_kinds: &["case_statement", "default_statement"],
        },
        Language::Ruby => CfgLanguageConfig {
            block_kinds: &["body_statement", "do", "then"],
            if_kinds: &["if", "unless", "elsif", "if_modifier", "unless_modifier"],
            loop_kinds: &["while", "until", "for", "while_modifier", "until_modifier"],
            return_kinds: &["return"],
            throw_kinds: &["raise"],
            stmt_kinds: &["call", "assignment", "break", "next"],
            // Neither classic `case`/`when` nor `case`/`in` falls through.
            switch_kinds: &["case", "case_match"],
            case_kinds: &["when", "in_clause", "else"],
        },
    }
}

// ── CfgBuilder ──────────────────────────────────────────────────────────────

/// Builds per-function control-flow graphs from a tree-sitter AST.
pub struct CfgBuilder;

/// Result of CFG construction for a single function.
#[derive(Debug, Default)]
pub struct CfgResult {
    pub nodes: Vec<CfgNode>,
    pub edges: Vec<CfgEdge>,
}

/// Context for building a CFG for one function.
struct CfgContext<'a> {
    function_id: SymbolId,
    nodes: Vec<CfgNode>,
    edges: Vec<CfgEdge>,
    source: &'a [u8],
    prev_node_id: Option<types::ids::CfgNodeId>,
    /// Abrupt exit sources connect to the single function Exit after the body
    /// walk, because the Exit ID is not available when they are emitted. Most
    /// sources are Return/Throw nodes; a Rust `?` also queues the containing
    /// node as a residual-return alternative while its success path continues.
    terminal_node_ids: Vec<(types::ids::CfgNodeId, CfgNodeKind)>,
    /// Break/continue targets are resolved after the destination Join/Loop is
    /// known. Numeric depth is used by PHP; source labels survive nested
    /// controls and path-isolated cleanup clones until their owner is reached.
    pending_break_node_ids: Vec<(types::ids::CfgNodeId, ControlTransferTarget)>,
    pending_continue_node_ids: Vec<(types::ids::CfgNodeId, ControlTransferTarget)>,
    /// Ruby `redo` restarts the innermost modeled loop/block body, while
    /// `retry` restarts the begin body owned by the enclosing rescue. Both
    /// remain pending across nested ensure/resource cleanup until that lexical
    /// owner is lowered.
    pending_redo_node_ids: Vec<types::ids::CfgNodeId>,
    pending_retry_node_ids: Vec<types::ids::CfgNodeId>,
    /// Direct goto sources and label entries are collected independently of
    /// lexical order, then resolved once the whole function body has been
    /// walked. This supports both forward and backward jumps without a
    /// synthetic label node.
    pending_goto_node_ids: Vec<PendingDirectGoto>,
    goto_label_targets: Vec<DirectGotoLabelTarget>,
    direct_goto_label_regions: HashMap<String, Vec<Vec<DirectGotoRegion>>>,
    /// A function with safely resolvable direct goto must keep scanning after
    /// abrupt statements so later label entries are still materialized. Such
    /// nodes stay disconnected unless a resolved goto makes them reachable.
    /// C# routes exits through finally/using cleanup and rejects jumps into a
    /// nested lexical/cleanup region or out of a finally clause. PHP routes
    /// exits through finally, rejects loop/switch entry, and keeps finally
    /// clauses closed to jumps in either direction.
    can_resolve_direct_goto: bool,
    /// Non-zero while lowering a path-isolated clone of one AST region.
    node_instance: u32,
    /// Monotonic deterministic instance allocator scoped to this CFG build.
    next_node_instance: u32,
    language: Language,
    config: CfgLanguageConfig,
    /// Pending call-site context for the next emitted statement node
    /// (Python with, Go go/defer, etc.).
    pending_call_context: CallContext,
    /// Persistent scope-level call context (applies to ALL nodes until reset).
    /// Used for React cleanup arrow bodies where every statement
    /// shares the same context.  When `pending_call_context` is `None`,
    /// `add_node` falls back to this value.
    scope_call_context: CallContext,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ControlTransferTarget {
    Depth(u32),
    Label(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectGotoRegionKind {
    LexicalBlock,
    LoopOrSwitch,
    TryFinally,
    Using,
    FinallyClause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectGotoRegion {
    kind: DirectGotoRegionKind,
    node_id: usize,
}

#[derive(Clone, Debug)]
struct PendingDirectGoto {
    source: CfgNodeId,
    label: String,
    source_regions: Vec<DirectGotoRegion>,
    target_instance: u32,
}

#[derive(Clone, Debug)]
struct DirectGotoLabelTarget {
    label: String,
    target: CfgNodeId,
    target_regions: Vec<DirectGotoRegion>,
    instance: u32,
}

#[derive(Clone, Copy)]
struct CfgCheckpoint {
    nodes_len: usize,
    edges_len: usize,
    terminal_len: usize,
    break_len: usize,
    continue_len: usize,
    redo_len: usize,
    retry_len: usize,
    goto_len: usize,
    prev_node_id: Option<types::ids::CfgNodeId>,
    node_instance: u32,
    next_node_instance: u32,
    pending_call_context: CallContext,
    scope_call_context: CallContext,
}

impl CfgBuilder {
    /// Build CFG for a function node.
    ///
    /// Scans the function body for statements and produces CFG nodes/edges.
    pub fn build(
        language: Language,
        function_id: &SymbolId,
        function_node: Node,
        source_bytes: &[u8],
    ) -> CfgResult {
        let config = cfg_config(language);
        let can_resolve_direct_goto = CfgContext::supports_direct_goto(language)
            && CfgContext::contains_direct_goto(function_node);

        // Detect React cleanup arrow: an arrow_function that is the direct
        // child of a return_statement (e.g., `return () => cleanup()`).
        // When true, all CFG nodes inside this function inherit
        // ReactEffectCleanup context so compose_effects marks their
        // Free effects as Deferred.
        let is_cleanup_return = function_node
            .parent()
            .map(|p| p.kind() == "return_statement")
            .unwrap_or(false);

        let mut ctx = CfgContext {
            function_id: *function_id,
            nodes: Vec::new(),
            edges: Vec::new(),
            source: source_bytes,
            prev_node_id: None,
            terminal_node_ids: Vec::new(),
            pending_break_node_ids: Vec::new(),
            pending_continue_node_ids: Vec::new(),
            pending_redo_node_ids: Vec::new(),
            pending_retry_node_ids: Vec::new(),
            pending_goto_node_ids: Vec::new(),
            goto_label_targets: Vec::new(),
            direct_goto_label_regions: HashMap::new(),
            can_resolve_direct_goto,
            node_instance: 0,
            next_node_instance: 1,
            language,
            config,
            pending_call_context: CallContext::None,
            scope_call_context: if is_cleanup_return {
                CallContext::ReactEffectCleanup
            } else {
                CallContext::None
            },
        };
        ctx.collect_direct_goto_label_regions(function_node);

        // 1. Create Entry node
        let entry_id = ctx.add_node(CfgNodeKind::Entry, 0, None);
        ctx.prev_node_id = Some(entry_id);

        // 2. Find the statement block
        let body = find_function_body(function_node, ctx.config.block_kinds);
        let has_body = body.is_some();

        // 3. Walk the body
        if let Some(body) = body {
            let body_range = node_text_range(&body, source_bytes);
            ctx.walk_block(body, body_range.start_byte);
        }

        // Goto labels have function scope and may appear before or after the
        // jump. Resolve only after the complete body traversal has discovered
        // every reachable or jump-reachable label entry.
        ctx.resolve_direct_gotos();

        // 4. If no body found, create a single Statement node
        if !has_body && ctx.prev_node_id.is_some() && ctx.nodes.len() == 1 {
            let fn_range = node_text_range(&function_node, source_bytes);
            ctx.add_node(CfgNodeKind::Statement, fn_range.start_byte, None);
        }

        // 5. Create Exit node and connect last node to exit
        let last = ctx.prev_node_id;
        let exit_id = ctx.add_node(CfgNodeKind::Exit, 0, None);
        let mut exit_sources = Vec::new();
        if let Some(last_id) = last {
            ctx.add_edge(&last_id, &exit_id, CfgEdgeKind::Normal);
            exit_sources.push(last_id);
        }
        let terminal_node_ids = std::mem::take(&mut ctx.terminal_node_ids);
        for (terminal_id, _) in terminal_node_ids {
            if !exit_sources.contains(&terminal_id) {
                ctx.add_edge(&terminal_id, &exit_id, CfgEdgeKind::Normal);
                exit_sources.push(terminal_id);
            }
        }

        let mut result = CfgResult {
            nodes: ctx.nodes,
            edges: ctx.edges,
        };
        if language == Language::Go {
            lower_go_defer_exits(&mut result);
        }
        result
    }
}

/// Expand a Go CFG with the finite runtime defer stack as part of the graph
/// state. A lexical join reached with two different registered stacks gets two
/// deterministic node identities, so an untaken conditional defer cannot leak
/// into the other path. Every edge into the function Exit then executes the
/// registered calls in LIFO order through owner-tagged `BlockExit` nodes.
///
/// A defer reachable repeatedly through a loop creates an unbounded runtime
/// stack. Likewise, many independent conditional defers can create an
/// exponential product. In either case the clone budget is exceeded and this
/// function leaves the original annotated CFG untouched rather than publishing
/// a partial or path-crossing graph.
fn lower_go_defer_exits(result: &mut CfgResult) -> bool {
    let registrations: HashSet<CfgNodeId> = result
        .nodes
        .iter()
        .filter(|node| {
            node.kind == CfgNodeKind::Statement && node.call_context == CallContext::GoDefer
        })
        .map(|node| node.id)
        .collect();
    if registrations.is_empty() {
        return true;
    }

    let Some(entry_id) = result
        .nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Entry)
        .map(|node| node.id)
    else {
        return false;
    };
    let Some(exit_id) = result
        .nodes
        .iter()
        .find(|node| node.kind == CfgNodeKind::Exit)
        .map(|node| node.id)
    else {
        return false;
    };

    let original_nodes: HashMap<_, _> = result
        .nodes
        .iter()
        .cloned()
        .map(|node| (node.id, node))
        .collect();
    let mut successors: HashMap<CfgNodeId, Vec<CfgEdge>> = HashMap::new();
    for edge in &result.edges {
        successors
            .entry(edge.source)
            .or_default()
            .push(edge.clone());
    }

    let mut reachable = HashSet::new();
    let mut reachable_queue = VecDeque::from([entry_id]);
    while let Some(node_id) = reachable_queue.pop_front() {
        if !reachable.insert(node_id) {
            continue;
        }
        if let Some(edges) = successors.get(&node_id) {
            reachable_queue.extend(edges.iter().map(|edge| edge.target));
        }
    }

    let mut expanded_nodes = result.nodes.clone();
    let mut expanded_edges = Vec::new();
    let mut edge_payloads = HashSet::new();
    let mut used_node_ids: HashSet<_> = result.nodes.iter().map(|node| node.id).collect();
    let mut next_instance = 1_u32;
    let mut clone_count = 0_usize;

    // The first runtime state reaching a lexical node keeps its original ID;
    // later defer-stack states receive deterministic lowering instances.
    let mut first_state_nodes = HashSet::from([entry_id]);
    let mut state_nodes = HashMap::from([((entry_id, Vec::<CfgNodeId>::new()), entry_id)]);
    let mut queue = VecDeque::from([(entry_id, Vec::<CfgNodeId>::new())]);
    let mut exit_routes = HashSet::new();

    while let Some((original_id, stack)) = queue.pop_front() {
        let output_source = state_nodes[&(original_id, stack.clone())];
        let mut successor_stack = stack;
        if registrations.contains(&original_id) {
            successor_stack.push(original_id);
            if successor_stack.len() > MAX_PATH_ISOLATED_CLONES_PER_REGION {
                return false;
            }
        }

        let Some(outgoing) = successors.get(&original_id) else {
            continue;
        };
        for edge in outgoing {
            if edge.target == exit_id {
                if !exit_routes.insert((output_source, successor_stack.clone())) {
                    continue;
                }
                if successor_stack.is_empty() {
                    push_unique_cfg_edge(
                        &mut expanded_edges,
                        &mut edge_payloads,
                        output_source,
                        exit_id,
                        edge.kind,
                    );
                    continue;
                }

                let mut tail = output_source;
                for registration_id in successor_stack.iter().rev() {
                    clone_count += 1;
                    if clone_count > MAX_PATH_ISOLATED_CLONES_PER_REGION {
                        return false;
                    }
                    let registration = &original_nodes[registration_id];
                    let block_exit =
                        fresh_go_defer_exit(registration, &mut used_node_ids, &mut next_instance);
                    let block_exit_id = block_exit.id;
                    expanded_nodes.push(block_exit);
                    push_unique_cfg_edge(
                        &mut expanded_edges,
                        &mut edge_payloads,
                        tail,
                        block_exit_id,
                        CfgEdgeKind::Defer,
                    );
                    tail = block_exit_id;
                }
                push_unique_cfg_edge(
                    &mut expanded_edges,
                    &mut edge_payloads,
                    tail,
                    exit_id,
                    CfgEdgeKind::Normal,
                );
                continue;
            }

            let state_key = (edge.target, successor_stack.clone());
            let output_target = if let Some(existing) = state_nodes.get(&state_key) {
                *existing
            } else {
                let output_target = if first_state_nodes.insert(edge.target) {
                    edge.target
                } else {
                    clone_count += 1;
                    if clone_count > MAX_PATH_ISOLATED_CLONES_PER_REGION {
                        return false;
                    }
                    let clone = fresh_cfg_clone(
                        &original_nodes[&edge.target],
                        &mut used_node_ids,
                        &mut next_instance,
                    );
                    let clone_id = clone.id;
                    expanded_nodes.push(clone);
                    clone_id
                };
                state_nodes.insert(state_key.clone(), output_target);
                queue.push_back(state_key);
                output_target
            };
            push_unique_cfg_edge(
                &mut expanded_edges,
                &mut edge_payloads,
                output_source,
                output_target,
                edge.kind,
            );
        }
    }

    // Keep syntax that the base builder intentionally retained for a possible
    // goto label but that remains disconnected from Entry. It cannot affect a
    // runtime path, and retaining it preserves source-level evidence.
    for edge in &result.edges {
        if !reachable.contains(&edge.source) {
            push_unique_cfg_edge(
                &mut expanded_edges,
                &mut edge_payloads,
                edge.source,
                edge.target,
                edge.kind,
            );
        }
    }

    result.nodes = expanded_nodes;
    result.edges = expanded_edges;
    true
}

fn fresh_cfg_clone(
    original: &CfgNode,
    used_ids: &mut HashSet<CfgNodeId>,
    next_instance: &mut u32,
) -> CfgNode {
    loop {
        let mut clone = CfgNode::new_with_instance(
            &original.function_id,
            original.kind,
            original.stmt_range,
            *next_instance,
        );
        *next_instance = next_instance.saturating_add(1);
        if used_ids.insert(clone.id) {
            clone.call_context = original.call_context;
            clone.managed_scope_start_byte = original.managed_scope_start_byte;
            clone.semantic_effects = original.semantic_effects.clone();
            return clone;
        }
    }
}

fn fresh_go_defer_exit(
    registration: &CfgNode,
    used_ids: &mut HashSet<CfgNodeId>,
    next_instance: &mut u32,
) -> CfgNode {
    let range = TextRange {
        start_byte: registration.stmt_range.end_byte,
        end_byte: registration.stmt_range.end_byte,
        start_line: registration.stmt_range.end_line,
        start_column: registration.stmt_range.end_column,
        end_line: registration.stmt_range.end_line,
        end_column: registration.stmt_range.end_column,
    };
    loop {
        let mut node = CfgNode::new_with_instance(
            &registration.function_id,
            CfgNodeKind::BlockExit,
            range,
            *next_instance,
        );
        *next_instance = next_instance.saturating_add(1);
        if used_ids.insert(node.id) {
            node.call_context = CallContext::GoDefer;
            node.managed_scope_start_byte = registration.managed_scope_start_byte;
            return node;
        }
    }
}

fn push_unique_cfg_edge(
    edges: &mut Vec<CfgEdge>,
    payloads: &mut HashSet<(CfgNodeId, CfgNodeId, CfgEdgeKind)>,
    source: CfgNodeId,
    target: CfgNodeId,
    kind: CfgEdgeKind,
) {
    if payloads.insert((source, target, kind)) {
        edges.push(CfgEdge::new(&source, &target, kind));
    }
}

fn is_comment_node_kind(kind: &str) -> bool {
    matches!(
        kind,
        "comment" | "line_comment" | "block_comment" | "doc_comment" | "html_comment"
    )
}

impl CfgContext<'_> {
    fn supports_direct_goto(language: Language) -> bool {
        matches!(
            language,
            Language::C | Language::Cpp | Language::Go | Language::CSharp | Language::Php
        )
    }

    fn contains_direct_goto(root: Node<'_>) -> bool {
        let mut pending = Vec::new();
        let mut cursor = root.walk();
        pending.extend(root.named_children(&mut cursor));

        while let Some(node) = pending.pop() {
            if node.kind() == "goto_statement" {
                return true;
            }
            if matches!(
                node.kind(),
                "function_definition"
                    | "function_item"
                    | "local_function_statement"
                    | "lambda_expression"
                    | "anonymous_method_expression"
                    | "func_literal"
                    | "anonymous_function_creation_expression"
                    | "arrow_function"
            ) {
                continue;
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        false
    }

    fn checkpoint(&self) -> CfgCheckpoint {
        CfgCheckpoint {
            nodes_len: self.nodes.len(),
            edges_len: self.edges.len(),
            terminal_len: self.terminal_node_ids.len(),
            break_len: self.pending_break_node_ids.len(),
            continue_len: self.pending_continue_node_ids.len(),
            redo_len: self.pending_redo_node_ids.len(),
            retry_len: self.pending_retry_node_ids.len(),
            goto_len: self.pending_goto_node_ids.len(),
            prev_node_id: self.prev_node_id,
            node_instance: self.node_instance,
            next_node_instance: self.next_node_instance,
            pending_call_context: self.pending_call_context,
            scope_call_context: self.scope_call_context,
        }
    }

    fn rollback_to(&mut self, checkpoint: CfgCheckpoint) {
        self.nodes.truncate(checkpoint.nodes_len);
        self.edges.truncate(checkpoint.edges_len);
        self.terminal_node_ids.truncate(checkpoint.terminal_len);
        self.pending_break_node_ids.truncate(checkpoint.break_len);
        self.pending_continue_node_ids
            .truncate(checkpoint.continue_len);
        self.pending_redo_node_ids.truncate(checkpoint.redo_len);
        self.pending_retry_node_ids.truncate(checkpoint.retry_len);
        self.pending_goto_node_ids.truncate(checkpoint.goto_len);
        self.prev_node_id = checkpoint.prev_node_id;
        self.node_instance = checkpoint.node_instance;
        self.next_node_instance = checkpoint.next_node_instance;
        self.pending_call_context = checkpoint.pending_call_context;
        self.scope_call_context = checkpoint.scope_call_context;
    }

    fn add_node(
        &mut self,
        kind: CfgNodeKind,
        start_byte: u32,
        stmt_node: Option<&Node>,
    ) -> types::ids::CfgNodeId {
        let has_rust_try_residual =
            stmt_node.is_some_and(|node| self.has_rust_try_residual(kind, *node));
        let range = if let Some(node) = stmt_node {
            let r = node.range();
            TextRange {
                start_byte: r.start_byte as u32,
                end_byte: r.end_byte as u32,
                start_line: r.start_point.row as u32,
                start_column: r.start_point.column as u32,
                end_line: r.end_point.row as u32,
                end_column: r.end_point.column as u32,
            }
        } else {
            TextRange {
                start_byte,
                end_byte: start_byte,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            }
        };
        let mut node =
            CfgNode::new_with_instance(&self.function_id, kind, range, self.node_instance);
        node.call_context = if self.pending_call_context != CallContext::None {
            let ctx = self.pending_call_context;
            self.pending_call_context = CallContext::None;
            ctx
        } else {
            self.scope_call_context
        };
        let id = node.id;
        self.nodes.push(node);
        if has_rust_try_residual {
            self.terminal_node_ids.push((id, CfgNodeKind::Return));
        }
        id
    }

    /// Rust's `?` has two paths: the successful value continues locally while
    /// the residual returns from the enclosing function. The containing CFG
    /// node already represents evaluation, so queue only an additional Exit
    /// continuation instead of inventing a second source statement.
    ///
    /// For control nodes, inspect only the header expression. Recursing over
    /// the full `if`/loop/match node would incorrectly attribute `?` from its
    /// body to the dispatch itself. Nested closures and async blocks own their
    /// own return boundary and must not terminate the enclosing function.
    fn has_rust_try_residual(&self, kind: CfgNodeKind, node: Node<'_>) -> bool {
        if self.language != Language::Rust {
            return false;
        }

        let subject = match kind {
            CfgNodeKind::Statement => Some(node),
            CfgNodeKind::Branch | CfgNodeKind::Loop => node
                .child_by_field_name("condition")
                .or_else(|| node.child_by_field_name("value")),
            _ => None,
        };
        subject.is_some_and(Self::contains_rust_try_expression)
    }

    fn contains_rust_try_expression(node: Node<'_>) -> bool {
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            if node.kind() == "try_expression" {
                return true;
            }
            if matches!(
                node.kind(),
                "closure_expression" | "async_block" | "function_item"
            ) {
                continue;
            }

            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        false
    }

    fn add_edge(
        &mut self,
        source: &types::ids::CfgNodeId,
        target: &types::ids::CfgNodeId,
        kind: CfgEdgeKind,
    ) {
        self.edges.push(CfgEdge::new(source, target, kind));
    }

    fn retag_edge(&mut self, index: usize, kind: CfgEdgeKind) {
        let edge = &mut self.edges[index];
        *edge = CfgEdge::new(&edge.source, &edge.target, kind);
    }

    fn mark_managed_scope(&mut self, node_id: types::ids::CfgNodeId, scope_start_byte: u32) {
        let node = self
            .nodes
            .iter_mut()
            .rev()
            .find(|node| node.id == node_id)
            .expect("newly emitted CFG node must exist");
        node.managed_scope_start_byte = Some(scope_start_byte);
    }

    fn emit_managed_resource(
        &mut self,
        stmt_node: &Node,
        context: CallContext,
        scope_start_byte: u32,
    ) -> types::ids::CfgNodeId {
        self.pending_call_context = context;
        let node_id = self.emit_stmt(
            CfgNodeKind::Statement,
            stmt_node.start_byte() as u32,
            stmt_node,
        );
        self.mark_managed_scope(node_id, scope_start_byte);
        node_id
    }

    fn append_managed_block_exit(
        &mut self,
        source: types::ids::CfgNodeId,
        scope_start_byte: u32,
        scope_end_byte: u32,
        context: CallContext,
    ) -> types::ids::CfgNodeId {
        let saved_instance = self.node_instance;
        self.node_instance = self.next_node_instance;
        self.next_node_instance = self.next_node_instance.saturating_add(1);
        let block_exit_id = self.add_node(CfgNodeKind::BlockExit, scope_end_byte, None);
        self.node_instance = saved_instance;

        let block_exit = self
            .nodes
            .last_mut()
            .expect("newly emitted BlockExit must exist");
        block_exit.call_context = context;
        block_exit.managed_scope_start_byte = Some(scope_start_byte);
        self.add_edge(&source, &block_exit_id, CfgEdgeKind::Normal);
        block_exit_id
    }

    /// Route every completion of one managed-resource body through a distinct
    /// BlockExit. Reusing one exit node would let return/throw/break/continue/
    /// retry/goto continuations cross into the normal successor. A Ruby block-level
    /// break/next exits the yielding call, so those isolated exits converge
    /// into the call's normal successor rather than escaping an outer loop.
    /// Every completion that is not already Throw also records a conservative
    /// Throw alternative for cleanup itself; the terminal queue then carries
    /// that alternative through enclosing managed scopes and finally regions.
    fn finish_managed_scope(
        &mut self,
        scope_start_byte: u32,
        scope_end_byte: u32,
        context: CallContext,
        checkpoint: CfgCheckpoint,
        goto_region: Option<DirectGotoRegion>,
    ) -> bool {
        let terminal_start = checkpoint.terminal_len;
        let break_start = checkpoint.break_len;
        let continue_start = checkpoint.continue_len;
        let retry_start = checkpoint.retry_len;
        let goto_start = checkpoint.goto_len;
        let exiting_goto_count = goto_region.map_or(0, |region| {
            self.pending_goto_node_ids[goto_start..]
                .iter()
                .filter(|pending| self.direct_goto_exits_region(pending, region))
                .count()
        });
        let clone_count = usize::from(self.prev_node_id.is_some())
            + (self.terminal_node_ids.len() - terminal_start)
            + (self.pending_break_node_ids.len() - break_start)
            + (self.pending_continue_node_ids.len() - continue_start)
            + (self.pending_retry_node_ids.len() - retry_start)
            + exiting_goto_count;
        if clone_count > MAX_PATH_ISOLATED_CLONES_PER_REGION {
            return false;
        }

        let normal_tail = self.prev_node_id.take();
        let pending_terminals = self.terminal_node_ids.split_off(terminal_start);
        let pending_breaks = self.pending_break_node_ids.split_off(break_start);
        let pending_continues = self.pending_continue_node_ids.split_off(continue_start);
        let pending_retries = self.pending_retry_node_ids.split_off(retry_start);
        let pending_gotos = self.pending_goto_node_ids.split_off(goto_start);

        let normal_exit = normal_tail.map(|tail| {
            self.append_managed_block_exit(tail, scope_start_byte, scope_end_byte, context)
        });
        if let Some(normal_exit) = normal_exit {
            self.terminal_node_ids
                .push((normal_exit, CfgNodeKind::Throw));
        }
        let mut normal_exits = normal_exit.into_iter().collect::<Vec<_>>();
        for (terminal_id, terminal_kind) in pending_terminals {
            let block_exit = self.append_managed_block_exit(
                terminal_id,
                scope_start_byte,
                scope_end_byte,
                context,
            );
            self.terminal_node_ids.push((block_exit, terminal_kind));
            if terminal_kind != CfgNodeKind::Throw {
                self.terminal_node_ids
                    .push((block_exit, CfgNodeKind::Throw));
            }
        }
        for (break_id, target) in pending_breaks {
            let block_exit =
                self.append_managed_block_exit(break_id, scope_start_byte, scope_end_byte, context);
            self.terminal_node_ids
                .push((block_exit, CfgNodeKind::Throw));
            if context == CallContext::RubyBlock && target == ControlTransferTarget::Depth(1) {
                normal_exits.push(block_exit);
            } else {
                self.pending_break_node_ids.push((block_exit, target));
            }
        }
        for (continue_id, target) in pending_continues {
            let block_exit = self.append_managed_block_exit(
                continue_id,
                scope_start_byte,
                scope_end_byte,
                context,
            );
            self.terminal_node_ids
                .push((block_exit, CfgNodeKind::Throw));
            if context == CallContext::RubyBlock && target == ControlTransferTarget::Depth(1) {
                normal_exits.push(block_exit);
            } else {
                self.pending_continue_node_ids.push((block_exit, target));
            }
        }
        for retry_id in pending_retries {
            let block_exit =
                self.append_managed_block_exit(retry_id, scope_start_byte, scope_end_byte, context);
            self.terminal_node_ids
                .push((block_exit, CfgNodeKind::Throw));
            self.pending_retry_node_ids.push(block_exit);
        }
        for mut pending in pending_gotos {
            if goto_region.is_some_and(|region| self.direct_goto_exits_region(&pending, region)) {
                pending.source = self.append_managed_block_exit(
                    pending.source,
                    scope_start_byte,
                    scope_end_byte,
                    context,
                );
                self.terminal_node_ids
                    .push((pending.source, CfgNodeKind::Throw));
            }
            self.pending_goto_node_ids.push(pending);
        }
        if context == CallContext::RubyBlock && normal_exits.len() > 1 {
            let join_id = self.add_node(CfgNodeKind::Join, scope_end_byte, None);
            for block_exit in normal_exits {
                self.add_edge(&block_exit, &join_id, CfgEdgeKind::Normal);
            }
            self.prev_node_id = Some(join_id);
        } else {
            self.prev_node_id = normal_exits.pop();
        }
        true
    }

    fn is_ruby(&self) -> bool {
        matches!(self.language, Language::Ruby)
    }

    fn is_kotlin(&self) -> bool {
        matches!(self.language, Language::Kotlin)
    }

    /// Check whether a `call` node has a `block` or `do_block` child.
    fn has_block_child(&self, call_node: &Node) -> bool {
        let mut cursor = call_node.walk();
        call_node
            .named_children(&mut cursor)
            .any(|c| c.kind() == "do_block" || c.kind() == "block")
    }

    /// Check whether a `call_expression` node has a `lambda_literal` child
    /// (indicating a trailing lambda like `.use { ... }`).
    /// Search recursively for a `lambda_literal` descendant of the given node.
    /// In Kotlin tree-sitter, trailing lambdas are nested inside `call_suffix`,
    /// not as direct children of `call_expression`.
    #[allow(clippy::only_used_in_recursion)]
    fn find_lambda_literal<'a>(&self, node: &Node<'a>) -> Option<Node<'a>> {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "lambda_literal" {
                return Some(child);
            }
            if child.child_count() > 0
                && let Some(found) = self.find_lambda_literal(&child)
            {
                return Some(found);
            }
        }
        None
    }

    fn has_lambda_child(&self, call_node: &Node) -> bool {
        self.find_lambda_literal(call_node).is_some()
    }

    /// Check whether a Kotlin `call_expression`'s callee name ends with `.use`.
    /// Scans named children (skipping `call_suffix` children) and checks if any
    /// child's text ends with `.use`. This avoids matching `list.map { }`,
    /// `run { }`, etc.
    fn is_kotlin_use_call(&self, call_node: &Node) -> bool {
        let mut cursor = call_node.walk();
        for child in call_node.named_children(&mut cursor) {
            if child.kind() == "call_suffix" {
                continue;
            }
            if let Ok(text) = child.utf8_text(self.source)
                && text.ends_with(".use")
            {
                return true;
            }
        }
        false
    }

    /// Check whether a Ruby `call` node's callee name matches a known
    /// resource-managing method (e.g. `File.open`, `Dir.chdir`).
    /// Filters out non-resource block calls like `[1,2,3].map { }`,
    /// `5.times { }`, `users.each { }`.
    fn is_ruby_resource_block_call(&self, call_node: &Node) -> bool {
        if let Some(name) = self.extract_callee_name(call_node) {
            matches!(
                name.as_str(),
                "File.open"
                    | "File.new"
                    | "IO.open"
                    | "IO.new"
                    | "open"
                    | "Tempfile.create"
                    | "Dir.chdir"
                    | "Dir.open"
                    | "Dir.new"
                    | "TCPServer.new"
                    | "UDPSocket.new"
            )
        } else {
            // Fallback: scan node text for known patterns
            if let Ok(text) = call_node.utf8_text(self.source) {
                text.starts_with("File.open")
                    || text.starts_with("File.new")
                    || text.starts_with("IO.open")
                    || text.starts_with("IO.new")
                    || text.starts_with("open(")
            } else {
                false
            }
        }
    }

    /// Extract callee function name from a call_expression node.
    fn extract_callee_name(&self, call_node: &Node) -> Option<String> {
        let mut cursor = call_node.walk();
        for child in call_node.named_children(&mut cursor) {
            if (child.kind() == "identifier" || child.kind() == "field_expression")
                && let Ok(text) = child.utf8_text(self.source)
            {
                return Some(text.to_string());
            }
        }
        None
    }

    fn walk_block(&mut self, block: Node, _block_start: u32) {
        let mut cursor = block.walk();
        let children: Vec<Node> = block
            .named_children(&mut cursor)
            .filter(|child| {
                !(self.language == Language::Rust
                    && block.kind() == "block"
                    && child.kind() == "label")
            })
            .collect();
        if self.is_ruby()
            && block.kind() == "body_statement"
            && children
                .iter()
                .any(|child| matches!(child.kind(), "rescue" | "else" | "ensure"))
        {
            self.walk_try(&[block], 0, block.start_byte() as u32);
            return;
        }
        self.walk_stmt_list(&children);
    }

    /// Walk a flat list of statement nodes using the per-statement dispatch.
    ///
    /// Shared by [`Self::walk_block`] (block bodies) and [`Self::walk_switch`]
    /// (switch-case bodies) so that control-flow constructs nested inside a
    /// `switch` case (if/loop/nested switch/etc.) are handled identically to
    /// top-level block statements.
    fn walk_stmt_list(&mut self, children: &[Node]) {
        // Process each statement in the block
        let mut i = 0;
        while i < children.len() {
            let stmt = children[i];
            let kind = stmt.kind();
            if is_comment_node_kind(kind) {
                i += 1;
                continue;
            }
            let stmt_range = node_text_range(&stmt, self.source);
            if self.is_kotlin() && kind == "label" {
                let mut body_idx = i;
                let mut labels = Vec::new();
                while children
                    .get(body_idx)
                    .is_some_and(|child| child.kind() == "label")
                {
                    if let Ok(label) = children[body_idx].utf8_text(self.source) {
                        let label = label.trim().trim_end_matches('@');
                        if !label.is_empty() {
                            labels.push(label.to_string());
                        }
                    }
                    body_idx += 1;
                }
                if let Some(loop_node) = children.get(body_idx)
                    && self.config.loop_kinds.contains(&loop_node.kind())
                    && !labels.is_empty()
                {
                    self.walk_loop_with_labels(children, body_idx, stmt_range.start_byte, &labels);
                    i = body_idx + 1;
                    continue;
                }
            }
            let abrupt_stmt = if kind == "expression_statement" {
                let mut cursor = stmt.walk();
                stmt.named_children(&mut cursor).next().unwrap_or(stmt)
            } else {
                stmt
            };

            if Self::supports_direct_goto(self.language) && abrupt_stmt.kind() == "goto_statement" {
                let range = node_text_range(&abrupt_stmt, self.source);
                let node_id =
                    self.emit_stmt(CfgNodeKind::Statement, range.start_byte, &abrupt_stmt);
                self.prev_node_id.take();
                if self.can_resolve_direct_goto
                    && let Some(target) = self.direct_goto_target(abrupt_stmt)
                {
                    self.pending_goto_node_ids.push(PendingDirectGoto {
                        source: node_id,
                        label: target,
                        source_regions: self.direct_goto_regions(abrupt_stmt),
                        target_instance: self.node_instance,
                    });
                }
                if self.can_resolve_direct_goto {
                    i += 1;
                    continue;
                }
                break;
            } else if self.is_break_statement(&abrupt_stmt) {
                let range = node_text_range(&abrupt_stmt, self.source);
                let node_id =
                    self.emit_stmt(CfgNodeKind::Statement, range.start_byte, &abrupt_stmt);
                self.prev_node_id.take();
                if let Some(target) = self.control_transfer_target(&abrupt_stmt, "break") {
                    self.pending_break_node_ids.push((node_id, target));
                }
                if self.can_resolve_direct_goto {
                    i += 1;
                    continue;
                }
                break;
            } else if self.is_continue_statement(&abrupt_stmt) {
                let range = node_text_range(&abrupt_stmt, self.source);
                let node_id =
                    self.emit_stmt(CfgNodeKind::Statement, range.start_byte, &abrupt_stmt);
                self.prev_node_id.take();
                if let Some(target) = self.control_transfer_target(&abrupt_stmt, "continue") {
                    self.pending_continue_node_ids.push((node_id, target));
                }
                if self.can_resolve_direct_goto {
                    i += 1;
                    continue;
                }
                break;
            } else if self.is_ruby() && abrupt_stmt.kind() == "redo" {
                let range = node_text_range(&abrupt_stmt, self.source);
                let node_id =
                    self.emit_stmt(CfgNodeKind::Statement, range.start_byte, &abrupt_stmt);
                self.prev_node_id.take();
                self.pending_redo_node_ids.push(node_id);
                break;
            } else if self.is_ruby() && abrupt_stmt.kind() == "retry" {
                let range = node_text_range(&abrupt_stmt, self.source);
                let node_id =
                    self.emit_stmt(CfgNodeKind::Statement, range.start_byte, &abrupt_stmt);
                self.prev_node_id.take();
                self.pending_retry_node_ids.push(node_id);
                break;
            } else if self.language == Language::Php && kind == "named_label_statement" {
                let node_start = self.nodes.len();
                self.emit_stmt(CfgNodeKind::Join, stmt_range.start_byte, &stmt);
                let label = self.direct_goto_label(stmt);
                self.record_direct_goto_label(label, stmt, node_start);
                i += 1;
            } else if kind == "labeled_statement" {
                self.walk_labeled_statement(stmt, stmt_range.start_byte);
                i += 1;
            } else if self.config.if_kinds.contains(&kind) {
                i = self.walk_if(children, i, stmt_range.start_byte);
            } else if self.config.loop_kinds.contains(&kind) {
                i = self.walk_loop(children, i, stmt_range.start_byte);
            } else if self.is_throw_statement(&abrupt_stmt) {
                let range = node_text_range(&abrupt_stmt, self.source);
                self.emit_stmt(CfgNodeKind::Throw, range.start_byte, &abrupt_stmt);
                if self.can_resolve_direct_goto {
                    i += 1;
                    continue;
                }
                break;
            } else if self.config.return_kinds.contains(&abrupt_stmt.kind()) {
                let range = node_text_range(&abrupt_stmt, self.source);
                self.emit_stmt(CfgNodeKind::Return, range.start_byte, &abrupt_stmt);
                // React cleanup return: `return () => { ... }` or `return () => expr`
                // Walk the arrow body with ReactEffectCleanup scope context, so
                // frees inside the cleanup callback get Deferred consumption style.
                let mut return_cursor = abrupt_stmt.walk();
                for child in abrupt_stmt.named_children(&mut return_cursor) {
                    if child.kind() == "arrow_function" {
                        self.walk_react_cleanup_arrow(&child, stmt_range.start_byte);
                        break;
                    }
                }
                if self.can_resolve_direct_goto {
                    i += 1;
                    continue;
                }
                break;
            } else if self.is_ruby()
                && kind == "call"
                && self.has_block_child(&stmt)
                && self.is_ruby_resource_block_call(&stmt)
            {
                // Ruby block-managed resource: File.open(...) { |f| ... }
                // Mark the resource call, then route every block completion
                // through an owner-bound BlockExit.
                let checkpoint = self.checkpoint();
                let resource_id = self.emit_managed_resource(
                    &stmt,
                    CallContext::RubyBlock,
                    stmt_range.start_byte,
                );
                let body_edge_start = self.edges.len();
                // Walk the block body (find do_block/block child → body_statement)
                let mut child_cursor = stmt.walk();
                for child in stmt.named_children(&mut child_cursor) {
                    if child.kind() == "do_block" || child.kind() == "block" {
                        if let Some(body) = find_function_body(child, self.config.block_kinds) {
                            self.walk_block(body, stmt_range.start_byte);
                        } else {
                            // Fallback: walk the block node directly
                            self.walk_block(child, stmt_range.start_byte);
                        }
                        break;
                    }
                }
                let body_entry = self.edges[body_edge_start..]
                    .iter()
                    .find(|edge| edge.source == resource_id)
                    .map(|edge| edge.target);
                self.resolve_ruby_redos(body_entry, checkpoint.redo_len);

                if !self.finish_managed_scope(
                    stmt_range.start_byte,
                    stmt.end_byte() as u32,
                    CallContext::RubyBlock,
                    checkpoint,
                    None,
                ) {
                    self.rollback_to(checkpoint);
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                }

                i += 1;
                continue;
            } else if self.is_kotlin()
                && kind == "call_expression"
                && self.has_lambda_child(&stmt)
                && self.is_kotlin_use_call(&stmt)
            {
                // Kotlin `.use {}` block-managed resource: File(...).use { ... }
                let checkpoint = self.checkpoint();
                self.emit_managed_resource(&stmt, CallContext::KotlinUse, stmt_range.start_byte);
                // Walk the lambda body (recursively find lambda_literal → function_body/statements)
                if let Some(lambda) = self.find_lambda_literal(&stmt) {
                    if let Some(body) = find_function_body(lambda, self.config.block_kinds) {
                        self.walk_block(body, stmt_range.start_byte);
                    } else {
                        // Fallback: walk the lambda node directly
                        self.walk_block(lambda, stmt_range.start_byte);
                    }
                }

                if !self.finish_managed_scope(
                    stmt_range.start_byte,
                    stmt.end_byte() as u32,
                    CallContext::KotlinUse,
                    checkpoint,
                    None,
                ) {
                    self.rollback_to(checkpoint);
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                }

                i += 1;
                continue;
            } else if self.language == Language::Rust
                && kind == "let_declaration"
                && stmt.child_by_field_name("alternative").is_some()
            {
                self.walk_rust_let_else(stmt, stmt_range.start_byte);
                i += 1;
            } else if self.config.stmt_kinds.contains(&kind) {
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                i += 1;
            } else if self.config.block_kinds.contains(&kind) {
                // Nested block
                self.walk_block(stmt, stmt_range.start_byte);
                i += 1;
            } else if kind == "statement_list" {
                // Go block wrapper: recurse through statement_list body
                self.walk_block(stmt, stmt_range.start_byte);
                i += 1;
            } else if self.is_kotlin() && kind == "statements" {
                // Kotlin block wrapper: function_body contains a `statements`
                // group that wraps the actual call_expression/property_declaration
                // children. Recurse through to process them.
                self.walk_block(stmt, stmt_range.start_byte);
                i += 1;
            } else if kind == "expression_statement" {
                // Rust wrapper: check if inner expression is if/loop
                let mut child_cursor = stmt.walk();
                let inner: Vec<Node> = stmt.named_children(&mut child_cursor).collect();
                let dispatched = if let Some(first) = inner.first() {
                    if first.kind() == "block"
                        && let Some(label) = self.embedded_control_label(*first)
                    {
                        let break_start = self.pending_break_node_ids.len();
                        self.walk_block(*first, first.start_byte() as u32);
                        self.resolve_labeled_breaks(&label, first.end_byte() as u32, break_start);
                        true
                    } else if self.config.if_kinds.contains(&first.kind()) {
                        self.walk_if_node(*first, stmt_range.start_byte);
                        true
                    } else if self.config.loop_kinds.contains(&first.kind()) {
                        self.walk_loop_node(*first, stmt_range.start_byte);
                        true
                    } else if self.config.switch_kinds.contains(&first.kind()) {
                        self.walk_switch_node(*first, stmt_range.start_byte);
                        true
                    } else if self.config.throw_kinds.contains(&first.kind()) {
                        self.emit_stmt(CfgNodeKind::Throw, stmt_range.start_byte, first);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !dispatched {
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                }
                i += 1;
            } else if kind == "try_with_resources_statement" || self.is_ruby() && kind == "begin" {
                i = self.walk_try(children, i, stmt_range.start_byte);
            } else if self.config.switch_kinds.contains(&kind) {
                // Switch/case sibling paths and supported fall-through.
                i = self.walk_switch(children, i, stmt_range.start_byte);
            } else if matches!(kind, "try_statement" | "try_expression" | "tryExpression") {
                i = self.walk_try(children, i, stmt_range.start_byte);
            } else if kind == "switch_statement" {
                // A `switch_statement` only reaches here when the active
                // language config has no `switch_kinds` entry.
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                i += 1;
            } else if kind == "preproc_if" || kind == "preproc_def" {
                // C/C++ preprocessor directives
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                i += 1;
            } else if kind == "using_statement" {
                // C# using: mark the resource declaration/expression and walk
                // either a block or a single-statement body.
                let checkpoint = self.checkpoint();
                let mut child_cursor = stmt.walk();
                let named: Vec<Node> = stmt.named_children(&mut child_cursor).collect();
                let body = stmt.child_by_field_name("body");

                // The one non-body named child is either a declaration or any
                // valid resource expression (for example `using (Open())`).
                for gc in &named {
                    if body.is_some_and(|body| body.id() == gc.id()) {
                        continue;
                    }
                    self.emit_managed_resource(gc, CallContext::CSharpUsing, stmt_range.start_byte);
                }
                if let Some(body) = body {
                    self.walk_branch_body(body);
                }
                if !self.finish_managed_scope(
                    stmt_range.start_byte,
                    stmt.end_byte() as u32,
                    CallContext::CSharpUsing,
                    checkpoint,
                    Some(DirectGotoRegion {
                        kind: DirectGotoRegionKind::Using,
                        node_id: stmt.id(),
                    }),
                ) {
                    self.rollback_to(checkpoint);
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                }

                i += 1;
                continue;
            } else if kind == "with_statement" {
                // Python with: mark the allocation clause and route every body
                // completion through an owner-bound BlockExit.
                let checkpoint = self.checkpoint();
                let mut child_cursor = stmt.walk();
                let named: Vec<Node> = stmt.named_children(&mut child_cursor).collect();

                // Walk with_clause children (e.g., open("file"))
                for gc in &named {
                    if gc.kind() == "with_clause" {
                        self.emit_managed_resource(
                            gc,
                            CallContext::PythonWith,
                            stmt_range.start_byte,
                        );
                    }
                }
                if let Some(body) = stmt.child_by_field_name("body") {
                    self.walk_branch_body(body);
                }
                if !self.finish_managed_scope(
                    stmt_range.start_byte,
                    stmt.end_byte() as u32,
                    CallContext::PythonWith,
                    checkpoint,
                    None,
                ) {
                    self.rollback_to(checkpoint);
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                }

                i += 1;
                continue;
            } else if kind == "go_statement" || kind == "defer_statement" {
                // Go goroutine/defer: set call context, process inner expression
                let context = if kind == "go_statement" {
                    CallContext::GoGoroutine
                } else {
                    CallContext::GoDefer
                };
                self.pending_call_context = context;
                let node_id = if let Some(inner) = self.find_first_expression(&stmt) {
                    self.process_go_defer_inner(&inner, stmt_range.start_byte)
                } else {
                    self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt)
                };
                if context == CallContext::GoDefer {
                    self.mark_managed_scope(node_id, stmt_range.start_byte);
                }
                i += 1;
                continue;
            } else {
                // Unknown constructs → treat as statement
                self.emit_stmt(CfgNodeKind::Statement, stmt_range.start_byte, &stmt);
                i += 1;
            }
        }
    }

    /// Lower Rust `let PATTERN = VALUE else { ... };` as a two-way pattern
    /// test. The successful match continues through the Join; the alternative
    /// is walked normally so an explicit return/break/continue remains abrupt.
    /// Rust requires the alternative to diverge, but keeping a normal tail for
    /// syntactically accepted incomplete code is the conservative fallback.
    fn walk_rust_let_else(&mut self, declaration: Node<'_>, start_byte: u32) {
        let Some(value) = declaration.child_by_field_name("value") else {
            self.emit_stmt(CfgNodeKind::Statement, start_byte, &declaration);
            return;
        };
        let Some(alternative) = declaration.child_by_field_name("alternative") else {
            self.emit_stmt(CfgNodeKind::Statement, start_byte, &declaration);
            return;
        };

        // Restrict the dispatch range to the evaluated value. Using the full
        // declaration would overlap calls in the else body and duplicate their
        // effects on the Branch node.
        let branch_id = self.add_node(CfgNodeKind::Branch, start_byte, Some(&value));
        if Self::contains_rust_try_expression(value) {
            self.terminal_node_ids
                .push((branch_id, CfgNodeKind::Return));
        }
        if let Some(previous) = self.prev_node_id.take() {
            self.add_edge(&previous, &branch_id, CfgEdgeKind::Normal);
        }

        let saved_edge_count = self.edges.len();
        self.prev_node_id = Some(branch_id);
        self.walk_branch_body(alternative);
        let alternative_tail = if self.edges.len() > saved_edge_count {
            self.retag_edge(saved_edge_count, CfgEdgeKind::FalseBranch);
            self.prev_node_id.take()
        } else {
            self.prev_node_id.take();
            Some(branch_id)
        };

        let join_id = self.add_node(CfgNodeKind::Join, start_byte.saturating_add(1), None);
        self.add_edge(&branch_id, &join_id, CfgEdgeKind::TrueBranch);
        if let Some(tail) = alternative_tail
            && tail != branch_id
        {
            self.add_edge(&tail, &join_id, CfgEdgeKind::Normal);
        } else if alternative_tail == Some(branch_id) {
            self.add_edge(&branch_id, &join_id, CfgEdgeKind::FalseBranch);
        }
        self.prev_node_id = Some(join_id);
    }

    /// Handle if/else: Branch → TrueBranch → cons → Join ← FalseBranch ← alt → Join
    /// Returns the index after the if_statement.
    fn walk_if(&mut self, children: &[Node], idx: usize, start_byte: u32) -> usize {
        let if_node = &children[idx];
        let consequence_kind =
            if self.is_ruby() && matches!(if_node.kind(), "unless" | "unless_modifier") {
                CfgEdgeKind::FalseBranch
            } else {
                CfgEdgeKind::TrueBranch
            };
        let alternative_kind = if consequence_kind == CfgEdgeKind::TrueBranch {
            CfgEdgeKind::FalseBranch
        } else {
            CfgEdgeKind::TrueBranch
        };

        // 1. Create Branch node, connect from previous
        let branch_id = self.add_node(CfgNodeKind::Branch, start_byte, Some(if_node));
        if let Some(prev) = self.prev_node_id.take() {
            self.add_edge(&prev, &branch_id, CfgEdgeKind::Normal);
        }

        // 2. Find consequence and alternative branches.
        let (cons_node, alt_node) = find_if_branches(*if_node, self.config.block_kinds);
        let alternative_clauses = find_if_alternative_clauses(*if_node);
        let has_sibling_conditionals = alternative_clauses
            .iter()
            .any(|node| matches!(node.kind(), "else_if_clause" | "elif_clause"));
        let mut direct_join_edges = Vec::new();

        // 3. Walk consequence body
        let cons_end = if let Some(cons) = cons_node {
            let saved_edge_count = self.edges.len();
            self.prev_node_id = Some(branch_id);
            self.walk_branch_body(cons);
            // Fix first edge: Branch→first node of consequence to TrueBranch
            if self.edges.len() > saved_edge_count {
                self.retag_edge(saved_edge_count, consequence_kind);
                self.prev_node_id.take()
            } else {
                // Empty consequence block still has a valid true path.
                direct_join_edges.push((branch_id, consequence_kind));
                self.prev_node_id.take();
                None
            }
        } else {
            direct_join_edges.push((branch_id, consequence_kind));
            None
        };

        // 4. PHP/Python expose `elseif`/`elif` as multiple sibling
        // alternatives on the original if node. Preserve the conditional
        // chain instead of collapsing it into one opaque false branch.
        let mut chained_alt_tails = Vec::new();
        let alt_end = if has_sibling_conditionals {
            let mut false_source = branch_id;
            let mut has_final_else = false;

            for alternative in &alternative_clauses {
                if matches!(alternative.kind(), "else_if_clause" | "elif_clause") {
                    let range = node_text_range(alternative, self.source);
                    let clause_branch =
                        self.add_node(CfgNodeKind::Branch, range.start_byte, Some(alternative));
                    self.add_edge(&false_source, &clause_branch, CfgEdgeKind::FalseBranch);

                    let saved_edge_count = self.edges.len();
                    self.prev_node_id = Some(clause_branch);
                    if let Some(body) = find_if_clause_body(*alternative, self.config.block_kinds) {
                        self.walk_branch_body(body);
                    }
                    if self.edges.len() > saved_edge_count {
                        self.retag_edge(saved_edge_count, CfgEdgeKind::TrueBranch);
                        if let Some(tail) = self.prev_node_id.take()
                            && tail != clause_branch
                        {
                            chained_alt_tails.push(tail);
                        }
                    } else {
                        direct_join_edges.push((clause_branch, CfgEdgeKind::TrueBranch));
                        self.prev_node_id.take();
                    }
                    false_source = clause_branch;
                } else if alternative.kind() == "else_clause" {
                    let saved_edge_count = self.edges.len();
                    self.prev_node_id = Some(false_source);
                    if let Some(body) = find_if_clause_body(*alternative, self.config.block_kinds) {
                        self.walk_branch_body(body);
                    }
                    if self.edges.len() > saved_edge_count {
                        self.retag_edge(saved_edge_count, CfgEdgeKind::FalseBranch);
                        if let Some(tail) = self.prev_node_id.take()
                            && tail != false_source
                        {
                            chained_alt_tails.push(tail);
                        }
                    } else {
                        direct_join_edges.push((false_source, CfgEdgeKind::FalseBranch));
                        self.prev_node_id.take();
                    }
                    has_final_else = true;
                    break;
                }
            }

            if !has_final_else {
                direct_join_edges.push((false_source, CfgEdgeKind::FalseBranch));
            }
            None
        } else if let Some(alt) = alt_node {
            let saved_edge_count = self.edges.len();
            self.prev_node_id = Some(branch_id);
            self.walk_branch_body(alt);
            // Fix first edge: Branch→first node of alternative to FalseBranch
            if self.edges.len() > saved_edge_count {
                self.retag_edge(saved_edge_count, alternative_kind);
                self.prev_node_id.take()
            } else {
                direct_join_edges.push((branch_id, alternative_kind));
                self.prev_node_id.take();
                None
            }
        } else {
            direct_join_edges.push((branch_id, alternative_kind));
            None
        };

        // 5. Create Join node and connect tails
        let join_id = self.add_node(CfgNodeKind::Join, start_byte + 1, None);

        // Connect consequence tail → Join (if branch didn't end with return/throw)
        if let Some(ref last) = cons_end
            && *last != branch_id
        {
            self.add_edge(last, &join_id, CfgEdgeKind::Normal);
        }
        // Connect alternative tail → Join
        if let Some(ref last) = alt_end
            && *last != branch_id
        {
            self.add_edge(last, &join_id, CfgEdgeKind::Normal);
        }
        for tail in &chained_alt_tails {
            self.add_edge(tail, &join_id, CfgEdgeKind::Normal);
        }
        for (source, kind) in &direct_join_edges {
            self.add_edge(source, &join_id, *kind);
        }

        self.prev_node_id = Some(join_id);
        idx + 1
    }

    /// Handle switch/case: Branch (dispatch) → CaseBranch → case body → Join,
    /// with case-tail → next-case edges where the language permits fall-through.
    ///
    /// C/C++/JS/TS/ArkTS/PHP and Java colon groups fall through implicitly;
    /// Java arrow rules and non-C sibling constructs do not. Go switch cases
    /// fall through only when they end in `fallthrough`; select communication
    /// cases never fall through. An unlabeled `break` is queued while walking
    /// the case: switch-like constructs consume it at their Join, while
    /// match-like constructs leave it for an enclosing loop or labeled block.
    ///
    /// Returns the index after the switch statement.
    fn walk_switch(&mut self, children: &[Node], idx: usize, start_byte: u32) -> usize {
        let switch_node = &children[idx];
        let is_go_select =
            self.language == Language::Go && switch_node.kind() == "select_statement";

        // 1. Create Branch (dispatch) node, connect from previous.
        let branch_id = self.add_node(CfgNodeKind::Branch, start_byte, Some(switch_node));
        if let Some(prev) = self.prev_node_id.take() {
            self.add_edge(&prev, &branch_id, CfgEdgeKind::Normal);
        }

        // 2. Find the case/default clauses. Go keeps cases as direct children of
        //    the switch node; C/Java/TS/C# nest them under a body container.
        let case_clauses = self.find_switch_cases(*switch_node);
        let break_start = self.pending_break_node_ids.len();
        let continue_start = self.pending_continue_node_ids.len();

        // 3. Walk every case from the dispatch and remember its entry/tail.
        //    Tail routing is deferred until all following case entries are known.
        let mut case_paths = Vec::with_capacity(case_clauses.len());
        for clause in &case_clauses {
            // Statement nodes belonging to this case clause (skip the case
            // label / pattern nodes, which are not executable statements).
            let body_stmts = self.case_body_statements(clause);
            let falls_through = self.case_falls_through(clause, &body_stmts);
            if body_stmts.is_empty() {
                case_paths.push((None, None, falls_through));
                continue;
            }

            let saved_edge_count = self.edges.len();
            self.prev_node_id = Some(branch_id);
            self.walk_stmt_list(&body_stmts);
            // Retag the first edge (Branch → first node of this case body) to
            // CaseBranch, matching how walk_if tags TrueBranch/FalseBranch.
            if self.edges.len() > saved_edge_count {
                self.retag_edge(saved_edge_count, CfgEdgeKind::CaseBranch);
                let entry = Some(self.edges[saved_edge_count].target);
                let tail = self.prev_node_id.take();
                case_paths.push((entry, tail, falls_through));
            } else {
                // Empty blocks (for example Java `case 1 -> {}` and Rust
                // `1 => {}`) are syntactically present but emit no CFG node.
                self.prev_node_id.take();
                case_paths.push((None, None, falls_through));
            }
        }

        // 4. Route each reachable tail or empty case either into the next
        //    executable case or out to the Join. Return/throw/break clear
        //    `prev_node_id`, so they do not also gain a fall-through edge here.
        let join_id = self.add_node(CfgNodeKind::Join, start_byte + 1, None);
        let mut direct_case_targets = Vec::new();
        for (idx, (entry, tail, falls_through)) in case_paths.iter().enumerate() {
            if let Some(tail) = tail {
                let target = if *falls_through {
                    case_paths[idx + 1..]
                        .iter()
                        .find_map(|(entry, _, _)| *entry)
                        .unwrap_or(join_id)
                } else {
                    join_id
                };
                self.add_edge(tail, &target, CfgEdgeKind::Normal);
            } else if entry.is_none() {
                let target = if *falls_through {
                    case_paths[idx + 1..]
                        .iter()
                        .find_map(|(entry, _, _)| *entry)
                        .unwrap_or(join_id)
                } else {
                    join_id
                };
                direct_case_targets.push(target);
            }
        }

        // Only language constructs with switch-style break ownership consume
        // pending breaks. Pattern/branch constructs in Python, Rust, Kotlin,
        // Cangjie, and Ruby leave them for an enclosing loop or labeled block.
        if self.switch_owns_break() {
            let pending_breaks = self.pending_break_node_ids.split_off(break_start);
            for (break_id, target) in pending_breaks {
                match target {
                    ControlTransferTarget::Depth(1) => {
                        self.add_edge(&break_id, &join_id, CfgEdgeKind::Break);
                    }
                    ControlTransferTarget::Depth(depth) => self
                        .pending_break_node_ids
                        .push((break_id, ControlTransferTarget::Depth(depth - 1))),
                    ControlTransferTarget::Label(label) => self
                        .pending_break_node_ids
                        .push((break_id, ControlTransferTarget::Label(label))),
                }
            }
        }
        // PHP counts a switch as one level for `continue N`; `continue 1`
        // leaves the switch, while deeper transfers continue resolving in the
        // enclosing loop. Other languages' continue targets skip switches.
        if self.language == Language::Php {
            let pending_continues = self.pending_continue_node_ids.split_off(continue_start);
            for (continue_id, target) in pending_continues {
                match target {
                    ControlTransferTarget::Depth(1) => {
                        self.add_edge(&continue_id, &join_id, CfgEdgeKind::Break);
                    }
                    ControlTransferTarget::Depth(depth) => self
                        .pending_continue_node_ids
                        .push((continue_id, ControlTransferTarget::Depth(depth - 1))),
                    ControlTransferTarget::Label(label) => self
                        .pending_continue_node_ids
                        .push((continue_id, ControlTransferTarget::Label(label))),
                }
            }
        }

        // A switch without default may match no case. A Go select without
        // default instead blocks until a communication can proceed, so it has
        // no synthetic skip path. Empty cases already contributed their direct
        // target above. Collapse identical targets because CfgEdgeId represents
        // one deterministic (source, target, kind) fact, not label identity.
        let has_default = case_clauses
            .iter()
            .any(|clause| self.is_default_case_clause(clause));
        if !is_go_select && !has_default {
            if self.is_ruby() && switch_node.kind() == "case_match" {
                // Ruby `case ... in` is exhaustive: unlike classic `case`, a
                // missing match without `else` raises NoMatchingPatternError.
                // Exact exception identity remains outside the CFG schema.
                let no_match_id = self.add_node(
                    CfgNodeKind::Throw,
                    switch_node.end_byte() as u32,
                    Some(switch_node),
                );
                self.terminal_node_ids
                    .push((no_match_id, CfgNodeKind::Throw));
                direct_case_targets.push(no_match_id);
            } else {
                direct_case_targets.push(join_id);
            }
        }
        for target in direct_case_targets {
            if !self.edges.iter().any(|edge| {
                edge.source == branch_id
                    && edge.target == target
                    && edge.kind == CfgEdgeKind::CaseBranch
            }) {
                self.add_edge(&branch_id, &target, CfgEdgeKind::CaseBranch);
            }
        }

        self.prev_node_id = (!is_go_select || !case_clauses.is_empty()).then_some(join_id);
        idx + 1
    }

    /// Handle common `try`/`catch`/`finally` shapes.
    ///
    /// The try dispatch is represented by a Branch. Normal execution enters
    /// the try body through a Normal edge; catch bodies are sibling Exception
    /// paths into a shared Join. Explicit Throw nodes inside the try also gain
    /// caught alternatives. Java/C#/PHP direct object-created throws stop at
    /// the first unguarded syntactically exact handler; unresolved cases retain
    /// every ordered handler alternative.
    ///
    /// A finally body is cloned per incoming continuation. Reusing one subgraph
    /// would create false crossovers (for example, return entering finally and
    /// leaving through the normal successor). Clones keep exact source ranges
    /// but receive deterministic lowering-instance IDs.
    fn walk_try(&mut self, children: &[Node], idx: usize, start_byte: u32) -> usize {
        let try_node = children[idx];
        let is_java_try_with_resources = try_node.kind() == "try_with_resources_statement";
        let is_ruby_try_region =
            self.is_ruby() && matches!(try_node.kind(), "body_statement" | "begin");
        let (try_body, catch_clauses, else_clause, finally_clause) =
            find_try_parts(try_node, self.config.block_kinds);

        if try_body.is_none()
            || (!is_java_try_with_resources
                && !is_ruby_try_region
                && catch_clauses.is_empty()
                && finally_clause.is_none())
        {
            self.emit_stmt(CfgNodeKind::Statement, start_byte, &try_node);
            return idx + 1;
        }

        if is_ruby_try_region
            && catch_clauses.is_empty()
            && else_clause.is_none()
            && finally_clause.is_none()
        {
            let try_body = try_body.expect("checked above");
            let mut cursor = try_body.walk();
            let body_children: Vec<_> = try_body.named_children(&mut cursor).collect();
            self.walk_stmt_list(&body_children);
            return idx + 1;
        }

        let checkpoint = self.checkpoint();
        let finally_body = finally_clause.and_then(|clause| {
            if is_ruby_try_region && clause.kind() == "ensure" {
                Some(clause)
            } else {
                find_if_clause_body(clause, self.config.block_kinds)
            }
        });
        let terminal_start = checkpoint.terminal_len;
        let break_start = checkpoint.break_len;
        let continue_start = checkpoint.continue_len;
        let redo_start = checkpoint.redo_len;
        let retry_start = checkpoint.retry_len;
        let goto_start = checkpoint.goto_len;
        let goto_region = (matches!(self.language, Language::CSharp | Language::Php)
            && finally_clause.is_some())
        .then_some(DirectGotoRegion {
            kind: DirectGotoRegionKind::TryFinally,
            node_id: try_node.id(),
        });

        let dispatch_id = if catch_clauses.is_empty() {
            None
        } else {
            let dispatch_id = self.add_node(CfgNodeKind::Branch, start_byte, Some(&try_node));
            if let Some(prev) = self.prev_node_id.take() {
                self.add_edge(&prev, &dispatch_id, CfgEdgeKind::Normal);
            }
            Some(dispatch_id)
        };

        let mut direct_join_edges = Vec::new();

        if let Some(dispatch_id) = dispatch_id {
            self.prev_node_id = Some(dispatch_id);
        }
        let saved_edge_count = self.edges.len();
        let try_body = try_body.expect("checked above");
        if is_java_try_with_resources {
            // Resource acquisition is inside the catchable try region, while
            // every body completion must run the implicit close protocol
            // before it can continue to an outer catch/finally continuation.
            if let Some(resources) = try_node.child_by_field_name("resources") {
                let mut resource_cursor = resources.walk();
                let resource_nodes: Vec<_> =
                    resources.named_children(&mut resource_cursor).collect();
                if resource_nodes.is_empty() {
                    self.emit_managed_resource(&resources, CallContext::JavaTryWith, start_byte);
                } else {
                    for resource in resource_nodes {
                        self.emit_managed_resource(&resource, CallContext::JavaTryWith, start_byte);
                    }
                }
            }
        }
        if is_ruby_try_region {
            let mut cursor = try_body.walk();
            let body_children: Vec<_> = try_body
                .named_children(&mut cursor)
                .take_while(|child| !matches!(child.kind(), "rescue" | "else" | "ensure"))
                .collect();
            self.walk_stmt_list(&body_children);
        } else {
            self.walk_branch_body(try_body);
        }
        if is_java_try_with_resources
            && !self.finish_managed_scope(
                start_byte,
                try_body.end_byte() as u32,
                CallContext::JavaTryWith,
                checkpoint,
                None,
            )
        {
            self.rollback_to(checkpoint);
            self.emit_stmt(CfgNodeKind::Statement, start_byte, &try_node);
            return idx + 1;
        }
        let mut normal_tail = if self.edges.len() > saved_edge_count {
            self.prev_node_id.take()
        } else {
            let tail = self.prev_node_id.take();
            if let Some(dispatch_id) = dispatch_id {
                direct_join_edges.push((dispatch_id, CfgEdgeKind::Normal));
                None
            } else {
                tail
            }
        };

        if is_java_try_with_resources && catch_clauses.is_empty() && finally_clause.is_none() {
            self.prev_node_id = normal_tail;
            return idx + 1;
        }

        // Only throws originating in the try body are candidates for these
        // handlers. Python else-clause throws, and throws from catch bodies,
        // propagate outside this try.
        let try_throw_ids: Vec<_> = self.terminal_node_ids[terminal_start..]
            .iter()
            .filter_map(|(node_id, kind)| (*kind == CfgNodeKind::Throw).then_some(*node_id))
            .collect();

        // Python's try/except/else shape executes `else` only on the normal
        // try path. Other common grammars do not expose a direct else clause.
        if let (Some(tail), Some(clause)) = (normal_tail, else_clause)
            && let Some(body) = if is_ruby_try_region && clause.kind() == "else" {
                Some(clause)
            } else {
                find_if_clause_body(clause, self.config.block_kinds)
            }
        {
            self.prev_node_id = Some(tail);
            self.walk_branch_body(body);
            normal_tail = self.prev_node_id.take();
        }

        let mut catch_tails = Vec::new();
        let mut catch_paths = Vec::new();
        let mut owned_retries = Vec::new();
        let mut has_empty_catch = false;
        for clause in &catch_clauses {
            let dispatch_id = dispatch_id.expect("catch clauses require dispatch");
            let saved_edge_count = self.edges.len();
            let catch_retry_start = self.pending_retry_node_ids.len();
            self.prev_node_id = Some(dispatch_id);
            let catch_body = if is_ruby_try_region && clause.kind() == "rescue" {
                clause.child_by_field_name("body")
            } else {
                find_if_clause_body(*clause, self.config.block_kinds)
            };
            if let Some(body) = catch_body {
                self.walk_branch_body(body);
            }
            owned_retries.extend(self.pending_retry_node_ids.split_off(catch_retry_start));

            if self.edges.len() > saved_edge_count {
                self.retag_edge(saved_edge_count, CfgEdgeKind::Exception);
                catch_paths.push((*clause, Some(self.edges[saved_edge_count].target)));
                if let Some(tail) = self.prev_node_id.take()
                    && tail != dispatch_id
                {
                    catch_tails.push(tail);
                }
            } else {
                catch_paths.push((*clause, None));
                has_empty_catch = true;
                self.prev_node_id.take();
            }
        }
        if let Some(dispatch_id) = dispatch_id {
            for retry_id in owned_retries {
                self.add_edge(&retry_id, &dispatch_id, CfgEdgeKind::Retry);
            }
        }

        // Keep the existing Throw→Exit edge as the uncaught path and add caught
        // alternatives here. Java/C#/PHP object creation gives a syntactically
        // proven thrown type. Once the ordered handler list reaches the first
        // unguarded exact match, later handlers are unreachable for that throw.
        // Earlier different types remain conservative alternatives because CFG
        // extraction has no inheritance graph. Ambiguous constructor-like calls
        // in Python/Kotlin/C++/Ruby intentionally retain every handler.
        for throw_id in &try_throw_ids {
            let thrown_type = self
                .nodes
                .iter()
                .find(|node| node.id == *throw_id && node.kind == CfgNodeKind::Throw)
                .and_then(|node| {
                    find_node_by_exact_range(
                        try_body,
                        node.stmt_range.start_byte,
                        node.stmt_range.end_byte,
                    )
                })
                .and_then(|node| explicit_object_creation_type(self.language, node, self.source));
            let mut selected_empty_catch = false;
            for (clause, target) in &catch_paths {
                if let Some(target) = target {
                    self.add_edge(throw_id, target, CfgEdgeKind::Exception);
                } else {
                    selected_empty_catch = true;
                }
                if thrown_type.as_deref().is_some_and(|thrown_type| {
                    handler_guarantees_exact_type(self.language, *clause, thrown_type, self.source)
                }) {
                    break;
                }
            }
            if selected_empty_catch {
                direct_join_edges.push((*throw_id, CfgEdgeKind::Exception));
            }
        }

        if has_empty_catch {
            direct_join_edges.push((
                dispatch_id.expect("empty catch requires dispatch"),
                CfgEdgeKind::Exception,
            ));
        }

        let normal_path_count =
            usize::from(normal_tail.is_some_and(|tail| dispatch_id != Some(tail)));
        let finally_clone_count = normal_path_count
            + catch_tails.len()
            + direct_join_edges.len()
            + (self.terminal_node_ids.len() - terminal_start)
            + (self.pending_break_node_ids.len() - break_start)
            + (self.pending_continue_node_ids.len() - continue_start)
            + (self.pending_redo_node_ids.len() - redo_start)
            + (self.pending_retry_node_ids.len() - retry_start)
            + goto_region.map_or(0, |region| {
                self.pending_goto_node_ids[goto_start..]
                    .iter()
                    .filter(|pending| self.direct_goto_exits_region(pending, region))
                    .count()
            });
        if finally_body.is_some() && finally_clone_count > MAX_PATH_ISOLATED_CLONES_PER_REGION {
            self.rollback_to(checkpoint);
            self.emit_stmt(CfgNodeKind::Statement, start_byte, &try_node);
            return idx + 1;
        }

        let join_id = self.add_node(CfgNodeKind::Join, start_byte + 1, None);

        if finally_clause.is_some() {
            let pending_terminals = self.terminal_node_ids.split_off(terminal_start);
            let pending_breaks = self.pending_break_node_ids.split_off(break_start);
            let pending_continues = self.pending_continue_node_ids.split_off(continue_start);
            let pending_redos = self.pending_redo_node_ids.split_off(redo_start);
            let pending_retries = self.pending_retry_node_ids.split_off(retry_start);
            let pending_gotos = self.pending_goto_node_ids.split_off(goto_start);

            if let Some(tail) = normal_tail
                && dispatch_id != Some(tail)
            {
                self.connect_path_through_finally(tail, CfgEdgeKind::Normal, finally_body, join_id);
            }
            for tail in catch_tails {
                self.connect_path_through_finally(tail, CfgEdgeKind::Normal, finally_body, join_id);
            }
            for (source, kind) in direct_join_edges {
                self.connect_path_through_finally(source, kind, finally_body, join_id);
            }

            for (terminal_id, terminal_kind) in pending_terminals {
                if let Some(tail) = self.walk_finally_clone(terminal_id, finally_body) {
                    self.terminal_node_ids.push((tail, terminal_kind));
                }
            }
            for (break_id, target) in pending_breaks {
                if let Some(tail) = self.walk_finally_clone(break_id, finally_body) {
                    self.pending_break_node_ids.push((tail, target));
                }
            }
            for (continue_id, target) in pending_continues {
                if let Some(tail) = self.walk_finally_clone(continue_id, finally_body) {
                    self.pending_continue_node_ids.push((tail, target));
                }
            }
            for redo_id in pending_redos {
                if let Some(tail) = self.walk_finally_clone(redo_id, finally_body) {
                    self.pending_redo_node_ids.push(tail);
                }
            }
            for retry_id in pending_retries {
                if let Some(tail) = self.walk_finally_clone(retry_id, finally_body) {
                    self.pending_retry_node_ids.push(tail);
                }
            }
            for mut pending in pending_gotos {
                if goto_region.is_some_and(|region| self.direct_goto_exits_region(&pending, region))
                {
                    let Some(tail) = self.walk_finally_clone(pending.source, finally_body) else {
                        continue;
                    };
                    pending.source = tail;
                }
                self.pending_goto_node_ids.push(pending);
            }
        } else {
            if let Some(tail) = normal_tail
                && dispatch_id != Some(tail)
            {
                self.add_edge(&tail, &join_id, CfgEdgeKind::Normal);
            }
            for tail in &catch_tails {
                self.add_edge(tail, &join_id, CfgEdgeKind::Normal);
            }
            for (source, kind) in &direct_join_edges {
                self.add_edge(source, &join_id, *kind);
            }
        }

        self.prev_node_id = Some(join_id);
        idx + 1
    }

    /// Lower one finally invocation with a fresh node-identity instance.
    /// Abrupt completion inside the finally body leaves `prev_node_id` empty,
    /// which suppresses the incoming continuation as required by the language.
    fn walk_finally_clone(
        &mut self,
        source: types::ids::CfgNodeId,
        finally_body: Option<Node<'_>>,
    ) -> Option<types::ids::CfgNodeId> {
        let Some(finally_body) = finally_body else {
            return Some(source);
        };

        let saved_prev = self.prev_node_id.take();
        let saved_instance = self.node_instance;
        let instance = self.next_node_instance;
        self.next_node_instance = self.next_node_instance.saturating_add(1);
        self.node_instance = instance;
        self.prev_node_id = Some(source);
        self.walk_branch_body(finally_body);
        let tail = self.prev_node_id.take();
        self.node_instance = saved_instance;
        self.prev_node_id = saved_prev;
        tail
    }

    fn connect_path_through_finally(
        &mut self,
        source: types::ids::CfgNodeId,
        entry_kind: CfgEdgeKind,
        finally_body: Option<Node<'_>>,
        target: types::ids::CfgNodeId,
    ) {
        let edge_start = self.edges.len();
        if let Some(tail) = self.walk_finally_clone(source, finally_body) {
            if self.edges.len() > edge_start {
                self.edges[edge_start].kind = entry_kind;
                self.add_edge(&tail, &target, CfgEdgeKind::Normal);
            } else {
                self.add_edge(&tail, &target, entry_kind);
            }
        }
    }

    /// Collect the case/default clause nodes of a switch statement.
    ///
    /// Handles both layouts observed across grammars:
    /// - Nested: `switch → body-container → case_clauses` (C/C++, Java, TS/JS, C#).
    /// - Flat: `switch → case_clauses` as direct children (Go).
    fn find_switch_cases<'a>(&self, switch_node: Node<'a>) -> Vec<Node<'a>> {
        // First, look for case clauses directly under the switch node (Go).
        let mut direct: Vec<Node> = Vec::new();
        let mut cursor = switch_node.walk();
        for child in switch_node.named_children(&mut cursor) {
            if self.config.case_kinds.contains(&child.kind()) {
                direct.push(child);
            }
        }
        if !direct.is_empty() {
            return direct;
        }

        // Otherwise, descend into the switch body container and collect the
        // case clauses nested inside it (C/Java/TS/C#).
        let mut cases: Vec<Node> = Vec::new();
        let mut cursor = switch_node.walk();
        for child in switch_node.named_children(&mut cursor) {
            // The body container is any child that itself contains case clauses.
            let mut inner_cursor = child.walk();
            let inner_cases: Vec<Node> = child
                .named_children(&mut inner_cursor)
                .filter(|c| self.config.case_kinds.contains(&c.kind()))
                .collect();
            if !inner_cases.is_empty() {
                cases.extend(inner_cases);
            }
        }
        cases
    }

    /// Return the executable statement nodes inside a case/default clause,
    /// skipping the case label / pattern children.
    ///
    /// Grammars differ in how case bodies are structured:
    /// - C/C++/Java/C#: statements are direct children of the case node, after
    ///   the label/pattern (e.g. `case_statement`'s `value`, Java `switch_label`).
    /// - Go: the case wraps its body in a `statement_list` child.
    /// - TS/JS: statements sit in the case node's `body` fields.
    /// - Cangjie/Rust/Kotlin arrow arms: everything before `=>`/`->` is
    ///   label/pattern/guard context; executable body nodes start afterwards.
    ///
    /// The shared `walk_stmt_list` dispatch already recurses through
    /// `statement_list` wrappers, so Go's container is returned as-is.
    ///
    /// Case labels/patterns/values are identified in two complementary ways so
    /// the walk stays general across grammars:
    /// - by tree-sitter field name (`value`/`type`/`alias`/`pattern`/`guard`/
    ///   `condition`) — used by C/C++, TS/JS, Go;
    /// - by node kind (`switch_label`, `*_pattern`, `when_clause`, …) — used by
    ///   Java and C# whose label nodes carry no field name.
    fn case_body_statements<'a>(&self, clause: &Node<'a>) -> Vec<Node<'a>> {
        let mut cursor = clause.walk();
        let mut stmts = Vec::new();
        let is_arrow_arm = matches!(
            clause.kind(),
            "matchCase" | "matchCaseBody" | "match_arm" | "when_entry"
        );
        let mut in_arrow_body = !is_arrow_arm;
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if is_arrow_arm && matches!(child.kind(), "=>" | "->") {
                    in_arrow_body = true;
                }
                if child.is_named() {
                    let include = if is_arrow_arm {
                        in_arrow_body
                    } else {
                        let field = cursor.field_name().unwrap_or("");
                        !is_case_label_field(field) && !is_case_label_kind(child.kind())
                    };
                    if include {
                        stmts.push(child);
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        stmts
    }

    fn is_break_statement(&self, node: &Node) -> bool {
        match node.kind() {
            "break_statement" | "break_expression" | "break" | "yield_statement" => true,
            "jump_expression" | "jumpExpression" => node
                .utf8_text(self.source)
                .is_ok_and(|text| text.trim_start().starts_with("break")),
            _ => false,
        }
    }

    fn is_continue_statement(&self, node: &Node) -> bool {
        match node.kind() {
            "continue_statement" | "continue_expression" | "next" => true,
            "jump_expression" | "jumpExpression" => node
                .utf8_text(self.source)
                .is_ok_and(|text| text.trim_start().starts_with("continue")),
            _ => false,
        }
    }

    fn is_throw_statement(&self, node: &Node) -> bool {
        self.config.throw_kinds.contains(&node.kind())
            || self.is_rust_builtin_diverging_macro(node)
            || matches!(node.kind(), "jump_expression" | "jumpExpression")
                && node
                    .utf8_text(self.source)
                    .is_ok_and(|text| text.trim_start().starts_with("throw"))
            || self.is_ruby()
                && node.kind() == "call"
                && node.utf8_text(self.source).is_ok_and(|text| {
                    let text = text.trim_start();
                    ["raise", "fail"].iter().any(|keyword| {
                        text == *keyword
                            || text.strip_prefix(keyword).is_some_and(|rest| {
                                rest.chars()
                                    .next()
                                    .is_some_and(|ch| ch.is_whitespace() || ch == '(')
                            })
                    })
                })
    }

    /// Rust's standard panic-like macros have the never type and terminate the
    /// local path. This is deliberately name-based and limited to unqualified
    /// prelude spellings; resolving a shadowing or re-exported macro requires a
    /// Rust macro resolver and remains outside the tree-sitter CFG boundary.
    fn is_rust_builtin_diverging_macro(&self, node: &Node<'_>) -> bool {
        if self.language != Language::Rust || node.kind() != "macro_invocation" {
            return false;
        }
        node.child_by_field_name("macro")
            .and_then(|name| name.utf8_text(self.source).ok())
            .is_some_and(|name| matches!(name, "panic" | "unreachable" | "todo" | "unimplemented"))
    }

    /// Resolve PHP numeric nesting and the source-level labels supported by
    /// Java, JS/TS/ArkTS, Go, Rust, and Kotlin. Labels remain pending across
    /// nested controls and cleanup clones until their lexical owner resolves
    /// them; unsupported value-bearing `break` forms remain depth one.
    fn control_transfer_target(&self, node: &Node, keyword: &str) -> Option<ControlTransferTarget> {
        if node.kind() == "yield_statement" {
            return Some(ControlTransferTarget::Depth(1));
        }

        let keyword = if node.kind() == "next" {
            "next"
        } else {
            keyword
        };
        let text = node.utf8_text(self.source).ok()?.trim();
        let tail = text
            .strip_prefix(keyword)?
            .trim()
            .trim_end_matches(';')
            .trim();

        if self.language == Language::Php {
            return if tail.is_empty() {
                Some(ControlTransferTarget::Depth(1))
            } else {
                tail.parse::<u32>()
                    .ok()
                    .filter(|depth| *depth > 0)
                    .map(ControlTransferTarget::Depth)
            };
        }

        let label = match self.language {
            Language::TypeScript | Language::JavaScript | Language::ArkTS | Language::Java => {
                (!tail.is_empty()).then_some(tail)
            }
            Language::Go | Language::Rust => {
                let mut cursor = node.walk();
                let label = node.child_by_field_name("label").or_else(|| {
                    node.named_children(&mut cursor)
                        .find(|child| matches!(child.kind(), "label" | "label_name"))
                });
                label.and_then(|label| label.utf8_text(self.source).ok())
            }
            Language::Kotlin => tail.strip_prefix('@'),
            _ => None,
        };
        Some(match label {
            Some(label) if !label.trim().is_empty() => ControlTransferTarget::Label(
                label
                    .trim()
                    .trim_start_matches('\'')
                    .trim_start_matches('@')
                    .to_string(),
            ),
            _ => ControlTransferTarget::Depth(1),
        })
    }

    fn case_falls_through(&self, clause: &Node, body_stmts: &[Node]) -> bool {
        match self.language {
            Language::TypeScript
            | Language::JavaScript
            | Language::ArkTS
            | Language::C
            | Language::Cpp
            | Language::Php => true,
            Language::Java => clause.kind() == "switch_block_statement_group",
            Language::Go => {
                clause.kind() != "communication_case"
                    && last_named_descendant(body_stmts)
                        .is_some_and(|node| node.kind() == "fallthrough_statement")
            }
            _ => false,
        }
    }

    fn switch_owns_break(&self) -> bool {
        matches!(
            self.language,
            Language::TypeScript
                | Language::JavaScript
                | Language::ArkTS
                | Language::Java
                | Language::C
                | Language::Cpp
                | Language::Go
                | Language::CSharp
                | Language::Php
        )
    }

    fn is_default_case_clause(&self, clause: &Node) -> bool {
        // Treat only syntactically proven catch-all arms as defaults. Python
        // also has syntax-only irrefutable capture/as/group/OR patterns. Rust
        // and Cangjie remain limited to direct unguarded wildcards until name
        // and pattern semantics are resolved.
        match clause.kind() {
            "switch_default" | "default_statement" | "default_case" | "else" => true,
            "case_clause" if self.language == Language::Python => {
                if clause.child_by_field_name("guard").is_some() {
                    return false;
                }
                let mut child_cursor = clause.walk();
                if clause
                    .children(&mut child_cursor)
                    .any(|child| child.kind() == ",")
                {
                    // `case value,:` is a one-element sequence pattern, not
                    // an irrefutable capture despite having one named child.
                    return false;
                }
                let mut cursor = clause.walk();
                let patterns: Vec<_> = clause
                    .named_children(&mut cursor)
                    .filter(|child| child.kind() == "case_pattern")
                    .collect();
                patterns.len() == 1 && self.python_pattern_is_irrefutable(patterns[0])
            }
            "match_arm" if self.language == Language::Rust => clause
                .child_by_field_name("pattern")
                .and_then(|pattern| pattern.utf8_text(self.source).ok())
                .is_some_and(|text| text.trim() == "_"),
            "matchCase" if self.language == Language::Cangjie => {
                let mut cursor = clause.walk();
                let children: Vec<_> = clause.named_children(&mut cursor).collect();
                !children.iter().any(|child| child.kind() == "patternGuard")
                    && children
                        .iter()
                        .any(|child| child.kind() == "wildcardPattern")
            }
            "matchCaseBody" if self.language == Language::Cangjie => clause
                .utf8_text(self.source)
                .ok()
                .and_then(|text| text.split_once("=>"))
                .and_then(|(label, _)| label.trim().strip_prefix("case"))
                .is_some_and(|pattern| pattern.trim() == "_"),
            "in_clause" if self.language == Language::Ruby => {
                clause.child_by_field_name("guard").is_none()
                    && clause
                        .child_by_field_name("pattern")
                        .is_some_and(Self::ruby_pattern_is_irrefutable)
            }
            "case_statement" if matches!(self.language, Language::C | Language::Cpp) => {
                clause.child_by_field_name("value").is_none()
            }
            "switch_block_statement_group" | "switch_rule" | "switch_section" => clause
                .utf8_text(self.source)
                .is_ok_and(|text| text.trim_start().starts_with("default")),
            "when_entry" => clause
                .utf8_text(self.source)
                .is_ok_and(|text| text.trim_start().starts_with("else")),
            _ => false,
        }
    }

    /// Ruby's unpinned local-variable pattern always captures and therefore
    /// matches any subject. Parenthesized/as/alternative forms inherit that
    /// property from their contained pattern. Structural and pinned patterns
    /// remain refutable without runtime type/value knowledge.
    fn ruby_pattern_is_irrefutable(pattern: Node<'_>) -> bool {
        match pattern.kind() {
            "identifier" => true,
            "as_pattern" => pattern
                .child_by_field_name("value")
                .is_some_and(Self::ruby_pattern_is_irrefutable),
            "parenthesized_pattern" => pattern
                .named_child(0)
                .is_some_and(Self::ruby_pattern_is_irrefutable),
            "alternative_pattern" => {
                let mut cursor = pattern.walk();
                pattern
                    .named_children(&mut cursor)
                    .any(Self::ruby_pattern_is_irrefutable)
            }
            _ => false,
        }
    }

    /// Python makes a syntactic distinction that is safe to exploit without
    /// name or type resolution: a single-segment dotted name in a case pattern
    /// is always a capture, while `Color.RED` is a value pattern. Irrefutability
    /// propagates through `as`, grouping parentheses, and an OR alternative.
    /// Structural sequence/mapping/class patterns remain refutable.
    fn python_pattern_is_irrefutable(&self, pattern: Node<'_>) -> bool {
        if pattern
            .utf8_text(self.source)
            .is_ok_and(|text| text.trim() == "_")
        {
            return true;
        }

        match pattern.kind() {
            "case_pattern" | "as_pattern" => pattern
                .named_child(0)
                .is_some_and(|child| self.python_pattern_is_irrefutable(child)),
            "dotted_name" => pattern.named_child_count() == 1,
            "union_pattern" => {
                let mut cursor = pattern.walk();
                pattern.children(&mut cursor).any(|child| {
                    child.kind() == "_"
                        || child.is_named() && self.python_pattern_is_irrefutable(child)
                })
            }
            "tuple_pattern" => {
                let mut child_cursor = pattern.walk();
                let has_direct_comma = pattern
                    .children(&mut child_cursor)
                    .any(|child| child.kind() == ",");
                !has_direct_comma
                    && pattern.named_child_count() == 1
                    && pattern
                        .named_child(0)
                        .is_some_and(|child| self.python_pattern_is_irrefutable(child))
            }
            _ => false,
        }
    }

    /// Walk a single branch body (consequence or alternative).
    /// If the node is a block, walk its children; otherwise emit as statement.
    fn walk_branch_body(&mut self, node: Node) {
        if self.config.block_kinds.contains(&node.kind())
            || self.is_ruby() && matches!(node.kind(), "else" | "ensure")
        {
            let range = node_text_range(&node, self.source);
            self.walk_block(node, range.start_byte);
        } else if self.config.if_kinds.contains(&node.kind()) {
            let range = node_text_range(&node, self.source);
            self.walk_if_node(node, range.start_byte);
        } else if self.config.loop_kinds.contains(&node.kind()) {
            let range = node_text_range(&node, self.source);
            self.walk_loop_node(node, range.start_byte);
        } else if self.config.switch_kinds.contains(&node.kind()) {
            let range = node_text_range(&node, self.source);
            self.walk_switch_node(node, range.start_byte);
        } else {
            // Single-statement body (e.g., `if (x) return 1;`)
            self.walk_single_statement_body(node);
        }
    }

    /// Classify an unwrapped single-statement control body through the same
    /// abrupt-transfer rules as [`Self::walk_stmt_list`].
    fn walk_single_statement_body(&mut self, node: Node<'_>) {
        self.walk_stmt_list(&[node]);
    }

    fn walk_labeled_statement(&mut self, node: Node<'_>, start_byte: u32) {
        let direct_goto_label = self.direct_goto_label(node);
        let node_start = self.nodes.len();
        let Some((label, body)) = self.labeled_statement_parts(node) else {
            self.emit_stmt(CfgNodeKind::Statement, start_byte, &node);
            self.record_direct_goto_label(direct_goto_label, node, node_start);
            return;
        };

        if self.config.loop_kinds.contains(&body.kind()) {
            self.walk_loop_with_labels(&[body], 0, start_byte, &[label]);
            self.record_direct_goto_label(direct_goto_label, node, node_start);
            return;
        }

        let break_start = self.pending_break_node_ids.len();
        self.walk_stmt_list(&[body]);
        self.resolve_labeled_breaks(&label, node.end_byte() as u32, break_start);
        self.record_direct_goto_label(direct_goto_label, node, node_start);
    }

    fn direct_goto_target(&self, node: Node<'_>) -> Option<String> {
        if !Self::supports_direct_goto(self.language) || node.kind() != "goto_statement" {
            return None;
        }
        let mut cursor = node.walk();
        let target = node.child_by_field_name("label").or_else(|| {
            node.named_children(&mut cursor)
                .find(|child| self.is_direct_goto_label_node(*child))
        })?;
        target
            .utf8_text(self.source)
            .ok()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_string)
    }

    fn direct_goto_label(&self, node: Node<'_>) -> Option<String> {
        let is_label = node.kind() == "labeled_statement"
            || (self.language == Language::Php && node.kind() == "named_label_statement");
        if !Self::supports_direct_goto(self.language) || !is_label {
            return None;
        }
        let mut cursor = node.walk();
        let label = node.child_by_field_name("label").or_else(|| {
            node.named_children(&mut cursor)
                .find(|child| self.is_direct_goto_label_node(*child))
        })?;
        label
            .utf8_text(self.source)
            .ok()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_string)
    }

    fn is_direct_goto_label_node(&self, node: Node<'_>) -> bool {
        matches!(node.kind(), "statement_identifier" | "label_name")
            || (self.language == Language::CSharp && node.kind() == "identifier")
            || (self.language == Language::Php && node.kind() == "name")
    }

    fn collect_direct_goto_label_regions(&mut self, root: Node<'_>) {
        if !matches!(self.language, Language::CSharp | Language::Php)
            || !self.can_resolve_direct_goto
        {
            return;
        }
        let mut pending = Vec::new();
        let mut cursor = root.walk();
        pending.extend(root.named_children(&mut cursor));
        while let Some(node) = pending.pop() {
            if matches!(node.kind(), "labeled_statement" | "named_label_statement")
                && let Some(label) = self.direct_goto_label(node)
            {
                let regions = self.direct_goto_regions(node);
                self.direct_goto_label_regions
                    .entry(label)
                    .or_default()
                    .push(regions);
            }
            if matches!(
                node.kind(),
                "function_definition"
                    | "local_function_statement"
                    | "lambda_expression"
                    | "anonymous_method_expression"
                    | "anonymous_function_creation_expression"
                    | "arrow_function"
            ) {
                continue;
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
    }

    fn direct_goto_regions(&self, node: Node<'_>) -> Vec<DirectGotoRegion> {
        if !matches!(self.language, Language::CSharp | Language::Php) {
            return Vec::new();
        }
        let mut regions = Vec::new();
        let mut ancestor = node.parent();
        while let Some(current) = ancestor {
            let kind = match self.language {
                Language::CSharp => match current.kind() {
                    "block" => Some(DirectGotoRegionKind::LexicalBlock),
                    "using_statement" => Some(DirectGotoRegionKind::Using),
                    "finally_clause" => Some(DirectGotoRegionKind::FinallyClause),
                    "try_statement" if Self::try_has_finally(current) => {
                        Some(DirectGotoRegionKind::TryFinally)
                    }
                    _ => None,
                },
                Language::Php => {
                    if self.config.loop_kinds.contains(&current.kind())
                        || self.config.switch_kinds.contains(&current.kind())
                    {
                        Some(DirectGotoRegionKind::LoopOrSwitch)
                    } else {
                        match current.kind() {
                            "finally_clause" => Some(DirectGotoRegionKind::FinallyClause),
                            "try_statement" if Self::try_has_finally(current) => {
                                Some(DirectGotoRegionKind::TryFinally)
                            }
                            _ => None,
                        }
                    }
                }
                _ => None,
            };
            if let Some(kind) = kind {
                regions.push(DirectGotoRegion {
                    kind,
                    node_id: current.id(),
                });
            }
            ancestor = current.parent();
        }
        regions
    }

    fn try_has_finally(node: Node<'_>) -> bool {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .any(|child| child.kind() == "finally_clause")
    }

    fn direct_goto_target_is_legal(
        &self,
        source_regions: &[DirectGotoRegion],
        target_regions: &[DirectGotoRegion],
    ) -> bool {
        match self.language {
            Language::CSharp => {
                target_regions
                    .iter()
                    .all(|region| source_regions.contains(region))
                    && source_regions.iter().all(|region| {
                        region.kind != DirectGotoRegionKind::FinallyClause
                            || target_regions.contains(region)
                    })
            }
            Language::Php => {
                let target_entries_are_legal = target_regions.iter().all(|region| {
                    region.kind != DirectGotoRegionKind::LoopOrSwitch
                        || source_regions.contains(region)
                });
                let same_finally_clauses = source_regions
                    .iter()
                    .filter(|region| region.kind == DirectGotoRegionKind::FinallyClause)
                    .eq(target_regions
                        .iter()
                        .filter(|region| region.kind == DirectGotoRegionKind::FinallyClause));
                target_entries_are_legal && same_finally_clauses
            }
            _ => true,
        }
    }

    fn direct_goto_exits_region(
        &self,
        pending: &PendingDirectGoto,
        region: DirectGotoRegion,
    ) -> bool {
        pending.source_regions.contains(&region)
            && self
                .direct_goto_label_regions
                .get(&pending.label)
                .is_some_and(|targets| {
                    !targets.is_empty()
                        && targets
                            .iter()
                            .all(|target_regions| !target_regions.contains(&region))
                })
    }

    fn record_direct_goto_label(
        &mut self,
        label: Option<String>,
        node: Node<'_>,
        node_start: usize,
    ) {
        let Some(label) = label else {
            return;
        };
        if let Some(target) = self.nodes.get(node_start).map(|entry| entry.id) {
            self.goto_label_targets.push(DirectGotoLabelTarget {
                label,
                target,
                target_regions: self.direct_goto_regions(node),
                instance: self.node_instance,
            });
        }
    }

    fn resolve_direct_gotos(&mut self) {
        let pending = std::mem::take(&mut self.pending_goto_node_ids);
        for pending in pending {
            if let Some(target) = self
                .goto_label_targets
                .iter()
                .find(|candidate| {
                    candidate.label == pending.label
                        && candidate.instance == pending.target_instance
                        && self.direct_goto_target_is_legal(
                            &pending.source_regions,
                            &candidate.target_regions,
                        )
                })
                .map(|candidate| candidate.target)
            {
                self.add_edge(&pending.source, &target, CfgEdgeKind::Goto);
            }
        }
    }

    fn resolve_labeled_breaks(&mut self, label: &str, join_byte: u32, break_start: usize) {
        let pending_breaks = self.pending_break_node_ids.split_off(break_start);
        let mut matching_breaks = Vec::new();
        for (break_id, target) in pending_breaks {
            match target {
                ControlTransferTarget::Label(target) if target == label => {
                    matching_breaks.push(break_id);
                }
                target => self.pending_break_node_ids.push((break_id, target)),
            }
        }

        if matching_breaks.is_empty() {
            return;
        }

        let normal_tail = self.prev_node_id.take();
        let join_id = self.add_node(CfgNodeKind::Join, join_byte, None);
        if let Some(normal_tail) = normal_tail {
            self.add_edge(&normal_tail, &join_id, CfgEdgeKind::Normal);
        }
        for break_id in matching_breaks {
            self.add_edge(&break_id, &join_id, CfgEdgeKind::Break);
        }
        self.prev_node_id = Some(join_id);
    }

    fn resolve_ruby_redos(&mut self, body_entry: Option<CfgNodeId>, redo_start: usize) {
        let pending_redos = self.pending_redo_node_ids.split_off(redo_start);
        let Some(body_entry) = body_entry else {
            return;
        };
        for redo_id in pending_redos {
            self.add_edge(&redo_id, &body_entry, CfgEdgeKind::Redo);
        }
    }

    fn labeled_statement_parts<'a>(&self, node: Node<'a>) -> Option<(String, Node<'a>)> {
        let mut cursor = node.walk();
        let named: Vec<_> = node.named_children(&mut cursor).collect();
        let label_node = node.child_by_field_name("label").or_else(|| {
            named.iter().copied().find(|child| {
                matches!(
                    child.kind(),
                    "identifier" | "statement_identifier" | "label_name"
                )
            })
        })?;
        let body = node
            .child_by_field_name("body")
            .filter(|child| !is_comment_node_kind(child.kind()))
            .or_else(|| {
                named.iter().copied().find(|child| {
                    child.id() != label_node.id() && !is_comment_node_kind(child.kind())
                })
            })?;
        let label = label_node
            .utf8_text(self.source)
            .ok()?
            .trim()
            .trim_start_matches('\'')
            .trim_start_matches('@')
            .to_string();
        (!label.is_empty()).then_some((label, body))
    }

    /// Handle for/while/do: Loop → body → LoopBack → exit (Join)
    fn walk_loop(&mut self, children: &[Node], idx: usize, start_byte: u32) -> usize {
        let labels: Vec<_> = self
            .embedded_control_label(children[idx])
            .into_iter()
            .collect();
        self.walk_loop_with_labels(children, idx, start_byte, &labels)
    }

    fn embedded_control_label(&self, node: Node<'_>) -> Option<String> {
        let label = node.child_by_field_name("label").or_else(|| {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find(|child| child.kind() == "label")
        })?;
        let label = label.utf8_text(self.source).ok()?.trim();
        let label = label.trim_start_matches('\'').trim_start_matches('@');
        (!label.is_empty()).then(|| label.to_string())
    }

    fn walk_loop_with_labels(
        &mut self,
        children: &[Node],
        idx: usize,
        start_byte: u32,
        labels: &[String],
    ) -> usize {
        let loop_node = &children[idx];
        let break_start = self.pending_break_node_ids.len();
        let continue_start = self.pending_continue_node_ids.len();
        let redo_start = self.pending_redo_node_ids.len();

        let body = find_loop_body(*loop_node, self.config.block_kinds);
        // Ruby's ordinary modifier form (`work while condition`) is pre-test,
        // but a `begin ... end while/until condition` body is explicitly
        // post-test. Both share one grammar node kind, so body shape determines
        // whether the incoming edge bypasses the first condition check.
        let ruby_post_test = self.is_ruby()
            && matches!(loop_node.kind(), "while_modifier" | "until_modifier")
            && body.is_some_and(|body| body.kind() == "begin");
        let incoming = self.prev_node_id.take();

        // 1. Create Loop node, connect from previous
        let loop_id = self.add_node(CfgNodeKind::Loop, start_byte, Some(loop_node));
        if !ruby_post_test && let Some(prev) = incoming {
            self.add_edge(&prev, &loop_id, CfgEdgeKind::Normal);
        }

        // 2. Find and walk the loop body
        let body_edge_start = self.edges.len();
        let body_node_start = self.nodes.len();

        let body_last = if let Some(body) = body {
            self.prev_node_id = if ruby_post_test {
                incoming
            } else {
                Some(loop_id)
            };

            if self.config.block_kinds.contains(&body.kind()) {
                let body_range = node_text_range(&body, self.source);
                self.walk_block(body, body_range.start_byte);
            } else {
                // Single-statement body
                self.walk_single_statement_body(body);
            }

            if ruby_post_test && self.nodes.len() == body_node_start {
                self.prev_node_id.take();
                None
            } else {
                self.prev_node_id.take()
            }
        } else {
            None
        };
        let body_entry = if ruby_post_test {
            self.nodes.get(body_node_start).map(|node| node.id)
        } else {
            self.edges[body_edge_start..]
                .iter()
                .find(|edge| edge.source == loop_id)
                .map(|edge| edge.target)
        };
        if ruby_post_test {
            if let Some(body_entry) = body_entry {
                self.add_edge(&loop_id, &body_entry, CfgEdgeKind::Normal);
            } else if let Some(prev) = incoming {
                self.add_edge(&prev, &loop_id, CfgEdgeKind::Normal);
            }
        }
        self.resolve_ruby_redos(body_entry, redo_start);

        // 3. LoopBack edge: last body node → Loop (if body didn't end with return/throw)
        if let Some(ref last) = body_last {
            self.add_edge(last, &loop_id, CfgEdgeKind::LoopBack);
        }

        // 4. Exit edge: Loop → Join (post-loop). Rust's unconditional `loop`
        // has no condition-false exit; only an explicit break can reach Join.
        let join_id = self.add_node(CfgNodeKind::Join, start_byte + 1, None);
        if !(self.language == Language::Rust && loop_node.kind() == "loop_expression") {
            self.add_edge(&loop_id, &join_id, CfgEdgeKind::Normal);
        }
        let pending_breaks = self.pending_break_node_ids.split_off(break_start);
        for (break_id, target) in pending_breaks {
            match target {
                ControlTransferTarget::Depth(1) => {
                    self.add_edge(&break_id, &join_id, CfgEdgeKind::Break);
                }
                ControlTransferTarget::Depth(depth) => self
                    .pending_break_node_ids
                    .push((break_id, ControlTransferTarget::Depth(depth - 1))),
                ControlTransferTarget::Label(target)
                    if labels.iter().any(|label| label == &target) =>
                {
                    self.add_edge(&break_id, &join_id, CfgEdgeKind::Break);
                }
                ControlTransferTarget::Label(target) => self
                    .pending_break_node_ids
                    .push((break_id, ControlTransferTarget::Label(target))),
            }
        }
        let pending_continues = self.pending_continue_node_ids.split_off(continue_start);
        for (continue_id, target) in pending_continues {
            match target {
                ControlTransferTarget::Depth(1) => {
                    self.add_edge(&continue_id, &loop_id, CfgEdgeKind::Continue);
                }
                ControlTransferTarget::Depth(depth) => self
                    .pending_continue_node_ids
                    .push((continue_id, ControlTransferTarget::Depth(depth - 1))),
                ControlTransferTarget::Label(target)
                    if labels.iter().any(|label| label == &target) =>
                {
                    self.add_edge(&continue_id, &loop_id, CfgEdgeKind::Continue);
                }
                ControlTransferTarget::Label(target) => self
                    .pending_continue_node_ids
                    .push((continue_id, ControlTransferTarget::Label(target))),
            }
        }

        self.prev_node_id = Some(join_id);
        idx + 1
    }

    /// Node-based wrapper for `walk_if`, used by expression wrappers.
    fn walk_if_node(&mut self, node: Node, start_byte: u32) {
        self.walk_if(&[node], 0, start_byte);
    }

    /// Node-based wrapper for `walk_loop`, used by expression wrappers.
    fn walk_loop_node(&mut self, node: Node, start_byte: u32) {
        self.walk_loop(&[node], 0, start_byte);
    }

    /// Node-based wrapper for `walk_switch`, used by expression wrappers.
    fn walk_switch_node(&mut self, node: Node, start_byte: u32) {
        self.walk_switch(&[node], 0, start_byte);
    }

    /// Emit a statement/return/throw node and connect to previous.
    fn emit_stmt(
        &mut self,
        kind: CfgNodeKind,
        start_byte: u32,
        stmt_node: &Node,
    ) -> types::ids::CfgNodeId {
        let node_id = self.add_node(kind, start_byte, Some(stmt_node));

        // (callee_name extraction removed — superseded by DataFlow-based analysis)

        // (effect annotation removed — superseded by EffectComposer DataFlow analysis)

        // Link from previous statement
        if let Some(prev) = self.prev_node_id.take() {
            self.add_edge(&prev, &node_id, CfgEdgeKind::Normal);
        }
        if matches!(kind, CfgNodeKind::Return | CfgNodeKind::Throw) {
            self.terminal_node_ids.push((node_id, kind));
        } else {
            self.prev_node_id = Some(node_id);
        }
        node_id
    }

    /// Find the first expression child inside a `go_statement` or `defer_statement` node.
    fn find_first_expression<'a>(&self, node: &Node<'a>) -> Option<Node<'a>> {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let kind = child.kind();
            if kind == "call_expression" || kind == "func_literal" || kind == "expression_statement"
            {
                return Some(child);
            }
        }
        None
    }

    /// Process the inner expression of a `go_statement`/`defer_statement`.
    fn process_go_defer_inner(&mut self, inner: &Node, start_byte: u32) -> types::ids::CfgNodeId {
        let mut inner_cursor = inner.walk();
        if inner.kind() == "expression_statement" {
            // Unwrap expression_statement wrapper (some tree-sitter grammars)
            if let Some(expr) = inner.named_children(&mut inner_cursor).next() {
                return self.emit_stmt(CfgNodeKind::Statement, start_byte, &expr);
            }
        }
        // Keep the complete call range. The function value and arguments are
        // evaluated at registration time, while the post-pass routes the
        // deferred call's resource-consumption effect to its exit clone.
        self.emit_stmt(CfgNodeKind::Statement, start_byte, inner)
    }

    /// Walk the body of a React cleanup arrow function with
    /// `ReactEffectCleanup` scope context.  All CFG nodes generated inside
    /// the body will inherit this context so that `compose_effects` can mark
    /// their `Free` effects as `Deferred`.
    ///
    /// Arrow bodies are walked disconnected from the parent CFG chain —
    /// `prev_node_id` is saved/restored so the cleanup nodes form their own
    /// isolated subgraph.
    fn walk_react_cleanup_arrow(&mut self, arrow_fn: &Node, start_byte: u32) {
        let saved_prev = self.prev_node_id;
        let saved_scope = self.scope_call_context;
        self.scope_call_context = CallContext::ReactEffectCleanup;

        // Find the arrow function body (skip formal_parameters).
        let body = find_function_body(*arrow_fn, self.config.block_kinds);
        if let Some(body_node) = body {
            if self.config.block_kinds.contains(&body_node.kind()) {
                // Block body `{ ... }` — walk each statement.
                // Start with no prev_node_id so these nodes are
                // disconnected from the parent chain (they sit after a
                // Return, which terminates the parent path).
                self.prev_node_id = None;
                self.walk_block(body_node, start_byte);
            } else {
                // Expression body `expr` — emit as single statement.
                self.prev_node_id = None;
                self.emit_stmt(CfgNodeKind::Statement, start_byte, &body_node);
            }
        }

        self.scope_call_context = saved_scope;
        self.prev_node_id = saved_prev;
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// For an if-like node, find the consequence and alternative branch bodies.
///
/// In tree-sitter, the else branch is wrapped in an `else_clause` node in some
/// languages (TS/JS/Rust) or may be a direct child in others (Java/Go/C).
/// This helper handles both cases.
fn find_if_branches<'a>(
    node: Node<'a>,
    block_kinds: &[&str],
) -> (Option<Node<'a>>, Option<Node<'a>>) {
    // Ruby postfix conditionals keep their executable statement in a named
    // `body` field rather than a block/consequence child.
    if matches!(node.kind(), "if_modifier" | "unless_modifier") {
        return (node.child_by_field_name("body"), None);
    }

    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();

    // Strategy 1: find direct block_kind children
    let blocks: Vec<Node> = children
        .iter()
        .filter(|c| block_kinds.contains(&c.kind()))
        .copied()
        .collect();

    let cons = if !blocks.is_empty() {
        Some(blocks[0])
    } else if children.len() >= 2 {
        // Fallback: index-based (children[0]=condition, children[1]=consequence)
        Some(children[1])
    } else {
        None
    };

    // Find alternative: look for else_clause wrapper first, then direct block_kind
    let mut alt = None;
    if blocks.len() >= 2 {
        alt = Some(blocks[1]);
    } else {
        // Search for wrapper nodes that contain the alternative body
        for child in &children {
            match child.kind() {
                "else_clause" | "else" => {
                    let mut sub_cursor = child.walk();
                    let sub_children: Vec<Node> = child.named_children(&mut sub_cursor).collect();
                    // Prefer a block_kind body, fall back to first child
                    for sub in &sub_children {
                        if block_kinds.contains(&sub.kind()) {
                            alt = Some(*sub);
                            break;
                        }
                    }
                    if alt.is_none() {
                        alt = sub_children.first().copied();
                    }
                    break;
                }
                _ => {}
            }
        }
        // Fallback: if still no alternative, try children[2] direct
        if alt.is_none() && children.len() > 2 {
            alt = children.get(2).copied();
        }
    }

    (cons, alt)
}

/// Direct sibling alternatives attached to an if node.
///
/// PHP and Python expose `elseif`/`elif` chains as repeated named children,
/// while TS/JS usually wrap a nested `if_statement` in one `else_clause`.
/// Only the repeated-clause shape is returned here; the ordinary two-way
/// branch continues to use [`find_if_branches`].
fn find_if_alternative_clauses(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| {
            matches!(
                child.kind(),
                "else_if_clause" | "elif_clause" | "else_clause"
            )
        })
        .collect()
}

/// Find the executable body of an `elseif`/`elif`/`else` clause.
fn find_if_clause_body<'a>(node: Node<'a>, block_kinds: &[&str]) -> Option<Node<'a>> {
    if block_kinds.contains(&node.kind()) {
        return Some(node);
    }
    if let Some(body) = node.child_by_field_name("body") {
        return Some(body);
    }

    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    children
        .iter()
        .find(|child| block_kinds.contains(&child.kind()))
        .copied()
        .or_else(|| children.last().copied())
}

/// Split the configured tree-sitter try shape into executable parts.
///
/// TypeScript/JavaScript/ArkTS, Java, C++, C#, Python, and PHP keep executable
/// parts as direct named children. Kotlin uses `try_expression` with
/// `catch_block`/`finally_block`; Cangjie exposes named body fields on
/// `tryExpression`. Ruby exposes the protected statement prefix and its
/// rescue/else/ensure clauses directly under either `body_statement` or
/// nested `begin`.
fn find_try_parts<'a>(
    node: Node<'a>,
    block_kinds: &[&str],
) -> (
    Option<Node<'a>>,
    Vec<Node<'a>>,
    Option<Node<'a>>,
    Option<Node<'a>>,
) {
    if matches!(node.kind(), "body_statement" | "begin") {
        let mut catch_clauses = Vec::new();
        let mut else_clause = None;
        let mut finally_clause = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "rescue" => catch_clauses.push(child),
                "else" => else_clause = Some(child),
                "ensure" => finally_clause = Some(child),
                _ => {}
            }
        }
        return (Some(node), catch_clauses, else_clause, finally_clause);
    }

    if node.kind() == "tryExpression" {
        let try_body = node.child_by_field_name("try_body");
        let finally_clause = node.child_by_field_name("finally_body");
        let mut catch_clauses = Vec::new();
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                if cursor.field_name() == Some("catch_body") {
                    catch_clauses.push(cursor.node());
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        return (try_body, catch_clauses, None, finally_clause);
    }

    let mut try_body = None;
    let mut catch_clauses = Vec::new();
    let mut else_clause = None;
    let mut finally_clause = None;
    let mut cursor = node.walk();

    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "catch_clause" | "except_clause" | "catch_block" => catch_clauses.push(child),
            "else_clause" => else_clause = Some(child),
            "finally_clause" | "finally_block" => finally_clause = Some(child),
            kind if try_body.is_none() && block_kinds.contains(&kind) => {
                try_body = Some(child);
            }
            _ => {}
        }
    }

    (try_body, catch_clauses, else_clause, finally_clause)
}

fn find_node_by_exact_range<'a>(
    node: Node<'a>,
    start_byte: u32,
    end_byte: u32,
) -> Option<Node<'a>> {
    if node.start_byte() as u32 == start_byte && node.end_byte() as u32 == end_byte {
        return Some(node);
    }
    if (node.start_byte() as u32) > start_byte || (node.end_byte() as u32) < end_byte {
        return None;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = find_node_by_exact_range(child, start_byte, end_byte) {
            return Some(found);
        }
    }
    None
}

fn find_descendant_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = find_descendant_kind(child, kind) {
            return Some(found);
        }
    }
    None
}

fn normalized_type_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    let normalized: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    (!normalized.is_empty()).then_some(normalized)
}

/// Return a thrown type only when the grammar proves an object creation.
/// Constructor-like calls in Python/Kotlin/C++ and Ruby constants are not type
/// proof without resolution, so they intentionally return `None`.
fn explicit_object_creation_type(
    language: Language,
    throw_node: Node<'_>,
    source: &[u8],
) -> Option<String> {
    if !matches!(language, Language::Java | Language::CSharp | Language::Php) {
        return None;
    }
    let throw_expression = if language == Language::Php {
        if throw_node.kind() == "throw_expression" {
            throw_node
        } else {
            let mut cursor = throw_node.walk();
            throw_node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "throw_expression")?
        }
    } else if throw_node.kind() == "throw_statement" {
        throw_node
    } else {
        let mut cursor = throw_node.walk();
        throw_node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "throw_statement")?
    };
    let creation = throw_expression
        .named_child(0)
        .filter(|child| child.kind() == "object_creation_expression")?;
    let type_node = creation.child_by_field_name("type").or_else(|| {
        let mut cursor = creation.walk();
        creation
            .named_children(&mut cursor)
            .find(|child| !matches!(child.kind(), "argument_list" | "arguments"))
    })?;
    normalized_type_text(type_node, source)
}

fn handler_guarantees_exact_type(
    language: Language,
    clause: Node<'_>,
    thrown_type: &str,
    source: &[u8],
) -> bool {
    if language == Language::CSharp && find_descendant_kind(clause, "catch_filter_clause").is_some()
    {
        return false;
    }

    let type_nodes = match language {
        Language::Java => find_descendant_kind(clause, "catch_type")
            .map(|catch_type| {
                let mut cursor = catch_type.walk();
                catch_type.named_children(&mut cursor).collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        Language::CSharp => find_descendant_kind(clause, "catch_declaration")
            .and_then(|declaration| declaration.child_by_field_name("type"))
            .into_iter()
            .collect(),
        Language::Php => clause
            .child_by_field_name("type")
            .map(|type_list| {
                let mut cursor = type_list.walk();
                type_list.named_children(&mut cursor).collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        _ => return false,
    };

    type_nodes
        .into_iter()
        .any(|type_node| normalized_type_text(type_node, source).as_deref() == Some(thrown_type))
}

/// Find the body block/statement of a loop node (for/while/do).
///
/// Looks for a child matching `block_kinds` first; if none found, returns
/// the last named child (single-statement body like `while (x) doSomething();`).
fn find_loop_body<'a>(node: Node<'a>, block_kinds: &[&str]) -> Option<Node<'a>> {
    if matches!(node.kind(), "while_modifier" | "until_modifier") {
        return node.child_by_field_name("body");
    }

    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();

    // Prefer block_kind children (statement_block, block, compound_statement)
    for child in &children {
        if block_kinds.contains(&child.kind()) {
            return Some(*child);
        }
    }

    // Fallback: last named child (for single-statement body)
    children.last().copied()
}

/// Last executable node in a case body, unwrapping the Go `statement_list`
/// container without descending into the internals of an expression statement.
fn last_named_descendant<'a>(nodes: &[Node<'a>]) -> Option<Node<'a>> {
    let last = *nodes.last()?;
    if last.kind() != "statement_list" {
        return Some(last);
    }

    let mut cursor = last.walk();
    let children: Vec<Node> = last.named_children(&mut cursor).collect();
    last_named_descendant(&children)
}

fn find_function_body<'a>(node: Node<'a>, block_kinds: &[&str]) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if block_kinds.contains(&child.kind()) {
            return Some(child);
        }
        // Arrow function: body might be an expression
        if node.kind() == "arrow_function" && child.kind() != "formal_parameters" {
            return Some(child);
        }
        // Recursive: the block might be nested
        if let Some(found) = find_function_body(child, block_kinds) {
            return Some(found);
        }
    }
    None
}

fn node_text_range(node: &Node, _source: &[u8]) -> TextRange {
    TextRange {
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
        start_line: node.start_position().row as u32,
        start_column: node.start_position().column as u32,
        end_line: node.end_position().row as u32,
        end_column: node.end_position().column as u32,
    }
}

/// Whether a tree-sitter field name identifies a case label/value/guard child
/// rather than an executable statement.  The same field name is used across
/// grammars for the same structural role:
///
/// | Field        | Languages                        |
/// |--------------|----------------------------------|
/// | `value`      | C/C++, TS/JS, Go (expr_switch)   |
/// | `type`       | Go (type_switch)                 |
/// | `alias`      | Go (type_switch variable bind)   |
/// | `pattern`    | C# (constant_pattern, etc.)      |
/// | `guard`      | C# (case_guard)                  |
/// | `condition`  | C++ (condition_clause, if/switch) |
/// | `communication` | Go (`select` send/receive)    |
pub fn is_case_label_field(field: &str) -> bool {
    matches!(
        field,
        "value" | "type" | "alias" | "pattern" | "guard" | "condition" | "communication"
    )
}

/// Whether a tree-sitter node kind represents a case label/pattern/guard that
/// should be skipped when extracting executable statements from a case clause.
///
/// This covers languages whose case labels are unnamed children (no field name
/// assigned by the grammar), notably Java and C#.
pub fn is_case_label_kind(kind: &str) -> bool {
    // Java (`switch_label`), C# (`case_switch_label`, `default_switch_label`)
    if kind.ends_with("_label") || kind == "switch_label" {
        return true;
    }
    // C#/Cangjie patterns and guards.
    if kind.ends_with("_pattern")
        || kind.ends_with("Pattern")
        || matches!(kind, "when_clause" | "patternGuard")
    {
        return true;
    }
    false
}

// ── CFG extraction helpers (used by extract.rs) ─────────────────────────

/// Function node kinds that CfgBuilder handles across languages.
const FUNCTION_NODE_KINDS: &[&str] = &[
    "function_declaration",
    "method_definition",
    "arrow_function",
    "generator_function_declaration",
    "generator_function",
    "function_definition",
    "async_function_definition",
    "method_declaration",
    "constructor_declaration",
    "function_item",      // tree-sitter-rust
    "functionDefinition", // tree-sitter-cangjie
    "mainDefinition",     // tree-sitter-cangjie entry point
    "method",             // tree-sitter-ruby
    "singleton_method",   // tree-sitter-ruby
];

/// Build per-function control-flow graphs by matching function symbols
/// to tree-sitter nodes.
pub(crate) fn build_cfg_for_functions<'a>(
    language: Language,
    root: Node<'a>,
    symbols: &[SymbolDef],
    source_bytes: &[u8],
) -> anyhow::Result<CfgResult> {
    let function_symbols: Vec<&SymbolDef> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor
            )
        })
        .collect();

    let mut all_nodes = Vec::new();
    let mut all_edges = Vec::new();

    for sym in &function_symbols {
        if let Some(func_node) = find_function_node(root, sym) {
            let result = CfgBuilder::build(language, &sym.id, func_node, source_bytes);
            all_nodes.extend(result.nodes);
            all_edges.extend(result.edges);
        }
    }

    Ok(CfgResult {
        nodes: all_nodes,
        edges: all_edges,
    })
}

/// Walk up from the symbol's name position to find the enclosing function node.
fn find_function_node<'a>(root: Node<'a>, symbol: &SymbolDef) -> Option<Node<'a>> {
    let pos = symbol.name_range.start_byte as usize;
    let mut node = root.descendant_for_byte_range(pos, pos)?;
    // Walk up parent chain to find the enclosing function node
    loop {
        if FUNCTION_NODE_KINDS.contains(&node.kind()) {
            return Some(node);
        }
        node = node.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages::create_frontend;
    use types::enums::Language;
    use types::ids::FileId;

    fn parse_ts(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let source_bytes = source.as_bytes().to_vec();
        let mut parser = tree_sitter::Parser::new();
        let frontend = create_frontend(Language::TypeScript).unwrap();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .unwrap();
        let tree = parser.parse(&source_bytes, None).unwrap();
        (tree, source_bytes)
    }

    fn find_function<'a>(tree: &'a tree_sitter::Tree, source: &[u8]) -> (Node<'a>, SymbolId) {
        let root = tree.root_node();
        let file_id = FileId::generate("test.ts");
        find_function_recursive(root, &file_id, source).expect("no function found")
    }

    fn find_function_recursive<'a>(
        node: Node<'a>,
        file_id: &FileId,
        source: &[u8],
    ) -> Option<(Node<'a>, SymbolId)> {
        if node.kind() == "function_declaration"
            || node.kind() == "method_definition"
            || node.kind() == "arrow_function"
        {
            let name = node
                .named_child(0)
                .and_then(|c| {
                    if c.kind() == "identifier" {
                        c.utf8_text(source).ok()
                    } else {
                        None
                    }
                })
                .unwrap_or("anon");
            let fid = SymbolId::generate(file_id, "typescript", name, "function", None);
            return Some((node, fid));
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(found) = find_function_recursive(child, file_id, source) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn test_cfg_builder_simple_function() {
        let source = "function hello() { const x = 1; return x; }";
        let (tree, source_bytes) = parse_ts(source);
        let (func_node, func_id) = find_function(&tree, &source_bytes);

        let result = CfgBuilder::build(Language::TypeScript, &func_id, func_node, &source_bytes);

        assert!(
            result.nodes.len() >= 3,
            "Expected at least Entry + Statement + Exit"
        );
        assert!(result.edges.len() >= 2, "Expected at least 2 edges");

        // Check that there's an Entry, at least one Statement/Return, and Exit
        let has_entry = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Entry);
        let has_exit = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Exit);
        assert!(has_entry);
        assert!(has_exit);
    }

    #[test]
    fn test_cfg_builder_if_else() {
        let source = r#"function check(x: number) {
  if (x > 0) { return 1; } else { return -1; }
}"#;
        let (tree, source_bytes) = parse_ts(source);
        let (func_node, func_id) = find_function(&tree, &source_bytes);

        let result = CfgBuilder::build(Language::TypeScript, &func_id, func_node, &source_bytes);

        let has_branch = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch);
        let has_join = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join);
        assert!(has_branch, "Expected Branch node for if/else");
        assert!(has_join, "Expected Join node for if/else");
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .unwrap();
        assert_eq!(branch.stmt_range.start_line, 1);
    }

    fn parse_lang(lang: Language, source: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let source_bytes = source.as_bytes().to_vec();
        let mut parser = tree_sitter::Parser::new();
        let frontend = create_frontend(lang).unwrap();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .unwrap();
        let tree = parser.parse(&source_bytes, None).unwrap();
        (tree, source_bytes)
    }

    /// Build a CFG for the first supported function node found in the tree.
    fn build_cfg_for_first_fn(lang: Language, source: &str) -> super::CfgResult {
        fn find_supported_function(node: Node<'_>) -> Option<Node<'_>> {
            if matches!(
                node.kind(),
                "function_definition"
                    | "function_declaration"
                    | "functionDefinition"
                    | "method_declaration"
                    | "function_item"
                    | "method"
                    | "singleton_method"
            ) {
                return Some(node);
            }
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find_map(find_supported_function)
        }

        let (tree, source_bytes) = parse_lang(lang, source);
        let root = tree.root_node();
        let file_id = FileId::generate("test.c");
        let func_node = find_supported_function(root).expect("no function definition found");
        let name = func_node
            .named_child(0)
            .and_then(|child| {
                (child.kind() == "identifier")
                    .then(|| child.utf8_text(&source_bytes).ok())
                    .flatten()
            })
            .unwrap_or("anon");
        let fid = SymbolId::generate(&file_id, "", name, "function", None);
        CfgBuilder::build(lang, &fid, func_node, &source_bytes)
    }

    /// TS/JS: find function by name.
    fn build_cfg_for_fn_ts(source: &str) -> super::CfgResult {
        let (tree, source_bytes) = parse_ts(source);
        let (func_node, func_id) = find_function(&tree, &source_bytes);
        CfgBuilder::build(Language::TypeScript, &func_id, func_node, &source_bytes)
    }

    /// Build a CFG for a Java method by wrapping it in a minimal class.
    fn build_cfg_for_java_method(method_src: &str) -> super::CfgResult {
        let source = format!("class T{{ {method_src} }}");
        let (tree, source_bytes) = parse_lang(Language::Java, &source);
        let root = tree.root_node();
        let file_id = FileId::generate("test.java");
        // Recursively find the first method_declaration.
        fn find_method<'a>(node: Node<'a>) -> Option<Node<'a>> {
            if node.kind() == "method_declaration" {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(found) = find_method(child) {
                    return Some(found);
                }
            }
            None
        }
        let func_node = find_method(root).expect("no method found");
        let name = func_node
            .named_child(1)
            .and_then(|c| {
                if c.kind() == "identifier" {
                    c.utf8_text(&source_bytes).ok()
                } else {
                    None
                }
            })
            .unwrap_or("anon");
        let fid = SymbolId::generate(&file_id, "", name, "method", None);
        CfgBuilder::build(Language::Java, &fid, func_node, &source_bytes)
    }

    fn build_cfg_for_cangjie_function(source: &str, expected_kind: &str) -> super::CfgResult {
        let (tree, source_bytes) = parse_lang(Language::Cangjie, source);
        let file_id = FileId::generate("test.cj");

        fn find_function<'a>(node: Node<'a>, expected_kind: &str) -> Option<Node<'a>> {
            if node.kind() == expected_kind {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(found) = find_function(child, expected_kind) {
                    return Some(found);
                }
            }
            None
        }

        let function_node = find_function(tree.root_node(), expected_kind)
            .unwrap_or_else(|| panic!("no {expected_kind} found"));
        let fid = SymbolId::generate(&file_id, "", "dispatch", "function", None);
        CfgBuilder::build(Language::Cangjie, &fid, function_node, &source_bytes)
    }

    fn cfg_node_id_for_text(
        result: &super::CfgResult,
        source: &str,
        kind: CfgNodeKind,
        expected: &str,
    ) -> types::ids::CfgNodeId {
        result
            .nodes
            .iter()
            .find(|node| {
                if node.kind != kind {
                    return false;
                }
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim() == expected)
            })
            .unwrap_or_else(|| panic!("no {kind:?} CFG node with text {expected:?}"))
            .id
    }

    fn has_cfg_edge(
        result: &super::CfgResult,
        source: types::ids::CfgNodeId,
        target: types::ids::CfgNodeId,
        kind: CfgEdgeKind,
    ) -> bool {
        result
            .edges
            .iter()
            .any(|edge| edge.source == source && edge.target == target && edge.kind == kind)
    }

    fn assert_cfg_edge_ids_match_payload(result: &super::CfgResult) {
        for edge in &result.edges {
            assert_eq!(
                edge.id,
                CfgEdge::new(&edge.source, &edge.target, edge.kind).id,
                "CfgEdgeId must encode the edge's final source, target, and kind"
            );
        }
    }

    fn cfg_reaches(
        result: &super::CfgResult,
        source: types::ids::CfgNodeId,
        target: types::ids::CfgNodeId,
    ) -> bool {
        let mut pending = vec![source];
        let mut visited = std::collections::HashSet::new();
        while let Some(node_id) = pending.pop() {
            if !visited.insert(node_id) {
                continue;
            }
            if node_id == target {
                return true;
            }
            pending.extend(
                result
                    .edges
                    .iter()
                    .filter(|edge| edge.source == node_id)
                    .map(|edge| edge.target),
            );
        }
        false
    }

    // ── Switch CFG tests ──────────────────────────────────────────

    #[test]
    fn test_cfg_retagged_edges_keep_deterministic_ids_in_sync() {
        let method = r#"void dispatch(int x) {
  if (x > 0) { yes(); } else { no(); }
  switch (x) { case 1 -> one(); default -> other(); }
  try { work(); } catch (RuntimeException error) { recover(); }
}"#;
        let result = build_cfg_for_java_method(method);

        for kind in [
            CfgEdgeKind::TrueBranch,
            CfgEdgeKind::FalseBranch,
            CfgEdgeKind::CaseBranch,
            CfgEdgeKind::Exception,
        ] {
            assert!(
                result.edges.iter().any(|edge| edge.kind == kind),
                "fixture must exercise {kind:?} retagging"
            );
        }
        assert_cfg_edge_ids_match_payload(&result);
    }

    #[test]
    fn test_switch_cfg_ts_models_implicit_fallthrough_break_and_default() {
        let source = r#"function dispatch(x: number) {
  switch (x) {
    case 1:
      first();
    case 2:
      second();
      break;
    case 3:
      third();
    default:
      fallback();
  }
}"#;
        let result = build_cfg_for_fn_ts(source);
        let first = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "first();");
        let second = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "second();");
        let break_node = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break;");
        let third = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "third();");
        let fallback = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "fallback();");
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("switch Branch");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("switch Join");

        assert!(
            has_cfg_edge(&result, first, second, CfgEdgeKind::Normal),
            "case 1 must fall through to case 2"
        );
        assert!(
            has_cfg_edge(&result, break_node, join.id, CfgEdgeKind::Break),
            "break must leave the switch"
        );
        assert!(
            has_cfg_edge(&result, third, fallback, CfgEdgeKind::Normal),
            "a non-empty case must be able to fall through to default"
        );
        assert!(
            !has_cfg_edge(&result, branch.id, join.id, CfgEdgeKind::CaseBranch),
            "an explicit default makes the synthetic no-match path impossible"
        );
    }

    #[test]
    fn test_switch_cfg_go_models_only_explicit_fallthrough() {
        let source = r#"package p
func dispatch(x int) {
  switch x {
  case 1:
    first()
    fallthrough
  case 2:
    second()
  case 3:
    third()
  }
}"#;
        let result = build_cfg_for_first_fn(Language::Go, source);
        let fallthrough =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "fallthrough");
        let second = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "second()");
        let third = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "third()");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("switch Join");

        assert!(
            has_cfg_edge(&result, fallthrough, second, CfgEdgeKind::Normal),
            "Go fallthrough must enter the next case"
        );
        assert!(
            has_cfg_edge(&result, second, join.id, CfgEdgeKind::Normal),
            "Go cases terminate implicitly without fallthrough"
        );
        assert!(
            !has_cfg_edge(&result, second, third, CfgEdgeKind::Normal),
            "ordinary Go cases must not fall through"
        );
    }

    #[test]
    fn test_switch_cfg_go_empty_case_exits_without_falling_into_default() {
        let source = r#"package p
func dispatch(x int) {
  switch x {
  default:
    fallback()
  case 1:
  case 2:
  }
  after()
}"#;
        let result = build_cfg_for_first_fn(Language::Go, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Go switch Branch");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("Go switch Join");
        let fallback = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "fallback()");

        assert!(has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::CaseBranch
        ));
        assert!(!has_cfg_edge(
            &result,
            branch.id,
            fallback,
            CfgEdgeKind::Normal
        ));
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| {
                    edge.source == branch.id
                        && edge.target == join.id
                        && edge.kind == CfgEdgeKind::CaseBranch
                })
                .count(),
            1,
            "identical empty-case paths must remain one deterministic CFG edge"
        );
    }

    #[test]
    fn test_select_cfg_go_models_communication_siblings_and_break() {
        let source = r#"package p
func stream(clientGone <-chan bool, ready <-chan int) int {
  select {
  case <-clientGone:
    return 1
  case value := <-ready:
    consume(value)
    break
  default:
    idle()
  }
  after()
  return 0
}"#;
        let result = build_cfg_for_first_fn(Language::Go, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Go select dispatch Branch");
        let return_one = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return 1");
        let consume =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "consume(value)");
        let break_node = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break");
        let idle = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "idle()");
        let after = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after()");
        let join = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Join
                    && has_cfg_edge(&result, break_node, node.id, CfgEdgeKind::Break)
            })
            .expect("select Join owned by break");

        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
                .count(),
            3
        );
        let case_targets: std::collections::HashSet<_> = result
            .edges
            .iter()
            .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
            .map(|edge| edge.target)
            .collect();
        assert_eq!(
            case_targets,
            std::collections::HashSet::from([return_one, consume, idle]),
            "communication headers are dispatch conditions, not case-body statements"
        );
        assert!(has_cfg_edge(
            &result,
            consume,
            break_node,
            CfgEdgeKind::Normal
        ));
        assert!(has_cfg_edge(&result, idle, join.id, CfgEdgeKind::Normal));
        assert!(has_cfg_edge(&result, join.id, after, CfgEdgeKind::Normal));
        assert!(!cfg_reaches(&result, return_one, after));
        assert!(!has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::CaseBranch
        ));
    }

    #[test]
    fn test_select_cfg_go_without_default_has_no_skip_path() {
        let source = r#"package p
func receive(ch <-chan int) {
  select {
  case value := <-ch:
    consume(value)
  }
  after()
}"#;
        let result = build_cfg_for_first_fn(Language::Go, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Go select dispatch Branch");
        let consume =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "consume(value)");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("select Join");

        assert!(has_cfg_edge(
            &result,
            branch.id,
            consume,
            CfgEdgeKind::CaseBranch
        ));
        assert!(!has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::CaseBranch
        ));
    }

    #[test]
    fn test_empty_select_cfg_go_has_no_reachable_successor() {
        let source = "package p\nfunc blockForever() { select {}\n after() }";
        let result = build_cfg_for_first_fn(Language::Go, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("empty Go select Branch");
        assert!(
            result.edges.iter().all(|edge| edge.source != branch.id),
            "empty select blocks forever"
        );
    }

    #[test]
    fn test_select_cfg_go_empty_communication_body_reaches_join() {
        let source = "package p\nfunc wait(ch <-chan int) { select { case <-ch: }\n after() }";
        let result = build_cfg_for_first_fn(Language::Go, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Go select Branch");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("Go select Join");
        let after = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after()");

        assert!(has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::CaseBranch
        ));
        assert!(has_cfg_edge(&result, join.id, after, CfgEdgeKind::Normal));
    }

    #[test]
    fn test_switch_cfg_java_arrow_rule_never_falls_through() {
        let method = "void dispatch(int x) { switch (x) { case 1 -> first(); case 2 -> second(); default -> fallback(); } }";
        let source = format!("class T{{ {method} }}");
        let result = build_cfg_for_java_method(method);
        let first = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "first();");
        let second = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "second();");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("switch Join");

        assert!(has_cfg_edge(&result, first, join.id, CfgEdgeKind::Normal));
        assert!(!has_cfg_edge(&result, first, second, CfgEdgeKind::Normal));
    }

    #[test]
    fn test_switch_cfg_java_empty_arrow_rule_reaches_join_as_case_path() {
        let method =
            "void dispatch(int x) { switch (x) { case 1 -> {} default -> fallback(); } after(); }";
        let result = build_cfg_for_java_method(method);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Java switch Branch");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("Java switch Join");

        assert!(has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::CaseBranch
        ));
        assert!(!has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::Normal
        ));
    }

    #[test]
    fn test_switch_cfg_nested_break_bypasses_fallthrough() {
        let source = r#"function dispatch(x: number, stop: boolean) {
  switch (x) {
    case 1:
      if (stop) { break; }
      work();
    case 2:
      next();
      break;
    default:
      fallback();
  }
}"#;
        let result = build_cfg_for_fn_ts(source);
        let break_node = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break;");
        let work = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "work();");
        let next = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "next();");

        assert!(
            cfg_reaches(&result, work, next),
            "false path must fall through"
        );
        assert!(
            !cfg_reaches(&result, break_node, next),
            "the true break path must bypass the next case"
        );
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == break_node)
                .count(),
            1,
            "break must have exactly one continuation"
        );
    }

    #[test]
    fn test_loop_cfg_resolves_nested_break_and_continue() {
        let source = r#"function run(flag: boolean, stop: boolean, skip: boolean) {
  while (flag) {
    if (stop) { break; }
    if (skip) { continue; }
    work();
  }
  after();
}"#;
        let result = build_cfg_for_fn_ts(source);
        let break_node = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break;");
        let continue_node =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "continue;");
        let after = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after();");
        let loop_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Loop)
            .expect("while Loop")
            .id;

        assert!(cfg_reaches(&result, break_node, after));
        assert!(has_cfg_edge(
            &result,
            continue_node,
            loop_id,
            CfgEdgeKind::Continue
        ));
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == continue_node)
                .count(),
            1,
            "continue must only loop back, without a direct exit continuation"
        );
    }

    #[test]
    fn test_java_labeled_loop_resolves_break_and_continue_to_outer_targets() {
        let method = r#"void scan(boolean stop, boolean skip) {
  outer: while (ready()) {
    if (stop) break outer;
    if (skip) continue outer;
    work();
  }
  after();
}"#;
        let source = format!("class T{{ {method} }}");
        let result = build_cfg_for_java_method(method);
        let loop_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Loop)
            .expect("labeled Java while Loop")
            .id;
        let break_id =
            cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "break outer;");
        let continue_id =
            cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "continue outer;");
        let after = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "after();");
        let loop_join = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Join
                    && has_cfg_edge(&result, break_id, node.id, CfgEdgeKind::Break)
            })
            .expect("labeled break target Join");

        assert!(has_cfg_edge(
            &result,
            continue_id,
            loop_id,
            CfgEdgeKind::Continue
        ));
        assert!(has_cfg_edge(
            &result,
            loop_join.id,
            after,
            CfgEdgeKind::Normal
        ));
    }

    #[test]
    fn test_java_labeled_break_runs_finally_before_leaving_loop() {
        let method = r#"void scan() {
  outer: while (ready()) {
    try {
      break outer;
    } finally {
      cleanup();
    }
  }
  after();
}"#;
        let source = format!("class T{{ {method} }}");
        let result = build_cfg_for_java_method(method);
        let break_id =
            cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "break outer;");
        let cleanup = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "cleanup();");
        let loop_join = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Join
                    && has_cfg_edge(&result, cleanup, node.id, CfgEdgeKind::Break)
            })
            .expect("cleanup continuation must own labeled break edge");

        assert!(has_cfg_edge(
            &result,
            break_id,
            cleanup,
            CfgEdgeKind::Normal
        ));
        assert!(!has_cfg_edge(
            &result,
            break_id,
            loop_join.id,
            CfgEdgeKind::Break
        ));
    }

    #[test]
    fn test_labeled_loops_resolve_targets_across_supported_grammars() {
        let fixtures = [
            (
                Language::JavaScript,
                "function scan(stop, skip) { outer: while (ready()) { if (stop) break outer; if (skip) continue outer; work(); } after(); }",
                "break outer;",
                "continue outer;",
            ),
            (
                Language::TypeScript,
                "function scan(stop: boolean, skip: boolean) { outer: while (ready()) { if (stop) break outer; if (skip) continue outer; work(); } after(); }",
                "break outer;",
                "continue outer;",
            ),
            (
                Language::ArkTS,
                "function scan(stop: boolean, skip: boolean): void { outer: while (ready()) { if (stop) break outer; if (skip) continue outer; work(); } after(); }",
                "break outer;",
                "continue outer;",
            ),
            (
                Language::Go,
                "package p\nfunc scan(stop bool, skip bool) { outer: for ready() { if stop { break outer }; if skip { continue outer }; work() }; after() }",
                "break outer",
                "continue outer",
            ),
            (
                Language::Rust,
                "fn scan(stop: bool, skip: bool) { 'outer: while ready() { if stop { break 'outer; } if skip { continue 'outer; } work(); } after(); }",
                "break 'outer",
                "continue 'outer",
            ),
            (
                Language::Kotlin,
                "fun scan(stop: Boolean, skip: Boolean) { outer@ while (ready()) { if (stop) break@outer; if (skip) continue@outer; work() }; after() }",
                "break@outer",
                "continue@outer",
            ),
        ];

        for (language, source, break_text, continue_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let loop_id = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Loop)
                .unwrap_or_else(|| panic!("{language:?} labeled Loop"))
                .id;
            let break_id =
                cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, break_text);
            let continue_id =
                cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, continue_text);

            assert!(
                result
                    .edges
                    .iter()
                    .any(|edge| { edge.source == break_id && edge.kind == CfgEdgeKind::Break }),
                "{language:?} labeled break must resolve"
            );
            assert!(
                has_cfg_edge(&result, continue_id, loop_id, CfgEdgeKind::Continue),
                "{language:?} labeled continue must target its loop"
            );
        }
    }

    #[test]
    fn test_java_nested_labeled_transfers_bypass_inner_loop_targets() {
        let method = r#"void scan(boolean stop, boolean skip) {
  outer: while (ready()) {
    while (innerReady()) {
      if (stop) break outer;
      if (skip) continue outer;
      innerWork();
    }
  }
  after();
}"#;
        let source = format!("class T{{ {method} }}");
        let result = build_cfg_for_java_method(method);
        let mut loops: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.kind == CfgNodeKind::Loop)
            .collect();
        loops.sort_by_key(|node| node.stmt_range.start_byte);
        let [outer_loop, inner_loop] = loops.as_slice() else {
            panic!("expected outer and inner Java loops")
        };
        let break_id =
            cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "break outer;");
        let continue_id =
            cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "continue outer;");
        let inner_join = result
            .edges
            .iter()
            .find(|edge| {
                edge.source == inner_loop.id
                    && edge.kind == CfgEdgeKind::Normal
                    && result
                        .nodes
                        .iter()
                        .any(|node| node.id == edge.target && node.kind == CfgNodeKind::Join)
            })
            .expect("inner loop exit")
            .target;
        let outer_join = result
            .edges
            .iter()
            .find(|edge| {
                edge.source == outer_loop.id
                    && edge.kind == CfgEdgeKind::Normal
                    && result
                        .nodes
                        .iter()
                        .any(|node| node.id == edge.target && node.kind == CfgNodeKind::Join)
            })
            .expect("outer loop exit")
            .target;

        assert!(has_cfg_edge(
            &result,
            break_id,
            outer_join,
            CfgEdgeKind::Break
        ));
        assert!(!has_cfg_edge(
            &result,
            break_id,
            inner_join,
            CfgEdgeKind::Break
        ));
        assert!(has_cfg_edge(
            &result,
            continue_id,
            outer_loop.id,
            CfgEdgeKind::Continue
        ));
        assert!(!has_cfg_edge(
            &result,
            continue_id,
            inner_loop.id,
            CfgEdgeKind::Continue
        ));
    }

    #[test]
    fn test_java_labeled_break_runs_managed_exit_before_leaving_loop() {
        let method = r#"void scan() {
  outer: while (ready()) {
    try (Resource resource = open()) {
      break outer;
    }
  }
  after();
}"#;
        let source = format!("class T{{ {method} }}");
        let result = build_cfg_for_java_method(method);
        let break_id =
            cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "break outer;");
        let managed_exit = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::BlockExit
                    && has_cfg_edge(&result, break_id, node.id, CfgEdgeKind::Normal)
            })
            .expect("labeled break managed BlockExit");

        assert!(
            result
                .edges
                .iter()
                .any(|edge| { edge.source == managed_exit.id && edge.kind == CfgEdgeKind::Break })
        );
        assert!(
            !result
                .edges
                .iter()
                .any(|edge| edge.source == break_id && edge.kind == CfgEdgeKind::Break)
        );
    }

    #[test]
    fn test_labeled_blocks_resolve_break_without_fake_statement_nodes() {
        let fixtures = [
            (
                Language::Java,
                "class T { void scan(boolean stop) { done: { if (stop) break done; work(); } after(); } }",
                "break done;",
                "after();",
            ),
            (
                Language::Rust,
                "fn scan(stop: bool) { 'done: { if stop { break 'done; } work(); } after(); }",
                "break 'done",
                "after();",
            ),
        ];

        for (language, source, break_text, after_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let break_id =
                cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, break_text);
            let after = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, after_text);
            let label_join = result
                .nodes
                .iter()
                .find(|node| {
                    node.kind == CfgNodeKind::Join
                        && has_cfg_edge(&result, break_id, node.id, CfgEdgeKind::Break)
                })
                .unwrap_or_else(|| panic!("{language:?} labeled block Join"));

            assert!(has_cfg_edge(
                &result,
                label_join.id,
                after,
                CfgEdgeKind::Normal
            ));
            assert!(!result.nodes.iter().any(|node| {
                if node.kind != CfgNodeKind::Statement {
                    return false;
                }
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim_start().starts_with("done:"))
            }));
        }
    }

    #[test]
    fn test_direct_goto_skips_lexical_fallthrough_across_c_family_grammars() {
        let fixtures = [
            (
                Language::C,
                "int run(void) { goto cleanup; skipped(); cleanup: finish(); return 0; }",
                "goto cleanup;",
                "skipped();",
                "finish();",
            ),
            (
                Language::Cpp,
                "int run() { goto cleanup; skipped(); cleanup: finish(); return 0; }",
                "goto cleanup;",
                "skipped();",
                "finish();",
            ),
            (
                Language::Go,
                "package p\nfunc run() { goto cleanup; skipped(); cleanup: finish(); return }",
                "goto cleanup",
                "skipped()",
                "finish()",
            ),
            (
                Language::CSharp,
                "class T { void Run() { goto Cleanup; Skipped(); Cleanup: Finish(); return; } }",
                "goto Cleanup;",
                "Skipped();",
                "Finish();",
            ),
        ];

        for (language, source, goto_text, skipped_text, target_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, goto_text);
            let skipped =
                cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, skipped_text);
            let target = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, target_text);

            assert!(
                has_cfg_edge(&result, goto_id, target, CfgEdgeKind::Goto),
                "{language:?} direct goto must reach its label"
            );
            assert!(
                !cfg_reaches(&result, goto_id, skipped),
                "{language:?} direct goto must not execute lexical fallthrough"
            );
        }
    }

    #[test]
    fn test_backward_goto_targets_the_labeled_statement() {
        let fixtures = [
            (
                Language::C,
                r#"int run(int again) {
retry:
    work();
    if (again) goto retry;
    return 0;
}"#,
                "goto retry;",
                "work();",
            ),
            (
                Language::CSharp,
                r#"class T { void Run(bool again) {
Retry:
    Work();
    if (again) goto Retry;
} }"#,
                "goto Retry;",
                "Work();",
            ),
        ];

        for (language, source, goto_text, work_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, goto_text);
            let work = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, work_text);

            assert!(
                result.edges.iter().any(|edge| {
                    edge.source == goto_id && edge.target == work && edge.kind == CfgEdgeKind::Goto
                }),
                "{language:?} backward goto must create a direct cycle to the label entry"
            );
        }
    }

    #[test]
    fn test_php_direct_goto_targets_the_standalone_label() {
        let source = r#"<?php
function run() {
    goto done;
    skipped();
done:
    finish();
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto done;");
        let skipped = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "skipped();");
        let label = cfg_node_id_for_text(&result, source, CfgNodeKind::Join, "done:");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");

        assert!(has_cfg_edge(&result, goto_id, label, CfgEdgeKind::Goto));
        assert!(has_cfg_edge(&result, label, finish, CfgEdgeKind::Normal));
        assert!(!cfg_reaches(&result, goto_id, skipped));
    }

    #[test]
    fn test_php_goto_may_enter_an_ordinary_block_but_not_a_loop_or_switch() {
        let ordinary_block = r#"<?php
function run($ready) {
    goto inside;
    if ($ready) {
inside:
        finish();
    }
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, ordinary_block);
        let goto_id = cfg_node_id_for_text(
            &result,
            ordinary_block,
            CfgNodeKind::Statement,
            "goto inside;",
        );
        let label = cfg_node_id_for_text(&result, ordinary_block, CfgNodeKind::Join, "inside:");
        assert!(has_cfg_edge(&result, goto_id, label, CfgEdgeKind::Goto));

        let loop_body = r#"<?php
function run($ready) {
    goto inside;
    while ($ready) {
inside:
        finish();
    }
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, loop_body);
        let goto_id =
            cfg_node_id_for_text(&result, loop_body, CfgNodeKind::Statement, "goto inside;");
        let finish = cfg_node_id_for_text(&result, loop_body, CfgNodeKind::Statement, "finish();");
        assert!(result.edges.iter().all(|edge| edge.source != goto_id));
        assert!(!cfg_reaches(&result, goto_id, finish));

        let switch_body = r#"<?php
function run($value) {
    goto inside;
    switch ($value) {
        case 1:
inside:
            finish();
    }
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, switch_body);
        let goto_id =
            cfg_node_id_for_text(&result, switch_body, CfgNodeKind::Statement, "goto inside;");
        let finish =
            cfg_node_id_for_text(&result, switch_body, CfgNodeKind::Statement, "finish();");
        assert!(result.edges.iter().all(|edge| edge.source != goto_id));
        assert!(!cfg_reaches(&result, goto_id, finish));

        let exit_loop = r#"<?php
function run($ready) {
    while ($ready) {
        goto done;
    }
done:
    finish();
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, exit_loop);
        let goto_id =
            cfg_node_id_for_text(&result, exit_loop, CfgNodeKind::Statement, "goto done;");
        let label = cfg_node_id_for_text(&result, exit_loop, CfgNodeKind::Join, "done:");
        assert!(has_cfg_edge(&result, goto_id, label, CfgEdgeKind::Goto));
    }

    #[test]
    fn test_php_goto_exits_nested_finally_regions_inner_to_outer() {
        let source = r#"<?php
function run() {
    try {
        try {
            goto done;
        } finally {
            inner_cleanup();
        }
    } finally {
        outer_cleanup();
    }
done:
    finish();
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto done;");
        let label = cfg_node_id_for_text(&result, source, CfgNodeKind::Join, "done:");
        let inner = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text == "inner_cleanup();")
                    && has_cfg_edge(&result, goto_id, node.id, CfgEdgeKind::Normal)
            })
            .map(|node| node.id)
            .expect("goto continuation must execute inner finally first");
        let outer = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text == "outer_cleanup();")
                    && has_cfg_edge(&result, inner, node.id, CfgEdgeKind::Normal)
            })
            .map(|node| node.id)
            .expect("goto continuation must execute outer finally second");

        assert!(has_cfg_edge(&result, outer, label, CfgEdgeKind::Goto));
    }

    #[test]
    fn test_php_goto_may_enter_catch_and_still_executes_finally() {
        let source = r#"<?php
function run() {
    goto recovered;
    try {
        work();
    } catch (RuntimeException $error) {
recovered:
        recover();
    } finally {
        cleanup();
    }
    finish();
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, source);
        let goto_id =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto recovered;");
        let label = cfg_node_id_for_text(&result, source, CfgNodeKind::Join, "recovered:");
        let recover = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "recover();");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");
        let cleanup = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text == "cleanup();")
                    && has_cfg_edge(&result, recover, node.id, CfgEdgeKind::Normal)
            })
            .map(|node| node.id)
            .expect("catch path must execute finally");

        assert!(has_cfg_edge(&result, goto_id, label, CfgEdgeKind::Goto));
        assert!(has_cfg_edge(&result, label, recover, CfgEdgeKind::Normal));
        assert!(cfg_reaches(&result, cleanup, finish));
    }

    #[test]
    fn test_php_goto_cannot_cross_a_finally_clause_boundary() {
        let leaving = r#"<?php
function run() {
    try {
        work();
    } finally {
        goto done;
    }
done:
    finish();
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, leaving);
        let goto_id = cfg_node_id_for_text(&result, leaving, CfgNodeKind::Statement, "goto done;");
        let finish = cfg_node_id_for_text(&result, leaving, CfgNodeKind::Statement, "finish();");
        assert!(result.edges.iter().all(|edge| edge.source != goto_id));
        assert!(!cfg_reaches(&result, goto_id, finish));

        let entering = r#"<?php
function run() {
    goto inside;
    try {
        work();
    } finally {
inside:
        cleanup();
    }
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, entering);
        let goto_id =
            cfg_node_id_for_text(&result, entering, CfgNodeKind::Statement, "goto inside;");
        let cleanup = cfg_node_id_for_text(&result, entering, CfgNodeKind::Statement, "cleanup();");
        assert!(result.edges.iter().all(|edge| edge.source != goto_id));
        assert!(!cfg_reaches(&result, goto_id, cleanup));
    }

    #[test]
    fn test_php_goto_within_finally_keeps_cloned_paths_isolated() {
        let source = r#"<?php
function run($stop) {
    try {
        if ($stop) return;
        work();
    } finally {
        goto within;
        skipped();
within:
        cleanup();
    }
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, source);
        let goto_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text == "goto within;")
            })
            .map(|node| node.id)
            .collect();
        assert_eq!(goto_ids.len(), 2, "normal and return finally clones");

        let mut targets = HashSet::new();
        for goto_id in goto_ids {
            let target = result
                .edges
                .iter()
                .find(|edge| edge.source == goto_id && edge.kind == CfgEdgeKind::Goto)
                .map(|edge| edge.target)
                .expect("goto must resolve within its finally clone");
            assert!(result.nodes.iter().any(|node| {
                node.id == target
                    && node.kind == CfgNodeKind::Join
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text == "within:")
            }));
            targets.insert(target);
        }
        assert_eq!(
            targets.len(),
            2,
            "each finally clone must retain its own label target"
        );
    }

    #[test]
    fn test_php_abrupt_finally_overrides_outgoing_goto() {
        let source = r#"<?php
function run() {
    try {
        goto done;
    } finally {
        throw new RuntimeException("stop");
    }
done:
    finish();
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto done;");
        let finally_throw = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Throw)
            .map(|node| node.id)
            .expect("finally throw terminal");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");

        assert!(cfg_reaches(&result, goto_id, finally_throw));
        assert!(!cfg_reaches(&result, goto_id, finish));
    }

    #[test]
    fn test_conditional_goto_has_no_false_join_continuation() {
        let source = r#"int run(int fail) {
    if (fail) goto cleanup;
    work();
cleanup:
    finish();
    return 0;
}"#;
        let result = build_cfg_for_first_fn(Language::C, source);
        let goto_id =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto cleanup;");
        let work = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "work();");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");
        let outgoing: Vec<_> = result
            .edges
            .iter()
            .filter(|edge| edge.source == goto_id)
            .collect();

        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].kind, CfgEdgeKind::Goto);
        assert_eq!(outgoing[0].target, finish);
        assert!(!cfg_reaches(&result, goto_id, work));
    }

    #[test]
    fn test_unresolved_direct_goto_terminates_local_best_effort_path() {
        let source = "int run(void) { goto missing; skipped(); return 0; }";
        let result = build_cfg_for_first_fn(Language::C, source);
        let goto_id =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto missing;");
        let skipped = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "skipped();");

        assert!(
            result.edges.iter().all(|edge| edge.source != goto_id),
            "an unresolved label must not invent a continuation"
        );
        assert!(!cfg_reaches(&result, goto_id, skipped));
    }

    #[test]
    fn test_csharp_goto_case_terminates_without_guessing_a_label_target() {
        let source = r#"class T {
  void Run(int value) {
    switch (value) {
      case 1:
        goto case 2;
        skipped();
      case 2:
        finish();
        break;
    }
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto case 2;");
        let skipped = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "skipped();");

        assert!(result.edges.iter().all(|edge| edge.source != goto_id));
        assert!(!cfg_reaches(&result, goto_id, skipped));
    }

    #[test]
    fn test_csharp_direct_goto_ignores_comment_before_labeled_body() {
        let source = r#"class T {
  void Run() {
    goto Done;
    skipped();
    Done:
    // label documentation
    if (ready()) finish();
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto Done;");
        let target = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Branch
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text.trim_start().starts_with("if (ready())"))
            })
            .expect("comment must not hide the labeled executable body");

        assert!(has_cfg_edge(&result, goto_id, target.id, CfgEdgeKind::Goto));
    }

    #[test]
    fn test_csharp_goto_exits_through_a_finally_region() {
        let source = r#"class T {
  void Run() {
    try {
      goto Done;
    } finally {
      cleanup();
    }
    Done: finish();
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto Done;");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");
        let cleanup = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "cleanup();");

        assert!(!has_cfg_edge(&result, goto_id, finish, CfgEdgeKind::Goto));
        assert!(has_cfg_edge(&result, goto_id, cleanup, CfgEdgeKind::Normal));
        assert!(has_cfg_edge(&result, cleanup, finish, CfgEdgeKind::Goto));
        assert!(cfg_reaches(&result, goto_id, finish));
    }

    #[test]
    fn test_csharp_abrupt_finally_overrides_outgoing_goto() {
        let source = r#"class T {
  int Run() {
    try {
      goto Done;
    } finally {
      return 1;
    }
    Done: return 2;
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto Done;");
        let finally_return =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return 1;");
        let target_return = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return 2;");

        assert!(cfg_reaches(&result, goto_id, finally_return));
        assert!(!cfg_reaches(&result, goto_id, target_return));
    }

    #[test]
    fn test_csharp_goto_exits_through_a_using_region() {
        let source = r#"class T {
  void Run() {
    using (Resource resource = Open()) {
      goto Done;
    }
    Done: finish();
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto Done;");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");
        let block_exit = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::BlockExit && node.call_context == CallContext::CSharpUsing
            })
            .expect("goto must execute the using cleanup");

        assert!(!has_cfg_edge(&result, goto_id, finish, CfgEdgeKind::Goto));
        assert!(has_cfg_edge(
            &result,
            goto_id,
            block_exit.id,
            CfgEdgeKind::Normal
        ));
        assert!(has_cfg_edge(
            &result,
            block_exit.id,
            finish,
            CfgEdgeKind::Goto
        ));
        assert!(cfg_reaches(&result, goto_id, finish));
    }

    #[test]
    fn test_csharp_goto_within_a_using_region_does_not_cleanup_early() {
        let source = r#"class T {
  void Run() {
    using (Resource resource = Open()) {
      goto Done;
      skipped();
      Done: finish();
    }
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto Done;");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");

        assert!(has_cfg_edge(&result, goto_id, finish, CfgEdgeKind::Goto));
        assert!(result.edges.iter().all(|edge| {
            edge.source != goto_id
                || !result
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.target && node.kind == CfgNodeKind::BlockExit)
        }));
    }

    #[test]
    fn test_csharp_goto_exits_nested_cleanup_regions_inner_to_outer() {
        let source = r#"class T {
  void Run() {
    try {
      using (Resource resource = Open()) {
        goto Done;
      }
    } finally {
      cleanup();
    }
    Done: finish();
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto Done;");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");
        let using_exit = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::BlockExit && node.call_context == CallContext::CSharpUsing
            })
            .expect("inner using cleanup");
        let cleanup = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text == "cleanup();")
                    && has_cfg_edge(&result, node.id, finish, CfgEdgeKind::Goto)
            })
            .map(|node| node.id)
            .expect("outer finally cleanup on the goto continuation");

        assert!(has_cfg_edge(
            &result,
            goto_id,
            using_exit.id,
            CfgEdgeKind::Normal
        ));
        assert!(has_cfg_edge(
            &result,
            using_exit.id,
            cleanup,
            CfgEdgeKind::Normal
        ));
        assert!(has_cfg_edge(&result, cleanup, finish, CfgEdgeKind::Goto));
    }

    #[test]
    fn test_csharp_goto_cannot_enter_a_using_region() {
        let source = r#"class T {
  void Run() {
    goto Inside;
    using (Resource resource = Open()) {
      Inside: finish();
    }
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto Inside;");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");

        assert!(result.edges.iter().all(|edge| edge.source != goto_id));
        assert!(!cfg_reaches(&result, goto_id, finish));
    }

    #[test]
    fn test_csharp_goto_cannot_leave_a_finally_clause() {
        let source = r#"class T {
  void Run() {
    try {
      work();
    } finally {
      goto Done;
    }
    Done: finish();
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto Done;");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");

        assert!(result.edges.iter().all(|edge| edge.source != goto_id));
        assert!(!cfg_reaches(&result, goto_id, finish));
    }

    #[test]
    fn test_csharp_goto_within_finally_keeps_cloned_paths_isolated() {
        let source = r#"class T {
  void Run(bool stop) {
    try {
      if (stop) return;
      work();
    } finally {
      goto Within;
      skipped();
      Within: cleanup();
    }
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text == "goto Within;")
            })
            .map(|node| node.id)
            .collect();
        assert_eq!(goto_ids.len(), 2, "normal and return finally clones");

        let mut targets = HashSet::new();
        for goto_id in goto_ids {
            let target = result
                .edges
                .iter()
                .find(|edge| edge.source == goto_id && edge.kind == CfgEdgeKind::Goto)
                .map(|edge| edge.target)
                .expect("goto must resolve within its finally clone");
            assert!(result.nodes.iter().any(|node| {
                node.id == target
                    && node.kind == CfgNodeKind::Statement
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text == "cleanup();")
            }));
            targets.insert(target);
        }
        assert_eq!(
            targets.len(),
            2,
            "each finally clone must retain its own label target"
        );
    }

    #[test]
    fn test_csharp_direct_goto_resolves_with_unrelated_cleanup_regions() {
        let source = r#"class T {
  void Run(bool skip) {
    if (skip) goto Done;
    using (Resource resource = Open()) {
      work();
    }
    Done: finish();
  }
}"#;
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let goto_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "goto Done;");
        let finish = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "finish();");

        assert!(has_cfg_edge(&result, goto_id, finish, CfgEdgeKind::Goto));
    }

    #[test]
    fn test_go_defer_executes_registered_calls_in_lifo_order() {
        let source = r#"package p
func run(a, b Closer) {
  defer a.Close()
  defer b.Close()
  work()
  return
}"#;
        let result = build_cfg_for_first_fn(Language::Go, source);
        let mut registrations: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                node.kind == CfgNodeKind::Statement && node.call_context == CallContext::GoDefer
            })
            .collect();
        registrations.sort_by_key(|node| node.managed_scope_start_byte);
        assert_eq!(registrations.len(), 2);

        let return_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return");
        let first_exit = result
            .edges
            .iter()
            .find(|edge| edge.source == return_id && edge.kind == CfgEdgeKind::Defer)
            .map(|edge| edge.target)
            .expect("return must enter the most recently registered defer");
        let first_exit_node = result
            .nodes
            .iter()
            .find(|node| node.id == first_exit)
            .expect("first defer execution node");
        assert_eq!(
            first_exit_node.managed_scope_start_byte, registrations[1].managed_scope_start_byte,
            "b.Close must execute before a.Close"
        );

        let second_exit = result
            .edges
            .iter()
            .find(|edge| edge.source == first_exit && edge.kind == CfgEdgeKind::Defer)
            .map(|edge| edge.target)
            .expect("the first execution must continue to the older defer");
        let second_exit_node = result
            .nodes
            .iter()
            .find(|node| node.id == second_exit)
            .expect("second defer execution node");
        assert_eq!(
            second_exit_node.managed_scope_start_byte,
            registrations[0].managed_scope_start_byte
        );
        assert!(result.edges.iter().any(|edge| {
            edge.source == second_exit
                && edge.kind == CfgEdgeKind::Normal
                && result
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.target && node.kind == CfgNodeKind::Exit)
        }));
        assert_cfg_edge_ids_match_payload(&result);
    }

    #[test]
    fn test_go_conditional_defer_does_not_cross_into_the_untaken_path() {
        let source = r#"package p
func run(ok bool, a Closer) {
  if ok {
    defer a.Close()
  }
  return
}"#;
        let result = build_cfg_for_first_fn(Language::Go, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("if branch");
        let true_target = result
            .edges
            .iter()
            .find(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::TrueBranch)
            .map(|edge| edge.target)
            .expect("true path");
        let false_target = result
            .edges
            .iter()
            .find(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::FalseBranch)
            .map(|edge| edge.target)
            .expect("false path");
        let reaches_defer_execution = |start| {
            let mut pending = vec![start];
            let mut visited = HashSet::new();
            while let Some(node_id) = pending.pop() {
                if !visited.insert(node_id) {
                    continue;
                }
                for edge in result.edges.iter().filter(|edge| edge.source == node_id) {
                    if edge.kind == CfgEdgeKind::Defer {
                        return true;
                    }
                    pending.push(edge.target);
                }
            }
            false
        };

        assert!(reaches_defer_execution(true_target));
        assert!(
            !reaches_defer_execution(false_target),
            "the false path must never execute a defer registered only in the true arm"
        );
        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Return)
                .count(),
            2,
            "the post-join continuation needs one identity per defer-stack state"
        );
    }

    #[test]
    fn test_go_loop_registered_defer_falls_back_atomically() {
        let source = r#"package p
func run(n int) {
  for i := 0; i < n; i++ {
    defer close(i)
  }
}"#;
        let result = build_cfg_for_first_fn(Language::Go, source);
        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|node| {
                    node.kind == CfgNodeKind::Statement && node.call_context == CallContext::GoDefer
                })
                .count(),
            1
        );
        assert!(
            result
                .nodes
                .iter()
                .all(|node| node.kind != CfgNodeKind::BlockExit),
            "an unbounded loop defer must not retain a partial execution stack"
        );
        assert!(
            result
                .edges
                .iter()
                .all(|edge| edge.kind != CfgEdgeKind::Defer),
            "atomic fallback must keep only the annotated base CFG"
        );
    }

    #[test]
    fn test_ruby_loop_next_and_break_use_control_edges() {
        let source = r#"def run(flag, skip)
  while flag
    if skip
      next
    end
    break
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let next = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "next");
        let break_node = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break");
        let loop_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Loop)
            .expect("while Loop")
            .id;

        assert!(has_cfg_edge(&result, next, loop_id, CfgEdgeKind::Continue));
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.source == break_node && edge.kind == CfgEdgeKind::Break)
        );
    }

    #[test]
    fn test_kotlin_and_cangjie_jump_break_is_not_return() {
        let kotlin_source = r#"fun run(flag: Boolean) {
    while (flag) {
        break
    }
}"#;
        let kotlin = build_cfg_for_first_fn(Language::Kotlin, kotlin_source);
        assert!(
            !kotlin
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Return)
        );
        assert!(
            kotlin
                .edges
                .iter()
                .any(|edge| edge.kind == CfgEdgeKind::Break)
        );

        let cangjie_source = r#"func run(): Unit {
    while (isReady()) {
        break
    }
}"#;
        let cangjie = build_cfg_for_cangjie_function(cangjie_source, "functionDefinition");
        assert!(
            !cangjie
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Return)
        );
        assert!(
            cangjie
                .edges
                .iter()
                .any(|edge| edge.kind == CfgEdgeKind::Break)
        );
    }

    #[test]
    fn test_kotlin_and_cangjie_jump_throw_is_not_return() {
        let kotlin = build_cfg_for_first_fn(
            Language::Kotlin,
            "fun fail() { throw RuntimeException(\"boom\") }",
        );
        assert_eq!(
            kotlin
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Throw)
                .count(),
            1
        );
        assert!(
            !kotlin
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Return)
        );

        let cangjie = build_cfg_for_cangjie_function(
            "func fail(): Unit { throw Exception(\"boom\") }",
            "functionDefinition",
        );
        assert_eq!(
            cangjie
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Throw)
                .count(),
            1
        );
        assert!(
            !cangjie
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Return)
        );
    }

    #[test]
    fn test_switch_cfg_java_colon_group_falls_through() {
        let method = "void dispatch(int x) { switch (x) { case 1: first(); case 2: second(); break; default: fallback(); } }";
        let source = format!("class T{{ {method} }}");
        let result = build_cfg_for_java_method(method);
        let first = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "first();");
        let second = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "second();");
        assert!(has_cfg_edge(&result, first, second, CfgEdgeKind::Normal));
    }

    #[test]
    fn test_switch_cfg_c_and_php_implicit_fallthrough() {
        let c_source = "void dispatch(int x) { switch (x) { case 1: first(); case 2: second(); break; default: fallback(); } }";
        let c_cfg = build_cfg_for_first_fn(Language::C, c_source);
        let c_first = cfg_node_id_for_text(&c_cfg, c_source, CfgNodeKind::Statement, "first();");
        let c_second = cfg_node_id_for_text(&c_cfg, c_source, CfgNodeKind::Statement, "second();");
        assert!(has_cfg_edge(&c_cfg, c_first, c_second, CfgEdgeKind::Normal));

        let php_source = "<?php function dispatch($x) { switch ($x) { case 1: first(); case 2: second(); break; default: fallback(); } }";
        let php_cfg = build_cfg_for_first_fn(Language::Php, php_source);
        let php_first =
            cfg_node_id_for_text(&php_cfg, php_source, CfgNodeKind::Statement, "first();");
        let php_second =
            cfg_node_id_for_text(&php_cfg, php_source, CfgNodeKind::Statement, "second();");
        assert!(has_cfg_edge(
            &php_cfg,
            php_first,
            php_second,
            CfgEdgeKind::Normal
        ));
    }

    #[test]
    fn test_switch_cfg_c_empty_trailing_case_falls_out_after_default() {
        let source = r#"void dispatch(int x) {
  switch (x) {
  default:
    fallback();
    break;
  case 1:
  }
  after();
}"#;
        let result = build_cfg_for_first_fn(Language::C, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("C switch Branch");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("C switch Join");

        assert!(has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::CaseBranch
        ));
    }

    #[test]
    fn test_php_multilevel_break_and_continue_resolve_across_switch() {
        let source = r#"<?php
function dispatch($x) {
    while ($x > 0) {
        switch ($x) {
            case 1:
                break 2;
            case 2:
                continue 2;
            default:
                break;
        }
        inside();
    }
    after();
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, source);
        let break_two = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break 2;");
        let continue_two =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "continue 2;");
        let after = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after();");
        let loop_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Loop)
            .expect("while Loop")
            .id;

        let break_edge = result
            .edges
            .iter()
            .find(|edge| edge.source == break_two)
            .expect("break 2 continuation");
        assert_eq!(break_edge.kind, CfgEdgeKind::Break);
        assert!(cfg_reaches(&result, break_edge.target, after));
        assert!(has_cfg_edge(
            &result,
            continue_two,
            loop_id,
            CfgEdgeKind::Continue
        ));
    }

    /// A TypeScript switch with 3 cases + default should produce one dispatch
    /// edge per arm, explicit Break edges, and no synthetic no-match edge.
    #[test]
    fn test_switch_cfg_ts() {
        let result = build_cfg_for_fn_ts(
            "function f(x: number) {
               switch (x) {
                 case 1: a(); break;
                 case 2: b(); break;
                 case 3: c(); break;
                 default: d();
               }
             }",
        );
        let has_branch = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch);
        let has_join = result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join);
        assert!(has_branch, "Expected Branch node for switch");
        assert!(has_join, "Expected Join node for switch");
        let branch = result
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Branch)
            .unwrap();
        // Find CaseBranch edges out of the Branch node
        let cb_count = result
            .edges
            .iter()
            .filter(|e| e.source == branch.id && e.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert!(
            cb_count >= 4,
            "Expected >= 4 CaseBranch edges (3 cases + default), got {cb_count}"
        );
        // Ensure each case body was connected to Join
        let join = result
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Join)
            .unwrap();
        let normal_to_join = result
            .edges
            .iter()
            .filter(|e| e.target == join.id && e.kind == CfgEdgeKind::Normal)
            .count();
        assert!(
            normal_to_join >= 1,
            "Expected the default tail to reach Join normally, got {normal_to_join}"
        );
        let break_to_join = result
            .edges
            .iter()
            .filter(|edge| edge.target == join.id && edge.kind == CfgEdgeKind::Break)
            .count();
        assert!(
            break_to_join >= 3,
            "Expected three explicit breaks into Join, got {break_to_join}"
        );
    }

    /// C switch with 3 cases + default.
    #[test]
    fn test_switch_cfg_c() {
        let result = build_cfg_for_first_fn(
            Language::C,
            "void f(int x) {
               switch (x) {
                 case 1: free(a); break;
                 case 2: g(); break;
                 case 3: h(); break;
                 default: i();
               }
             }",
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Expected Branch for C switch"
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Expected Join for C switch"
        );
        let branch = result
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Branch)
            .unwrap();
        let cb_count = result
            .edges
            .iter()
            .filter(|e| e.source == branch.id && e.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert!(
            cb_count >= 4,
            "Expected >= 4 CaseBranch edges for C switch (3 cases + default)"
        );
    }

    /// Java switch_expression with 3 cases.
    #[test]
    fn test_switch_cfg_java() {
        let result = build_cfg_for_java_method(
            "void f(int x) {
               switch (x) {
                 case 1: a(); break;
                 case 2: b(); break;
                 case 3: c(); break;
                 default: d();
               }
             }",
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Expected Branch for Java switch"
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Expected Join for Java switch"
        );
        let branch = result
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Branch)
            .unwrap();
        let cb_count = result
            .edges
            .iter()
            .filter(|e| e.source == branch.id && e.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert!(
            cb_count >= 4,
            "Expected 3 cases + default for Java switch, got {cb_count}"
        );
    }

    /// Go switch with 2 cases + default.
    #[test]
    fn test_switch_cfg_go() {
        let result = build_cfg_for_first_fn(
            Language::Go,
            "func f(x int) {
               switch x {
               case 1: a()
               case 2: b()
               default: c()
               }
             }",
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Branch),
            "Expected Branch for Go switch"
        );
        assert!(
            result.nodes.iter().any(|n| n.kind == CfgNodeKind::Join),
            "Expected Join for Go switch"
        );
        let branch = result
            .nodes
            .iter()
            .find(|n| n.kind == CfgNodeKind::Branch)
            .unwrap();
        let cb_count = result
            .edges
            .iter()
            .filter(|e| e.source == branch.id && e.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert!(
            cb_count >= 3,
            "Expected 2 cases + default for Go switch, got {cb_count}"
        );
    }

    #[test]
    fn test_match_cfg_cangjie() {
        let result = build_cfg_for_cangjie_function(
            r#"func dispatch(command: String): Unit {
    match (command) {
        case "list" | "ls" => listVersion()
        case "install" => install()
        case _ => unknown()
    }
}"#,
            "functionDefinition",
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Expected Branch for Cangjie match");
        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Join),
            "Expected Join for Cangjie match"
        );
        let case_edges = result
            .edges
            .iter()
            .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert_eq!(
            case_edges, 3,
            "An unguarded Cangjie wildcard arm suppresses the synthetic no-match edge"
        );
    }

    #[test]
    fn test_conditionless_match_cfg_cangjie_preserves_wildcard_body() {
        let result = build_cfg_for_cangjie_function(
            r#"func dispatch(): Unit {
    match {
        case isReady() => handle()
        case _ => fallback()
    }
}"#,
            "functionDefinition",
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Expected Branch for conditionless Cangjie match");
        let case_edges = result
            .edges
            .iter()
            .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert_eq!(
            case_edges, 2,
            "A conditionless Cangjie wildcard arm is exhaustive"
        );
    }

    #[test]
    fn test_match_cfg_cangjie_empty_block_reaches_join_as_case_path() {
        let result = build_cfg_for_cangjie_function(
            r#"func dispatch(command: Int64): Unit {
    match (command) {
        case 1 => {}
        case _ => fallback()
    }
}"#,
            "functionDefinition",
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Cangjie match Branch");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("Cangjie match Join");

        assert!(has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::CaseBranch
        ));
    }

    #[test]
    fn test_match_cfg_cangjie_guarded_wildcard_keeps_no_match_path() {
        let result = build_cfg_for_cangjie_function(
            r#"func dispatch(command: String): Unit {
    match (command) {
        case _ where isReady() => ready()
        case "install" => install()
    }
}"#,
            "functionDefinition",
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Expected Branch for guarded Cangjie wildcard");
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
                .count(),
            3,
            "A guarded wildcard is not exhaustive"
        );
    }

    #[test]
    fn test_match_cfg_rust_through_expression_statement() {
        let result = build_cfg_for_first_fn(
            Language::Rust,
            r#"fn dispatch(command: i32) {
    match command {
        n if n > 0 => positive(n),
        0 => zero(),
        _ => fallback(),
    };
}"#,
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Expected Branch for Rust match expression statement");
        let case_edges = result
            .edges
            .iter()
            .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert_eq!(
            case_edges, 3,
            "An unguarded Rust wildcard arm makes the synthetic no-match path impossible"
        );
    }

    #[test]
    fn test_match_cfg_rust_empty_arm_reaches_join_as_case_path() {
        let source = r#"fn dispatch(command: i32) {
    match command {
        1 => {},
        _ => fallback(),
    };
    after();
}"#;
        let result = build_cfg_for_first_fn(Language::Rust, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Rust match Branch");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("Rust match Join");

        assert!(has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::CaseBranch
        ));
        assert!(!has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::Normal
        ));
    }

    #[test]
    fn test_match_cfg_rust_guarded_wildcard_keeps_no_match_path() {
        let result = build_cfg_for_first_fn(
            Language::Rust,
            r#"fn dispatch(command: i32) {
    match command {
        _ if ready() => guarded(),
        0 => zero(),
    };
}"#,
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Expected Branch for guarded Rust match");
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
                .count(),
            3,
            "A wildcard with a guard is not exhaustive"
        );
    }

    #[test]
    fn test_if_cfg_rust_through_expression_statement() {
        let result = build_cfg_for_first_fn(
            Language::Rust,
            r#"fn check(flag: bool) {
    if flag { yes() } else { no() };
}"#,
        );
        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Branch),
            "Expected Branch for wrapped Rust if expression"
        );
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.kind == CfgEdgeKind::TrueBranch),
            "Expected TrueBranch for wrapped Rust if expression"
        );
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.kind == CfgEdgeKind::FalseBranch),
            "Expected FalseBranch for wrapped Rust if expression"
        );
    }

    #[test]
    fn test_rust_try_operator_preserves_success_and_residual_exit_paths() {
        let source = r#"fn load() -> Result<(), Error> {
    let value = open()?;
    consume(value);
    Ok(())
}"#;
        let result = build_cfg_for_first_fn(Language::Rust, source);
        let propagation = cfg_node_id_for_text(
            &result,
            source,
            CfgNodeKind::Statement,
            "let value = open()?;",
        );
        let consume =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "consume(value);");
        let exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Exit)
            .expect("Rust CFG Exit")
            .id;

        assert!(has_cfg_edge(
            &result,
            propagation,
            consume,
            CfgEdgeKind::Normal
        ));
        assert!(has_cfg_edge(
            &result,
            propagation,
            exit,
            CfgEdgeKind::Normal
        ));
    }

    #[test]
    fn test_rust_try_operator_in_condition_has_residual_exit_path() {
        let source = r#"fn check() -> Result<(), Error> {
    if ready()? {
        work();
    }
    after();
    Ok(())
}"#;
        let result = build_cfg_for_first_fn(Language::Rust, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Rust if Branch");
        let exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Exit)
            .expect("Rust CFG Exit");

        assert!(has_cfg_edge(
            &result,
            branch.id,
            exit.id,
            CfgEdgeKind::Normal
        ));
    }

    #[test]
    fn test_rust_try_operator_in_match_and_loop_headers_has_residual_exit_path() {
        let fixtures = [
            (
                r#"fn dispatch() -> Result<(), Error> {
    match load()? {
        Some(value) => consume(value),
        None => idle(),
    }
    Ok(())
}"#,
                CfgNodeKind::Branch,
            ),
            (
                r#"fn drain() -> Result<(), Error> {
    while ready()? {
        work();
    }
    Ok(())
}"#,
                CfgNodeKind::Loop,
            ),
        ];

        for (source, control_kind) in fixtures {
            let result = build_cfg_for_first_fn(Language::Rust, source);
            let control = result
                .nodes
                .iter()
                .find(|node| node.kind == control_kind)
                .unwrap_or_else(|| panic!("Rust {control_kind:?}"));
            let exit = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Exit)
                .expect("Rust CFG Exit");
            assert!(has_cfg_edge(
                &result,
                control.id,
                exit.id,
                CfgEdgeKind::Normal
            ));
        }
    }

    #[test]
    fn test_rust_try_operator_inside_closure_does_not_exit_enclosing_function() {
        let fixtures = [
            (
                r#"fn make_callback() {
    let callback = || -> Result<(), Error> { work()?; Ok(()) };
    register(callback);
}"#,
                "let callback = || -> Result<(), Error> { work()?; Ok(()) };",
            ),
            (
                r#"fn make_future() {
    let future = async { work().await?; Ok::<(), Error>(()) };
    spawn(future);
}"#,
                "let future = async { work().await?; Ok::<(), Error>(()) };",
            ),
        ];

        for (source, statement_text) in fixtures {
            let result = build_cfg_for_first_fn(Language::Rust, source);
            let nested_callable =
                cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, statement_text);
            let exit = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Exit)
                .expect("Rust CFG Exit");

            assert!(!has_cfg_edge(
                &result,
                nested_callable,
                exit.id,
                CfgEdgeKind::Normal
            ));
        }
    }

    #[test]
    fn test_rust_try_operator_in_explicit_return_does_not_duplicate_exit_edge() {
        let source = r#"fn load() -> Result<Value, Error> {
    return open()?;
}"#;
        let result = build_cfg_for_first_fn(Language::Rust, source);
        let return_id =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return open()?");
        let exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Exit)
            .expect("Rust CFG Exit")
            .id;

        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == return_id && edge.target == exit)
                .count(),
            1
        );
    }

    #[test]
    fn test_rust_let_else_keeps_success_and_diverging_alternative_paths_separate() {
        let source = r#"fn take(value: Option<i32>) -> i32 {
    let Some(inner) = value else {
        return 0;
    };
    consume(inner);
    1
}"#;
        let result = build_cfg_for_first_fn(Language::Rust, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Rust let-else Branch")
            .id;
        let alternative_return =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return 0");
        let consume =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "consume(inner);");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("Rust let-else success Join")
            .id;

        assert!(has_cfg_edge(
            &result,
            branch,
            alternative_return,
            CfgEdgeKind::FalseBranch
        ));
        assert!(has_cfg_edge(&result, branch, join, CfgEdgeKind::TrueBranch));
        assert!(has_cfg_edge(&result, join, consume, CfgEdgeKind::Normal));
        assert!(!has_cfg_edge(
            &result,
            alternative_return,
            join,
            CfgEdgeKind::Normal
        ));
    }

    #[test]
    fn test_rust_let_else_value_try_operator_adds_a_third_residual_path() {
        let source = r#"fn take() -> Result<i32, Error> {
    let Some(inner) = load()? else {
        return Ok(0);
    };
    Ok(inner)
}"#;
        let result = build_cfg_for_first_fn(Language::Rust, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Rust let-else Branch")
            .id;
        let alternative_return =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return Ok(0)");
        let exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Exit)
            .expect("Rust CFG Exit")
            .id;

        assert!(has_cfg_edge(
            &result,
            branch,
            alternative_return,
            CfgEdgeKind::FalseBranch
        ));
        assert!(has_cfg_edge(&result, branch, exit, CfgEdgeKind::Normal));
        assert_eq!(
            result
                .nodes
                .iter()
                .find(|node| node.id == branch)
                .and_then(|node| {
                    source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                }),
            Some("load()?")
        );
    }

    #[test]
    fn test_rust_unconditional_loop_in_let_else_cannot_reach_success_join() {
        let source = r#"fn take(value: Option<i32>) {
    let Some(inner) = value else {
        loop {
            wait();
        }
    };
    consume(inner);
}"#;
        let result = build_cfg_for_first_fn(Language::Rust, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Rust let-else Branch")
            .id;
        let alternative_loop = result
            .edges
            .iter()
            .find(|edge| edge.source == branch && edge.kind == CfgEdgeKind::FalseBranch)
            .expect("Rust let-else alternative")
            .target;
        let success_join = result
            .edges
            .iter()
            .find(|edge| edge.source == branch && edge.kind == CfgEdgeKind::TrueBranch)
            .expect("Rust let-else success")
            .target;
        let consume =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "consume(inner);");

        assert_eq!(
            result
                .nodes
                .iter()
                .find(|node| node.id == alternative_loop)
                .map(|node| node.kind),
            Some(CfgNodeKind::Loop)
        );
        assert!(!cfg_reaches(&result, alternative_loop, success_join));
        assert!(cfg_reaches(&result, success_join, consume));
    }

    #[test]
    fn test_rust_builtin_panic_macro_in_let_else_is_abrupt() {
        let source = r#"fn take(value: Option<i32>) {
    let Some(inner) = value else {
        panic!("missing value");
    };
    consume(inner);
}"#;
        let result = build_cfg_for_first_fn(Language::Rust, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Rust let-else Branch")
            .id;
        let panic_node = cfg_node_id_for_text(
            &result,
            source,
            CfgNodeKind::Throw,
            "panic!(\"missing value\")",
        );
        let success_join = result
            .edges
            .iter()
            .find(|edge| edge.source == branch && edge.kind == CfgEdgeKind::TrueBranch)
            .expect("Rust let-else success")
            .target;
        let exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Exit)
            .expect("Rust CFG Exit")
            .id;

        assert!(has_cfg_edge(
            &result,
            branch,
            panic_node,
            CfgEdgeKind::FalseBranch
        ));
        assert!(has_cfg_edge(&result, panic_node, exit, CfgEdgeKind::Normal));
        assert!(!cfg_reaches(&result, panic_node, success_join));
    }

    #[test]
    fn test_rust_only_known_builtin_diverging_macros_terminate_the_path() {
        for macro_name in ["panic", "unreachable", "todo", "unimplemented"] {
            let source = format!("fn stop() {{\n    {macro_name}!(\"reason\");\n    after();\n}}");
            let result = build_cfg_for_first_fn(Language::Rust, &source);
            let terminal = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Throw)
                .unwrap_or_else(|| panic!("{macro_name}! must be terminal"));
            assert!(result.nodes.iter().all(|node| {
                source.get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                    != Some("after();")
            }));
            let exit = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Exit)
                .expect("Rust CFG Exit");
            assert!(has_cfg_edge(
                &result,
                terminal.id,
                exit.id,
                CfgEdgeKind::Normal
            ));
        }

        let source = "fn keep_going() { custom_fail!(); after(); }";
        let result = build_cfg_for_first_fn(Language::Rust, source);
        let custom =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "custom_fail!();");
        let after = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after();");
        assert!(has_cfg_edge(&result, custom, after, CfgEdgeKind::Normal));
    }

    #[test]
    fn test_comments_are_not_materialized_as_cfg_statements() {
        let fixtures = [
            (
                Language::C,
                "void f(void) { // before\n work(); /* between */ done(); }",
            ),
            (
                Language::Java,
                "class T { void f() { // before\n work(); /* between */ done(); } }",
            ),
            (
                Language::Python,
                "def f():\n    # before\n    work()\n    # between\n    done()\n",
            ),
            (
                Language::Rust,
                "fn f() { // before\n work(); /* between */ done(); }",
            ),
        ];

        for (language, source) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let statements: Vec<_> = result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Statement)
                .collect();
            assert_eq!(statements.len(), 2, "{language:?}: {statements:?}");
            assert!(statements.iter().all(|node| {
                source
                    .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                    .is_some_and(|text| !text.trim_start().starts_with(['/', '#']))
            }));
        }
    }

    #[test]
    fn test_comment_only_function_connects_entry_directly_to_exit() {
        let source = "fn documented() {\n    // no executable statement\n}";
        let result = build_cfg_for_first_fn(Language::Rust, source);
        let entry = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Entry)
            .expect("CFG Entry")
            .id;
        let exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Exit)
            .expect("CFG Exit")
            .id;

        assert_eq!(result.nodes.len(), 2);
        assert!(has_cfg_edge(&result, entry, exit, CfgEdgeKind::Normal));
    }

    #[test]
    fn test_match_cfg_python_with_guard_and_capture_pattern() {
        let result = build_cfg_for_first_fn(
            Language::Python,
            r#"def dispatch(command):
    match command:
        case value if value > 0:
            positive(value)
        case 0:
            zero()
        case _:
            fallback()
"#,
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Expected Branch for Python match statement");
        let case_edges = result
            .edges
            .iter()
            .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert_eq!(
            case_edges, 3,
            "An unguarded Python wildcard case makes the synthetic no-match path impossible"
        );
    }

    #[test]
    fn test_match_cfg_python_guarded_wildcard_keeps_no_match_path() {
        let result = build_cfg_for_first_fn(
            Language::Python,
            r#"def dispatch(command):
    match command:
        case _ if ready():
            guarded()
        case 0:
            zero()
"#,
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Expected Branch for guarded Python match");
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
                .count(),
            3,
            "A wildcard with a guard is not exhaustive"
        );
    }

    #[test]
    fn test_match_cfg_python_irrefutable_capture_patterns_suppress_no_match_path() {
        let fixtures = [
            "value",
            "_ as whole",
            "(value)",
            "[value] | value",
            "Color.RED | _",
        ];

        for pattern in fixtures {
            let source = format!(
                "def dispatch(command):\n    match command:\n        case {pattern}:\n            consume(command)\n"
            );
            let result = build_cfg_for_first_fn(Language::Python, &source);
            let branch = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Branch)
                .unwrap_or_else(|| panic!("Python match Branch for {pattern}"));
            assert_eq!(
                result
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch
                    })
                    .count(),
                1,
                "irrefutable pattern {pattern:?} must not retain synthetic no-match"
            );
        }
    }

    #[test]
    fn test_match_cfg_python_refutable_or_guarded_patterns_keep_no_match_path() {
        let fixtures = [
            "value if ready(value)",
            "Color.RED",
            "(Color.RED)",
            "value,",
            "[*rest]",
        ];

        for pattern in fixtures {
            let source = format!(
                "def dispatch(command):\n    match command:\n        case {pattern}:\n            consume(command)\n"
            );
            let result = build_cfg_for_first_fn(Language::Python, &source);
            let branch = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Branch)
                .unwrap_or_else(|| panic!("Python match Branch for {pattern}"));
            assert_eq!(
                result
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch
                    })
                    .count(),
                2,
                "refutable/guarded pattern {pattern:?} must retain synthetic no-match"
            );
        }
    }

    #[test]
    fn test_ruby_case_in_has_sibling_paths_and_implicit_no_match_throw() {
        let source = r#"def dispatch(value)
  case value
  in [head, *tail]
    consume(head)
  in {kind: "ready"}
    ready()
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Ruby case/in Branch");
        let consume =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "consume(head)");
        let ready = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "ready()");
        let implicit_throw = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Throw)
            .expect("case/in without an exhaustive arm raises on no match");
        let exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Exit)
            .expect("method Exit");

        for target in [consume, ready, implicit_throw.id] {
            assert!(has_cfg_edge(
                &result,
                branch.id,
                target,
                CfgEdgeKind::CaseBranch
            ));
        }
        assert!(has_cfg_edge(
            &result,
            implicit_throw.id,
            exit.id,
            CfgEdgeKind::Normal
        ));
    }

    #[test]
    fn test_ruby_case_in_irrefutable_capture_suppresses_only_unguarded_no_match() {
        for (pattern, expect_throw) in [("value", false), ("_", false), ("value if ready", true)] {
            let source = format!(
                "def dispatch(input)\n  case input\n  in {pattern}\n    consume(input)\n  end\nend\n"
            );
            let result = build_cfg_for_first_fn(Language::Ruby, &source);
            assert_eq!(
                result
                    .nodes
                    .iter()
                    .any(|node| node.kind == CfgNodeKind::Throw),
                expect_throw,
                "pattern {pattern:?}"
            );
        }
    }

    #[test]
    fn test_ruby_case_in_implicit_no_match_throw_reaches_rescue() {
        let source = r#"def dispatch(value)
  begin
    case value
    in 0
      zero()
    end
  rescue NoMatchingPatternError
    recover()
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let implicit_throw = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Throw)
            .expect("case/in implicit no-match Throw");
        let recover = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "recover()");

        assert!(has_cfg_edge(
            &result,
            implicit_throw.id,
            recover,
            CfgEdgeKind::Exception
        ));
    }

    #[test]
    fn test_non_break_owning_sibling_constructs_propagate_break_to_enclosing_loop() {
        let fixtures = [
            (
                Language::Ruby,
                r#"def run(active, value)
  while active
    case value
    when 0
      break
    else
      work()
    end
    after_case()
  end
  after_loop()
end
"#,
                "after_case()",
                "after_loop()",
            ),
            (
                Language::Python,
                r#"def run(active, value):
    while active:
        match value:
            case 0:
                break
            case _:
                work()
        after_match()
    after_loop()
"#,
                "after_match()",
                "after_loop()",
            ),
            (
                Language::Rust,
                r#"fn run(active: bool, value: i32) {
    while active {
        match value {
            0 => break,
            _ => work(),
        }
        after_match();
    }
    after_loop();
}
"#,
                "after_match();",
                "after_loop();",
            ),
            (
                Language::Kotlin,
                r#"fun run(active: Boolean, value: Int) {
    while (active) {
        when (value) {
            0 -> break
            else -> work()
        }
        afterWhen()
    }
    afterLoop()
}
"#,
                "afterWhen()",
                "afterLoop()",
            ),
            (
                Language::Cangjie,
                r#"func run(value: Int64): Unit {
    while (isReady()) {
        match (value) {
            case 0 => break
            case _ => work()
        }
        afterMatch()
    }
    afterLoop()
}
"#,
                "afterMatch()",
                "afterLoop()",
            ),
        ];

        for (language, source, after_sibling_text, after_loop_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let break_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break");
            let after_sibling =
                cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, after_sibling_text);
            let after_loop =
                cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, after_loop_text);
            let sibling_join = result
                .nodes
                .iter()
                .find(|node| {
                    node.kind == CfgNodeKind::Join
                        && has_cfg_edge(&result, node.id, after_sibling, CfgEdgeKind::Normal)
                })
                .unwrap_or_else(|| panic!("{language:?} sibling Join"));
            let loop_join = result
                .nodes
                .iter()
                .find(|node| {
                    node.kind == CfgNodeKind::Join
                        && has_cfg_edge(&result, node.id, after_loop, CfgEdgeKind::Normal)
                })
                .unwrap_or_else(|| panic!("{language:?} loop Join"));

            assert_ne!(sibling_join.id, loop_join.id, "{language:?}");
            assert!(
                has_cfg_edge(&result, break_id, loop_join.id, CfgEdgeKind::Break),
                "{language:?} break must leave the enclosing loop"
            );
            assert!(
                !has_cfg_edge(&result, break_id, sibling_join.id, CfgEdgeKind::Break),
                "{language:?} match/case/when must not consume loop break"
            );
        }
    }

    #[test]
    fn test_when_cfg_kotlin_with_guard_and_else() {
        let result = build_cfg_for_first_fn(
            Language::Kotlin,
            r#"fun dispatch(command: Int) {
    when (command) {
        1 if command > 0 -> positive(command)
        0 -> zero()
        else -> fallback()
    }
}"#,
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Expected Branch for Kotlin when expression");
        let case_edges = result
            .edges
            .iter()
            .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert_eq!(
            case_edges, 3,
            "Expected three Kotlin when entries; else makes no-match impossible"
        );
    }

    #[test]
    fn test_when_cfg_kotlin_empty_block_reaches_join_as_case_path() {
        let result = build_cfg_for_first_fn(
            Language::Kotlin,
            r#"fun dispatch(command: Int) {
    when (command) {
        1 -> {}
        else -> fallback()
    }
}"#,
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Kotlin when Branch");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("Kotlin when Join");

        assert!(has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::CaseBranch
        ));
    }

    #[test]
    fn test_case_cfg_ruby_with_multiple_when_and_else() {
        let result = build_cfg_for_first_fn(
            Language::Ruby,
            r#"def dispatch(command)
  case command
  when "install", "add"
    install()
  when "remove"
    remove()
  else
    fallback()
  end
end
"#,
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Expected Branch for Ruby case expression");
        let case_edges = result
            .edges
            .iter()
            .filter(|edge| edge.source == branch.id && edge.kind == CfgEdgeKind::CaseBranch)
            .count();
        assert_eq!(
            case_edges, 3,
            "Expected two Ruby when clauses and else; else makes no-match impossible"
        );
    }

    #[test]
    fn test_case_cfg_ruby_empty_when_reaches_join_as_case_path() {
        let result = build_cfg_for_first_fn(
            Language::Ruby,
            r#"def dispatch(command)
  case command
  when "skip"
  else
    fallback()
  end
end
"#,
        );
        let branch = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Branch)
            .expect("Ruby case Branch");
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("Ruby case Join");

        assert!(has_cfg_edge(
            &result,
            branch.id,
            join.id,
            CfgEdgeKind::CaseBranch
        ));
    }

    #[test]
    fn test_cfg_php_branch_loop_switch_return_and_throw() {
        let result = build_cfg_for_first_fn(
            Language::Php,
            r#"<?php
function dispatch($command) {
    if ($command > 0) {
        positive();
    } else {
        fallback();
    }
    while ($command > 0) {
        tick();
        $command--;
    }
    foreach ([1, 2] as $item) {
        visit($item);
    }
    do {
        retry();
    } while ($command < 0);
    switch ($command) {
        case 1:
            install();
            break;
        default:
            unknown();
    }
    if ($command < 0) {
        throw new RuntimeException();
    }
    return $command;
}
"#,
        );

        assert!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Branch)
                .count()
                >= 3,
            "expected two if branches and one switch dispatch"
        );
        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Loop)
                .count(),
            3,
            "expected while, foreach, and do loops"
        );
        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Return),
            "expected PHP return node"
        );
        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Throw),
            "expected wrapped PHP throw expression"
        );
        assert!(
            result
                .edges
                .iter()
                .filter(|edge| edge.kind == CfgEdgeKind::CaseBranch)
                .count()
                >= 2,
            "expected case and default paths without an impossible no-match edge"
        );

        let exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Exit)
            .expect("expected PHP Exit node");
        for terminal in result
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, CfgNodeKind::Return | CfgNodeKind::Throw))
        {
            assert!(
                result.edges.iter().any(|edge| {
                    edge.source == terminal.id
                        && edge.target == exit.id
                        && edge.kind == CfgEdgeKind::Normal
                }),
                "terminal {:?} must connect to the function Exit",
                terminal.kind
            );
        }
    }

    #[test]
    fn test_cfg_php_elseif_chain_preserves_every_body() {
        let source = r#"<?php
function classify($value) {
    if ($value > 0) {
        positive();
    } elseif ($value === 0) {
        zero();
    } elseif ($value === -1) {
        minus_one();
    } else {
        fallback();
    }
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, source);
        let statements: Vec<&str> = result
            .nodes
            .iter()
            .filter(|node| node.kind == CfgNodeKind::Statement)
            .map(|node| {
                &source[node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize]
            })
            .collect();

        for expected in ["positive();", "zero();", "minus_one();", "fallback();"] {
            assert!(
                statements
                    .iter()
                    .any(|statement| statement.contains(expected)),
                "missing PHP elseif body statement {expected}; got {statements:?}"
            );
        }
        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Branch)
                .count(),
            3,
            "if plus two elseif conditions must remain distinct branches"
        );
    }

    #[test]
    fn test_cfg_php_alternative_control_syntax() {
        let result = build_cfg_for_first_fn(
            Language::Php,
            r#"<?php
function dispatch($value, $items) {
    if ($value > 0):
        positive();
    elseif ($value === 0):
        zero();
    else:
        fallback();
    endif;

    foreach ($items as $item):
        visit($item);
    endforeach;

    switch ($value):
        case 1:
            install();
            break;
        default:
            unknown();
    endswitch;
}
"#,
        );

        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Branch)
                .count(),
            3,
            "expected if, elseif, and switch branches in PHP alternative syntax"
        );
        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Loop)
                .count(),
            1,
            "expected foreach loop in PHP alternative syntax"
        );
        assert!(
            result
                .edges
                .iter()
                .filter(|edge| edge.kind == CfgEdgeKind::CaseBranch)
                .count()
                >= 2,
            "expected alternative switch case/default paths"
        );
    }

    #[test]
    fn test_try_catch_cfg_ts_without_finally() {
        let result = build_cfg_for_fn_ts(
            "function f() {
               try { risky(); } catch(e) { handle(e); }
               after();
             }",
        );
        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Branch)
                .count(),
            1,
            "try/catch dispatch should produce one Branch"
        );
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.kind == CfgEdgeKind::Exception),
            "catch body must be reachable through an Exception edge"
        );
        assert!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::Statement)
                .count()
                >= 3,
            "try body, catch body, and following statement must remain visible"
        );
    }

    #[test]
    fn test_managed_resource_return_paths_have_isolated_block_exits_across_grammars() {
        let fixtures = [
            (
                Language::Java,
                "class App { int f(boolean flag) { try (Resource r = open()) { if (flag) return 1; work(); } after(); return 0; } }",
                "return 1;",
                "work();",
                "after();",
            ),
            (
                Language::CSharp,
                "class App { int F(bool flag) { using (var r = Open()) { if (flag) return 1; Work(); } After(); return 0; } }",
                "return 1;",
                "Work();",
                "After();",
            ),
            (
                Language::Python,
                "def f(flag):\n    with open('x') as resource:\n        if flag:\n            return 1\n        work()\n    after()\n    return 0\n",
                "return 1",
                "work()",
                "after()",
            ),
            (
                Language::Kotlin,
                "fun f(flag: Boolean): Int { open().use { if (flag) { return 1 }; work() }; after(); return 0 }",
                "return 1",
                "work()",
                "after()",
            ),
            (
                Language::Ruby,
                "def f(flag)\n  File.open('x') do |resource|\n    if flag\n      return 1\n    end\n    work()\n  end\n  after()\n  return 0\nend\n",
                "return 1",
                "work()",
                "after()",
            ),
        ];

        for (language, source, return_text, work_text, after_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let return_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, return_text);
            let work_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, work_text);
            let after_id =
                cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, after_text);
            let block_exits: Vec<_> = result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::BlockExit)
                .map(|node| node.id)
                .collect();

            assert_eq!(
                block_exits.len(),
                2,
                "{language:?}: normal and return paths need isolated BlockExit nodes"
            );
            assert!(
                block_exits
                    .iter()
                    .any(|exit| cfg_reaches(&result, return_id, *exit)),
                "{language:?}: return must execute managed cleanup"
            );
            assert!(
                block_exits
                    .iter()
                    .any(|exit| cfg_reaches(&result, work_id, *exit)),
                "{language:?}: normal path must execute managed cleanup"
            );
            assert!(cfg_reaches(&result, work_id, after_id), "{language:?}");
            assert!(
                !cfg_reaches(&result, return_id, after_id),
                "{language:?}: return path must not cross into normal continuation"
            );
        }
    }

    #[test]
    fn test_managed_resource_throw_resumes_outer_handler_after_block_exit() {
        let fixtures = [
            (
                Language::Java,
                "class App { void f() { try { try (Resource r = open()) { throw new Error(); } } catch (Error error) { handle(); } } }",
                "throw new Error();",
                Some("handle();"),
            ),
            (
                Language::CSharp,
                "class App { void F() { try { using (var r = Open()) { throw new Error(); } } catch (Error error) { Handle(); } } }",
                "throw new Error();",
                Some("Handle();"),
            ),
            (
                Language::Python,
                "def f():\n    try:\n        with open('x') as resource:\n            raise Error()\n    except Error:\n        handle()\n",
                "raise Error()",
                Some("handle()"),
            ),
            (
                Language::Kotlin,
                "fun f() { try { open().use { throw Error() } } catch (error: Error) { handle() } }",
                "throw Error()",
                Some("handle()"),
            ),
            (
                Language::Ruby,
                "def f\n  File.open('x') do |resource|\n    raise Error\n  end\nend\n",
                "raise Error",
                None,
            ),
        ];

        for (language, source, throw_text, handler_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let throw_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Throw, throw_text);
            let block_exit = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::BlockExit)
                .unwrap_or_else(|| panic!("{language:?}: missing BlockExit"))
                .id;
            assert!(has_cfg_edge(
                &result,
                throw_id,
                block_exit,
                CfgEdgeKind::Normal
            ));

            if let Some(handler_text) = handler_text {
                let handler =
                    cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, handler_text);
                assert!(has_cfg_edge(
                    &result,
                    block_exit,
                    handler,
                    CfgEdgeKind::Exception
                ));
                assert!(!has_cfg_edge(
                    &result,
                    throw_id,
                    handler,
                    CfgEdgeKind::Exception
                ));
            }
        }
    }

    #[test]
    fn test_managed_cleanup_exception_reaches_handler_across_grammars() {
        let fixtures = [
            (
                Language::Java,
                "class App { void f(boolean stop) { try (Resource resource = open()) { if (stop) return; work(); } catch (Error error) { handle(); } after(); } }",
                "handle();",
            ),
            (
                Language::CSharp,
                "class App { void F(bool stop) { try { using (var resource = Open()) { if (stop) return; Work(); } } catch (Error error) { Handle(); } After(); } }",
                "Handle();",
            ),
            (
                Language::Python,
                "def f(stop):\n    try:\n        with open('x') as resource:\n            if stop:\n                return\n            work()\n    except Error:\n        handle()\n    after()\n",
                "handle()",
            ),
            (
                Language::Kotlin,
                "fun f(stop: Boolean) { try { open().use { if (stop) { return }; work() } } catch (error: Error) { handle() }; after() }",
                "handle()",
            ),
            (
                Language::Ruby,
                "def f(stop)\n  begin\n    File.open('x') do |resource|\n      if stop\n        return\n      end\n      work()\n    end\n  rescue Error\n    handle()\n  end\n  after()\nend\n",
                "handle()",
            ),
        ];

        for (language, source, handler_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let handler =
                cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, handler_text);
            let block_exits: Vec<_> = result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::BlockExit)
                .map(|node| node.id)
                .collect();
            assert_eq!(
                block_exits.len(),
                2,
                "{language:?}: normal and return exits"
            );
            for block_exit in block_exits {
                assert!(
                    has_cfg_edge(&result, block_exit, handler, CfgEdgeKind::Exception),
                    "{language:?}: cleanup on every completion may enter the lexical handler"
                );
            }
        }
    }

    #[test]
    fn test_nested_cleanup_exception_runs_outer_managed_exit_before_handler() {
        let method_source = "void f() { try (Outer outer = openOuter()) { try (Inner inner = openInner()) { work(); } afterInner(); } catch (Error error) { handle(); } after(); }";
        let source = format!("class T{{ {method_source} }}");
        let result = build_cfg_for_java_method(method_source);
        let handler = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "handle();");
        let after_inner =
            cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "afterInner();");
        let mut owners: Vec<_> = result
            .nodes
            .iter()
            .filter_map(|node| {
                (node.kind == CfgNodeKind::BlockExit)
                    .then_some(node.managed_scope_start_byte)
                    .flatten()
            })
            .collect();
        owners.sort_unstable();
        owners.dedup();
        assert_eq!(owners.len(), 2, "inner and outer resource owners");
        let outer_owner = owners[0];
        let inner_owner = owners[1];
        let inner_exit = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::BlockExit
                    && node.managed_scope_start_byte == Some(inner_owner)
            })
            .expect("inner managed exit")
            .id;
        let outer_exits: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                node.kind == CfgNodeKind::BlockExit
                    && node.managed_scope_start_byte == Some(outer_owner)
            })
            .map(|node| node.id)
            .collect();

        assert_eq!(
            outer_exits.len(),
            2,
            "normal and propagated-exception outer exits"
        );
        assert!(has_cfg_edge(
            &result,
            inner_exit,
            after_inner,
            CfgEdgeKind::Normal
        ));
        let propagated_exit = outer_exits
            .iter()
            .copied()
            .find(|outer_exit| has_cfg_edge(&result, inner_exit, *outer_exit, CfgEdgeKind::Normal))
            .expect("inner cleanup exception must enter a distinct outer cleanup exit");
        assert!(has_cfg_edge(
            &result,
            propagated_exit,
            handler,
            CfgEdgeKind::Exception
        ));
        assert!(!has_cfg_edge(
            &result,
            inner_exit,
            handler,
            CfgEdgeKind::Exception
        ));
    }

    #[test]
    fn test_cleanup_exception_can_enter_empty_handler() {
        let method_source = "void f() { try (Resource resource = open()) { return; } catch (Error error) { } after(); }";
        let source = format!("class T{{ {method_source} }}");
        let result = build_cfg_for_java_method(method_source);
        let block_exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::BlockExit)
            .expect("managed exit")
            .id;
        let join = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Join)
            .expect("try join")
            .id;
        let after = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "after();");

        assert!(has_cfg_edge(
            &result,
            block_exit,
            join,
            CfgEdgeKind::Exception
        ));
        assert!(has_cfg_edge(&result, join, after, CfgEdgeKind::Normal));
    }

    #[test]
    fn test_managed_resource_break_and_continue_execute_distinct_block_exits() {
        let source = "def f(flag, stop, skip):\n    while flag:\n        with open('x') as resource:\n            if stop:\n                break\n            if skip:\n                continue\n            work()\n        inside()\n    after()\n";
        let result = build_cfg_for_first_fn(Language::Python, source);
        let break_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break");
        let continue_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "continue");
        let inside_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "inside()");
        let block_exits: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.kind == CfgNodeKind::BlockExit)
            .map(|node| node.id)
            .collect();
        assert_eq!(block_exits.len(), 3, "normal, break, and continue paths");

        let break_exit = block_exits
            .iter()
            .copied()
            .find(|exit| has_cfg_edge(&result, break_id, *exit, CfgEdgeKind::Normal))
            .expect("break must execute BlockExit");
        let continue_exit = block_exits
            .iter()
            .copied()
            .find(|exit| has_cfg_edge(&result, continue_id, *exit, CfgEdgeKind::Normal))
            .expect("continue must execute BlockExit");
        assert_ne!(break_exit, continue_exit);
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.source == break_exit && edge.kind == CfgEdgeKind::Break)
        );
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.source == continue_exit && edge.kind == CfgEdgeKind::Continue)
        );
        assert!(!cfg_reaches(&result, break_id, inside_id));
        let continue_outgoing: Vec<_> = result
            .edges
            .iter()
            .filter(|edge| edge.source == continue_exit)
            .collect();
        assert_eq!(continue_outgoing.len(), 2, "continue or cleanup throw");
        let continue_edge = continue_outgoing
            .iter()
            .find(|edge| edge.kind == CfgEdgeKind::Continue)
            .expect("successful cleanup must preserve continue");
        assert_ne!(continue_edge.target, inside_id);
        assert!(continue_outgoing.iter().any(|edge| {
            edge.kind == CfgEdgeKind::Normal
                && result
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.target && node.kind == CfgNodeKind::Exit)
        }));
    }

    #[test]
    fn test_cleanup_throw_alternative_does_not_duplicate_function_exit_edge() {
        let result =
            build_cfg_for_java_method("void f() { try (Resource resource = open()) { work(); } }");
        let block_exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::BlockExit)
            .expect("managed exit")
            .id;
        let exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Exit)
            .expect("function exit")
            .id;

        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == block_exit && edge.target == exit)
                .count(),
            1,
            "normal completion and cleanup throw share the unique function Exit"
        );
    }

    #[test]
    fn test_cleanup_exception_in_handler_does_not_reenter_same_handler() {
        let method_source = r#"void f() {
            try { throw new Error(); }
            catch (Error error) {
                try (Resource resource = open()) { work(); }
            }
            after();
        }"#;
        let source = format!("class T{{ {method_source} }}");
        let result = build_cfg_for_java_method(method_source);
        let body_throw =
            cfg_node_id_for_text(&result, &source, CfgNodeKind::Throw, "throw new Error();");
        let handler = result
            .edges
            .iter()
            .find(|edge| edge.source == body_throw && edge.kind == CfgEdgeKind::Exception)
            .expect("body throw must enter catch handler")
            .target;
        let block_exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::BlockExit)
            .expect("managed exit in catch body")
            .id;

        assert!(
            !has_cfg_edge(&result, block_exit, handler, CfgEdgeKind::Exception),
            "an exception raised while executing a handler must propagate outward"
        );
    }

    #[test]
    fn test_ruby_resource_block_break_and_next_resume_after_yielding_call() {
        let source = r#"def f(flag, stop, skip)
  while flag
    File.open('x') do |resource|
      if stop
        break
      end
      if skip
        next
      end
      work()
    end
    inside()
    break
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let break_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                node.kind == CfgNodeKind::Statement
                    && source.get(range).is_some_and(|text| text.trim() == "break")
            })
            .map(|node| node.id)
            .collect();
        assert_eq!(break_ids.len(), 2, "block break and outer-loop break");
        let next_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "next");
        let inside_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "inside()");
        let after_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after()");
        let block_exits: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.kind == CfgNodeKind::BlockExit)
            .map(|node| node.id)
            .collect();
        assert_eq!(block_exits.len(), 3, "normal, break, and next block exits");

        let block_break = break_ids
            .iter()
            .copied()
            .find(|break_id| {
                block_exits
                    .iter()
                    .any(|exit| has_cfg_edge(&result, *break_id, *exit, CfgEdgeKind::Normal))
            })
            .expect("block break must execute managed cleanup");
        let outer_break = break_ids
            .iter()
            .copied()
            .find(|break_id| *break_id != block_break)
            .expect("outer-loop break");

        assert!(cfg_reaches(&result, block_break, inside_id));
        assert!(cfg_reaches(&result, next_id, inside_id));
        assert!(!cfg_reaches(&result, outer_break, inside_id));
        assert!(cfg_reaches(&result, outer_break, after_id));
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.source == outer_break && edge.kind == CfgEdgeKind::Break)
        );
        assert!(!result.edges.iter().any(|edge| {
            block_exits.contains(&edge.source)
                && matches!(edge.kind, CfgEdgeKind::Break | CfgEdgeKind::Continue)
        }));
    }

    #[test]
    fn test_ruby_redo_restarts_the_current_loop_body_without_rechecking_condition() {
        let source = r#"def run(done)
  while ready?
    work()
    redo unless done
    tail()
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let redo = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "redo");
        let work = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "work()");
        let loop_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Loop)
            .map(|node| node.id)
            .expect("while loop");

        assert!(has_cfg_edge(&result, redo, work, CfgEdgeKind::Redo));
        assert!(
            result
                .edges
                .iter()
                .any(|edge| { edge.target == redo && edge.kind == CfgEdgeKind::FalseBranch })
        );
        assert!(!has_cfg_edge(&result, redo, loop_id, CfgEdgeKind::Continue));
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == redo)
                .count(),
            1,
            "redo is an abrupt transfer with no lexical fallthrough"
        );
    }

    #[test]
    fn test_ruby_modifier_loop_checks_condition_before_plain_body() {
        let source = r#"def run(ready)
  work() while ready
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let entry = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Entry)
            .map(|node| node.id)
            .expect("method entry");
        let loop_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Loop)
            .map(|node| node.id)
            .expect("modifier while loop");
        let work = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "work()");
        let after = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after()");
        let join = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Join
                    && has_cfg_edge(&result, loop_id, node.id, CfgEdgeKind::Normal)
            })
            .map(|node| node.id)
            .expect("modifier loop join");

        assert!(has_cfg_edge(&result, entry, loop_id, CfgEdgeKind::Normal));
        assert!(has_cfg_edge(&result, loop_id, work, CfgEdgeKind::Normal));
        assert!(has_cfg_edge(&result, work, loop_id, CfgEdgeKind::LoopBack));
        assert!(has_cfg_edge(&result, join, after, CfgEdgeKind::Normal));
        assert!(!has_cfg_edge(&result, entry, work, CfgEdgeKind::Normal));
    }

    #[test]
    fn test_ruby_begin_modifier_loop_executes_body_before_first_condition() {
        let source = r#"def run(done)
  begin
    work()
  end until done
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let entry = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Entry)
            .map(|node| node.id)
            .expect("method entry");
        let loop_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Loop)
            .map(|node| node.id)
            .expect("post-test until loop");
        let work = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "work()");
        let after = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after()");
        let join = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Join
                    && has_cfg_edge(&result, loop_id, node.id, CfgEdgeKind::Normal)
            })
            .map(|node| node.id)
            .expect("modifier loop join");

        assert!(has_cfg_edge(&result, entry, work, CfgEdgeKind::Normal));
        assert!(has_cfg_edge(&result, work, loop_id, CfgEdgeKind::LoopBack));
        assert!(has_cfg_edge(&result, loop_id, work, CfgEdgeKind::Normal));
        assert!(has_cfg_edge(&result, join, after, CfgEdgeKind::Normal));
        assert!(!has_cfg_edge(&result, entry, loop_id, CfgEdgeKind::Normal));
    }

    #[test]
    fn test_ruby_begin_modifier_loop_resolves_next_redo_and_break() {
        let source = r#"def run(skip, again, stop)
  begin
    work()
    next if skip
    redo if again
    break if stop
    tail()
  end while ready?
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let loop_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Loop)
            .map(|node| node.id)
            .expect("post-test while loop");
        let work = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "work()");
        let next = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "next");
        let redo = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "redo");
        let break_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break");
        let tail = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "tail()");
        let join = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Join
                    && has_cfg_edge(&result, loop_id, node.id, CfgEdgeKind::Normal)
            })
            .map(|node| node.id)
            .expect("modifier loop join");

        assert!(has_cfg_edge(&result, next, loop_id, CfgEdgeKind::Continue));
        assert!(has_cfg_edge(&result, redo, work, CfgEdgeKind::Redo));
        assert!(has_cfg_edge(&result, break_id, join, CfgEdgeKind::Break));
        assert!(has_cfg_edge(&result, tail, loop_id, CfgEdgeKind::LoopBack));
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == redo)
                .count(),
            1,
            "redo must restart the body without checking the condition"
        );
    }

    #[test]
    fn test_ruby_redo_executes_inner_ensure_before_restarting_loop_body() {
        let source = r#"def run(again)
  while ready?
    begin
      redo if again
    ensure
      cleanup()
    end
    tail()
  end
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let redo = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "redo");
        let loop_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Loop)
            .map(|node| node.id)
            .expect("while loop");
        let body_entry = result
            .edges
            .iter()
            .find(|edge| {
                edge.source == loop_id
                    && result
                        .nodes
                        .iter()
                        .any(|node| node.id == edge.target && node.kind != CfgNodeKind::Join)
            })
            .map(|edge| edge.target)
            .expect("loop body entry");
        let cleanup = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text == "cleanup()")
                    && has_cfg_edge(&result, redo, node.id, CfgEdgeKind::Normal)
            })
            .map(|node| node.id)
            .expect("redo continuation must execute ensure");

        assert!(has_cfg_edge(
            &result,
            cleanup,
            body_entry,
            CfgEdgeKind::Redo
        ));
        assert!(!has_cfg_edge(&result, redo, body_entry, CfgEdgeKind::Redo));
    }

    #[test]
    fn test_ruby_abrupt_ensure_overrides_redo() {
        let source = r#"def run
  while ready?
    begin
      redo
    ensure
      return 1
    end
    tail()
  end
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let redo = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "redo");
        let ensure_return = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return 1");
        let tail = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "tail()");

        assert!(has_cfg_edge(
            &result,
            redo,
            ensure_return,
            CfgEdgeKind::Normal
        ));
        assert!(
            !result
                .edges
                .iter()
                .any(|edge| edge.kind == CfgEdgeKind::Redo)
        );
        assert!(!cfg_reaches(&result, redo, tail));
    }

    #[test]
    fn test_ruby_retry_restarts_rescued_begin_without_running_its_ensure_first() {
        let source = r#"def run(again)
  begin
    load()
  rescue
    recover()
    retry if again
  ensure
    cleanup()
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let retry = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "retry");
        let outgoing: Vec<_> = result
            .edges
            .iter()
            .filter(|edge| edge.source == retry)
            .collect();

        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].kind, CfgEdgeKind::Retry);
        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.id == outgoing[0].target && node.kind == CfgNodeKind::Branch)
        );
    }

    #[test]
    fn test_ruby_retry_executes_nested_ensure_but_bypasses_the_rescued_ensure() {
        let source = r#"def run
  begin
    load()
    raise Error
  rescue
    begin
      retry
    ensure
      inner_cleanup()
    end
  ensure
    outer_cleanup()
  end
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let retry = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "retry");
        let inner_cleanup = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize)
                        .is_some_and(|text| text == "inner_cleanup()")
                    && has_cfg_edge(&result, retry, node.id, CfgEdgeKind::Normal)
            })
            .map(|node| node.id)
            .expect("nested ensure on retry path");
        let restart = result
            .edges
            .iter()
            .find(|edge| edge.source == inner_cleanup && edge.kind == CfgEdgeKind::Retry)
            .expect("nested ensure tail must restart rescued begin");

        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.id == restart.target && node.kind == CfgNodeKind::Branch)
        );
        assert!(!result.edges.iter().any(|edge| {
            edge.source == inner_cleanup
                && edge.kind == CfgEdgeKind::Normal
                && result.nodes.iter().any(|node| {
                    node.id == edge.target
                        && source
                            .get(
                                node.stmt_range.start_byte as usize
                                    ..node.stmt_range.end_byte as usize,
                            )
                            .is_some_and(|text| text == "outer_cleanup()")
                })
        }));
    }

    #[test]
    fn test_ruby_resource_block_redo_stays_inside_but_retry_runs_cleanup() {
        let redo_source = r#"def run(again)
  File.open('data.txt') do |resource|
    work(resource)
    redo if again
    tail(resource)
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, redo_source);
        let redo = cfg_node_id_for_text(&result, redo_source, CfgNodeKind::Statement, "redo");
        let work = cfg_node_id_for_text(
            &result,
            redo_source,
            CfgNodeKind::Statement,
            "work(resource)",
        );
        assert!(has_cfg_edge(&result, redo, work, CfgEdgeKind::Redo));
        assert!(result.edges.iter().all(|edge| {
            edge.source != redo
                || !result
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.target && node.kind == CfgNodeKind::BlockExit)
        }));

        let retry_source = r#"def run
  begin
    load()
  rescue
    File.open('data.txt') do |resource|
      retry
    end
  end
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, retry_source);
        let retry = cfg_node_id_for_text(&result, retry_source, CfgNodeKind::Statement, "retry");
        let block_exit = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::BlockExit
                    && has_cfg_edge(&result, retry, node.id, CfgEdgeKind::Normal)
            })
            .map(|node| node.id)
            .expect("retry must run resource cleanup");
        assert!(
            result
                .edges
                .iter()
                .any(|edge| { edge.source == block_exit && edge.kind == CfgEdgeKind::Retry })
        );
    }

    #[test]
    fn test_csharp_using_expression_and_single_statement_body_are_structured() {
        let source =
            "class App { void F(bool flag) { using (Open()) if (flag) return; After(); } }";
        let result = build_cfg_for_first_fn(Language::CSharp, source);
        let return_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return;");
        let after_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "After();");
        let managed_resource = result.nodes.iter().find(|node| {
            node.call_context == CallContext::CSharpUsing && node.managed_scope_start_byte.is_some()
        });
        assert!(
            managed_resource.is_some(),
            "using expression must be marked"
        );
        assert_eq!(
            result
                .nodes
                .iter()
                .filter(|node| node.kind == CfgNodeKind::BlockExit)
                .count(),
            2
        );
        assert!(!cfg_reaches(&result, return_id, after_id));
    }

    #[test]
    fn test_managed_scope_clone_budget_rolls_back_region_atomically() {
        let cases = (0..=MAX_PATH_ISOLATED_CLONES_PER_REGION)
            .map(|value| format!("case {value}: return;"))
            .collect::<Vec<_>>()
            .join(" ");
        let method_source = format!(
            "void f(int value) {{ try (Resource resource = open()) {{ switch (value) {{ {cases} }} }} after(); }}"
        );
        let source = format!("class T{{ {method_source} }}");
        let result = build_cfg_for_java_method(&method_source);

        assert!(
            !result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Return),
            "an over-budget managed region must not retain partially lowered returns"
        );
        assert!(
            !result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::BlockExit),
            "an over-budget managed region must not retain partial cleanup clones"
        );
        assert!(
            !result
                .nodes
                .iter()
                .any(|node| node.managed_scope_start_byte.is_some()),
            "rollback must remove acquisition ownership metadata"
        );

        let opaque_try = result
            .nodes
            .iter()
            .find(|node| {
                if node.kind != CfgNodeKind::Statement {
                    return false;
                }
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim_start().starts_with("try ("))
            })
            .expect("the whole managed region must fall back to one opaque statement");
        let after = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "after();");
        assert!(has_cfg_edge(
            &result,
            opaque_try.id,
            after,
            CfgEdgeKind::Normal
        ));
    }

    #[test]
    fn test_java_try_with_resources_catch_runs_after_managed_exit() {
        let method_source = "void f() { try (Resource resource = open()) { throw new IOException(); } catch (IOException error) { handle(error); } after(); }";
        let source = format!("class T{{ {method_source} }}");
        let result = build_cfg_for_java_method(method_source);
        let throw_id = cfg_node_id_for_text(
            &result,
            &source,
            CfgNodeKind::Throw,
            "throw new IOException();",
        );
        let handler_id =
            cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "handle(error);");
        let block_exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::BlockExit)
            .expect("try-with-resources BlockExit")
            .id;

        assert!(has_cfg_edge(
            &result,
            throw_id,
            block_exit,
            CfgEdgeKind::Normal
        ));
        assert!(has_cfg_edge(
            &result,
            block_exit,
            handler_id,
            CfgEdgeKind::Exception
        ));
        assert!(!has_cfg_edge(
            &result,
            throw_id,
            handler_id,
            CfgEdgeKind::Exception
        ));
    }

    #[test]
    fn test_java_try_with_resources_finally_clones_after_each_managed_exit() {
        let method_source = "void f(boolean stop) { try (Resource resource = open()) { if (stop) return; work(); } finally { cleanup(); } after(); }";
        let source = format!("class T{{ {method_source} }}");
        let result = build_cfg_for_java_method(method_source);
        let return_id = cfg_node_id_for_text(&result, &source, CfgNodeKind::Return, "return;");
        let work_id = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "work();");
        let after_id = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "after();");
        let block_exits: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.kind == CfgNodeKind::BlockExit)
            .map(|node| node.id)
            .collect();
        let cleanup_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                if node.kind != CfgNodeKind::Statement {
                    return false;
                }
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim() == "cleanup();")
            })
            .map(|node| node.id)
            .collect();

        assert_eq!(block_exits.len(), 2, "normal and return managed exits");
        assert_eq!(
            cleanup_ids.len(),
            4,
            "each managed exit has cleanup-success and cleanup-throw finally paths"
        );
        assert!(block_exits.iter().any(|block_exit| {
            has_cfg_edge(&result, return_id, *block_exit, CfgEdgeKind::Normal)
                && cleanup_ids
                    .iter()
                    .any(|cleanup| cfg_reaches(&result, *block_exit, *cleanup))
        }));
        assert!(block_exits.iter().any(|block_exit| {
            cfg_reaches(&result, work_id, *block_exit)
                && cleanup_ids
                    .iter()
                    .any(|cleanup| cfg_reaches(&result, *block_exit, *cleanup))
        }));
        assert!(!cfg_reaches(&result, return_id, after_id));
        assert!(cfg_reaches(&result, work_id, after_id));
    }

    #[test]
    fn test_java_try_with_resources_catch_and_finally_preserve_continuations() {
        let method_source = "void f(boolean fail) { try (Resource resource = open()) { if (fail) throw new IOException(); work(); } catch (IOException error) { recover(error); } finally { cleanup(); } after(); }";
        let source = format!("class T{{ {method_source} }}");
        let result = build_cfg_for_java_method(method_source);
        let throw_id = cfg_node_id_for_text(
            &result,
            &source,
            CfgNodeKind::Throw,
            "throw new IOException();",
        );
        let recover_id =
            cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "recover(error);");
        let work_id = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "work();");
        let after_id = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "after();");
        let managed_throw_exit = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::BlockExit
                    && has_cfg_edge(&result, throw_id, node.id, CfgEdgeKind::Normal)
            })
            .expect("throw continuation managed exit")
            .id;
        let cleanup_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                if node.kind != CfgNodeKind::Statement {
                    return false;
                }
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim() == "cleanup();")
            })
            .map(|node| node.id)
            .collect();

        assert!(has_cfg_edge(
            &result,
            managed_throw_exit,
            recover_id,
            CfgEdgeKind::Exception
        ));
        assert!(
            cleanup_ids
                .iter()
                .any(|cleanup| cfg_reaches(&result, recover_id, *cleanup))
        );
        assert!(
            cleanup_ids
                .iter()
                .any(|cleanup| cfg_reaches(&result, work_id, *cleanup))
        );
        assert!(cfg_reaches(&result, recover_id, after_id));
        assert!(cfg_reaches(&result, work_id, after_id));
    }

    #[test]
    fn test_try_finally_clones_cleanup_per_continuation_without_path_crossover() {
        let source = r#"function f(flag: boolean) {
  try {
    if (flag) return;
    if (explode()) throw new Error();
    risky();
  } catch(e) {
    handle(e);
  } finally {
    cleanup();
  }
  after();
}"#;
        let result = build_cfg_for_fn_ts(source);
        let return_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return;");
        let risky_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "risky();");
        let handle_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "handle(e);");
        let after_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after();");
        let cleanup_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                if node.kind != CfgNodeKind::Statement {
                    return false;
                }
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim() == "cleanup();")
            })
            .map(|node| node.id)
            .collect();

        assert_eq!(
            cleanup_ids.len(),
            4,
            "normal, caught, explicit-return, and uncaught-dispatch continuations need isolated finally clones"
        );
        assert!(
            result
                .edges
                .iter()
                .any(|edge| edge.kind == CfgEdgeKind::Exception),
            "try/catch paths must remain explicit when finally is present"
        );
        assert!(cfg_reaches(&result, risky_id, after_id));
        assert!(cfg_reaches(&result, handle_id, after_id));
        assert!(
            !cfg_reaches(&result, return_id, after_id),
            "return continuation must execute finally and then exit, never cross into normal continuation"
        );
    }

    #[test]
    fn test_return_inside_finally_overrides_incoming_return_continuation() {
        let source = r#"function f() {
  try {
    return 1;
  } finally {
    return 2;
  }
  after();
}"#;
        let result = build_cfg_for_fn_ts(source);
        let returns: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.kind == CfgNodeKind::Return)
            .collect();
        let after_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after();");

        assert_eq!(returns.len(), 2);
        let outer_return = returns
            .iter()
            .find(|node| {
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim() == "return 1;")
            })
            .expect("missing incoming return");
        let finally_return = returns
            .iter()
            .find(|node| {
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim() == "return 2;")
            })
            .expect("missing overriding finally return");
        assert!(cfg_reaches(&result, outer_return.id, finally_return.id));
        assert!(!cfg_reaches(&result, outer_return.id, after_id));
    }

    #[test]
    fn test_ruby_ensure_clones_normal_return_and_rescue_continuations() {
        let source = r#"def f(flag, fail_now)
  begin
    if flag
      return 1
    end
    if fail_now
      raise Error
    end
    work()
  rescue Error
    recover()
  else
    success()
  ensure
    cleanup()
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let return_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return 1");
        let raise_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Throw, "raise Error");
        let work_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "work()");
        let recover_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "recover()");
        let success_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "success()");
        let after_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after()");
        let cleanup_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(range)
                        .is_some_and(|text| text.trim() == "cleanup()")
            })
            .map(|node| node.id)
            .collect();

        assert_eq!(
            cleanup_ids.len(),
            4,
            "normal, rescue, return, and throw paths"
        );
        assert!(cfg_reaches(&result, work_id, success_id));
        assert!(cfg_reaches(&result, success_id, after_id));
        assert!(cfg_reaches(&result, raise_id, recover_id));
        assert!(cfg_reaches(&result, recover_id, after_id));
        assert!(!cfg_reaches(&result, return_id, after_id));
        assert!(
            cleanup_ids
                .iter()
                .any(|cleanup| cfg_reaches(&result, return_id, *cleanup))
        );
    }

    #[test]
    fn test_ruby_method_body_ensure_and_overriding_return() {
        let source = r#"def f
  return 1
ensure
  return 2
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let incoming = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return 1");
        let overriding = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, "return 2");

        assert!(cfg_reaches(&result, incoming, overriding));
        assert_eq!(
            result
                .edges
                .iter()
                .filter(|edge| edge.source == incoming)
                .count(),
            1,
            "the incoming return must only continue into ensure"
        );
    }

    #[test]
    fn test_ruby_plain_begin_is_transparent_without_synthetic_join() {
        let source = r#"def f
  begin
    work()
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let work = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "work()");
        let after = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "after()");

        assert!(has_cfg_edge(&result, work, after, CfgEdgeKind::Normal));
        assert!(
            !result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Join),
            "plain begin/end is lexical grouping, not a synthetic branch"
        );
    }

    #[test]
    fn test_ruby_empty_rescue_does_not_execute_exception_pattern() {
        let source = r#"def f
  begin
    raise Error
  rescue Error
  ensure
    cleanup()
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let raise_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Throw, "raise Error");
        let cleanup_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(range)
                        .is_some_and(|text| text.trim() == "cleanup()")
            })
            .map(|node| node.id)
            .collect();

        assert!(
            cleanup_ids
                .iter()
                .any(|cleanup| cfg_reaches(&result, raise_id, *cleanup))
        );
        assert!(!result.nodes.iter().any(|node| {
            let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
            node.kind == CfgNodeKind::Statement
                && source.get(range).is_some_and(|text| text.trim() == "Error")
        }));
    }

    #[test]
    fn test_ruby_break_and_next_execute_isolated_ensure_clones() {
        let source = r#"def f(flag, stop, skip)
  while flag
    begin
      if stop
        break
      end
      if skip
        next
      end
      work()
    ensure
      cleanup()
    end
    inside()
  end
  after()
end
"#;
        let result = build_cfg_for_first_fn(Language::Ruby, source);
        let break_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break");
        let next_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "next");
        let inside_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "inside()");
        let cleanup_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(range)
                        .is_some_and(|text| text.trim() == "cleanup()")
            })
            .map(|node| node.id)
            .collect();

        assert_eq!(
            cleanup_ids.len(),
            3,
            "normal, break, and next continuations"
        );
        assert!(cleanup_ids.iter().any(|cleanup| has_cfg_edge(
            &result,
            break_id,
            *cleanup,
            CfgEdgeKind::Normal
        )));
        assert!(cleanup_ids.iter().any(|cleanup| has_cfg_edge(
            &result,
            next_id,
            *cleanup,
            CfgEdgeKind::Normal
        )));
        assert!(!cfg_reaches(&result, break_id, inside_id));
        assert!(
            result.edges.iter().any(|edge| {
                cleanup_ids.contains(&edge.source) && edge.kind == CfgEdgeKind::Break
            })
        );
        let next_cleanup = cleanup_ids
            .iter()
            .copied()
            .find(|cleanup| has_cfg_edge(&result, next_id, *cleanup, CfgEdgeKind::Normal))
            .expect("next must enter its own ensure clone");
        let outgoing: Vec<_> = result
            .edges
            .iter()
            .filter(|edge| edge.source == next_cleanup)
            .collect();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].kind, CfgEdgeKind::Continue);
        assert_ne!(outgoing[0].target, inside_id);
    }

    #[test]
    fn test_break_continuation_executes_finally_before_leaving_loop() {
        let source = r#"function f(flag: boolean) {
  while (flag) {
    try {
      if (stop()) break;
      work();
    } finally {
      cleanup();
    }
    inside();
  }
  after();
}"#;
        let result = build_cfg_for_fn_ts(source);
        let break_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "break;");
        let inside_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "inside();");
        let cleanup_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(range)
                        .is_some_and(|text| text.trim() == "cleanup();")
            })
            .map(|node| node.id)
            .collect();
        assert_eq!(cleanup_ids.len(), 2, "normal and break continuations");
        assert!(
            cleanup_ids
                .iter()
                .any(|cleanup| cfg_reaches(&result, break_id, *cleanup))
        );
        assert!(!cfg_reaches(&result, break_id, inside_id));
        assert!(
            result.edges.iter().any(|edge| {
                cleanup_ids.contains(&edge.source) && edge.kind == CfgEdgeKind::Break
            })
        );
    }

    #[test]
    fn test_continue_continuation_executes_finally_before_loop_back() {
        let source = r#"function f(flag: boolean) {
  while (flag) {
    try {
      if (skip()) continue;
      work();
    } finally {
      cleanup();
    }
    inside();
  }
}"#;
        let result = build_cfg_for_fn_ts(source);
        let continue_id =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "continue;");
        let inside_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "inside();");
        let cleanup_ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| {
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                node.kind == CfgNodeKind::Statement
                    && source
                        .get(range)
                        .is_some_and(|text| text.trim() == "cleanup();")
            })
            .map(|node| node.id)
            .collect();
        assert_eq!(cleanup_ids.len(), 2, "normal and continue continuations");
        let continue_cleanup = cleanup_ids
            .iter()
            .copied()
            .find(|cleanup| has_cfg_edge(&result, continue_id, *cleanup, CfgEdgeKind::Normal))
            .expect("continue must execute a finally clone");
        let outgoing: Vec<_> = result
            .edges
            .iter()
            .filter(|edge| edge.source == continue_cleanup)
            .collect();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].kind, CfgEdgeKind::Continue);
        assert_ne!(outgoing[0].target, inside_id);
    }

    #[test]
    fn test_throw_continuation_through_inner_finally_reaches_outer_catch() {
        let source = r#"function f() {
  try {
    try {
      throw new Error();
    } finally {
      cleanup();
    }
  } catch (error) {
    handle(error);
  }
}"#;
        let result = build_cfg_for_fn_ts(source);
        let throw_id =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Throw, "throw new Error();");
        let cleanup_id =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "cleanup();");
        let handle_id =
            cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "handle(error);");
        assert!(has_cfg_edge(
            &result,
            throw_id,
            cleanup_id,
            CfgEdgeKind::Normal
        ));
        assert!(has_cfg_edge(
            &result,
            cleanup_id,
            handle_id,
            CfgEdgeKind::Exception
        ));
        assert!(!has_cfg_edge(
            &result,
            throw_id,
            handle_id,
            CfgEdgeKind::Exception
        ));
    }

    #[test]
    fn test_try_finally_continuations_across_common_grammars() {
        let fixtures = [
            (
                Language::JavaScript,
                "function f(flag) { try { if (flag) return; work(); } finally { cleanup(); } after(); }",
                "return;",
                "work();",
                "cleanup();",
                "after();",
            ),
            (
                Language::ArkTS,
                "function f(flag: boolean): void { try { if (flag) return; work(); } finally { cleanup(); } after(); }",
                "return;",
                "work();",
                "cleanup();",
                "after();",
            ),
            (
                Language::Java,
                "class App { void f(boolean flag) { try { if (flag) return; work(); } finally { cleanup(); } after(); } }",
                "return;",
                "work();",
                "cleanup();",
                "after();",
            ),
            (
                Language::CSharp,
                "class App { void f(bool flag) { try { if (flag) return; work(); } finally { cleanup(); } after(); } }",
                "return;",
                "work();",
                "cleanup();",
                "after();",
            ),
            (
                Language::Php,
                "<?php function f($flag) { try { if ($flag) return; work(); } finally { cleanup(); } after(); }",
                "return;",
                "work();",
                "cleanup();",
                "after();",
            ),
            (
                Language::Python,
                "def f(flag):\n    try:\n        if flag:\n            return\n        work()\n    finally:\n        cleanup()\n    after()\n",
                "return",
                "work()",
                "cleanup()",
                "after()",
            ),
            (
                Language::Kotlin,
                "fun f(flag: Boolean) { try { if (flag) { return }; work() } finally { cleanup() }; after() }",
                "return",
                "work()",
                "cleanup()",
                "after()",
            ),
            (
                Language::Cangjie,
                "func f(flag: Bool): Unit { try { if (flag) { return } work() } finally { cleanup() } after() }",
                "return",
                "work()",
                "cleanup()",
                "after()",
            ),
        ];

        for (language, source, return_text, work_text, cleanup_text, after_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let return_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Return, return_text);
            let work_id = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, work_text);
            let after_id =
                cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, after_text);
            let cleanup_count = result
                .nodes
                .iter()
                .filter(|node| {
                    let range =
                        node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                    node.kind == CfgNodeKind::Statement
                        && source
                            .get(range)
                            .is_some_and(|text| text.trim() == cleanup_text)
                })
                .count();
            assert_eq!(cleanup_count, 2, "{language:?}: normal + return clones");
            assert!(cfg_reaches(&result, work_id, after_id), "{language:?}");
            assert!(!cfg_reaches(&result, return_id, after_id), "{language:?}");
        }
    }

    #[test]
    fn test_try_finally_clone_budget_rolls_back_region_atomically() {
        let cases = (0..=MAX_PATH_ISOLATED_CLONES_PER_REGION)
            .map(|value| format!("case {value}: return;"))
            .collect::<Vec<_>>()
            .join(" ");
        let source = format!(
            "function f(value: number) {{ try {{ switch (value) {{ {cases} }} }} finally {{ cleanup(); }} after(); }}"
        );
        let result = build_cfg_for_fn_ts(&source);

        assert!(
            !result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Return),
            "an over-budget region must not retain partially lowered returns"
        );
        assert!(
            !result.nodes.iter().any(|node| {
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim() == "cleanup();")
            }),
            "an over-budget region must not retain partial finally clones"
        );

        let opaque_try = result
            .nodes
            .iter()
            .find(|node| {
                if node.kind != CfgNodeKind::Statement {
                    return false;
                }
                let range = node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize;
                source
                    .get(range)
                    .is_some_and(|text| text.trim_start().starts_with("try {"))
            })
            .expect("over-budget try/finally must fall back to one opaque statement")
            .id;
        let after = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "after();");
        assert!(has_cfg_edge(
            &result,
            opaque_try,
            after,
            CfgEdgeKind::Normal
        ));
    }

    #[test]
    fn test_ruby_ensure_clone_budget_rolls_back_method_region_atomically() {
        let clauses = (0..=MAX_PATH_ISOLATED_CLONES_PER_REGION)
            .map(|value| format!("  when {value}\n    return {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let source =
            format!("def f(value)\n  case value\n{clauses}\n  end\nensure\n  cleanup()\nend\n");
        let result = build_cfg_for_first_fn(Language::Ruby, &source);

        assert!(
            !result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Return),
            "an over-budget ensure region must not retain partial returns"
        );
        assert!(
            !result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Branch),
            "rollback must remove the partially lowered case and ensure region"
        );
        let statements: Vec<_> = result
            .nodes
            .iter()
            .filter(|node| node.kind == CfgNodeKind::Statement)
            .collect();
        assert_eq!(statements.len(), 1, "the whole method region is opaque");
        let range = statements[0].stmt_range.start_byte as usize
            ..statements[0].stmt_range.end_byte as usize;
        assert!(
            source
                .get(range)
                .is_some_and(|text| text.trim_start().starts_with("case value"))
        );
    }

    #[test]
    fn test_try_catch_cfg_php_routes_exact_throw_to_first_guaranteed_handler() {
        let source = r#"<?php
function load($path) {
    try {
        if (!$path) {
            throw new RuntimeException("empty");
        }
        read_file($path);
    } catch (RuntimeException $error) {
        recover_runtime($error);
    } catch (Throwable $error) {
        recover_any($error);
    }
    return $path;
}
"#;
        let result = build_cfg_for_first_fn(Language::Php, source);
        let exception_edges = result
            .edges
            .iter()
            .filter(|edge| edge.kind == CfgEdgeKind::Exception)
            .count();
        assert_eq!(
            exception_edges, 3,
            "two dispatch paths plus one exact throw path"
        );
        let throw_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Throw)
            .expect("explicit PHP throw must remain a terminal node")
            .id;
        let runtime_handler = cfg_node_id_for_text(
            &result,
            source,
            CfgNodeKind::Statement,
            "recover_runtime($error);",
        );
        let catch_all_handler = cfg_node_id_for_text(
            &result,
            source,
            CfgNodeKind::Statement,
            "recover_any($error);",
        );
        assert!(has_cfg_edge(
            &result,
            throw_id,
            runtime_handler,
            CfgEdgeKind::Exception
        ));
        assert!(!has_cfg_edge(
            &result,
            throw_id,
            catch_all_handler,
            CfgEdgeKind::Exception
        ));
    }

    #[test]
    fn test_explicit_object_creation_stops_at_first_exact_handler() {
        let fixtures = [
            (
                Language::Java,
                "class App { void f() { try { throw new IOError(); } catch (IOError error) { exact(); } catch (Other error) { later(); } } }",
                "exact();",
                "later();",
            ),
            (
                Language::CSharp,
                "class App { void F() { try { throw new IOError(); } catch (IOError error) { Exact(); } catch (Other error) { Later(); } } }",
                "Exact();",
                "Later();",
            ),
            (
                Language::Php,
                "<?php function f() { try { throw new IOError(); } catch (IOError $error) { exact(); } catch (Other $error) { later(); } }",
                "exact();",
                "later();",
            ),
        ];

        for (language, source, exact_text, later_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let throw_id = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Throw)
                .unwrap_or_else(|| panic!("{language:?}: explicit Throw node"))
                .id;
            let exact = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, exact_text);
            let later = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, later_text);

            assert!(
                has_cfg_edge(&result, throw_id, exact, CfgEdgeKind::Exception),
                "{language:?}: exact handler"
            );
            assert!(
                !has_cfg_edge(&result, throw_id, later, CfgEdgeKind::Exception),
                "{language:?}: an unguarded exact handler makes later handlers unreachable for this explicit throw"
            );
        }
    }

    #[test]
    fn test_unknown_or_guarded_throw_type_keeps_later_handler_alternatives() {
        let fixtures = [
            (
                Language::Java,
                "class App { void f(Error error) { try { throw error; } catch (IOError caught) { first(); } catch (Other caught) { later(); } } }",
                "later();",
            ),
            (
                Language::CSharp,
                "class App { void F() { try { throw new IOError(); } catch (IOError error) when (ready()) { First(); } catch (Other error) { Later(); } } }",
                "Later();",
            ),
            (
                Language::Python,
                "def f():\n    try:\n        raise IOError()\n    except IOError:\n        first()\n    except Other:\n        later()\n",
                "later()",
            ),
            (
                Language::Kotlin,
                "fun f() { try { throw IOError() } catch (error: IOError) { first() } catch (error: Other) { later() } }",
                "later()",
            ),
            (
                Language::Cpp,
                "void f() { try { throw IOError(); } catch (const IOError& error) { first(); } catch (const Other& error) { later(); } }",
                "later();",
            ),
            (
                Language::Ruby,
                "def f\n  begin\n    raise IOError\n  rescue IOError => error\n    first()\n  rescue Other => error\n    later()\n  end\nend\n",
                "later()",
            ),
            (
                Language::Java,
                "class App { void f() { try { throw wrap(new IOError()); } catch (IOError error) { first(); } catch (Other error) { later(); } } }",
                "later();",
            ),
            (
                Language::CSharp,
                "class App { void F() { try { throw Wrap(new IOError()); } catch (IOError error) { First(); } catch (Other error) { Later(); } } }",
                "Later();",
            ),
            (
                Language::Php,
                "<?php function f() { try { throw wrap(new IOError()); } catch (IOError $error) { first(); } catch (Other $error) { later(); } }",
                "later();",
            ),
        ];

        for (language, source, later_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let throw_id = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Throw)
                .expect("explicit Throw node")
                .id;
            let later = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, later_text);
            assert!(
                has_cfg_edge(&result, throw_id, later, CfgEdgeKind::Exception),
                "{language:?}: unknown types and guarded exact handlers cannot prune later alternatives"
            );
        }
    }

    #[test]
    fn test_exact_handler_cutoff_preserves_earlier_unknown_inheritance_alternative() {
        let source = "class App { void f() { try { throw new pkg.IOError(); } catch (First error) { first(); } catch (pkg.IOError error) { exact(); } catch (Later error) { later(); } } }";
        let result = build_cfg_for_first_fn(Language::Java, source);
        let throw_id = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::Throw)
            .expect("explicit Throw node")
            .id;
        let first = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "first();");
        let exact = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "exact();");
        let later = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "later();");

        assert!(has_cfg_edge(
            &result,
            throw_id,
            first,
            CfgEdgeKind::Exception
        ));
        assert!(has_cfg_edge(
            &result,
            throw_id,
            exact,
            CfgEdgeKind::Exception
        ));
        assert!(!has_cfg_edge(
            &result,
            throw_id,
            later,
            CfgEdgeKind::Exception
        ));
    }

    #[test]
    fn test_exact_type_in_union_or_empty_handler_stops_later_handlers() {
        let fixtures = [
            (
                Language::Java,
                "class App { void f() { try { throw new IOError(); } catch (Other | IOError error) { } catch (Later error) { later(); } } }",
                "later();",
            ),
            (
                Language::Php,
                "<?php function f() { try { throw new IOError(); } catch (Other|IOError $error) { } catch (Later $error) { later(); } }",
                "later();",
            ),
            (
                Language::CSharp,
                "class App { void F() { try { throw new IOError(); } catch (IOError error) { } catch (Later error) { Later(); } } }",
                "Later();",
            ),
        ];

        for (language, source, later_text) in fixtures {
            let result = build_cfg_for_first_fn(language, source);
            let throw_id = result
                .nodes
                .iter()
                .find(|node| node.kind == CfgNodeKind::Throw)
                .expect("explicit Throw node")
                .id;
            let later = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, later_text);
            assert!(result.edges.iter().any(|edge| {
                edge.source == throw_id
                    && edge.kind == CfgEdgeKind::Exception
                    && result
                        .nodes
                        .iter()
                        .any(|node| node.id == edge.target && node.kind == CfgNodeKind::Join)
            }));
            assert!(!has_cfg_edge(
                &result,
                throw_id,
                later,
                CfgEdgeKind::Exception
            ));
        }
    }

    #[test]
    fn test_managed_exit_keeps_all_handler_alternatives_despite_body_throw_type() {
        let method_source = "void f() { try (Resource resource = open()) { throw new IOError(); } catch (IOError error) { exact(); } catch (Other error) { later(); } }";
        let source = format!("class T{{ {method_source} }}");
        let result = build_cfg_for_java_method(method_source);
        let block_exit = result
            .nodes
            .iter()
            .find(|node| node.kind == CfgNodeKind::BlockExit)
            .expect("managed exit")
            .id;
        let exact = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "exact();");
        let later = cfg_node_id_for_text(&result, &source, CfgNodeKind::Statement, "later();");

        assert!(has_cfg_edge(
            &result,
            block_exit,
            exact,
            CfgEdgeKind::Exception
        ));
        assert!(has_cfg_edge(
            &result,
            block_exit,
            later,
            CfgEdgeKind::Exception
        ));
    }

    #[test]
    fn test_finally_clone_keeps_all_handler_alternatives_despite_body_throw_type() {
        let source = "class App { void f() { try { try { throw new IOError(); } finally { cleanup(); } } catch (IOError error) { exact(); } catch (Other error) { later(); } } }";
        let result = build_cfg_for_first_fn(Language::Java, source);
        let cleanup = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "cleanup();");
        let exact = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "exact();");
        let later = cfg_node_id_for_text(&result, source, CfgNodeKind::Statement, "later();");

        assert!(has_cfg_edge(
            &result,
            cleanup,
            exact,
            CfgEdgeKind::Exception
        ));
        assert!(has_cfg_edge(
            &result,
            cleanup,
            later,
            CfgEdgeKind::Exception
        ));
    }

    #[test]
    fn test_try_except_else_cfg_python() {
        let source = r#"def load(path):
    try:
        if not path:
            raise ValueError("empty")
        read_file(path)
    except ValueError as error:
        recover(error)
    else:
        complete()
    return path
"#;
        let result = build_cfg_for_first_fn(Language::Python, source);
        assert!(
            result
                .edges
                .iter()
                .filter(|edge| edge.kind == CfgEdgeKind::Exception)
                .count()
                >= 2,
            "expected dispatch and explicit raise paths to the Python handler"
        );
        let complete = result
            .nodes
            .iter()
            .find(|node| {
                node.kind == CfgNodeKind::Statement
                    && source
                        [node.stmt_range.start_byte as usize..node.stmt_range.end_byte as usize]
                        .contains("complete()")
            })
            .expect("Python try/except/else missing else body");
        assert!(
            result
                .edges
                .iter()
                .any(|edge| { edge.target == complete.id && edge.kind == CfgEdgeKind::Normal }),
            "Python try else body must be reachable only from the normal path"
        );
    }

    #[test]
    fn test_try_catch_cfg_cpp_with_explicit_throw() {
        let result = build_cfg_for_first_fn(
            Language::Cpp,
            r#"void load(bool ready) {
    try {
        if (!ready) {
            throw Error();
        }
        read();
    } catch (const Error& error) {
        recover(error);
    }
}
"#,
        );
        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Throw),
            "C++ throw_statement must remain a Throw node"
        );
        assert!(
            result
                .edges
                .iter()
                .filter(|edge| edge.kind == CfgEdgeKind::Exception)
                .count()
                >= 2,
            "expected C++ try dispatch and explicit throw handler paths"
        );
    }

    #[test]
    fn test_try_catch_cfg_java_with_explicit_throw() {
        let result = build_cfg_for_java_method(
            r#"void load(boolean ready) {
    try {
        if (!ready) {
            throw new IllegalStateException();
        }
        read();
    } catch (IllegalStateException error) {
        recover(error);
    }
}"#,
        );
        assert!(
            result
                .nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Throw),
            "Java throw_statement must remain a Throw node"
        );
        assert!(
            result
                .edges
                .iter()
                .filter(|edge| edge.kind == CfgEdgeKind::Exception)
                .count()
                >= 2,
            "expected Java try dispatch and explicit throw handler paths"
        );
    }
}
