//! Read-only, source-located investigation of calls and non-call value uses.
//!
//! Lexical declarations and names written in type syntax are investigation
//! context, not points-to results or new call edges. No resolution rows are written.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    sync::Arc,
};

use db::Store;
use tree_sitter::{Node, Parser, Tree};
use types::*;

mod control;
mod cpp;
mod function;
mod navigation;
mod operations;
mod preprocessing;
mod session;
pub use session::CallContextSession;
pub mod references;
mod templates;
pub mod value_flow;
mod values;

pub const MAX_CONTEXT_FILE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CONTEXT_TOTAL_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_CONTEXT_ITEMS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextLocation {
    pub file_id: FileId,
    pub range: TextRange,
}

#[derive(Debug, Clone)]
pub enum ContextSubject {
    Call(Box<ReferenceUse>),
    Region {
        location: ContextLocation,
        symbol_id: Option<SymbolId>,
    },
}

impl ContextSubject {
    pub fn call(&self) -> Option<&ReferenceUse> {
        match self {
            Self::Call(call) => Some(call),
            Self::Region { .. } => None,
        }
    }

    pub fn location(&self) -> ContextLocation {
        match self {
            Self::Call(call) => ContextLocation {
                file_id: call.file_id,
                range: call.range,
            },
            Self::Region { location, .. } => location.clone(),
        }
    }

    fn reference(reference: &ReferenceUse) -> Self {
        if reference.kind == ReferenceKind::Call {
            Self::Call(Box::new(reference.clone()))
        } else {
            Self::Region {
                location: ContextLocation {
                    file_id: reference.file_id,
                    range: reference.range,
                },
                symbol_id: reference.source_symbol,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextItemKind {
    Declaration,
    Definition,
    Reference,
    ControlCondition,
}

#[derive(Debug, Clone)]
pub struct CallContextItem {
    pub subject: ContextSubject,
    pub kind: ContextItemKind,
    /// The item's role in an investigation, never a relationship strength.
    pub role: &'static str,
    pub location: ContextLocation,
    pub symbol_id: Option<SymbolId>,
    pub related_locations: Vec<ContextLocation>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct CallContextGap {
    pub subject: ContextSubject,
    pub code: &'static str,
    pub message: String,
    pub related_locations: Vec<ContextLocation>,
}

#[derive(Debug, Default)]
pub struct CallContextResult {
    pub items: Vec<CallContextItem>,
    pub gaps: Vec<CallContextGap>,
    pub files_read: usize,
    pub bytes_read: usize,
}

struct ParsedSource {
    source: String,
    tree: Tree,
    cpp: Option<types::cpp::CppFileTypes>,
    stamp: session::SourceStamp,
}

struct Investigation<'a> {
    store: &'a Store,
    root: &'a Path,
    canceled: &'a dyn Fn() -> bool,
    parsed: BTreeMap<FileId, Result<Arc<ParsedSource>, String>>,
    result: CallContextResult,
    symbol_static: BTreeMap<SymbolId, bool>,
    reusable: Option<&'a mut session::ReusableSources>,
    admitted_bytes: usize,
}

/// Navigate indexed C++ callable names and containing bodies independently of
/// stored calls, then inspect intersecting calls or written value uses. This uses
/// stored lexical scopes/bindings and parses only the source needed for their
/// declaration/initializer syntax. Currently C++ supplies receiver and argument context;
/// unsupported languages retain an explicit limitation. Results never alter Store.
pub fn inspect_call_context(
    store: &Store,
    root: &Path,
    path: &str,
    start_byte: u32,
    end_byte: u32,
    include_control_conditions: bool,
    canceled: &dyn Fn() -> bool,
) -> anyhow::Result<CallContextResult> {
    let _diagnostic = tracing::debug_span!(target: "atlas_context_work", "context_selection", path, start_byte, end_byte).entered();
    let mut query = Investigation {
        store,
        root,
        canceled,
        parsed: BTreeMap::new(),
        result: CallContextResult::default(),
        symbol_static: BTreeMap::new(),
        reusable: None,
        admitted_bytes: 0,
    };
    query.inspect(path, start_byte, end_byte, include_control_conditions)?;
    Ok(query.result)
}

impl Investigation<'_> {
    fn inspect(
        &mut self,
        path: &str,
        start_byte: u32,
        end_byte: u32,
        include_control_conditions: bool,
    ) -> anyhow::Result<()> {
        self.check()?;
        let Some(file) = self
            .store
            .find_files_by_path_prefix(path)?
            .into_iter()
            .find(|f| f.path == path)
        else {
            return Ok(());
        };
        let references = self.store.find_references_by_file(&file.file_id)?;
        let calls: Vec<_> = references
            .iter()
            .filter(|r| {
                r.kind == ReferenceKind::Call
                    && r.range.start_byte < end_byte
                    && start_byte < r.range.end_byte
            })
            .collect();
        self.callable_navigation(&file, start_byte, end_byte)?;
        if include_control_conditions {
            self.control_conditions(file.file_id, start_byte, end_byte)?;
        }
        if file.language != Language::Cpp {
            for call in calls {
                self.gap(call, "syntax_context_unsupported", "Receiver and argument context are not implemented for this language; stored calls and name candidates remain available.", vec![])?;
            }
            return Ok(());
        }
        self.preprocessing_context(file.file_id, start_byte, end_byte)?;
        self.operation_regions(file.file_id, start_byte, end_byte)?;
        self.symbol_static = self
            .store
            .find_symbols_by_file(&file.file_id)?
            .into_iter()
            .map(|s| (s.id, s.static_))
            .collect();
        let bindings = self.store.find_bindings_by_file(&file.file_id)?;
        let scopes: BTreeMap<_, _> = self
            .store
            .find_scopes_by_file(&file.file_id)?
            .into_iter()
            .map(|s| (s.id, s))
            .collect();
        if calls.is_empty() {
            self.values(&file, start_byte, end_byte, &references, &bindings, &scopes)?;
        }
        for call in calls {
            self.check()?;
            self.template_callee(call)?;
            self.receiver(call, &references, &bindings, &scopes)?;
            self.arguments(call, &bindings, &scopes)?;
        }
        self.check()?;
        Ok(())
    }
}

impl Investigation<'_> {
    fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!(self.canceled)(), "call context canceled");
        Ok(())
    }

