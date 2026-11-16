//! Written template callees and declaration navigation. These facts do not
//! select an overload, specialization, receiver, or instantiated function body.
use super::*;

impl Investigation<'_> {
    pub(super) fn template_callee(&mut self, call: &ReferenceUse) -> anyhow::Result<()> {
        // A written template argument list requires '<'. Avoid reading a
        // different source region solely to investigate ordinary calls.
        if !call.text.contains('<') {
            return Ok(());
        }
        let parsed = match self.source(call.file_id) {
            Ok(parsed) => parsed,
            Err(error) => return self.gap(call, "source_context_unavailable", error, vec![]),
        };
        let Some(template) =
            extraction::cpp_template_call(parsed.tree.root_node(), call, &parsed.source)
        else {
            return Ok(());
        };
        let name_at = ContextLocation {
            file_id: call.file_id,
            range: template.name_range,
        };
        let arguments_at = ContextLocation {
            file_id: call.file_id,
            range: template.arguments_range,
        };
        let written = template.name.as_str();
        self.item(call, "callable_name", name_at.clone(), None, vec![arguments_at.clone()],
            "Name written in this template callee. It is a declaration-search key, not a selected template instance or runtime target.")?;
        self.item(call, "template_arguments", arguments_at.clone(), None, vec![name_at.clone()],
            "Explicit template-argument syntax, including empty lists. Types, values, packs, deduction and specialization selection remain to be checked.")?;
        let mut declarations = Vec::new();
        for symbol in self.store.find_symbols_by_name(written)? {
            self.check()?;
            if symbol.language != Language::Cpp
                || !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
            {
                continue;
            }
            let declaration = ContextLocation {
                file_id: symbol.file_id,
                range: symbol.name_range,
            };
            let body = ContextLocation {
                file_id: symbol.file_id,
                range: symbol.range,
            };
            self.item(call, "same_name", declaration.clone(), Some(symbol.id), vec![name_at.clone(), arguments_at.clone(), body],
                "Indexed callable with the name written in the template callee. Scope, receiver, template parameters, overloads and specializations have not been selected by this match; inspect its declaration/body.")?;
            declarations.push(declaration);
        }
        if self
            .store
            .find_resolved_callsite_by_reference_id(&call.id)?
            .is_none()
        {
            let mut related = vec![name_at, arguments_at];
            related.extend(declarations);
            self.gap(call, "template_call_target_unestablished",
                "This explicit template call has no recorded selected target. Written name, arguments and indexed same-name declarations are available for source investigation; template applicability, deduction and specialization selection are not established. Missing candidates do not distinguish absent input from omitted extraction.", related)?;
        }
        Ok(())
    }
}
