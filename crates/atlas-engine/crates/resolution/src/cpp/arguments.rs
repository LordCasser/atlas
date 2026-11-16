//! Ordinary and associated free-function lookup from recorded argument types.
//! This does not instantiate templates or infer arbitrary expression types.
use super::*;

struct ArgumentType {
    declared: CppDeclaredType,
    scope: String,
    site: ReferenceUse,
    lvalue: bool,
    // Top-level pointer const from a capture or a containing object's cv.
    // CppDeclaredType::const_ describes the pointee for pointer types.
    capture_const: bool,
}

/// A builtin integral expression can establish applicability without choosing
/// its implementation-dependent promoted type. This weaker fact must not rank
/// overloads, instantiate templates or supply a value-flow type.
enum ArgumentEvidence {
    Exact(Box<ArgumentType>),
    IntegralValue,
}

fn integral_type(identity: &ArgumentIdentity<'_>) -> bool {
    matches!(&identity.named, NamedType::Fundamental(name) if matches!(name.as_str(),
        "bool" | "char" | "signed char" | "unsigned char" | "wchar_t" | "char8_t"
        | "char16_t" | "char32_t" | "short" | "unsigned short" | "int" | "unsigned int"
        | "long" | "unsigned long" | "long long" | "unsigned long long"))
}

/// A class template's declaration alone is not its instantiated type identity.
/// Keep type arguments and their pointer/reference/cv layers at their use site.
struct ArgumentIdentity<'a> {
    named: NamedType<'a>,
    arguments: Vec<ArgumentIdentity<'a>>,
    modifiers: (bool, bool, bool, bool),
}

impl ArgumentIdentity<'_> {
    fn same_identity(&self, other: &Self) -> bool {
        self.named.same_identity(&other.named)
            && self.arguments.len() == other.arguments.len()
            && self
                .arguments
                .iter()
                .zip(&other.arguments)
                .all(|(a, b)| a.modifiers == b.modifiers && a.same_identity(b))
    }
}

/// Character types whose spelling/encoding needs no target or build setting.
/// u8 changed type across C++ versions; multicharacter and conditional escapes
/// require implementation support. Keep those forms unknown without that input.
fn character_literal_type(text: &str) -> Option<&'static str> {
    let (prefix, tail) = text.split_once('\'')?;
    let body = tail.strip_suffix('\'')?;
    // Numeric escapes can use the corresponding unsigned range without
    // changing the literal's type. Their numeric runtime value is not inferred.
    let (name, numeric_limit) = match prefix {
        "" => ("char", Some(255)),
        "L" => ("wchar_t", None), // Underlying integer type is implementation-defined.
        "u" => ("char16_t", Some(65535)),
        "U" => ("char32_t", Some(u32::MAX)),
        _ => return None,
    };
    let encoded = |c: char| {
        (c.is_ascii() && !c.is_control() && !matches!(c, '\'' | '\\'))
            || (prefix == "u" && !c.is_ascii() && c.len_utf16() == 1)
            || (prefix == "U" && !c.is_ascii())
    };
    if let Some(escape) = body.strip_prefix('\\') {
        if matches!(
            escape,
            "'" | "\"" | "?" | "\\" | "a" | "b" | "f" | "n" | "r" | "t" | "v"
        ) {
            return Some(name);
        }
        let (digits, radix) = if let Some(digits) = escape.strip_prefix('x') {
            (digits, 16)
        } else if escape.len() <= 3 && escape.bytes().all(|b| matches!(b, b'0'..=b'7')) {
            (escape, 8)
        } else {
            let digits = escape
                .strip_prefix('u')
                .filter(|s| s.len() == 4)
                .or_else(|| escape.strip_prefix('U').filter(|s| s.len() == 8))?;
            if !matches!(prefix, "u" | "U") || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let c = char::from_u32(u32::from_str_radix(digits, 16).ok()?)?;
            return (prefix == "U" || c.len_utf16() == 1).then_some(name);
        };
        if digits.is_empty()
            || !digits.bytes().all(|b| {
                if radix == 16 {
                    b.is_ascii_hexdigit()
                } else {
                    matches!(b, b'0'..=b'7')
                }
            })
        {
            return None;
        }
        return (u32::from_str_radix(digits, radix).ok()? <= numeric_limit?).then_some(name);
    }
    let mut chars = body.chars();
    let c = chars.next()?;
    (chars.next().is_none() && encoded(c)).then_some(name)
}