    fn reserve_item(&self) -> anyhow::Result<()> {
        self.check()?;
        anyhow::ensure!(
            self.result.items.len() + self.result.gaps.len() < MAX_CONTEXT_ITEMS,
            "call context exceeds 10000 records; select a smaller region"
        );
        Ok(())
    }

    fn item(
        &mut self,
        call: &ReferenceUse,
        role: &'static str,
        location: ContextLocation,
        symbol_id: Option<SymbolId>,
        related_locations: Vec<ContextLocation>,
        message: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.reserve_item()?;
        self.result.items.push(CallContextItem {
            subject: ContextSubject::reference(call),
            kind: match role {
                "initializer"
                | "type_reference"
                | "type_alias_target"
                | "argument_expression"
                | "argument_initializer"
                | "callable_name"
                | "template_arguments" => ContextItemKind::Reference,
                _ => ContextItemKind::Declaration,
            },
            role,
            location,
            symbol_id,
            related_locations,
            message: message.into(),
        });
        Ok(())
    }

    fn gap(
        &mut self,
        call: &ReferenceUse,
        code: &'static str,
        message: impl Into<String>,
        related_locations: Vec<ContextLocation>,
    ) -> anyhow::Result<()> {
        self.reserve_item()?;
        self.result.gaps.push(CallContextGap {
            subject: ContextSubject::reference(call),
            code,
            message: message.into(),
            related_locations,
        });
        Ok(())
    }

    fn source(&mut self, id: FileId) -> Result<Arc<ParsedSource>, String> {
        if let Some(parsed) = self.parsed.get(&id) {
            return parsed.clone();
        }
        let parsed = self.load_source(id);
        if let (Some(reusable), Ok(source)) = (&mut self.reusable, &parsed) {
            reusable.insert(id, source.clone());
        }
        self.parsed.insert(id, parsed.clone());
        parsed
    }

