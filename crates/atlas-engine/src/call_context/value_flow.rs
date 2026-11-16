//! On-demand dependency tracing through selected recorded calls. All materialized
//! facts live in a disposable store; Ready indexes and source remain read-only.
use super::*;
use types::trace::{TraceDiagnostic, TracePath};

mod defaults;
mod dereferences;
mod fields;
mod returns;
mod writebacks;

#[derive(Debug, Clone)]
pub struct ValueSelection {
    pub path: String,
    pub range: std::ops::Range<u32>,
}
#[derive(Debug, Clone)]
pub struct ValueFlowOptions {
    pub max_depth: usize,
    pub max_paths: usize,
    pub max_functions: usize,
    /// Outer caller to inner callee, using exact recorded call/callee ranges.
    pub call_context: Vec<ValueSelection>,
}
#[derive(Debug, Clone)]
pub struct ValuePoint {
    pub location: ContextLocation,
    pub kind: String,
    pub name: Option<String>,
    pub call_context: Vec<ContextLocation>,
}
#[derive(Debug, Clone)]
pub struct ValueStep {
    pub from: ValuePoint,
    pub to: ValuePoint,
    pub kind: String,
}
#[derive(Debug, Clone)]
pub struct ValuePath {
    pub sink: ValuePoint,
    pub endpoint: ValuePoint,
    pub steps: Vec<ValueStep>,
}
#[derive(Debug, Default)]
pub struct ValueFlowResult {
    pub scopes: Vec<ContextLocation>,
    pub paths: Vec<ValuePath>,
    pub gaps: Vec<(ContextLocation, String, String, Vec<ContextLocation>)>,
    pub files_read: usize,
    pub bytes_read: usize,
    pub truncated: bool,
}
impl ValueFlowResult {
    fn gap(&mut self, at: ContextLocation, code: &str, message: impl Into<String>) {
        self.gaps.push((at, code.into(), message.into(), vec![]));
    }
}
fn location(file_id: FileId, range: TextRange) -> ContextLocation {
    ContextLocation { file_id, range }
}

struct Query<'a> {
    investigation: Investigation<'a>,
    temporary: Arc<Store>,
    loaded: BTreeSet<SymbolId>,
    attempted_calls: BTreeSet<CallsiteId>,
    uncertain_outputs: BTreeMap<DataNodeId, Vec<(ContextLocation, Option<ContextLocation>)>>,
    dereference_inputs: BTreeMap<DataNodeId, ContextLocation>,
    field_receivers: BTreeMap<DataNodeId, Vec<ContextLocation>>,
    attempted_outputs: BTreeSet<DataNodeId>,
    output_functions: BTreeSet<SymbolId>,
    return_conditions: returns::ReturnConditions,
    options: &'a ValueFlowOptions,
    result: ValueFlowResult,
    node_count: usize,
    edge_count: usize,
}

/// One recorded backward path per value role. Known call results are expanded
/// on demand; starting inside a callee needs an explicit context to select its
/// caller. Neither branches, heap identity nor execution are proved exhaustive.
pub fn trace_value(
    store: &Store,
    root: &Path,
    selection: &ValueSelection,
    options: &ValueFlowOptions,
    canceled: &dyn Fn() -> bool,
) -> anyhow::Result<ValueFlowResult> {
    anyhow::ensure!(
        (1..=100).contains(&options.max_depth)
            && (1..=32).contains(&options.max_paths)
            && (1..=16).contains(&options.max_functions)
            && options.call_context.len() <= 100,
        "invalid value flow limits"
    );
    anyhow::ensure!(
        selection.range.start < selection.range.end,
        "value range must not be empty"
    );
    anyhow::ensure!(!canceled(), "value query canceled");
    let temporary = Arc::new(Store::open_in_memory()?);
    temporary.init_schema()?;
    let mut query = Query {
        investigation: Investigation {
            store,
            root,
            canceled,
            parsed: BTreeMap::new(),
            result: CallContextResult::default(),
            symbol_static: BTreeMap::new(),
            reusable: None,
            admitted_bytes: 0,
        },
        temporary,
        loaded: BTreeSet::new(),
        attempted_calls: BTreeSet::new(),
        uncertain_outputs: BTreeMap::new(),
        dereference_inputs: BTreeMap::new(),
        field_receivers: BTreeMap::new(),
        attempted_outputs: BTreeSet::new(),
        output_functions: BTreeSet::new(),
        return_conditions: returns::ReturnConditions::default(),
        options,
        result: ValueFlowResult::default(),
        node_count: 0,
        edge_count: 0,
    };
    query.run(selection)?;
    query.investigation.check()?;
    query.result.files_read = query.investigation.result.files_read;
    query.result.bytes_read = query.investigation.result.bytes_read;
    Ok(query.result)
}

