//! Written default inputs for an already selected direct call. Defaults belong
//! to declarations visible at that call, not automatically to its chosen body.
use super::*;
use std::collections::{BTreeSet, VecDeque};

impl Query<'_> {
    fn included_files(
        &mut self,
        root: FileId,
        before: u32,
        at: &ContextLocation,
    ) -> anyhow::Result<Option<BTreeSet<FileId>>> {
        let paths: Vec<String> = self
            .investigation
            .store
            .get_metadata(resolution::KEY_INCLUDE_PATHS)?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default();
        let mut pending = VecDeque::from([root]);
        let mut visited = BTreeSet::new();
        let mut imports_examined = 0;
        while let Some(file) = pending.pop_front() {
            self.investigation.check()?;
            if !visited.insert(file) {
                continue;
            }
            if visited.len() > 2048 {
                self.result.truncated = true;
                self.result.gap(at.clone(), "value_default_lookup_budget", "Default-argument include visibility exceeded 2048 files; remaining declarations were not examined.");
                return Ok(None);
            }
            for import in self.investigation.store.find_imports_by_file(&file)? {
                self.investigation.check()?;
                if import.kind != ImportKind::Include
                    || (file == root && import.range.start_byte >= before)
                {
                    continue;
                }
                imports_examined += 1;
                if imports_examined > 8192 {
                    self.result.truncated = true;
                    self.result.gap(at.clone(), "value_default_lookup_budget", "Default-argument visibility exceeded 8192 includes; remaining declarations were not examined.");
                    return Ok(None);
                }
                if let Some(target) = self
                    .investigation
                    .store
                    .resolve_include_file(&import, &paths)?
                {
                    pending.push_back(target.file_id);
                }
            }
        }
        Ok(Some(visited))
    }

    pub(super) fn default_argument(
        &mut self,
        parameter: &DataNode,
        context: &[CallsiteId],
    ) -> anyhow::Result<Option<ValuePoint>> {
        let (Some(index), Some(id), Some(owner)) =
            (parameter.arg_index, context.last(), parameter.function_id)
        else {
            return Ok(None);
        };
        let calls = self.temporary.find_resolved_callsites_by_id(id)?;
        let [selected] = calls.as_slice() else {
            return Ok(None);
        };
        let call = &selected.callsite;
        if selected.callee != owner || index < call.args.len() as u32 {
            return Ok(None);
        }
        let Some(callee) = self.investigation.store.find_symbol_by_id(&owner)? else {
            return Ok(None);
        };
        let Some(caller) = self.investigation.store.find_symbol_by_id(&call.caller)? else {
            return Ok(None);
        };
        let call_at = location(caller.file_id, call.range);
        let Some(source) = self.source(&call_at)? else {
            return Ok(None);
        };
        let Some(syntax) = cpp::expression(source.tree.root_node(), call.range)
            .filter(|n| n.kind() == "call_expression" && !n.has_error())
        else {
            return Ok(None);
        };
        let Some(function) = syntax.child_by_field_name("function") else {
            return Ok(None);
        };
        if !matches!(
            function.kind(),
            "identifier" | "qualified_identifier" | "field_expression"
        ) {
            return Ok(None);
        }
        let Some(arguments) = syntax.child_by_field_name("arguments") else {
            return Ok(None);
        };
        let mut cursor = arguments.walk();
        if arguments
            .named_children(&mut cursor)
            .filter(|n| n.kind() != "comment")
            .count()
            != call.args.len()
        {
            return Ok(None);
        }
        // A pointer/callback with a different written name cannot acquire the
        // defaults of the function body to which it happens to refer.
        let Some(reference_id) = call.reference_id else {
            return Ok(None);
        };
        let reference = self
            .investigation
            .store
            .find_references_by_file(&caller.file_id)?
            .into_iter()
            .find(|r| r.id == reference_id);
        if reference.as_ref().is_none_or(|r| r.name != callee.name) {
            return Ok(None);
        }
        let Some(body_facts) = self
            .investigation
            .store
            .cpp_types_for_file(&callee.file_id)?
        else {
            return Ok(None);
        };
        let Some(body_type) = body_facts
            .callables
            .iter()
            .find(|c| c.symbol_id == callee.id)
        else {
            return Ok(None);
        };
        if body_type.is_virtual || index as usize >= body_type.parameter_types.len() {
            return Ok(None);
        }
        let Some(visible) = self.included_files(caller.file_id, call.range.start_byte, &call_at)?
        else {
            return Ok(None);
        };
        let Some(body_visible) =
            self.included_files(callee.file_id, callee.name_range.start_byte, &call_at)?
        else {
            return Ok(None);
        };
        let candidates = self
            .investigation
            .store
            .find_symbols_by_name(&callee.name)?;
        if candidates.len() > 10_000 {
            self.result.truncated = true;
            self.result.gap(
                call_at,
                "value_default_lookup_budget",
                "More than 10000 declaration candidates; defaults were not selected.",
            );
            return Ok(None);
        }
        let mut defaults = vec![];
        for declaration in candidates {
            self.investigation.check()?;
            if declaration.qualified_name != callee.qualified_name
                || declaration.language != Language::Cpp
                || !visible.contains(&declaration.file_id)
                || (declaration.file_id == caller.file_id
                    && declaration.name_range.start_byte > call.range.start_byte)
            {
                continue;
            }
            let Some(parsed) =
                self.source(&location(declaration.file_id, declaration.name_range))?
            else {
                return Ok(None);
            };
            let Some(signature) = parsed
                .cpp
                .as_ref()
                .and_then(|f| f.callables.iter().find(|c| c.symbol_id == declaration.id))
            else {
                self.result.gaps.push((call_at.clone(), "value_default_declaration_unavailable".into(), "A visible same-named declaration lacks supported callable identity; default lookup remains unestablished.".into(), vec![location(declaration.file_id, declaration.name_range)]));
                return Ok(None);
            };
            if !resolution::cpp_declarations_match(
                &declaration,
                signature,
                &callee,
                body_type,
                body_visible.contains(&declaration.file_id),
            ) {
                continue;
            }
            // Only declarations supplying this omitted suffix participate. A
            // prototype with no defaults does not erase an earlier default.
            if signature.minimum_arity > call.args.len() as u32 {
                continue;
            }
            let Some(name) = cpp::expression(parsed.tree.root_node(), declaration.name_range)
            else {
                return Ok(None);
            };
            let function = std::iter::successors(Some(name), |n| n.parent())
                .take_while(|n| !matches!(n.kind(), "compound_statement" | "translation_unit"))
                .find(|n| n.kind() == "function_declarator");
            let Some(parameters) = function.and_then(|n| n.child_by_field_name("parameters"))
            else {
                return Ok(None);
            };
            let mut cursor = parameters.walk();
            let Some(default) = parameters
                .named_children(&mut cursor)
                .filter(|n| {
                    matches!(
                        n.kind(),
                        "parameter_declaration" | "optional_parameter_declaration"
                    )
                })
                .nth(index as usize)
                .and_then(|n| n.child_by_field_name("default_value"))
                .filter(|n| !n.has_error() && !n.is_missing())
            else {
                return Ok(None);
            };
            defaults.push((
                location(declaration.file_id, cpp::range(default)),
                cpp::text(default, &parsed.source).to_string(),
                location(declaration.file_id, declaration.name_range),
            ));
        }
        let [(at, expression, declaration)] = defaults.as_slice() else {
            if defaults.len() > 1 {
                self.result.gaps.push((call_at, "value_default_argument_ambiguous".into(), "Multiple visible declarations provide this default input; no expression was selected, including identical repeated defaults.".into(), defaults.into_iter().map(|(at, _, _)| at).collect()));
            }
            return Ok(None);
        };
        let mut evaluation_context = self.point(parameter, context)?.call_context;
        evaluation_context.pop();
        self.result.gaps.push((at.clone(), "value_default_expression_unexpanded".into(), "This visible declaration supplies the omitted input for the selected call. The default expression's evaluation, conversions, effects and further dependencies were not expanded; source/inspect can continue at its declaration and call.".into(), vec![declaration.clone(), call_at]));
        Ok(Some(ValuePoint {
            location: at.clone(),
            kind: "default_argument".into(),
            name: Some(expression.clone()),
            call_context: evaluation_context,
        }))
    }
}