    fn load_source(&mut self, id: FileId) -> Result<Arc<ParsedSource>, String> {
        self.check().map_err(|e| e.to_string())?;
        let file = self
            .store
            .get_file(&id)
            .map_err(|e| e.to_string())?
            .ok_or("source metadata is unavailable")?;
        let root = self.root.canonicalize().map_err(|e| e.to_string())?;
        let expected = root.join(&file.path);
        let path = expected.canonicalize().map_err(|e| e.to_string())?;
        if path != expected || !path.starts_with(&root) {
            return Err("source path is not an ordinary file inside the selected root".into());
        }
        let mut input = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let metadata = input.metadata().map_err(|e| e.to_string())?;
        let remaining = MAX_CONTEXT_TOTAL_BYTES.saturating_sub(self.admitted_bytes);
        if !metadata.is_file() || metadata.len() > MAX_CONTEXT_FILE_BYTES.min(remaining) as u64 {
            return Err("source exceeds the 8 MiB file or 32 MiB investigation read limit; read a smaller source range separately".into());
        }
        let stamp = session::SourceStamp::new(&metadata);
        if let Some(reusable) = &mut self.reusable {
            if let Some(parsed) = reusable.get(id, &stamp) {
                self.admitted_bytes += parsed.source.len();
                tracing::debug!(target: "atlas_context_work", event = "source_reuse", path = %file.path, bytes = parsed.source.len());
                return Ok(parsed);
            }
        }
        self.result.files_read += 1;
        tracing::debug!(target: "atlas_context_work", event = "source_open", path = %file.path);
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            self.check().map_err(|e| e.to_string())?;
            let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            self.result.bytes_read += n;
            self.admitted_bytes += n;
            tracing::debug!(target: "atlas_context_work", event = "source_read", path = %file.path, bytes = n);
            if bytes.len() + n > MAX_CONTEXT_FILE_BYTES
                || self.admitted_bytes > MAX_CONTEXT_TOTAL_BYTES
            {
                return Err("source grew beyond the investigation read limit".into());
            }
            bytes.extend_from_slice(&buffer[..n]);
        }
        let source = String::from_utf8(bytes).map_err(|_| "context source is not valid UTF-8")?;
        let cpp = if file.language == Language::Cpp {
            self.store
                .cpp_types_for_file(&id)
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        let frontend = if file.language == Language::Cpp {
            let (ranges, members) = cpp
                .as_ref()
                .map(|facts| {
                    (
                        facts.normalized_annotations.clone(),
                        facts.normalized_member_macros.clone(),
                    )
                })
                .unwrap_or_default();
            extraction::cpp_annotations::frontend(ranges, members)
        } else {
            extraction::create_frontend(file.language)
        }
        .ok_or("source grammar is unavailable")?;
        let mut parser = Parser::new();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .map_err(|e| e.to_string())?;
        let parser_source = frontend.parser.parser_source(&source);
        if parser_source.len() != source.len() {
            return Err("parser source does not preserve byte positions".into());
        }
        let mut progress = |_: &tree_sitter::ParseState| {
            if (self.canceled)() {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        };
        tracing::debug!(target: "atlas_context_work", event = "parse", path = %file.path, bytes = source.len());
        let tree = parser
            .parse_with_options(
                &mut |offset, _| parser_source.as_bytes().get(offset..).unwrap_or(&[]),
                None,
                Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
            )
            .ok_or("context parse was interrupted")?;
        let tree = frontend
            .parser
            .refine_tree(&parser_source, tree, self.canceled)
            .ok_or("context refinement was interrupted")?;
        drop(parser_source);
        Ok(Arc::new(ParsedSource {
            source,
            tree,
            cpp,
            stamp,
        }))
    }

    fn receiver(
        &mut self,
        call: &ReferenceUse,
        references: &[ReferenceUse],
        bindings: &[BindingDef],
        scopes: &BTreeMap<ScopeId, ScopeDef>,
    ) -> anyhow::Result<()> {
        if call.receiver.is_none() {
            return Ok(());
        }
        let parsed = match self.source(call.file_id) {
            Ok(source) => source,
            Err(error) => return self.gap(call, "source_context_unavailable", error, vec![]),
        };
        let receiver = if call.kind == ReferenceKind::Usage {
            cpp::expression(parsed.tree.root_node(), call.range)
        } else {
            cpp::receiver(parsed.tree.root_node(), call.range)
        };
        let Some(receiver) = receiver else {
            // Written namespace/type qualification is not an object receiver.
            if call
                .receiver
                .as_ref()
                .is_some_and(|r| call.text == format!("{r}::{}", call.name))
            {
                return Ok(());
            }
            return self.gap(
                call,
                "receiver_syntax_unavailable",
                "The stored call could not be associated with a supported member-call expression.",
                vec![],
            );
        };
        let location = loc(call.file_id, receiver);
        if receiver.has_error() {
            return self.gap(
                call,
                "receiver_syntax_unavailable",
                "The receiver expression has syntax errors; no binding is selected.",
                vec![location],
            );
        }
        if receiver.kind() == "call_expression" {
            return self.initializer(call, receiver, references, location);
        }
        let name = if call.kind == ReferenceKind::FieldAccess && receiver.kind() == "this" {
            Some((call.name.as_str(), true))
        } else {
            cpp::receiver_name(receiver, &parsed.source)
        };
        let Some((name, explicit_member)) = name else {
            return self.gap(call, "receiver_expression_unsupported", "The receiver is not a simple lexical identifier or direct call; inspect its source expression.", vec![location]);
        };
        let found = if explicit_member {
            vec![]
        } else {
            visible_bindings(
                bindings,
                scopes,
                call.scope_id,
                name,
                receiver.start_byte() as u32,
                parsed
                    .cpp
                    .as_ref()
                    .map_or(&[], |facts| facts.lambda_captures.as_slice()),
            )
        };
        if found.is_empty() {
            let capture_available = !cpp::inside_lambda(receiver)
                || parsed.cpp.as_ref().is_some_and(|facts| {
                    resolution::cpp_captures::field_access(
                        facts,
                        call,
                        (!explicit_member).then_some(name),
                        |id| self.symbol_static.get(&id).copied(),
                    )
                    .is_some()
                });
            if self.member_receiver(call, name, location.clone(), capture_available)? {
                return Ok(());
            }
            return self.gap(call, "receiver_binding_unavailable", "No recorded lexical binding or direct field in the caller's class accounts for this receiver. Namespace lookup, inheritance and missing declarations remain open.", vec![location]);
        }
        if found.len() > 1 {
            self.gap(call, "receiver_binding_ambiguous", "Several declarations introduce this name in the nearest recorded scope; all are retained without selecting a value.", found.iter().map(|b| ContextLocation { file_id: b.file_id, range: b.range }).collect())?;
        }
        let mut has_declaration = false;
        for binding in found {
            self.check()?;
            if cpp::inside_lambda(receiver)
                && !parsed.cpp.as_ref().is_some_and(|facts| {
                    facts
                        .values
                        .iter()
                        .find(|value| value.binding_id == Some(binding.id))
                        .is_some_and(|value| {
                            resolution::cpp_captures::local_effect(facts, name, value, call)
                                .is_some()
                        })
                })
            {
                self.gap(call, "receiver_capture_unverified", "Recorded capture facts do not establish access to this lexical declaration from the closure. The declaration and receiver remain source locations for investigation.", vec![
                    location.clone(),
                    ContextLocation { file_id: binding.file_id, range: binding.range },
                ])?;
                continue;
            }
            has_declaration = true;
            let Some(syntax) = cpp::binding(parsed.tree.root_node(), binding.range) else {
                self.item(
                    call,
                    binding_role(call),
                    ContextLocation {
                        file_id: binding.file_id,
                        range: binding.range,
                    },
                    binding.symbol_id,
                    vec![location.clone()],
                    "Recorded lexical binding; its declaration syntax is unavailable.",
                )?;
                self.gap(
                    call,
                    "binding_syntax_unavailable",
                    "Cannot locate the binding's declaration and initializer syntax.",
                    vec![ContextLocation {
                        file_id: binding.file_id,
                        range: binding.range,
                    }],
                )?;
                continue;
            };
            let declaration = loc(call.file_id, syntax.declaration);
            self.item(call, binding_role(call), declaration.clone(), binding.symbol_id, vec![location.clone()], format!("Lexical declaration of {name}; an initializer is not proof of the receiver's current value."))?;
            if let Some(ty) = syntax.type_node {
                self.type_context(call, ty, &parsed, declaration.clone(), true)?;
            }
            if let Some(value) = syntax.initializer {
                self.initializer(call, value, references, declaration)?;
            }
        }
        if !has_declaration {
            return Ok(());
        }
        self.gap(call, "receiver_semantics_unverified", "Declaration/initializer/type syntax is available, but assignments, aliases, template semantics and runtime dispatch have not been established by this investigation.", vec![location])
    }

    fn arguments(
        &mut self,
        call: &ReferenceUse,
        bindings: &[BindingDef],
        scopes: &BTreeMap<ScopeId, ScopeDef>,
    ) -> anyhow::Result<()> {
        let arguments = match self.store.find_callsite_by_reference_id(&call.id)? {
            Some(site) => site.args,
            None if call.arity == Some(0) => return Ok(()),
            None => {
                // Persisted callsites require a caller identity. Written argument
                // positions do not: reuse the extraction frontend for this exact
                // reference, without creating a Callsite, caller or target binding.
                let parsed = match self.source(call.file_id) {
                    Ok(source) => source,
                    Err(error) => {
                        return self.gap(call, "source_context_unavailable", error, vec![]);
                    }
                };
                let parts = extraction::callsite_spec::create_extractor(Language::Cpp)
                    .extract_callsite(
                        parsed.tree.root_node(),
                        call.range.start_byte as usize,
                        call.range.end_byte as usize,
                        &parsed.source,
                    )
                    .filter(|parts| {
                        parts.callee_range.start_byte == call.range.start_byte
                            && parts.callee_range.end_byte == call.range.end_byte
                            && call
                                .arity
                                .is_none_or(|arity| parts.argument_ranges.len() == arity as usize)
                    });
                let Some(parts) = parts else {
                    return self.gap(call, "argument_locations_unavailable", "Argument syntax could not be matched to this recorded callee and argument count; inspect the original call source.", vec![]);
                };
                parts
                    .argument_ranges
                    .into_iter()
                    .enumerate()
                    .map(|(index, range)| ArgumentFact {
                        index: index as u32,
                        name: None,
                        value: parsed.source[range.start_byte as usize..range.end_byte as usize]
                            .to_owned(),
                        range: Some(range),
                        data_node_id: None,
                    })
                    .collect()
            }
        };
        if arguments.is_empty() {
            return Ok(());
        }
        self.argument_parameters(call, &arguments)?;
        let parsed = match self.source(call.file_id) {
            Ok(source) => source,
            Err(error) => return self.gap(call, "source_context_unavailable", error, vec![]),
        };
        let closures = parsed
            .cpp
            .as_ref()
            .map_or(&[][..], |facts| facts.lambda_captures.as_slice());
        // Copy identities before mutating the investigation; these are existing
        // callable entities, never identities manufactured by the source reparse.
        let callable_ids: BTreeMap<_, _> = closures
            .iter()
            .filter_map(|lambda| {
                lambda
                    .symbol_id
                    .filter(|id| self.symbol_static.contains_key(id))
                    .map(|id| ((lambda.range.start_byte, lambda.range.end_byte), id))
            })
            .collect();
        let mut argument_locations = vec![];
        for argument in arguments {
            self.check()?;
            let Some(range) = argument.range else {
                self.gap(
                    call,
                    "argument_locations_unavailable",
                    format!(
                        "Argument {} has no recorded source range.",
                        argument.index + 1
                    ),
                    vec![],
                )?;
                continue;
            };
            let location = ContextLocation {
                file_id: call.file_id,
                range,
            };
            argument_locations.push(location.clone());
            self.item(call, "argument_expression", location.clone(), callable_ids.get(&(range.start_byte, range.end_byte)).copied(), vec![], format!("Written argument {}; its position does not establish a runtime value or parameter mapping.", argument.index + 1))?;
            let Some(expression) = cpp::expression(parsed.tree.root_node(), range) else {
                self.gap(call, "argument_syntax_unavailable", "The recorded argument range cannot be associated with an exact syntax node; inspect its source.", vec![location])?;
                continue;
            };
            if expression.has_error() {
                self.gap(call, "argument_syntax_unavailable", "The argument contains parser errors; its written location remains available without selecting a binding.", vec![location])?;
                continue;
            }
            // Only plain local/parameter names have declaration navigation here.
            // Other expressions remain independently readable, not silently dropped.
            if expression.kind() != "identifier" {
                continue;
            }
            let name = cpp::text(expression, &parsed.source);
            let found = visible_bindings(
                bindings,
                scopes,
                call.scope_id,
                name,
                range.start_byte,
                closures,
            );
            if found.is_empty() {
                self.gap(call, "argument_binding_unavailable", "No recorded lexical declaration accounts for this argument name. Fields, globals, function names and missing declarations require further investigation.", vec![location])?;
                continue;
            }
            if found.len() > 1 {
                self.gap(call, "argument_binding_ambiguous", "Several declarations introduce this argument name in the nearest recorded scope; retain all origins without selecting a value.", found.iter().map(|binding| ContextLocation { file_id: binding.file_id, range: binding.range }).collect())?;
            }
            for binding in found {
                let origin = ContextLocation {
                    file_id: binding.file_id,
                    range: binding.range,
                };
                if cpp::inside_lambda(expression)
                    && !parsed.cpp.as_ref().is_some_and(|facts| {
                        facts
                            .values
                            .iter()
                            .find(|value| value.binding_id == Some(binding.id))
                            .is_some_and(|value| {
                                resolution::cpp_captures::local_effect(facts, name, value, call)
                                    .is_some()
                            })
                    })
                {
                    self.gap(call, "argument_capture_unverified", "Recorded captures do not establish access to this lexical declaration from the closure; inspect the use and declaration separately.", vec![location.clone(), origin])?;
                    continue;
                }
                let Some(syntax) = cpp::binding_origin(parsed.tree.root_node(), binding.range)
                else {
                    self.item(
                        call,
                        "argument_binding",
                        origin.clone(),
                        binding.symbol_id,
                        vec![location.clone()],
                        "Recorded lexical declaration; initializer syntax is unavailable.",
                    )?;
                    self.gap(
                        call,
                        "argument_syntax_unavailable",
                        "Cannot locate the argument declaration's syntax.",
                        vec![origin],
                    )?;
                    continue;
                };
                let declaration = loc(call.file_id, syntax.declaration);
                self.item(call, "argument_binding", declaration.clone(), binding.symbol_id, vec![location.clone()], format!("Recorded lexical declaration of argument {name}; this is not a reaching-value or resolved-symbol-reference result."))?;
                if syntax.declaration.has_error() {
                    self.gap(call, "argument_declaration_syntax_partial", "The declaration contains parser errors. Its written declaration/initializer extents are investigation candidates; no type, value or execution conclusion follows.", vec![location.clone(), declaration.clone()])?;
                }
                if !syntax.declaration.has_error()
                    && let Some(ty) = syntax.type_node
                {
                    self.type_context(call, ty, &parsed, declaration.clone(), false)?;
                }
                if let Some(initializer) = syntax.initializer {
                    let at = loc(call.file_id, initializer);
                    let symbol = callable_ids
                        .get(&(at.range.start_byte, at.range.end_byte))
                        .copied();
                    self.item(call, "argument_initializer", at, symbol, vec![location.clone(), declaration], "Expression written at the argument's declaration. Later assignments and aliasing are not tracked; an indexed callable ID identifies its body, not whether this call executes it.")?;
                }
            }
        }
        self.gap(call, "argument_value_unestablished", "Argument source and available declaration origins are investigation context. Reaching values, argument-to-parameter propagation and callback execution have not been computed by this query.", argument_locations)
    }

    fn argument_parameters(
        &mut self,
        call: &ReferenceUse,
        arguments: &[ArgumentFact],
    ) -> anyhow::Result<()> {
        let Some(target) = self
            .store
            .find_resolved_callsite_by_reference_id(&call.id)?
            .map(|site| self.store.find_symbol_by_id(&site.callee))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        if target.language != Language::Cpp {
            return Ok(());
        }
        let origin = ContextLocation {
            file_id: target.file_id,
            range: target.name_range,
        };
        let actual_source = match self.source(call.file_id) {
            Ok(parsed) => parsed,
            Err(error) => return self.gap(call, "source_context_unavailable", error, vec![]),
        };
        if arguments.iter().any(|argument| {
            argument
                .range
                .and_then(|range| cpp::expression(actual_source.tree.root_node(), range))
                .is_none_or(|node| node.has_error() || node.kind() == "parameter_pack_expansion")
        }) {
            return self.gap(call, "parameter_positions_unavailable", "Argument syntax is incomplete or contains an unexpanded parameter pack; written argument positions cannot be mapped to this target's parameters.", vec![origin]);
        }
        let parsed = match self.source(target.file_id) {
            Ok(parsed) => parsed,
            Err(error) => return self.gap(call, "source_context_unavailable", error, vec![origin]),
        };
        let Some(parameters) = cpp::callable(parsed.tree.root_node(), target.name_range)
            .and_then(|syntax| syntax.parameters)
        else {
            return self.gap(call, "parameter_positions_unavailable", "The recorded target's parameter syntax is unavailable; inspect the original declaration before mapping arguments.", vec![origin]);
        };
        let mut cursor = parameters.walk();
        let parameters: Vec<_> = parameters
            .named_children(&mut cursor)
            .filter(|node| node.kind() != "comment")
            .collect();
        if parameters.iter().any(|parameter| {
            !matches!(
                parameter.kind(),
                "parameter_declaration" | "optional_parameter_declaration"
            ) || parameter.has_error()
                || parameter
                    .child(0)
                    .is_some_and(|child| child.kind() == "this")
        }) || arguments
            .iter()
            .any(|argument| argument.index as usize >= parameters.len())
        {
            return self.gap(call, "parameter_positions_unavailable", "Parameter packs, variadic or explicit-object parameters, syntax recovery or incompatible argument positions prevent this positional mapping. No written parameter is selected.", vec![origin]);
        }
        for argument in arguments {
            self.check()?;
            let Some(range) = argument.range else {
                continue;
            };
            self.item(call, "argument_parameter", loc(target.file_id, parameters[argument.index as usize]), Some(target.id),
                vec![ContextLocation { file_id: call.file_id, range }],
                format!("Written parameter {} of the recorded call target corresponds positionally to argument {}. The entity ID identifies that callee, not the parameter. Conversion, reaching values and callback execution are not established by this correspondence.", argument.index + 1, argument.index + 1))?;
        }
        Ok(())
    }

    fn member_receiver(
        &mut self,
        call: &ReferenceUse,
        name: &str,
        receiver: ContextLocation,
        capture_available: bool,
    ) -> anyhow::Result<bool> {
        let Some(caller) = call
            .source_symbol
            .map(|id| self.store.find_symbol_by_id(&id))
            .transpose()?
            .flatten()
        else {
            return Ok(false);
        };
        let Some((owner, _)) = caller.qualified_name.rsplit_once("::") else {
            return Ok(false);
        };
        let mut fields = vec![];
        for field in self
            .store
            .find_symbols_by_qname(&format!("{owner}::{name}"))?
        {
            self.check()?;
            if field.language != Language::Cpp || field.kind != SymbolKind::Field {
                continue;
            }
            let Some(container) = field
                .container
                .map(|id| self.store.find_symbol_by_id(&id))
                .transpose()?
                .flatten()
            else {
                continue;
            };
            if matches!(container.kind, SymbolKind::Class | SymbolKind::Struct)
                && container.qualified_name == owner
            {
                fields.push(field);
            }
        }
        if fields.is_empty() {
            return Ok(false);
        }
        if !capture_available {
            let mut locations = vec![receiver];
            locations.extend(fields.iter().map(|field| ContextLocation {
                file_id: field.file_id,
                range: field.name_range,
            }));
            self.gap(call, "receiver_capture_unverified", "Recorded capture and enclosing-object facts do not support accessing these outer fields. The locations are investigation candidates; unsupported syntax is not proof that a capture is absent.", locations)?;
            return Ok(true);
        }
        if fields.len() > 1 {
            self.gap(call, "receiver_binding_ambiguous", "Several indexed class-field declarations account for this name; all are retained without selecting a build variant or object value.", fields.iter().map(|f| ContextLocation { file_id: f.file_id, range: f.name_range }).collect())?;
        }
        for field in fields {
            let at = ContextLocation {
                file_id: field.file_id,
                range: field.name_range,
            };
            let parsed = match self.source(field.file_id) {
                Ok(parsed) => parsed,
                Err(error) => {
                    self.item(
                        call,
                        binding_role(call),
                        at.clone(),
                        Some(field.id),
                        vec![receiver.clone()],
                        "Indexed field in the caller's class; declaration source is unavailable.",
                    )?;
                    self.gap(call, "source_context_unavailable", error, vec![at])?;
                    continue;
                }
            };
            let Some(syntax) = cpp::binding(parsed.tree.root_node(), field.name_range) else {
                self.item(
                    call,
                    binding_role(call),
                    at.clone(),
                    Some(field.id),
                    vec![receiver.clone()],
                    "Indexed field in the caller's class; declaration syntax is unavailable.",
                )?;
                self.gap(
                    call,
                    "binding_syntax_unavailable",
                    "Cannot associate the indexed field with supported declaration syntax.",
                    vec![at],
                )?;
                continue;
            };
            let declaration = loc(field.file_id, syntax.declaration);
            self.item(call, binding_role(call), declaration.clone(), Some(field.id), vec![receiver.clone()], "Field declared in the caller's class; this establishes a declaration origin, not the receiver's current value or smart-pointer semantics.")?;
            if let Some(ty) = syntax.type_node {
                self.type_context(call, ty, &parsed, declaration, true)?;
            }
        }
        self.gap(call, "receiver_semantics_unverified", "Class-field declarations and written types are available. Initializers, assignments, aliases, dereference operators and runtime dispatch remain unverified.", vec![receiver])?;
        Ok(true)
    }

    fn initializer(
        &mut self,
        call: &ReferenceUse,
        value: Node<'_>,
        references: &[ReferenceUse],
        origin: ContextLocation,
    ) -> anyhow::Result<()> {
        let location = loc(call.file_id, value);
        self.item(call, "initializer", location.clone(), None, vec![origin], "Expression written at the receiver's origin; this is not a points-to or reaching-value result.")?;
        if value.has_error() || value.kind() != "call_expression" {
            return self.gap(call, "initializer_value_unestablished", "Only a direct call initializer has callable-return context here; aliases, compound expressions and later assignments remain open.", vec![location]);
        }
        let Some(callee) = value.child_by_field_name("function") else {
            return Ok(());
        };
        let init_calls: Vec<_> = references
            .iter()
            .filter(|r| {
                r.kind == ReferenceKind::Call
                    && r.range.start_byte >= callee.start_byte() as u32
                    && r.range.end_byte <= callee.end_byte() as u32
            })
            .collect();
        if init_calls.len() != 1 {
            return self.gap(
                call,
                "initializer_call_unavailable",
                "No unique stored call reference accounts for this initializer's callee.",
                vec![location],
            );
        }
        let init_call = init_calls[0];
        let declarations = if let Some(target) = &init_call.resolved {
            match self.store.find_symbol_by_id(&target.symbol_id)? {
                Some(symbol) => self.store.find_symbols_by_qname(&symbol.qualified_name)?,
                None => vec![],
            }
        } else {
            self.gap(call, "initializer_target_unresolved", "The initializer call has no resolved target; same-name callable declarations are retained as hypotheses.", vec![location.clone()])?;
            self.store.find_symbols_by_name(&init_call.name)?
        };
        let mut found = false;
        for symbol in declarations.into_iter().filter(|s| {
            s.language == Language::Cpp
                && matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
        }) {
            self.check()?;
            found = true;
            let file = match self.source(symbol.file_id) {
                Ok(file) => file,
                Err(error) => {
                    let at = ContextLocation {
                        file_id: symbol.file_id,
                        range: symbol.name_range,
                    };
                    self.item(
                        call,
                        "callable_declaration",
                        at.clone(),
                        Some(symbol.id),
                        vec![location.clone()],
                        "Indexed callable candidate; its source could not be inspected.",
                    )?;
                    self.gap(call, "source_context_unavailable", error, vec![at])?;
                    continue;
                }
            };
            let Some(declaration) = cpp::callable(file.tree.root_node(), symbol.name_range) else {
                self.gap(call, "return_declaration_unavailable", "The indexed callable has no supported declaration syntax at its recorded name.", vec![ContextLocation { file_id: symbol.file_id, range: symbol.name_range }])?;
                continue;
            };
            let at = ContextLocation {
                file_id: symbol.file_id,
                range: declaration.header,
            };
            let basis = if init_call.resolved.is_some() {
                "Declaration with the indexed initializer target's qualified name; each overload remains an investigation candidate."
            } else {
                "Same-name declaration for an unresolved initializer call; applicability is unverified."
            };
            self.item(
                call,
                "callable_declaration",
                at.clone(),
                Some(symbol.id),
                vec![location.clone()],
                basis,
            )?;
            if let Some(ty) = declaration.type_node {
                self.type_context(call, ty, &file, at, true)?;
            } else {
                self.gap(
                    call,
                    "return_type_unavailable",
                    "The declaration does not supply a supported explicit return-type syntax.",
                    vec![at],
                )?;
            }
        }
        if !found {
            self.gap(call, "initializer_declaration_unavailable", "No indexed callable declaration accounts for the initializer; source search remains necessary.", vec![location])?;
        }
        Ok(())
    }

    fn type_context(
        &mut self,
        call: &ReferenceUse,
        ty: Node<'_>,
        parsed: &ParsedSource,
        declaration: ContextLocation,
        receiver_members: bool,
    ) -> anyhow::Result<()> {
        for name_node in cpp::type_names(ty, &parsed.source) {
            self.check()?;
            let name = cpp::text(name_node, &parsed.source);
            let mention = loc(declaration.file_id, name_node);
            self.item(call, "type_reference", mention.clone(), None, vec![declaration.clone()], format!("{name} is written in declaration type syntax. A template argument or alias name is a source clue, not a resolved or effective type."))?;
            let symbols = self.store.find_symbols_by_name(name)?;
            let mut found = false;
            for symbol in symbols.into_iter().filter(|s| {
                s.language == Language::Cpp
                    && matches!(
                        s.kind,
                        SymbolKind::Class
                            | SymbolKind::Struct
                            | SymbolKind::Enum
                            | SymbolKind::TypeAlias
                    )
            }) {
                self.check()?;
                found = true;
                let at = ContextLocation {
                    file_id: symbol.file_id,
                    range: symbol.name_range,
                };
                self.item(call, "type_declaration", at.clone(), Some(symbol.id), vec![mention.clone()], "Indexed type declaration with this written name; qualification, aliases and applicability remain to be checked.")?;
                if symbol.kind == SymbolKind::TypeAlias
                    && let Some(facts) = self.store.cpp_types_for_file(&symbol.file_id)?
                    && let Some(alias) = facts
                        .aliases
                        .iter()
                        .find(|alias| alias.symbol_id == symbol.id && alias.target.is_some())
                {
                    self.item(call, "type_alias_target", ContextLocation {
                        file_id: symbol.file_id,
                        range: alias.target_range,
                    }, None, vec![at.clone(), mention.clone()],
                        "Written target syntax of this indexed alias candidate. Its name, macro expansion and applicability are not resolved; follow the source without treating this as a type binding.")?;
                }
                self.type_recovery_context(call, &symbol, &at)?;
                if !receiver_members {
                    continue;
                }
                for member in self
                    .store
                    .find_symbols_by_qname(&format!("{}::{}", symbol.qualified_name, call.name))?
                {
                    self.check()?;
                    if member.language != Language::Cpp {
                        continue;
                    }
                    self.item(call, "type_member", ContextLocation { file_id: member.file_id, range: member.range }, Some(member.id), vec![at.clone()], "Member name under a type mentioned by the receiver declaration/initializer. This is an investigation candidate, not a resolved member lookup.")?;
                }
            }
            if !found {
                self.gap(call, "type_declaration_unavailable", format!("No indexed type declaration matches the written name {name}; dependencies, aliases or extraction may be missing."), vec![mention])?;
            }
        }
        Ok(())
    }

    fn type_recovery_context(
        &mut self,
        call: &ReferenceUse,
        symbol: &SymbolDef,
        declaration: &ContextLocation,
    ) -> anyhow::Result<()> {
        if !matches!(symbol.kind, SymbolKind::Class | SymbolKind::Struct)
            || self
                .store
                .get_file(&symbol.file_id)?
                .is_none_or(|file| file.status == ParseStatus::Success)
        {
            return Ok(());
        }
        // Clean candidate files need no additional read. Partially parsed
        // files reuse the same bounded, annotation-aware parser as receivers.
        let parsed = match self.source(symbol.file_id) {
            Ok(parsed) => parsed,
            Err(error) => {
                return self.gap(
                    call,
                    "source_context_unavailable",
                    error,
                    vec![declaration.clone()],
                );
            }
        };
        let Some(record) = cpp::record(parsed.tree.root_node(), symbol.name_range) else {
            return self.gap(
                call,
                "candidate_type_syntax_unavailable",
                format!("Cannot associate candidate type {} with its own class/struct syntax. Candidate applicability and declaration coverage remain unverified.", symbol.qualified_name),
                vec![declaration.clone()],
            );
        };
        let mut cursor = record.walk();
        loop {
            self.check()?;
            let node = cursor.node();
            if node.is_error() || node.is_missing() {
                self.gap(
                    call,
                    "candidate_type_syntax_recovery",
                    format!("Candidate type {} has {}. Its applicability and declaration/body coverage remain unverified; parser recovery is not a compiler diagnostic or a proven cause of this call's unresolved target.", symbol.qualified_name, if node.is_missing() { "a missing-token anchor (zero-width)" } else { "an unparsed region" }),
                    vec![declaration.clone(), loc(symbol.file_id, node)],
                )?;
            } else if node.has_error() && cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return Ok(());
                }
            }
        }
    }
}