impl Query<'_> {
    // Keep established storage exposure separate from an unknown call effect.
    // The latter is a located limit on dependent paths, not a new definition.
    fn classify_call_outputs(
        &mut self,
        facts: &mut FileFacts,
        parsed: &ParsedSource,
    ) -> anyhow::Result<()> {
        let mut excluded = BTreeSet::new();
        let mut types = BTreeMap::new();
        let mut uncertain = BTreeMap::new();
        for node in facts
            .data_nodes
            .iter()
            .filter(|n| n.kind == DataNodeKind::CallOutput)
        {
            self.investigation.check()?;
            let calls = node
                .callsite_id
                .map(|id| self.investigation.store.find_callsites_by_id(&id))
                .transpose()?
                .unwrap_or_default();
            let call_location = calls.first().map(|call| location(node.file_id, call.range));
            let exposed = (|| -> anyhow::Result<Option<bool>> {
                let ([call], Some(index)) = (calls.as_slice(), node.arg_index) else {
                    return Ok(None);
                };
                let Some(arg) = call.args.get(index as usize).and_then(|a| a.range) else {
                    return Ok(None);
                };
                let Some(mut syntax) = parsed
                    .tree
                    .root_node()
                    .descendant_for_byte_range(arg.start_byte as usize, arg.end_byte as usize)
                else {
                    return Ok(None);
                };
                while syntax.kind() == "parenthesized_expression" {
                    let mut cursor = syntax.walk();
                    let children: Vec<_> = syntax
                        .named_children(&mut cursor)
                        .filter(|n| n.kind() != "comment")
                        .collect();
                    let [inner] = children.as_slice() else { break };
                    syntax = *inner;
                }
                // Extraction only creates these nodes for a binding or &binding.
                if syntax.kind() == "pointer_expression" {
                    return Ok(Some(true));
                }
                if syntax.kind() != "identifier" {
                    return Ok(None);
                }
                let resolved = self
                    .investigation
                    .store
                    .find_resolved_callsites_by_id(&call.id)?;
                let [resolved] = resolved.as_slice() else {
                    return Ok(None);
                };
                let Some(callee) = self
                    .investigation
                    .store
                    .find_symbol_by_id(&resolved.callee)?
                else {
                    return Ok(None);
                };
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    types.entry(callee.file_id)
                {
                    entry.insert(
                        self.investigation
                            .store
                            .cpp_types_for_file(&callee.file_id)?,
                    );
                }
                let Some(declared) = types[&callee.file_id]
                    .as_ref()
                    .and_then(|f| f.callables.iter().find(|c| c.symbol_id == callee.id))
                    .and_then(|c| c.parameter_declared_types.get(index as usize))
                    .and_then(Option::as_ref)
                else {
                    // A missing effective type does not erase a written T&.
                    // Read only the selected declaration; target selection is
                    // still the existing source/compiler call relation.
                    return self
                        .reference_parameter(&callee, index)
                        .map(|reference| reference.then_some(true));
                };
                if declared.reference {
                    return Ok(Some(true));
                }
                let builtin = !declared.name.is_empty()
                    && declared.name.split_whitespace().all(|word| {
                        matches!(
                            word,
                            "bool"
                                | "char"
                                | "wchar_t"
                                | "char8_t"
                                | "char16_t"
                                | "char32_t"
                                | "short"
                                | "int"
                                | "long"
                                | "signed"
                                | "unsigned"
                                | "float"
                                | "double"
                                | "__int128"
                        )
                    });
                Ok(
                    (declared.template_arguments.is_empty() && (declared.pointer || builtin))
                        .then_some(false),
                )
            })()?;
            if exposed != Some(true) {
                excluded.insert(node.id);
                if exposed.is_none() {
                    uncertain.insert(node.id, (location(node.file_id, node.range), call_location));
                }
            }
        }
        for edge in &facts.dataflow_edges {
            if let Some(effect) = uncertain.get(&edge.source) {
                self.uncertain_outputs
                    .entry(edge.target)
                    .or_default()
                    .push(effect.clone());
            }
        }
        facts.data_nodes.retain(|n| !excluded.contains(&n.id));
        facts
            .dataflow_edges
            .retain(|e| !excluded.contains(&e.source) && !excluded.contains(&e.target));
        Ok(())
    }

    fn output_limits(&mut self, node: &DataNode) {
        if let Some(effects) = self.uncertain_outputs.remove(&node.id) {
            for (argument, call) in effects {
                let mut related = vec![location(node.file_id, node.range)];
                related.extend(call);
                self.result.gaps.push((argument, "value_call_effect_unestablished".into(),
                    "This call may affect a binding used by the displayed path, but its parameter binding or write effects are not established. Earlier definitions remain possibilities, not proof that the call preserved the value; inspect the argument and call source.".into(), related));
            }
        }
    }

    fn selection(&self, selection: &ValueSelection) -> anyhow::Result<ContextLocation> {
        let file = self
            .investigation
            .store
            .find_files_by_path_prefix(&selection.path)?
            .into_iter()
            .find(|f| f.path == selection.path);
        // FileId is internal only. The adapter retains the requested path for an unindexed selection.
        Ok(location(
            file.map_or_else(|| FileId::generate(&selection.path), |f| f.file_id),
            TextRange {
                start_byte: selection.range.start,
                end_byte: selection.range.end,
                ..Default::default()
            },
        ))
    }

    fn source(&mut self, at: &ContextLocation) -> anyhow::Result<Option<Arc<ParsedSource>>> {
        self.investigation.check()?;
        let Some(file) = self.investigation.store.get_file(&at.file_id)? else {
            self.result.gap(at.clone(), "value_file_unindexed", "No indexed language/scope facts for this file; source/text inspection remains available.");
            return Ok(None);
        };
        if file.language != Language::Cpp {
            self.result.gap(
                at.clone(),
                "value_language_unsupported",
                "This value query currently supports C++ only.",
            );
            return Ok(None);
        }
        let parsed = match self.investigation.source(file.file_id) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.investigation.check()?;
                self.result
                    .gap(at.clone(), "value_source_unavailable", error);
                return Ok(None);
            }
        };
        if blake3::hash(parsed.source.as_bytes()).to_hex().as_str() != file.content_hash {
            self.result.gap(
                at.clone(),
                "value_source_changed",
                "Source bytes no longer match the selected index; reindex before tracing values.",
            );
            return Ok(None);
        }
        Ok(Some(parsed))
    }

    fn owner(&mut self, at: &ContextLocation) -> anyhow::Result<Option<SymbolDef>> {
        let Some(parsed) = self.source(at)? else {
            return Ok(None);
        };
        let owner = self.investigation.function_owner(at, &parsed)?;
        if owner.is_none() {
            self.result.gap(at.clone(), "value_scope_unavailable", "No unique indexed function body owns this expression; inspect source and scope diagnostics.");
        }
        Ok(owner)
    }

    fn load(&mut self, owner: &SymbolDef, requested: &ContextLocation) -> anyhow::Result<bool> {
        if self.loaded.contains(&owner.id) {
            return Ok(true);
        }
        if self.loaded.len() >= self.options.max_functions {
            self.result.truncated = true;
            self.result.gaps.push((requested.clone(), "value_function_limit".into(), "The function budget stopped this call boundary; increase max_functions or inspect the callee separately.".into(), vec![location(owner.file_id, owner.range)]));
            return Ok(false);
        }
        let at = location(owner.file_id, owner.range);
        let Some(parsed) = self.source(&at)? else {
            return Ok(false);
        };
        let syntax = parsed.tree.root_node().descendant_for_byte_range(
            owner.range.start_byte as usize,
            owner.range.end_byte as usize,
        );
        if !syntax.is_some_and(|node| {
            matches!(node.kind(), "function_definition" | "lambda_expression")
                && cpp::range(node) == owner.range
        }) || parsed
            .cpp
            .as_ref()
            .is_some_and(|facts| facts.unverified_callable_scopes.contains(&owner.id))
        {
            self.result.gap(
                at,
                "value_body_unavailable",
                "The recorded declaration has no unique supported function body at this location.",
            );
            return Ok(false);
        }
        let file = self
            .investigation
            .store
            .get_file(&owner.file_id)?
            .expect("source file checked");
        let unit = AnalysisUnit::from_function(file.file_id, owner.id, owner.range);
        let mut facts = self.investigation.function_facts(owner, &parsed, false)?;
        self.investigation.check()?;
        facts.data_nodes.retain(|n| n.function_id == Some(owner.id));
        let counted_nodes = facts.data_nodes.len();
        let counted_edges = facts.dataflow_edges.len();
        if self.node_count + counted_nodes > 20_000 || self.edge_count + counted_edges > 100_000 {
            self.result.truncated = true;
            self.result.gap(
                at,
                "value_analysis_budget",
                "The transient graph and pending effects exceed this query's budget.",
            );
            return Ok(false);
        }
        self.classify_call_outputs(&mut facts, &parsed)?;
        self.limit_dereferences(&mut facts, &parsed)?;
        self.limit_field_reads(&mut facts, &parsed)?;
        let ids: BTreeSet<_> = facts.data_nodes.iter().map(|n| n.id).collect();
        facts
            .dataflow_edges
            .retain(|e| ids.contains(&e.source) && ids.contains(&e.target));
        for diagnostic in &facts.diagnostics {
            let range = diagnostic.range.unwrap_or(owner.range);
            if diagnostic.level != DiagnosticLevel::Info
                && range.start_byte <= owner.range.end_byte
                && owner.range.start_byte <= range.end_byte
            {
                self.result.gap(
                    location(file.file_id, range),
                    "value_extraction_limit",
                    &diagnostic.message,
                );
            }
        }
        if facts.budget_exceeded
            || facts.lexical_failed
            || facts.dataflow_failed
            || facts.cfg_failed
        {
            self.result.truncated |= facts.budget_exceeded;
            self.result.gap(at.clone(), "value_analysis_incomplete", "Extraction or CFG analysis was incomplete; unrecorded dependencies remain unknown.");
        }
        if self.temporary.get_file(&file.file_id)?.is_none() {
            let mut base = facts.clone();
            base.symbols = self
                .investigation
                .store
                .find_symbols_by_file(&file.file_id)?;
            base.scopes = self
                .investigation
                .store
                .find_scopes_by_file(&file.file_id)?;
            base.bindings = self
                .investigation
                .store
                .find_bindings_by_file(&file.file_id)?;
            base.binding_uses.clear();
            base.data_nodes.clear();
            base.dataflow_edges.clear();
            base.cfg_nodes.clear();
            base.cfg_edges.clear();
            base.diagnostics.clear();
            self.temporary.insert_file_facts(&base)?;
        }
        self.temporary.replace_dataflow_for_unit(
            &unit,
            &facts.data_nodes,
            &facts.dataflow_edges,
            &facts.bindings,
            &facts.binding_uses,
            &facts.cfg_nodes,
            &facts.cfg_edges,
        )?;
        // Lazy extraction returns data facts only. Preserve original call
        // coordinates for output-boundary continuation without selecting a
        // target or loading a callee body merely to explain a stopping point.
        let output_calls: BTreeSet<_> = facts
            .data_nodes
            .iter()
            .filter(|n| n.kind == DataNodeKind::CallOutput)
            .filter_map(|n| n.callsite_id)
            .collect();
        if !output_calls.is_empty() {
            let calls: Vec<_> = self
                .investigation
                .store
                .find_callsites_by_file(&owner.file_id)?
                .into_iter()
                .filter(|c| output_calls.contains(&c.id))
                .collect();
            self.temporary.insert_callsites(&calls)?;
        }
        self.node_count += counted_nodes;
        self.edge_count += counted_edges;
        self.loaded.insert(owner.id);
        self.result.scopes.push(at);
        Ok(true)
    }

    fn boundary(&mut self, id: CallsiteId, at: &ContextLocation) -> anyhow::Result<bool> {
        if !self.attempted_calls.insert(id) {
            return Ok(false);
        }
        let calls = self
            .investigation
            .store
            .find_resolved_callsites_by_id(&id)?;
        let [resolved] = calls.as_slice() else {
            self.result.gap(at.clone(), "value_call_target_unavailable", "No unique recorded call target is available; use inspect/calls and source/text continuation.");
            return Ok(false);
        };
        let Some(callee) = self
            .investigation
            .store
            .find_symbol_by_id(&resolved.callee)?
        else {
            self.result.gap(
                at.clone(),
                "value_callee_unavailable",
                "The recorded call target has no available declaration.",
            );
            return Ok(false);
        };
        let Some(caller) = self
            .investigation
            .store
            .find_symbol_by_id(&resolved.callsite.caller)?
        else {
            return Ok(false);
        };
        if !self.load(&caller, at)? || !self.load(&callee, at)? {
            return Ok(false);
        }
        let Some(reference) = self
            .investigation
            .store
            .find_references_by_file(&caller.file_id)?
            .into_iter()
            .find(|r| Some(r.id) == resolved.callsite.reference_id)
        else {
            return Ok(false);
        };
        self.temporary.insert_references(&[reference])?;
        let mut call = resolved.callsite.clone();
        for arg in &mut call.args {
            arg.data_node_id = None;
        }
        self.temporary.insert_callsites(&[call])?;
        self.temporary.update_callsite_arg_data_nodes(
            &AnalysisUnit::from_function(caller.file_id, caller.id, caller.range),
            &self.temporary.find_data_nodes_by_function(&caller.id)?,
        )?;
        Ok(true)
    }

    fn context(
        &mut self,
        owner: &SymbolDef,
        requested: &ContextLocation,
    ) -> anyhow::Result<Option<Vec<CallsiteId>>> {
        let mut ids = vec![];
        let mut previous = None;
        for selection in &self.options.call_context {
            self.investigation.check()?;
            let at = self.selection(selection)?;
            let calls: Vec<_> = self
                .investigation
                .store
                .find_callsites_by_file(&at.file_id)?
                .into_iter()
                .filter(|c| {
                    [Some(c.range), c.callee_range]
                        .into_iter()
                        .flatten()
                        .any(|range| {
                            range.start_byte == at.range.start_byte
                                && range.end_byte == at.range.end_byte
                        })
                })
                .collect();
            let [call] = calls.as_slice() else {
                self.result.gap(
                    at,
                    "value_call_context_unavailable",
                    "A context location must select one recorded call or its exact callee range.",
                );
                return Ok(None);
            };
            let resolved = self
                .investigation
                .store
                .find_resolved_callsites_by_id(&call.id)?;
            let [resolved] = resolved.as_slice() else {
                self.result.gap(
                    at,
                    "value_call_context_unavailable",
                    "The selected context call has no unique recorded target.",
                );
                return Ok(None);
            };
            if previous.is_some_and(|id| id != call.caller) {
                self.result.gap(at, "value_call_context_mismatch", "Context calls do not form an outer-to-inner chain of recorded caller/callee relations.");
                return Ok(None);
            }
            previous = Some(resolved.callee);
            ids.push(call.id);
            if self
                .temporary
                .find_resolved_callsites_by_id(&call.id)?
                .is_empty()
                && !self.boundary(call.id, &at)?
            {
                return Ok(None);
            }
        }
        if previous.is_some_and(|id| id != owner.id) {
            self.result.gap(
                requested.clone(),
                "value_call_context_mismatch",
                "The innermost context call does not target the selected function.",
            );
            return Ok(None);
        }
        Ok(Some(ids))
    }

    fn run(&mut self, selection: &ValueSelection) -> anyhow::Result<()> {
        let requested = self.selection(selection)?;
        let Some(owner) = self.owner(&requested)? else {
            return Ok(());
        };
        if !self.load(&owner, &requested)? {
            return Ok(());
        }
        let Some(context) = self.context(&owner, &requested)? else {
            return Ok(());
        };
        let mut sinks: Vec<_> = self
            .temporary
            .find_data_nodes_by_function(&owner.id)?
            .into_iter()
            .filter(|n| {
                !matches!(
                    n.kind,
                    DataNodeKind::CallOutput | DataNodeKind::ParameterOutput
                ) && n.range.start_byte <= requested.range.start_byte
                    && requested.range.end_byte <= n.range.end_byte
            })
            .collect();
        if let Some(width) = sinks.iter().map(|n| n.range.byte_len()).min() {
            sinks.retain(|n| n.range.byte_len() == width);
        }
        sinks.sort_by_key(|n| (n.kind.as_str(), n.id));
        if sinks.len() > self.options.max_paths {
            self.result.truncated = true;
            self.result.gap(requested.clone(), "value_result_limit", "Additional value roles at this position were not traced; narrow the expression or increase max_paths.");
        }
        let mut engine = analysis::trace::TraceEngine::new(self.temporary.clone());
        // Each traversed edge can enter at most one call. Bound the supplied
        // context plus new steps so returned contexts remain valid query input.
        let depth = self.options.max_depth.min(100 - context.len());
        if depth < self.options.max_depth {
            self.result.truncated = true;
            self.result.gap(requested.clone(), "value_context_budget", "The supplied call context and new trace steps share a bound of 100; fewer steps were available for this continuation.");
        }
        for sink in sinks.into_iter().take(self.options.max_paths) {
            loop {
                self.investigation.check()?;
                let response = engine.trace_data_node(&sink.id, depth, &context);
                anyhow::ensure!(
                    response.ok,
                    "value trace failed: {:?}",
                    response.diagnostics
                );
                if let Some(trace) = response.result {
                    if self.constrain_return_exits(&trace, &requested, &context, &mut engine)? {
                        continue;
                    }
                    if !trace
                        .diagnostics
                        .iter()
                        .any(|d| d.code.as_deref().is_some_and(|s| s.contains("depth")))
                        && let Some(endpoint) = &trace.source.data_node
                        && match endpoint.kind {
                            DataNodeKind::CallReturn => {
                                if let Some(id) = endpoint.callsite_id {
                                    self.boundary(id, &location(endpoint.file_id, endpoint.range))?
                                } else {
                                    false
                                }
                            }
                            DataNodeKind::CallOutput => self.writeback(endpoint)?,
                            _ => false,
                        }
                    {
                        continue;
                    }
                    self.append_trace(&sink, &context, trace)?;
                } else {
                    for diagnostic in &response.diagnostics {
                        diagnostic_gap(
                            &mut self.result,
                            &location(sink.file_id, sink.range),
                            diagnostic,
                        );
                    }
                }
                break;
            }
        }
        if self.result.paths.is_empty() {
            self.result.gap(requested.clone(), "value_node_unavailable", "No recorded value at this expression; source and declaration inspection remain available.");
        }
        self.result.gap(location(owner.file_id, owner.range), "value_flow_limited", "Recorded dependencies and selected call boundaries were examined. Paths do not prove execution, exhaustive origins, value equality, field store-to-read identity or callback invocation. Unexpanded alternatives require separate investigation.");
        Ok(())
    }

    fn point(&self, node: &DataNode, context: &[CallsiteId]) -> anyhow::Result<ValuePoint> {
        let mut locations = vec![];
        for id in context {
            let calls = self.temporary.find_callsites_by_id(id)?;
            let [call] = calls.as_slice() else {
                anyhow::bail!("missing trace call context")
            };
            let caller = self
                .temporary
                .find_symbol_by_id(&call.caller)?
                .ok_or_else(|| anyhow::anyhow!("missing context caller"))?;
            locations.push(location(caller.file_id, call.range));
        }
        Ok(ValuePoint {
            location: location(node.file_id, node.range),
            kind: if self.dereference_inputs.contains_key(&node.id) {
                "dereference"
            } else {
                node.kind.as_str()
            }
            .into(),
            name: node.name.clone(),
            call_context: locations,
        })
    }

    fn append_trace(
        &mut self,
        sink: &DataNode,
        context: &[CallsiteId],
        trace: TracePath,
    ) -> anyhow::Result<()> {
        for diagnostic in &trace.diagnostics {
            diagnostic_gap(
                &mut self.result,
                &location(sink.file_id, sink.range),
                diagnostic,
            );
        }
        self.output_limits(sink);
        let mut steps = vec![];
        for (i, step) in trace.steps.iter().enumerate() {
            self.investigation.check()?;
            let from = self
                .temporary
                .get_data_node(&step.from_node_id)?
                .ok_or_else(|| anyhow::anyhow!("missing predecessor"))?;
            let to = self
                .temporary
                .get_data_node(&step.to_node_id)?
                .ok_or_else(|| anyhow::anyhow!("missing successor"))?;
            self.output_limits(&from);
            self.output_limits(&to);
            let to_context = trace
                .steps
                .get(i + 1)
                .map_or(context, |next| &next.call_context);
            if step.edge_kind == DataFlowKind::FieldLoad {
                self.result.gaps.push((location(to.file_id, to.range), "value_field_contents_unestablished".into(), "This dependency identifies the field receiver, not the value stored in the field. Store-to-read correspondence and aliasing were not computed.".into(), vec![location(from.file_id, from.range)]));
            }
            if step.edge_kind == DataFlowKind::FieldStore {
                self.result.gap(location(to.file_id, to.range), "value_store_effect_unestablished", "The assigned input was traced. Overloaded assignment effects and subsequent reads were not established.");
            }
            steps.push(ValueStep {
                from: self.point(&from, &step.call_context)?,
                to: self.point(&to, to_context)?,
                kind: step.edge_kind.as_str().into(),
            });
        }
        if let Some(endpoint) = trace.source.data_node {
            self.output_limits(&endpoint);
            if let Some(receivers) = self.field_receivers.get(&endpoint.id) {
                self.result.gaps.push((location(endpoint.file_id, endpoint.range), "value_field_contents_unestablished".into(),
                    "No stored value has been established for this field read. Related locations identify its receiver, not its contents; inspect the receiver and possible stores separately.".into(), receivers.clone()));
            }
            if let Some(operand) = self.dereference_inputs.get(&endpoint.id) {
                self.result.gaps.push((location(endpoint.file_id, endpoint.range), "value_dereference_unestablished".into(),
                    "The object/value produced by this dereference, including any overloaded operator effects, has not been established. Its operand locates an address or object to inspect, not the value produced here.".into(), vec![operand.clone()]));
            }
            let mut final_endpoint = self.point(&endpoint, &trace.source.call_context)?;
            if endpoint.kind == DataNodeKind::Parameter && !trace.source.call_context.is_empty() {
                let calls = self
                    .temporary
                    .find_resolved_callsites_by_id(trace.source.call_context.last().unwrap())?;
                let mapped = endpoint.arg_index.is_some_and(|index| {
                    calls.iter().any(|call| {
                        call.callsite
                            .args
                            .get(index as usize)
                            .is_some_and(|arg| arg.data_node_id.is_some())
                    })
                });
                if !mapped {
                    let related = self
                        .point(&endpoint, &trace.source.call_context)?
                        .call_context;
                    if steps.len() < self.options.max_depth.min(100 - context.len())
                        && let Some(default) =
                            self.default_argument(&endpoint, &trace.source.call_context)?
                    {
                        steps.insert(
                            0,
                            ValueStep {
                                from: default.clone(),
                                to: final_endpoint,
                                kind: "default_argument".into(),
                            },
                        );
                        final_endpoint = default;
                    } else {
                        if steps.len() >= self.options.max_depth.min(100 - context.len()) {
                            self.result.truncated = true;
                            self.result.gap(location(endpoint.file_id, endpoint.range), "value_default_depth_limit", "The path budget stopped investigation of the omitted input; continue at the parameter with its selected call context.");
                        }
                        self.result.gaps.push((location(endpoint.file_id, endpoint.range), "value_argument_unavailable".into(), "The selected invocation has no supported actual or default argument mapping to this parameter; inspect the declaration, call context and analysis limits.".into(), related));
                    }
                }
            }
            self.result.paths.push(ValuePath {
                sink: self.point(sink, context)?,
                endpoint: final_endpoint,
                steps,
            });
        }
        Ok(())
    }
}