pub(super) fn fundamental(name: &str) -> Option<String> {
    let words: Vec<_> = name.split_whitespace().collect();
    match words.as_slice() {
        [
            "bool" | "char" | "wchar_t" | "char8_t" | "char16_t" | "char32_t" | "float" | "double"
            | "void",
        ] => Some(name.into()),
        ["long", "double"] => Some("long double".into()),
        _ => {
            if words.is_empty()
                || !words.iter().all(|w| {
                    matches!(
                        *w,
                        "signed" | "unsigned" | "short" | "long" | "int" | "char"
                    )
                })
            {
                return None;
            }
            let unsigned = words.contains(&"unsigned");
            let base = if words.contains(&"char") {
                "char"
            } else if words.contains(&"short") {
                "short"
            } else {
                match words.iter().filter(|w| **w == "long").count() {
                    0 => "int",
                    1 => "long",
                    2 => "long long",
                    _ => return None,
                }
            };
            Some(if unsigned {
                format!("unsigned {base}")
            } else if base == "char" {
                "signed char".into()
            } else {
                base.into()
            })
        }
    }
}

impl TypeIndex {
    pub(super) fn check_template_argument_types(
        &self,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
        symbol: &SymbolDef,
        remaining: &mut usize,
    ) -> Lookup<()> {
        let call = self
            .template_call(reference)
            .ok_or(LookupFailure::Unspecified)?;
        let written_types = call.arguments.as_ref().ok_or(LookupFailure::Unspecified)?;
        let callable = self
            .written_callable(symbol)
            .ok_or(LookupFailure::Unspecified)?;
        let parameters = callable
            .template_parameters
            .as_ref()
            .ok_or(LookupFailure::Unspecified)?;
        if parameters.len() != written_types.len() {
            return Err(LookupFailure::Unspecified);
        }
        let owner = reference
            .source_symbol
            .and_then(|id| ctx.symbols_by_id.get(&id))
            .ok_or(LookupFailure::Unspecified)?;
        let caller_scope = owner
            .qualified_name
            .rsplit_once("::")
            .map_or("", |(scope, _)| scope);
        let site = ReferenceUse {
            range: call.arguments_range,
            ..reference.clone()
        };
        let bindings = written_types
            .iter()
            .map(|ty| self.identity_with_budget(ty, caller_scope, &site, remaining))
            .collect::<Lookup<Vec<_>>>()?;
        // Substitution can invalidate the signature even when no function
        // argument is supplied. A void return is valid; a void reference or
        // a named void parameter is not. This checks the represented type
        // shape, without instantiating the function body.
        for (ty, is_parameter) in callable
            .parameter_declared_types
            .iter()
            .map(|ty| (ty.as_ref(), true))
            .chain(std::iter::once((callable.return_type.as_ref(), false)))
        {
            let ty = ty.ok_or(LookupFailure::Unspecified)?;
            if let Some(position) = parameters.iter().position(|name| name == &ty.name) {
                let is_void = matches!(&bindings[position].named,
                    NamedType::Fundamental(name) if name == "void");
                if is_void && !ty.pointer && (is_parameter || ty.reference) {
                    return Err(LookupFailure::Unspecified);
                }
            }
        }
        let args = self
            .written_arguments(reference)
            .ok_or(LookupFailure::Unspecified)?;
        if Some(args.len()) != reference.arity.map(|arity| arity as usize)
            || args.len() > callable.parameter_declared_types.len()
        {
            return Err(LookupFailure::Unspecified);
        }
        let declaration_scope = symbol
            .qualified_name
            .rsplit_once("::")
            .map_or("", |(scope, _)| scope);
        let declaration_site = ReferenceUse {
            file_id: symbol.file_id,
            range: symbol.name_range,
            ..reference.clone()
        };
        for (i, expression) in args.iter().enumerate() {
            let arg = self.argument(expression, reference, ctx, remaining)?;
            let arg_id =
                self.identity_with_budget(&arg.declared, &arg.scope, &arg.site, remaining)?;
            let parameter = callable.parameter_declared_types[i]
                .as_ref()
                .ok_or(LookupFailure::Unspecified)?;
            let independent;
            let (parameter_id, extra_const, extra_volatile) = if let Some(position) =
                parameters.iter().position(|name| name == &parameter.name)
            {
                if !parameter.template_arguments.is_empty() {
                    return Err(LookupFailure::Unspecified);
                }
                (
                    &bindings[position],
                    written_types[position].const_,
                    written_types[position].volatile,
                )
            } else {
                fn dependent(ty: &CppDeclaredType, names: &[String]) -> bool {
                    names
                        .iter()
                        .any(|n| ty.name == *n || ty.name.starts_with(&format!("{n}::")))
                        || ty.template_arguments.iter().any(|t| dependent(t, names))
                }
                if dependent(parameter, parameters) {
                    return Err(LookupFailure::Unspecified);
                }
                independent = self.identity_with_budget(
                    parameter,
                    declaration_scope,
                    &declaration_site,
                    remaining,
                )?;
                (&independent, false, false)
            };
            let const_ = parameter.const_ || extra_const;
            let volatile = parameter.volatile || extra_volatile;
            let void_value = matches!(&parameter_id.named, NamedType::Fundamental(name) if name == "void")
                && !parameter.pointer;
            let cv =
                (!arg.declared.const_ || const_ || (!parameter.pointer && !parameter.reference))
                    && (!arg.declared.volatile
                        || volatile
                        || (!parameter.pointer && !parameter.reference));
            let reference_ok = !parameter.reference
                || if callable.parameter_types[i].ends_with("&&") {
                    !arg.lvalue
                } else {
                    arg.lvalue || (const_ && !volatile)
                };
            if void_value
                || !arg_id.same_identity(parameter_id)
                || arg.declared.pointer != parameter.pointer
                || !cv
                || !reference_ok
                || (arg.capture_const && arg.declared.pointer && parameter.reference)
            {
                return Err(LookupFailure::Unspecified);
            }
        }
        Ok(())
    }

