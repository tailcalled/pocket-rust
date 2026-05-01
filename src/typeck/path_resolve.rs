use super::{
    EnumEntry, EnumTable, LifetimeRepr, ReExportTable, RType, StructTable, UseEntry,
    enum_lookup, int_kind_from_name, is_visible_from, resolve_via_reexports,
    resolve_via_use_scopes, struct_lookup_resolved, type_defining_module,
};
use crate::ast::{PathSegment, Type, TypeKind};
use crate::span::{Error, Span};

// Resolve a path expression's segments to an absolute lookup path. Handles
// `Self::…` substitution: replaces a leading `Self` segment with the impl
// target's struct name. Used by both typeck and codegen for call and struct
// literal lookups.
pub fn resolve_full_path(
    current_module: &Vec<String>,
    self_target: Option<&RType>,
    segments: &Vec<PathSegment>,
) -> Vec<String> {
    let mut full = current_module.clone();
    let mut start = 0;
    if !segments.is_empty() && segments[0].name == "Self" {
        if let Some(RType::Struct { path: target_path, .. }) = self_target {
            if let Some(last) = target_path.last() {
                full.push(last.clone());
                start = 1;
            }
        }
    }
    let mut i = start;
    while i < segments.len() {
        full.push(segments[i].name.clone());
        i += 1;
    }
    full
}

pub fn segments_to_string(segs: &Vec<PathSegment>) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < segs.len() {
        if i > 0 {
            s.push_str("::");
        }
        s.push_str(&segs[i].name);
        i += 1;
    }
    s
}

pub fn place_to_string(p: &Vec<String>) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < p.len() {
        if i > 0 {
            s.push('.');
        }
        s.push_str(&p[i]);
        i += 1;
    }
    s
}