fn diagnostic_gap(
    result: &mut ValueFlowResult,
    fallback: &ContextLocation,
    diagnostic: &TraceDiagnostic,
) {
    fn places(value: &serde_json::Value, fallback: FileId, out: &mut Vec<ContextLocation>) {
        if let Some(object) = value.as_object() {
            let file = object
                .get("file_id")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(fallback);
            for key in ["location", "range"] {
                if let Some(v) = object.get(key)
                    && let Ok(range) = serde_json::from_value::<TextRange>(v.clone())
                {
                    out.push(location(file, range));
                }
            }
            for value in object.values() {
                places(value, file, out);
            }
        } else if let Some(array) = value.as_array() {
            for value in array {
                places(value, fallback, out);
            }
        }
    }
    let mut related = vec![];
    let mut primary = fallback.clone();
    if let Some(detail) = &diagnostic.detail
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(detail)
    {
        if let Some(at) = value.get("at") {
            let mut at_locations = vec![];
            places(at, fallback.file_id, &mut at_locations);
            if let Some(at) = at_locations.into_iter().next() {
                primary = at;
            }
        }
        places(&value, fallback.file_id, &mut related);
    }
    related.sort_by_key(|at| (at.file_id, at.range.start_byte, at.range.end_byte));
    related.dedup();
    let code = diagnostic.code.as_deref().unwrap_or("value_trace_limit");
    if code.contains("depth") || code.contains("truncat") {
        result.truncated = true;
    }
    result
        .gaps
        .push((primary, code.into(), diagnostic.message.clone(), related));
}