    fn written_arguments(
        &self,
        reference: &ReferenceUse,
    ) -> Option<&[types::cpp::CppArgumentExpression]> {
        let (file, position) = self.arguments.get(&reference.id)?;
        let (_, args) = self.files.get(file)?.arguments.get(*position)?;
        Some(args)
    }

    /// Reject a demonstrated conflict without requiring complete argument
    /// inference for every otherwise selected declaration. This is not overload
    /// ranking or a proof that the remaining argument conversions are valid.
    pub(super) fn check_argument_conflicts(
        &self,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
        symbol: &SymbolDef,
        deductions_remaining: &mut usize,
    ) -> Lookup<()> {
        use types::cpp::CppArgumentExpression as Expr;
        let Some(callable) = self.callable(symbol) else {
            return Ok(());
        };
        let Some(args) = self.written_arguments(reference) else {
            return Ok(());
        };
        if Some(args.len()) != reference.arity.map(|arity| arity as usize) {
            return Ok(());
        }
        for (written, parameter) in args.iter().zip(&callable.parameter_declared_types) {
            if !parameter.as_ref().is_some_and(|ty| ty.pointer) {
                continue;
            }
            match written {
                Expr::Name(name) => {
                    // Macro replacement can change a written variable into a
                    // null pointer constant. Recorded spelling alone cannot
                    // establish the argument type in that case.
                    if self.visible_files(reference.file_id).iter().any(|file| {
                        self.files.get(file).is_some_and(|facts| {
                            facts.macros.iter().any(|definition| {
                                definition.name == *name
                                    && (*file != reference.file_id
                                        || definition.range.start_byte < reference.range.start_byte)
                            })
                        })
                    }) {
                        continue;
                    }
                }
                Expr::Literal(value)
                    if value.bytes().all(|c| c.is_ascii_digit())
                        && value.parse::<u32>().is_ok_and(|n| n != 0) => {}
                _ => continue,
            }
            let Ok(arg) = self.argument(written, reference, ctx, deductions_remaining) else {
                continue;
            };
            // Const integral names can be null pointer constants under older
            // C++ rules. No language-version fact currently resolves that case.
            if arg.declared.pointer || arg.declared.const_ {
                continue;
            }
            let Ok(ArgumentIdentity {
                named: NamedType::Fundamental(name),
                ..
            }) = self.identity(&arg.declared, &arg.scope, &arg.site)
            else {
                // Classes can have user-defined conversions to pointer types;
                // missing or unsupported type identity is not incompatibility.
                continue;
            };
            if name == "void" {
                continue;
            }
            return Err(LookupFailure::Type(Box::new(
                types::cpp::CppTypeLookupFailure {
                    kind: types::cpp::CppTypeLookupFailureKind::ArgumentIncompatible,
                    name,
                    scope: arg.scope,
                    file_id: arg.site.file_id,
                    range: arg.site.range,
                    related_declarations: vec![(symbol.file_id, symbol.range)],
                },
            )));
        }
        Ok(())
    }

