use super::{LifetimeRepr, RType};
use crate::span::{Error, Span};

pub fn find_lifetime_source(
    param_lifetimes: &Vec<Option<LifetimeRepr>>,
    target: &LifetimeRepr,
) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < param_lifetimes.len() {
        if let Some(plt) = &param_lifetimes[i] {
            if plt == target {
                out.push(i);
            }
        }
        i += 1;
    }
    out
}

// Walks an RType, replacing every `LifetimeRepr::Inferred(0)` placeholder
// with a fresh `Inferred(N)` allocated from `next_id`. Used per function
// during signature resolution to give each elided ref a unique id. Also
// freshens struct `lifetime_args` so e.g. `Wrapper<'_, T>` elided slots
// get distinct ids.
pub fn freshen_inferred_lifetimes(rt: &mut RType, next_id: &mut u32) {
    match rt {
        RType::Ref { inner, lifetime, .. } => {
            if let LifetimeRepr::Inferred(id) = lifetime {
                if *id == 0 {
                    *id = *next_id;
                    *next_id += 1;
                }
            }
            freshen_inferred_lifetimes(inner, next_id);
        }
        RType::RawPtr { inner, .. } => freshen_inferred_lifetimes(inner, next_id),
        RType::Struct { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                if let LifetimeRepr::Inferred(id) = &mut lifetime_args[i] {
                    if *id == 0 {
                        *id = *next_id;
                        *next_id += 1;
                    }
                }
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                freshen_inferred_lifetimes(&mut type_args[i], next_id);
                i += 1;
            }
        }
        RType::Int(_) | RType::Param(_) | RType::Bool => {}
        RType::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                freshen_inferred_lifetimes(&mut elems[i], next_id);
                i += 1;
            }
        }
        RType::Enum { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                if let LifetimeRepr::Inferred(id) = &mut lifetime_args[i] {
                    if *id == 0 {
                        *id = *next_id;
                        *next_id += 1;
                    }
                }
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                freshen_inferred_lifetimes(&mut type_args[i], next_id);
                i += 1;
            }
        }
        RType::Slice(inner) => freshen_inferred_lifetimes(inner, next_id),
        RType::Str => {}
        RType::AssocProj { base, .. } => freshen_inferred_lifetimes(base, next_id),
        RType::Never => {}
        RType::Char => {}
        // Opaque carries no lifetime args — the bounds + pin are
        // tracked on the FnSymbol, not on the RType node.
        RType::Opaque { .. } => {}
    }
}

// Rejects an `RType` carrying any `LifetimeRepr::Inferred(_)` lifetime.
// Used for struct field types — Rust requires explicit lifetime annotations
// on refs inside struct fields, so an elided lifetime there is an error.
pub fn require_no_inferred_lifetimes(
    rt: &RType,
    span: &Span,
    file: &str,
) -> Result<(), Error> {
    match rt {
        RType::Ref { inner, lifetime, .. } => {
            if matches!(lifetime, LifetimeRepr::Inferred(_)) {
                return Err(Error {
                    file: file.to_string(),
                    message: "missing lifetime specifier on reference in struct field"
                        .to_string(),
                    span: span.copy(),
                });
            }
            require_no_inferred_lifetimes(inner, span, file)
        }
        RType::RawPtr { inner, .. } => require_no_inferred_lifetimes(inner, span, file),
        RType::Struct { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                if matches!(lifetime_args[i], LifetimeRepr::Inferred(_)) {
                    return Err(Error {
                        file: file.to_string(),
                        message: "missing lifetime argument on struct in field type"
                            .to_string(),
                        span: span.copy(),
                    });
                }
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                require_no_inferred_lifetimes(&type_args[i], span, file)?;
                i += 1;
            }
            Ok(())
        }
        RType::Int(_) | RType::Param(_) | RType::Bool => Ok(()),
        RType::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                require_no_inferred_lifetimes(&elems[i], span, file)?;
                i += 1;
            }
            Ok(())
        }
        RType::Enum { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                if matches!(lifetime_args[i], LifetimeRepr::Inferred(_)) {
                    return Err(Error {
                        file: file.to_string(),
                        message: "missing lifetime argument on enum in field type"
                            .to_string(),
                        span: span.copy(),
                    });
                }
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                require_no_inferred_lifetimes(&type_args[i], span, file)?;
                i += 1;
            }
            Ok(())
        }
        RType::Slice(inner) => require_no_inferred_lifetimes(inner, span, file),
        RType::Str => Ok(()),
        RType::AssocProj { base, .. } => require_no_inferred_lifetimes(base, span, file),
        RType::Never => Ok(()),
        RType::Char => Ok(()),
        RType::Opaque { .. } => Ok(()),
    }
}