pub fn resolve_type(
    ty: &Type,
    current_module: &Vec<String>,
    structs: &StructTable,
    enums: &EnumTable,
    self_target: Option<&RType>,
    type_params: &Vec<String>,
    use_scope: &Vec<UseEntry>,
    reexports: &ReExportTable,
    file: &str,
) -> Result<RType, Error> {
    match &ty.kind {
        TypeKind::Path(path) => {
            if path.segments.len() == 1 {
                if path.segments[0].name == "bool" {
                    return Ok(RType::Bool);
                }
                if let Some(k) = int_kind_from_name(&path.segments[0].name) {
                    return Ok(RType::Int(k));
                }
                // Check if it's an in-scope type parameter.
                let name = &path.segments[0].name;
                let mut i = 0;
                while i < type_params.len() {
                    if type_params[i] == *name {
                        return Ok(RType::Param(name.clone()));
                    }
                    i += 1;
                }
            }
            // Try use-table resolution: probe for both struct and enum
            // entries (a use-imported name could be either).
            let raw_segs: Vec<String> =
                path.segments.iter().map(|s| s.name.clone()).collect();
            let mut full = if let Some(p) = resolve_via_use_scopes(
                &raw_segs,
                use_scope,
                |cand| {
                    struct_lookup_resolved(structs, reexports, cand).is_some()
                        || enum_lookup_resolved(enums, reexports, cand).is_some()
                },
            ) {
                p
            } else {
                let mut full = current_module.clone();
                let mut i = 0;
                while i < path.segments.len() {
                    full.push(path.segments[i].name.clone());
                    i += 1;
                }
                full
            };
            let last = &path.segments[path.segments.len() - 1];
            // Try enum first (so a name shared with a struct in different
            // modules picks the right one through use-scope resolution).
            // In practice struct/enum names live in disjoint namespaces
            // per module, so this is just a "look both places, take what
            // matches."
            if let Some(e_entry) = enum_lookup_resolved(enums, reexports, &full) {
                full = e_entry.path.clone();
                if !is_visible_from(&type_defining_module(&e_entry.path), e_entry.is_pub, current_module) {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!("enum `{}` is private", place_to_string(&e_entry.path)),
                        span: path.span.copy(),
                    });
                }
                if e_entry.type_params.len() != last.args.len() {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "wrong number of type arguments for `{}`: expected {}, got {}",
                            place_to_string(&full),
                            e_entry.type_params.len(),
                            last.args.len()
                        ),
                        span: path.span.copy(),
                    });
                }
                let lifetime_args = resolve_lifetime_args(
                    &last.lifetime_args,
                    &e_entry.lifetime_params,
                    &full,
                    file,
                    &path.span,
                )?;
                let mut type_args: Vec<RType> = Vec::new();
                let mut i = 0;
                while i < last.args.len() {
                    type_args.push(resolve_type(
                        &last.args[i],
                        current_module,
                        structs,
                        enums,
                        self_target,
                        type_params,
                        use_scope,
                        reexports,
                        file,
                    )?);
                    i += 1;
                }
                return Ok(RType::Enum {
                    path: full,
                    type_args,
                    lifetime_args,
                });
            }
            let entry = match struct_lookup_resolved(structs, reexports, &full) {
                Some(e) => e,
                None => {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!("unknown type: {}", segments_to_string(&path.segments)),
                        span: path.span.copy(),
                    });
                }
            };
            // Use the canonical path returned by the resolver — that's
            // what downstream type representation expects (e.g.
            // `RType::Struct.path` should be the trait's actual
            // location, not the re-export alias).
            full = entry.path.clone();
            if !is_visible_from(&type_defining_module(&entry.path), entry.is_pub, current_module) {
                return Err(Error {
                    file: file.to_string(),
                    message: format!("struct `{}` is private", place_to_string(&entry.path)),
                    span: path.span.copy(),
                });
            }
            if entry.type_params.len() != last.args.len() {
                return Err(Error {
                    file: file.to_string(),
                    message: format!(
                        "wrong number of type arguments for `{}`: expected {}, got {}",
                        place_to_string(&full),
                        entry.type_params.len(),
                        last.args.len()
                    ),
                    span: path.span.copy(),
                });
            }
            let lifetime_args = resolve_lifetime_args(
                &last.lifetime_args,
                &entry.lifetime_params,
                &full,
                file,
                &path.span,
            )?;
            let mut type_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < last.args.len() {
                let t = resolve_type(
                    &last.args[i],
                    current_module,
                    structs,
                    enums,
                    self_target,
                    type_params,
                    use_scope,
                    reexports,
                    file,
                )?;
                type_args.push(t);
                i += 1;
            }
            Ok(RType::Struct {
                path: full,
                type_args,
                lifetime_args,
            })
        }
        TypeKind::Ref { inner, mutable, lifetime } => {
            let r = resolve_type(
                inner,
                current_module,
                structs,
                enums,
                self_target,
                type_params,
                use_scope,
                reexports,
                file,
            )?;
            // Phase B: structurally carry the lifetime — `'a` becomes
            // `Named("a")`; elided refs and the `'_` anonymous lifetime
            // both use the `Inferred(0)` placeholder, freshened later.
            let lt = match lifetime {
                Some(lt) if lt.name == "_" => LifetimeRepr::Inferred(0),
                Some(lt) => LifetimeRepr::Named(lt.name.clone()),
                None => LifetimeRepr::Inferred(0),
            };
            Ok(RType::Ref {
                inner: Box::new(r),
                mutable: *mutable,
                lifetime: lt,
            })
        }
        TypeKind::RawPtr { inner, mutable } => {
            let r = resolve_type(
                inner,
                current_module,
                structs,
                enums,
                self_target,
                type_params,
                use_scope,
                reexports,
                file,
            )?;
            Ok(RType::RawPtr {
                inner: Box::new(r),
                mutable: *mutable,
            })
        }
        TypeKind::SelfType => match self_target {
            Some(rt) => Ok(rt.clone()),
            None => Err(Error {
                file: file.to_string(),
                message: "`Self` is only valid inside an `impl` block".to_string(),
                span: ty.span.copy(),
            }),
        },
        TypeKind::Tuple(elems) => {
            let mut out: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                out.push(resolve_type(
                    &elems[i],
                    current_module,
                    structs,
                    enums,
                    self_target,
                    type_params,
                    use_scope,
                    reexports,
                    file,
                )?);
                i += 1;
            }
            Ok(RType::Tuple(out))
        }
    }
}