    pub(super) fn local_value<'a>(
        &'a self,
        name: &str,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
    ) -> Lookup<Option<LocalValue<'a>>> {
        let facts = self
            .files
            .get(&reference.file_id)
            .ok_or(LookupFailure::Unspecified)?;
        let mut scope = reference.scope_id;
        while let Some(id) = scope {
            let bindings: Vec<_> = ctx
                .bindings
                .iter()
                .filter(|b| {
                    b.scope_id == id
                        && b.name == name
                        && b.visible_from_byte <= reference.range.start_byte
                })
                .collect();
            if !bindings.is_empty() {
                let [binding] = bindings.as_slice() else {
                    return Err(LookupFailure::Unspecified);
                };
                let value = facts
                    .values
                    .iter()
                    .find(|v| v.binding_id == Some(binding.id))
                    .ok_or(LookupFailure::Unspecified)?;
                if Self::type_name_shadowed(value, binding.scope_id, ctx) {
                    return Err(LookupFailure::Unspecified);
                }
                let capture = crate::cpp_captures::local_effect(facts, name, value, reference)
                    .ok_or(LookupFailure::Unspecified)?;
                return Ok(Some(LocalValue { value, capture }));
            }
            scope = ctx.scope_parents.get(&id).copied();
        }
        Ok(None)
    }

    pub(super) fn is_member_set(&self, qname: &str, visible: &[&SymbolDef], file: FileId) -> bool {
        let imported = self.visible_files(file);
        visible.iter().any(|s| s.kind == SymbolKind::Method)
            || qname.rsplit_once("::").is_some_and(|(owner, _)| {
                self.records.get(owner).is_some_and(|records| {
                    records.iter().any(|(s, _)| imported.contains(&s.file_id))
                })
            })
    }

    fn check_identifier_macros(&self, name: &str, reference: &ReferenceUse) -> Lookup<()> {
        let declarations: Vec<_> = self
            .visible_files(reference.file_id)
            .into_iter()
            .flat_map(|file| {
                self.files.get(&file).into_iter().flat_map(move |facts| {
                    facts
                        .macros
                        .iter()
                        .filter(move |definition| {
                            definition.name == name
                                && definition.parameters.is_none()
                                && (file != reference.file_id
                                    || definition.range.start_byte < reference.range.start_byte)
                        })
                        .map(move |definition| (file, definition.range))
                })
            })
            .collect();
        if !declarations.is_empty() {
            return Err(LookupFailure::name_restricted(
                name,
                "",
                reference,
                declarations,
            ));
        }
        Ok(())
    }

    fn field_argument(
        &self,
        arg: &types::cpp::CppArgumentExpression,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
        deductions_remaining: &mut usize,
    ) -> Lookup<ArgumentType> {
        let types::cpp::CppArgumentExpression::Field {
            object,
            name,
            arrow,
        } = arg
        else {
            return Err(LookupFailure::Unspecified);
        };
        // An object-like macro can replace either written component. These
        // facts do not perform expansion; a function-like macro without an
        // invocation does not replace this identifier.
        let names = std::iter::once(name.as_str()).chain(match object.as_ref() {
            types::cpp::CppArgumentExpression::Name(name) => Some(name.as_str()),
            _ => None,
        });
        for name in names {
            self.check_identifier_macros(name, reference)?;
        }
        *deductions_remaining = deductions_remaining
            .checked_sub(1)
            .ok_or(LookupFailure::Unspecified)?;
        let object = self.argument(object, reference, ctx, deductions_remaining)?;
        // Builtin member access only. A class's operator-> and temporaries
        // require their own established expression types/value categories.
        if object.declared.pointer != *arrow || (!arrow && !object.lvalue) {
            return Err(LookupFailure::Unspecified);
        }
        let identity = self.identity_with_budget(
            &object.declared,
            &object.scope,
            &object.site,
            deductions_remaining,
        )?;
        let (record, record_type) = identity.named.record()?;
        if record_type.template_parameters.is_some() || !identity.arguments.is_empty() {
            return Err(LookupFailure::Unspecified);
        }
        let fields: Vec<_> = self
            .fields
            .values()
            .flatten()
            .filter(|(symbol, _)| symbol.name == *name)
            .map(|(symbol, _)| symbol.clone())
            .collect();
        let site = ReferenceUse {
            name: name.clone(),
            ..reference.clone()
        };
        let found = self.member_lookup(record, record_type, &site, &fields, &mut HashSet::new())?;
        let [field] = found.as_slice() else {
            return Err(LookupFailure::Unspecified);
        };
        let value = self
            .fields
            .get(&field.qualified_name)
            .and_then(|values| values.iter().find(|(symbol, _)| symbol.id == field.id))
            .map(|(_, value)| value)
            .ok_or(LookupFailure::Unspecified)?;
        // Bit-field reference binding has additional restrictions. Do not
        // treat it as an ordinary lvalue while ranking overloads.
        if value.bit_field {
            return Err(LookupFailure::Unspecified);
        }
        let mut declared = value
            .declared_type
            .clone()
            .ok_or(LookupFailure::Unspecified)?;
        let mut capture_const = false;
        // Reference members and static members retain their own cv; an
        // ordinary member inherits object cv, with mutable suppressing const.
        if !field.static_ && !declared.reference {
            if declared.pointer {
                if object.declared.volatile {
                    return Err(LookupFailure::Unspecified);
                }
                capture_const = object.declared.const_ && !value.mutable_;
            } else {
                declared.const_ |= object.declared.const_ && !value.mutable_;
                declared.volatile |= object.declared.volatile;
            }
        }
        Ok(ArgumentType {
            declared,
            scope: value.lookup_scope.clone(),
            site: ReferenceUse {
                file_id: field.file_id,
                range: value.declaration_range,
                ..reference.clone()
            },
            lvalue: true,
            capture_const,
        })
    }

    fn argument(
        &self,
        arg: &types::cpp::CppArgumentExpression,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
        deductions_remaining: &mut usize,
    ) -> Lookup<ArgumentType> {
        use types::cpp::CppArgumentExpression as Expr;
        if matches!(arg, Expr::Field { .. }) {
            return self.field_argument(arg, reference, ctx, deductions_remaining);
        }
        if let Expr::Binary {
            operator,
            left,
            right,
        } = arg
        {
            let mut left = self.argument(left, reference, ctx, deductions_remaining)?;
            let right = self.argument(right, reference, ctx, deductions_remaining)?;
            if left.declared.pointer || right.declared.pointer {
                return Err(LookupFailure::Unspecified);
            }
            let ArgumentIdentity {
                named: NamedType::Fundamental(lhs),
                ..
            } = self.identity(&left.declared, &left.scope, &left.site)?
            else {
                return Err(LookupFailure::Unspecified);
            };
            let ArgumentIdentity {
                named: NamedType::Fundamental(rhs),
                ..
            } = self.identity(&right.declared, &right.scope, &right.site)?
            else {
                return Err(LookupFailure::Unspecified);
            };
            let integral = matches!(
                lhs.as_str(),
                "int"
                    | "unsigned int"
                    | "long"
                    | "unsigned long"
                    | "long long"
                    | "unsigned long long"
            );
            let result = if lhs == rhs
                && ((integral
                    && matches!(
                        operator.as_str(),
                        "+" | "-" | "*" | "/" | "%" | "<<" | ">>" | "&" | "|" | "^"
                    ))
                    || (matches!(lhs.as_str(), "float" | "double" | "long double")
                        && matches!(operator.as_str(), "+" | "-" | "*" | "/")))
            {
                lhs
            } else if lhs != "void"
                && rhs != "void"
                && matches!(
                    operator.as_str(),
                    "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||"
                )
            {
                "bool".into()
            } else {
                return Err(LookupFailure::Unspecified);
            };
            left.declared.name = result;
            left.declared.reference = false;
            left.declared.const_ = false;
            left.declared.volatile = false;
            left.lvalue = false;
            left.capture_const = false;
            return Ok(left);
        }
        let (name, is_literal) = match arg {
            Expr::Name(name) => (name.as_str(), false),
            Expr::Literal(value) => (value.as_str(), true),
            _ => return Err(LookupFailure::Unspecified),
        };
        let literal = if is_literal && matches!(name, "true" | "false") {
            Some("bool")
        }
        // Unsuffixed decimal constants in this range have type int on every
        // supported C++ target; larger/suffixed expressions need more facts.
        else if is_literal
            && !name.is_empty()
            && name.bytes().all(|c| c.is_ascii_digit())
            && (name == "0" || !name.starts_with('0'))
            && name.parse::<u32>().is_ok_and(|n| n <= 32767)
        {
            Some("int")
        } else if is_literal {
            character_literal_type(name)
        } else {
            None
        };
        if let Some(name) = literal {
            return Ok(ArgumentType {
                declared: CppDeclaredType {
                    name: name.into(),
                    template_arguments: vec![],
                    pointer: false,
                    reference: false,
                    const_: false,
                    volatile: false,
                },
                scope: String::new(),
                site: reference.clone(),
                lvalue: false,
                capture_const: false,
            });
        }
        if is_literal || !plain_identifier(name) {
            return Err(LookupFailure::Unspecified);
        }
        let value = self
            .local_value(name, reference, ctx)?
            .ok_or(LookupFailure::Unspecified)?;
        let ValueType {
            mut declared,
            scope,
            declaration,
        } = self.value_type(value.value, ctx, deductions_remaining)?;
        value.capture.apply(&mut declared);
        let (file_id, range) =
            declaration.unwrap_or((reference.file_id, value.value.declaration_range));
        Ok(ArgumentType {
            declared,
            scope,
            site: ReferenceUse {
                file_id,
                range,
                ..reference.clone()
            },
            // A named variable is an lvalue even when declared as T&&.
            lvalue: true,
            capture_const: value.capture.const_,
        })
    }

    fn identity<'a>(
        &'a self,
        ty: &CppDeclaredType,
        scope: &str,
        site: &ReferenceUse,
    ) -> Lookup<ArgumentIdentity<'a>> {
        self.identity_with_budget(ty, scope, site, &mut 64)
    }

    fn identity_with_budget<'a>(
        &'a self,
        ty: &CppDeclaredType,
        scope: &str,
        site: &ReferenceUse,
        remaining: &mut usize,
    ) -> Lookup<ArgumentIdentity<'a>> {
        let failure = |kind| {
            LookupFailure::Type(Box::new(types::cpp::CppTypeLookupFailure {
                kind,
                name: ty.name.clone(),
                scope: scope.into(),
                file_id: site.file_id,
                range: site.range,
                related_declarations: Vec::new(),
            }))
        };
        if *remaining == 0 {
            return Err(failure(
                types::cpp::CppTypeLookupFailureKind::AnalysisBudgetExceeded,
            ));
        }
        *remaining -= 1;
        let lookup = self.lookup_type_in(
            ty,
            scope,
            TypeLookup::at(site, &self.visible_files(site.file_id)),
            &mut TypeLookupPath::new(),
            false,
        );
        let named = if ty.template_arguments.is_empty() {
            lookup?.ok_or(LookupFailure::Unspecified)?
        } else {
            // The existing lookup verifies a primary template with explicit
            // type parameters, matching arity and no visible specialization.
            // Dependent bases require instantiation before ADL is complete.
            let Ok(Some(NamedType::Record(symbol, record))) = lookup else {
                return Err(failure(
                    types::cpp::CppTypeLookupFailureKind::TemplateUnsupported,
                ));
            };
            if record.template_parameters.is_none()
                || !record.bases.as_ref().is_some_and(Vec::is_empty)
            {
                return Err(failure(
                    types::cpp::CppTypeLookupFailureKind::TemplateUnsupported,
                ));
            }
            NamedType::Record(symbol, record)
        };
        let arguments = ty
            .template_arguments
            .iter()
            .map(|argument| self.identity_with_budget(argument, scope, site, remaining))
            .collect::<Lookup<_>>()?;
        Ok(ArgumentIdentity {
            named,
            arguments,
            modifiers: (ty.pointer, ty.reference, ty.const_, ty.volatile),
        })
    }

    fn adl_limited(&self, scope: &str, reference: &ReferenceUse, files: &HashSet<FileId>) -> bool {
        files
            .iter()
            .filter_map(|id| self.files.get(id))
            .any(|facts| {
                facts.adl_limits.iter().any(|limit| {
                    limit.scope == scope
                        && limit
                            .name
                            .as_deref()
                            .is_none_or(|name| name == reference.name)
                })
            })
    }

    fn associated_record(
        &self,
        record: &SymbolDef,
        ty: &CppRecordType,
        reference: &ReferenceUse,
        files: &HashSet<FileId>,
        names: &mut HashSet<String>,
        visited: &mut HashSet<SymbolId>,
    ) -> Lookup<()> {
        if visited.len() >= 64 {
            return Err(LookupFailure::Unspecified);
        }
        if !visited.insert(record.id) {
            return Ok(());
        }
        if self.limited_record(record, ty)
            || (ty.template_parameters.is_some() && !ty.bases.as_ref().is_some_and(Vec::is_empty))
            || self.adl_limited(&record.qualified_name, reference, files)
        {
            return Err(LookupFailure::Unspecified);
        }
        let parent = record
            .qualified_name
            .rsplit_once("::")
            .map_or("", |(p, _)| p);
        if let Some(records) = self.records.get(parent) {
            let enclosing: Vec<_> = records
                .iter()
                .filter(|(s, t)| t.is_definition && files.contains(&s.file_id))
                .collect();
            let [(s, t)] = enclosing.as_slice() else {
                return Err(LookupFailure::Unspecified);
            };
            self.associated_record(s, t, reference, files, names, visited)?;
        } else {
            if self.adl_limited(parent, reference, files)
                || (!parent.is_empty()
                    && !self.names.get(parent).is_some_and(|entries| {
                        entries
                            .iter()
                            .any(|(f, _, k)| *k == SymbolKind::Namespace && files.contains(f))
                    }))
            {
                return Err(LookupFailure::Unspecified);
            }
            names.insert(parent.into());
        }
        for base in ty.bases.as_ref().ok_or(LookupFailure::Unspecified)? {
            let (s, t) = self.base_record(record, base, reference)?;
            self.associated_record(s, t, reference, files, names, visited)?;
        }
        Ok(())
    }

    fn associated_argument(
        &self,
        identity: &ArgumentIdentity<'_>,
        reference: &ReferenceUse,
        files: &HashSet<FileId>,
        names: &mut HashSet<String>,
        visited: &mut HashSet<SymbolId>,
    ) -> Lookup<()> {
        if let NamedType::Record(record, ty) = &identity.named {
            self.associated_record(record, ty, reference, files, names, visited)?;
        }
        // Traverse arguments even if this primary template was already seen:
        // Box<First> and Box<Second> have different associated entities.
        for argument in &identity.arguments {
            self.associated_argument(argument, reference, files, names, visited)?;
        }
        Ok(())
    }

    fn integral_expression(
        &self,
        expression: &types::cpp::CppArgumentExpression,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
        remaining: &mut usize,
    ) -> Lookup<bool> {
        *remaining = remaining.checked_sub(1).ok_or(LookupFailure::Unspecified)?;
        if let types::cpp::CppArgumentExpression::Binary {
            operator,
            left,
            right,
        } = expression
        {
            // Builtin integer arithmetic is closed over integer values. The
            // exact promoted/common type is unnecessary for this property.
            return Ok(matches!(
                operator.as_str(),
                "+" | "-" | "*" | "/" | "%" | "<<" | ">>" | "&" | "|" | "^"
            ) && self.integral_expression(left, reference, ctx, remaining)?
                && self.integral_expression(right, reference, ctx, remaining)?);
        }
        if let types::cpp::CppArgumentExpression::Name(name) = expression {
            self.check_identifier_macros(name, reference)?;
        }
        let argument = self.argument(expression, reference, ctx, remaining)?;
        Ok(!argument.declared.pointer
            && integral_type(&self.identity_with_budget(
                &argument.declared,
                &argument.scope,
                &argument.site,
                remaining,
            )?))
    }

    fn argument_evidence(
        &self,
        expression: &types::cpp::CppArgumentExpression,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
        remaining: &mut usize,
    ) -> Lookup<ArgumentEvidence> {
        match self.argument(expression, reference, ctx, remaining) {
            Ok(argument) => Ok(ArgumentEvidence::Exact(Box::new(argument))),
            Err(failure) => {
                if matches!(
                    self.integral_expression(expression, reference, ctx, remaining),
                    Ok(true)
                ) {
                    Ok(ArgumentEvidence::IntegralValue)
                } else {
                    // Preserve the original missing-type/lookup diagnostic;
                    // failure to establish a weaker fact is not incompatibility.
                    Err(failure)
                }
            }
        }
    }

    pub(super) fn free_call(
        &self,
        reference: &ReferenceUse,
        ctx: &ResolutionContext,
        ordinary: &[&SymbolDef],
        candidates: &[SymbolDef],
        deductions_remaining: &mut usize,
    ) -> Lookup<ResolvedTarget> {
        // A non-function declaration terminates ordinary lookup and suppresses
        // ADL. Local function declarations/bindings are handled before this path.
        if ordinary.iter().any(|s| s.kind != SymbolKind::Function) {
            return Err(LookupFailure::Unspecified);
        }
        let arity = reference.arity.ok_or(LookupFailure::Unspecified)? as usize;
        let written_args = self.written_arguments(reference);
        let args: Vec<_> = match (arity, written_args) {
            (0, None) => vec![],
            (_, Some(args)) if args.len() == arity => args
                .iter()
                .map(|arg| self.argument_evidence(arg, reference, ctx, deductions_remaining))
                .collect::<Lookup<_>>()?,
            _ => return Err(LookupFailure::Unspecified),
        };
        let files = self.visible_files(reference.file_id);
        let identities: Vec<_> = args
            .iter()
            .map(|argument| match argument {
                ArgumentEvidence::Exact(a) => {
                    self.identity(&a.declared, &a.scope, &a.site).map(Some)
                }
                // Fundamental integer types have no associated entities.
                ArgumentEvidence::IntegralValue => Ok(None),
            })
            .collect::<Lookup<_>>()?;
        let mut namespaces = HashSet::new();
        let mut visited = HashSet::new();
        for id in identities.iter().flatten() {
            self.associated_argument(id, reference, &files, &mut namespaces, &mut visited)?;
        }
        let mut visible = ordinary.to_vec();
        for namespace in namespaces {
            let qname = if namespace.is_empty() {
                reference.name.clone()
            } else {
                format!("{namespace}::{}", reference.name)
            };
            if self.imported_name_limited(&qname, reference.file_id, &files) {
                return Err(LookupFailure::Unspecified);
            }
            for symbol in candidates.iter().filter(|s| {
                s.language == Language::Cpp
                    && s.qualified_name == qname
                    && s.kind == SymbolKind::Function
                    && files.contains(&s.file_id)
                    && (s.file_id != reference.file_id
                        || s.name_range.start_byte <= reference.range.start_byte)
            }) {
                if !visible.iter().any(|s| s.id == symbol.id) {
                    visible.push(symbol);
                }
            }
        }
        if self.template_call(reference).is_some() {
            if identities.iter().any(Option::is_none) {
                return Err(LookupFailure::Unspecified);
            }
            return super::templates::choose(reference, &visible, candidates, arity, self);
        }
        let mut exact: BTreeMap<(String, Vec<String>, String), Vec<&SymbolDef>> = BTreeMap::new();
        let mut alternatives: BTreeMap<(String, Vec<String>, String), Vec<&SymbolDef>> =
            BTreeMap::new();
        let mut all_applicable = true;
        for symbol in visible {
            let callable = self.callable(symbol).ok_or(LookupFailure::Unspecified)?;
            if arity < callable.minimum_arity as usize || arity > callable.parameter_types.len() {
                continue;
            }
            if callable.parameter_declared_types.len() != callable.parameter_types.len() {
                return Err(LookupFailure::Unspecified);
            }
            let scope = symbol
                .qualified_name
                .rsplit_once("::")
                .map_or("", |(p, _)| p);
            let site = ReferenceUse {
                file_id: symbol.file_id,
                range: symbol.name_range,
                ..reference.clone()
            };
            let mut all_exact = true;
            for (i, (evidence, identity)) in args.iter().zip(&identities).enumerate() {
                let parameter = callable.parameter_declared_types[i]
                    .as_ref()
                    .ok_or(LookupFailure::Unspecified)?;
                let parameter_id = self.identity(parameter, scope, &site)?;
                let integral_parameter =
                    !parameter.pointer && !parameter.reference && integral_type(&parameter_id);
                let ArgumentEvidence::Exact(arg) = evidence else {
                    all_exact = false;
                    all_applicable &= integral_parameter;
                    continue;
                };
                let identity = identity.as_ref().ok_or(LookupFailure::Unspecified)?;
                if arg.capture_const && arg.declared.pointer && parameter.reference {
                    // Pointer-variable cv is separate from pointee cv and is not
                    // represented by the current parameter type subset.
                    return Err(LookupFailure::Unspecified);
                }
                // Unknown conversions never beat an identity conversion, but
                // unknown TYPE identity might tie it and therefore blocks choice.
                let cv_allowed = (!arg.declared.const_
                    || parameter.const_
                    || (!parameter.pointer && !parameter.reference))
                    && (!arg.declared.volatile
                        || parameter.volatile
                        || (!parameter.pointer && !parameter.reference));
                let reference_allowed = !parameter.reference
                    || if callable.parameter_types[i].ends_with("&&") {
                        !arg.lvalue
                    } else {
                        arg.lvalue || (parameter.const_ && !parameter.volatile)
                    };
                let exact_argument = identity.same_identity(&parameter_id)
                    && arg.declared.pointer == parameter.pointer
                    && cv_allowed
                    && reference_allowed;
                all_exact &= exact_argument;
                all_applicable &=
                    integral_parameter && !arg.declared.pointer && integral_type(identity);
            }
            let key = (
                symbol.qualified_name.clone(),
                callable.parameter_types.clone(),
                callable.qualifiers.clone(),
            );
            alternatives.entry(key.clone()).or_default().push(symbol);
            if all_exact {
                exact.entry(key).or_default().push(symbol);
            }
        }
        // Standard integral conversions establish applicability for a sole
        // declaration identity, never a preference over another overload.
        // Keep every arity-compatible alternative, including non-integral
        // parameters; absence of an exact match does not eliminate them.
        if exact.is_empty() && alternatives.len() == 1 && all_applicable {
            // Check every input of this integral-parameter candidate, including
            // inputs whose recorded type already matches. Macro replacement of
            // another argument can invalidate the whole call. Defer this work
            // until after exact matching so existing overload selection keeps
            // its original deduction budget and no conversions are ranked.
            for expression in written_args.ok_or(LookupFailure::Unspecified)? {
                if !self.integral_expression(expression, reference, ctx, deductions_remaining)? {
                    return Err(LookupFailure::Unspecified);
                }
            }
            exact = alternatives;
        }
        // Multiple identity-conversion candidates require ranking/tie rules not
        // supplied here; do not break ties by file, order, or namespace proximity.
        if exact.len() != 1 {
            return Err(LookupFailure::Unspecified);
        }
        let ((qname, _, _), declarations) =
            exact.into_iter().next().ok_or(LookupFailure::Unspecified)?;
        choose_target(reference, &qname, &declarations, candidates, arity, self)
    }
}
