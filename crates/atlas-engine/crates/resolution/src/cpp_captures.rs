//! Shared capture prerequisites for call resolution and declaration investigation.
//! Uses stored facts; does not parse source, write edges or infer execution.
use types::cpp::{CppDeclaredType, CppFileTypes, CppValueType};
use types::{ReferenceUse, SymbolId};

#[derive(Default, Clone, Copy)]
pub struct CaptureEffect {
    pub copy: bool,
    pub const_: bool,
}

impl CaptureEffect {
    pub fn apply(self, ty: &mut CppDeclaredType) {
        if self.copy {
            ty.reference = false;
        }
        // CppDeclaredType::const_ qualifies the pointee for pointer types.
        // A closure's const operator does not make the pointed-to object const.
        if self.const_ && !ty.pointer {
            ty.const_ = true;
        }
    }
}

/// Capture effects on an existing lexical declaration. None preserves unsupported
/// capture/shadowing semantics; it is not a claim that the C++ program is invalid.
pub fn local_effect(
    facts: &CppFileTypes,
    name: &str,
    value: &CppValueType,
    reference: &ReferenceUse,
) -> Option<CaptureEffect> {
    use types::cpp::CppCaptureKind as Kind;
    let mut effect = CaptureEffect::default();
    for lambda in &facts.lambda_captures {
        let point = reference.range.start_byte;
        if point < lambda.body_range.start_byte
            || point >= lambda.body_range.end_byte
            || (value.declaration_range.start_byte >= lambda.range.start_byte
                && value.declaration_range.end_byte <= lambda.range.end_byte)
        {
            continue;
        }
        let captures = lambda.captures.as_ref()?;
        let explicit = captures
            .iter()
            .find(|(captured, _)| captured == name)
            .map(|(_, kind)| *kind);
        // An init-capture shadows outer names regardless of their storage.
        if explicit == Some(Kind::Unknown) {
            return None;
        }
        if !value.capture_required {
            if explicit.is_some() {
                return None;
            }
            continue;
        }
        if effect.copy {
            // A further capture now denotes a closure data member rather
            // than the original binding. That member's identity/type is
            // not represented here; do not flatten nested copy captures.
            return None;
        }
        match explicit.or(lambda.default) {
            Some(Kind::Copy) => {
                effect.copy = true;
                effect.const_ |= !lambda.mutable_;
            }
            Some(Kind::Reference) => {}
            _ => return None,
        }
    }
    Some(effect)
}

/// Validate the recorded enclosing-object prerequisites, using only this file's
/// capture/callable facts and the enclosing symbol's static flag. Shared with
/// read-only declaration context; this does not choose a field or call target.
pub fn field_access(
    file: &CppFileTypes,
    reference: &ReferenceUse,
    unqualified_name: Option<&str>,
    symbol_is_static: impl Fn(SymbolId) -> Option<bool>,
) -> Option<()> {
    if file.this_capture_unavailable.contains(&reference.id) {
        return None;
    }
    let mut owner = reference.source_symbol;
    let mut crossed_closure = false;
    for _ in 0..=file.lambda_captures.len() {
        let Some(lambda) = file
            .lambda_captures
            .iter()
            .find(|lambda| lambda.symbol_id.is_some() && lambda.symbol_id == owner)
        else {
            if crossed_closure {
                let owner = owner?;
                let callable = file.callables.iter().find(|c| c.symbol_id == owner)?;
                if symbol_is_static(owner)?
                    || callable.qualifiers.contains("const")
                    || callable.qualifiers.contains("volatile")
                {
                    return None;
                }
            }
            return Some(());
        };
        let captures = lambda.captures.as_ref()?;
        if unqualified_name
            .is_some_and(|name| captures.iter().any(|(captured, _)| captured == name))
        {
            return None;
        }
        crossed_closure = true;
        owner = lambda.enclosing_symbol;
    }
    None
}
