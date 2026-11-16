//! Bind a recorded normal-exit reference state to one invocation's output.
//! No heap/pointer alias inference or new call-target selection happens here.
use super::*;

impl Query<'_> {
    /// Add output summaries only when following an invocation's reference
    /// output. Existing body facts and already expanded call edges stay intact.
    fn load_output_states(
        &mut self,
        function: SymbolId,
        at: &ContextLocation,
    ) -> anyhow::Result<bool> {
        if self.output_functions.contains(&function) {
            return Ok(true);
        }
        let Some(owner) = self.investigation.store.find_symbol_by_id(&function)? else {
            return Ok(false);
        };
        let Some(parsed) = self.source(&location(owner.file_id, owner.range))? else {
            return Ok(false);
        };
        let mut facts = self.investigation.function_facts(&owner, &parsed, true)?;
        facts.data_nodes.retain(|n| n.function_id == Some(function));
        self.classify_call_outputs(&mut facts, &parsed)?;
        self.limit_dereferences(&mut facts, &parsed)?;
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
                    location(owner.file_id, range),
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
            self.result.gap(at.clone(), "value_analysis_incomplete", "Reference exit extraction was incomplete; remaining output states require investigation.");
        }
        let existing_nodes: BTreeSet<_> = self
            .temporary
            .find_data_nodes_by_function(&function)?
            .iter()
            .map(|n| n.id)
            .collect();
        let existing_edges: BTreeSet<_> = self
            .temporary
            .find_dataflow_edges_by_function(&function)?
            .iter()
            .map(|e| e.id)
            .collect();
        facts.data_nodes.retain(|n| !existing_nodes.contains(&n.id));
        facts
            .dataflow_edges
            .retain(|e| !existing_edges.contains(&e.id));
        if self.node_count + facts.data_nodes.len() > 20_000
            || self.edge_count + facts.dataflow_edges.len() > 100_000
        {
            self.result.truncated = true;
            self.result.gap(
                at.clone(),
                "value_analysis_budget",
                "Reference output summaries exceed the temporary graph budget.",
            );
            return Ok(false);
        }
        self.temporary.insert_data_nodes(&facts.data_nodes)?;
        self.temporary
            .insert_dataflow_edges(&facts.dataflow_edges)?;
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
            self.temporary.update_callsite_arg_data_nodes(
                &AnalysisUnit::from_function(owner.file_id, owner.id, owner.range),
                &self.temporary.find_data_nodes_by_function(&owner.id)?,
            )?;
        }
        self.node_count += facts.data_nodes.len();
        self.edge_count += facts.dataflow_edges.len();
        self.output_functions.insert(function);
        Ok(true)
    }

    pub(super) fn reference_parameter(
        &mut self,
        callee: &SymbolDef,
        index: u32,
    ) -> anyhow::Result<bool> {
        let Some(parsed) = self.source(&location(callee.file_id, callee.name_range))? else {
            return Ok(false);
        };
        let Some(parameters) =
            cpp::callable(parsed.tree.root_node(), callee.name_range).and_then(|c| c.parameters)
        else {
            return Ok(false);
        };
        let mut cursor = parameters.walk();
        let parameters: Vec<_> = parameters
            .named_children(&mut cursor)
            .filter(|n| n.kind() != "comment")
            .collect();
        if parameters.iter().any(|p| {
            !matches!(
                p.kind(),
                "parameter_declaration" | "optional_parameter_declaration"
            ) || p.has_error()
                || p.child(0).is_some_and(|c| c.kind() == "this")
        }) {
            return Ok(false);
        }
        Ok(parameters
            .get(index as usize)
            .is_some_and(|p| extraction::is_cpp_reference_parameter(*p)))
    }

    pub(super) fn writeback(&mut self, output: &DataNode) -> anyhow::Result<bool> {
        if !self.attempted_outputs.insert(output.id) {
            return Ok(false);
        }
        let (Some(id), Some(index)) = (output.callsite_id, output.arg_index) else {
            return Ok(false);
        };
        let at = location(output.file_id, output.range);
        let Some(parsed) = self.source(&at)? else {
            return Ok(false);
        };
        let Some(mut argument) = cpp::expression(parsed.tree.root_node(), output.range) else {
            return Ok(false);
        };
        while argument.kind() == "parenthesized_expression" {
            let mut cursor = argument.walk();
            let children: Vec<_> = argument
                .named_children(&mut cursor)
                .filter(|n| n.kind() != "comment")
                .collect();
            let [inner] = children.as_slice() else { break };
            argument = *inner;
        }
        if argument.kind() != "identifier" {
            self.result.gap(at, "value_writeback_argument_unavailable",
                "This output needs pointer/alias effects rather than a direct reference binding; inspect the argument and callee.");
            return Ok(false);
        }
        if self
            .temporary
            .find_resolved_callsites_by_id(&id)?
            .is_empty()
            && !self.boundary(id, &at)?
        {
            return Ok(false);
        }
        let calls = self.temporary.find_resolved_callsites_by_id(&id)?;
        let [call] = calls.as_slice() else {
            return Ok(false);
        };
        if !self.load_output_states(call.callee, &at)? {
            return Ok(false);
        }
        let nodes = self.temporary.find_data_nodes_by_function(&call.callee)?;
        let parameters: Vec<_> = nodes
            .iter()
            .filter(|n| n.kind == DataNodeKind::Parameter && n.arg_index == Some(index))
            .collect();
        let [parameter] = parameters.as_slice() else {
            self.result.gap(at, "value_writeback_parameter_unavailable",
                "No uniquely recorded parameter matches this argument; inspect the selected declaration/body.");
            return Ok(false);
        };
        let exits: Vec<_> = nodes
            .iter()
            .filter(|n| {
                n.kind == DataNodeKind::ParameterOutput
                    && n.arg_index == Some(index)
                    && n.binding_id == parameter.binding_id
            })
            .collect();
        if exits.is_empty() {
            self.result.gaps.push((at, "value_writeback_exit_unavailable".into(),
                "No normal-exit state was recorded for this reference parameter; missing facts or non-returning paths remain unknown.".into(),
                vec![location(parameter.file_id, parameter.range)]));
            return Ok(false);
        }
        let mut linked = false;
        for exit in &exits {
            self.investigation.check()?;
            let definitions = self.temporary.find_dataflow_edges_by_target(&exit.id)?;
            if definitions.is_empty() {
                self.result.gaps.push((location(exit.file_id, exit.range), "value_writeback_exit_unavailable".into(),
                "No definition was recorded for this parameter at this normal exit; inspect the exit and parameter.".into(),
                vec![location(parameter.file_id, parameter.range), at.clone()]));
                continue;
            }
            if self.edge_count >= 100_000 {
                self.result.truncated = true;
                self.result.gap(
                    at,
                    "value_analysis_budget",
                    "The temporary edge budget stopped reference writeback expansion.",
                );
                return Ok(linked);
            }
            self.temporary.insert_dataflow_edges(&[DataFlowEdge::new(
                types::DataFlowEdgeId::generate(
                    &exit.id,
                    &output.id,
                    DataFlowKind::WritebackToCall.as_str(),
                ),
                exit.id,
                output.id,
                DataFlowKind::WritebackToCall,
                output.range,
                1.0,
            )])?;
            self.edge_count += 1;
            linked = true;
            if exits.len() > 1 {
                let mut related = vec![location(parameter.file_id, parameter.range), at.clone()];
                for definition in definitions {
                    if let Some(node) = self.temporary.get_data_node(&definition.source)? {
                        related.push(location(node.file_id, node.range));
                    }
                }
                self.result.gaps.push((location(exit.file_id, exit.range), "value_writeback_exit_alternatives".into(),
                "This normal exit has its own possible reference definitions. Other exits remain alternatives unless a separately reported caller return condition excludes them. Inspect this exit and its related definitions.".into(), related));
            }
        }
        if linked {
            self.result.gaps.push((at, "value_writeback_limited".into(),
            "This invocation is linked to recorded reference definitions at normal exits. Branch feasibility, alias writes, overloaded assignment effects and complete exit coverage are not established; retained alternatives must be investigated.".into(),
            vec![location(parameter.file_id, parameter.range), location(output.file_id, call.callsite.range)]));
        }
        Ok(linked)
    }
}