// Validates that every `LifetimeRepr::Named` inside an `RType` references a
// lifetime declared in `lifetime_params`. Used to reject signatures that
// reference an undeclared `'a`.
pub fn validate_named_lifetimes(
    rt: &RType,
    lifetime_params: &Vec<String>,
    span: &Span,
    file: &str,
) -> Result<(), Error> {
    match rt {
        RType::Ref { inner, lifetime, .. } => {
            check_named_in_scope(lifetime, lifetime_params, span, file)?;
            validate_named_lifetimes(inner, lifetime_params, span, file)
        }
        RType::RawPtr { inner, .. } => {
            validate_named_lifetimes(inner, lifetime_params, span, file)
        }
        RType::Struct { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                check_named_in_scope(&lifetime_args[i], lifetime_params, span, file)?;
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                validate_named_lifetimes(&type_args[i], lifetime_params, span, file)?;
                i += 1;
            }
            Ok(())
        }
        RType::Int(_) | RType::Param(_) | RType::Bool => Ok(()),
        RType::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                validate_named_lifetimes(&elems[i], lifetime_params, span, file)?;
                i += 1;
            }
            Ok(())
        }
        RType::Enum { type_args, lifetime_args, .. } => {
            let mut i = 0;
            while i < lifetime_args.len() {
                check_named_in_scope(&lifetime_args[i], lifetime_params, span, file)?;
                i += 1;
            }
            let mut i = 0;
            while i < type_args.len() {
                validate_named_lifetimes(&type_args[i], lifetime_params, span, file)?;
                i += 1;
            }
            Ok(())
        }
        RType::Slice(inner) => validate_named_lifetimes(inner, lifetime_params, span, file),
        RType::Str => Ok(()),
        RType::AssocProj { base, .. } => {
            validate_named_lifetimes(base, lifetime_params, span, file)
        }
        RType::Never => Ok(()),
        RType::Char => Ok(()),
        RType::Opaque { .. } => Ok(()),
    }
}

fn check_named_in_scope(
    lt: &LifetimeRepr,
    lifetime_params: &Vec<String>,
    span: &Span,
    file: &str,
) -> Result<(), Error> {
    if let LifetimeRepr::Named(name) = lt {
        // `'static` is a built-in lifetime; always in scope without
        // an enclosing `<'static, …>` declaration. (It carries
        // structurally just like any other Named lifetime — Phase D
        // doesn't enforce its "outlives everything" semantics.)
        if name == "static" {
            return Ok(());
        }
        let mut found = false;
        let mut i = 0;
        while i < lifetime_params.len() {
            if lifetime_params[i] == *name {
                found = true;
                break;
            }
            i += 1;
        }
        if !found {
            return Err(Error {
                file: file.to_string(),
                message: format!("undeclared lifetime `'{}`", name),
                span: span.copy(),
            });
        }
    }
    Ok(())
}

// Lifetime elision: returns the index of the param whose outermost ref
// lifetime should propagate to the return ref. Rule 3 (a `&self` receiver
// wins as the source) takes precedence over rule 2 (otherwise: exactly one
// ref param → its lifetime). `&mut T -> &U` is allowed (downgrade); `&T -> &mut U`
// is rejected. Returns the source param index; the caller copies that
// param's outermost lifetime into the return ref.
pub fn find_elision_source(
    param_types: &Vec<RType>,
    self_idx: Option<usize>,
    ret_mutable: bool,
    ret_span: &Span,
    file: &str,
) -> Result<usize, Error> {
    // Rule 3: a self-receiver that's a ref shorts the search.
    if let Some(idx) = self_idx {
        if let RType::Ref {
            mutable: src_mutable,
            ..
        } = &param_types[idx]
        {
            if ret_mutable && !*src_mutable {
                return Err(Error {
                    file: file.to_string(),
                    message: "cannot return `&mut` from a `&self` receiver".to_string(),
                    span: ret_span.copy(),
                });
            }
            return Ok(idx);
        }
        // Self is owned (consuming) — fall through to rule 2.
    }
    let mut source: Option<usize> = None;
    let mut count: usize = 0;
    let mut i = 0;
    while i < param_types.len() {
        if let RType::Ref { .. } = &param_types[i] {
            count += 1;
            source = Some(i);
        }
        i += 1;
    }
    if count != 1 {
        return Err(Error {
            file: file.to_string(),
            message: format!(
                "function returning a reference must have exactly one reference parameter (found {})",
                count
            ),
            span: ret_span.copy(),
        });
    }
    let src_idx = source.expect("count == 1");
    let src_mutable = match &param_types[src_idx] {
        RType::Ref { mutable, .. } => *mutable,
        _ => unreachable!(),
    };
    if ret_mutable && !src_mutable {
        return Err(Error {
            file: file.to_string(),
            message: "cannot return `&mut` from a `&` parameter".to_string(),
            span: ret_span.copy(),
        });
    }
    Ok(src_idx)
}