fn visible_bindings<'a>(
    bindings: &'a [BindingDef],
    scopes: &BTreeMap<ScopeId, ScopeDef>,
    mut scope: Option<ScopeId>,
    name: &str,
    before: u32,
    closures: &[types::cpp::CppLambdaCapture],
) -> Vec<&'a BindingDef> {
    let mut visited = BTreeSet::new();
    while let Some(id) = scope {
        if !visited.insert(id) {
            break;
        }
        let found: Vec<_> = bindings
            .iter()
            .filter(|b| b.scope_id == id && b.name == name && b.visible_from_byte <= before)
            .collect();
        if !found.is_empty() {
            return found;
        }
        let Some(current) = scopes.get(&id) else {
            break;
        };
        if current.kind == ScopeKind::Function
            && !closures.iter().any(|lambda| lambda.range == current.range)
        {
            break;
        }
        scope = current.parent_id;
    }
    vec![]
}

fn loc(file_id: FileId, node: Node<'_>) -> ContextLocation {
    ContextLocation {
        file_id,
        range: cpp::range(node),
    }
}

fn binding_role(reference: &ReferenceUse) -> &'static str {
    if reference.kind == ReferenceKind::Usage
        || (reference.kind == ReferenceKind::FieldAccess
            && reference.receiver.as_deref() == Some("this"))
    {
        "value_binding"
    } else {
        "receiver_binding"
    }
}

#[cfg(all(test, feature = "cpp"))]
mod tests;