// Validate and resolve the lifetime args at a struct/enum type-position
// path against the type's declared `lifetime_params`. Either fully
// elided (yields `Inferred(0)` placeholders, one per param) or a
// 1:1 explicit list (each `'a`-style name → `Named`, `'_` → fresh
// `Inferred(0)`). Used by both the struct and enum branches of
// `resolve_type` to share the validation.
pub(super) fn resolve_lifetime_args(
    args: &Vec<crate::ast::Lifetime>,
    params: &Vec<String>,
    full: &Vec<String>,
    file: &str,
    span: &Span,
) -> Result<Vec<LifetimeRepr>, Error> {
    if args.is_empty() {
        let mut out: Vec<LifetimeRepr> = Vec::new();
        let mut i = 0;
        while i < params.len() {
            out.push(LifetimeRepr::Inferred(0));
            i += 1;
        }
        return Ok(out);
    }
    if args.len() != params.len() {
        return Err(Error {
            file: file.to_string(),
            message: format!(
                "wrong number of lifetime arguments for `{}`: expected {}, got {}",
                place_to_string(full),
                params.len(),
                args.len()
            ),
            span: span.copy(),
        });
    }
    let mut out: Vec<LifetimeRepr> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i].name == "_" {
            out.push(LifetimeRepr::Inferred(0));
        } else {
            out.push(LifetimeRepr::Named(args[i].name.clone()));
        }
        i += 1;
    }
    Ok(out)
}

// Re-export-aware enum lookup. Mirrors `struct_lookup_resolved`.
pub fn enum_lookup_resolved<'a>(
    enums: &'a EnumTable,
    reexports: &ReExportTable,
    path: &Vec<String>,
) -> Option<&'a EnumEntry> {
    if let Some(e) = enum_lookup(enums, path) {
        return Some(e);
    }
    let target = resolve_via_reexports(path, reexports, |cand| {
        enum_lookup(enums, cand).is_some()
    })?;
    enum_lookup(enums, &target)
}

// A path matches an enum variant if its prefix names an enum and the
// last segment matches one of that enum's variants. Returns the
// canonical enum path + variant index. The probe is use-scope and
// re-export aware — `Option::Some` resolves through `use std::option::Option`,
// `std::*` glob, `pub use`, etc.
pub fn lookup_variant_path(
    enums: &EnumTable,
    reexports: &ReExportTable,
    use_scope: &Vec<UseEntry>,
    current_module: &Vec<String>,
    raw_segs: &Vec<String>,
) -> Option<(Vec<String>, usize)> {
    if raw_segs.len() < 2 {
        return None;
    }
    let prefix_len = raw_segs.len() - 1;
    let variant_name = raw_segs[prefix_len].clone();
    let prefix_segs: Vec<String> = raw_segs[..prefix_len].to_vec();
    // Try use-scope resolution first; the prefix must name an enum.
    let enum_path: Vec<String> =
        resolve_via_use_scopes(&prefix_segs, use_scope, |cand| {
            enum_lookup_resolved(enums, reexports, cand).is_some()
        })
        .unwrap_or_else(|| {
            let mut full = current_module.clone();
            let mut i = 0;
            while i < prefix_segs.len() {
                full.push(prefix_segs[i].clone());
                i += 1;
            }
            full
        });
    let entry = enum_lookup_resolved(enums, reexports, &enum_path)?;
    let mut i = 0;
    while i < entry.variants.len() {
        if entry.variants[i].name == variant_name {
            return Some((entry.path.clone(), i));
        }
        i += 1;
    }
    None
}
