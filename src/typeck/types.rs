use super::{
    EnumTable, StructTable, TraitTable, VariantPayloadResolved, enum_lookup, place_to_string,
    solve_impl, solve_impl_in_ctx, struct_lookup,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IntKind {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    U128,
    I128,
    Usize,
    Isize,
}

pub fn int_kind_name(k: &IntKind) -> &'static str {
    match k {
        IntKind::U8 => "u8",
        IntKind::I8 => "i8",
        IntKind::U16 => "u16",
        IntKind::I16 => "i16",
        IntKind::U32 => "u32",
        IntKind::I32 => "i32",
        IntKind::U64 => "u64",
        IntKind::I64 => "i64",
        IntKind::U128 => "u128",
        IntKind::I128 => "i128",
        IntKind::Usize => "usize",
        IntKind::Isize => "isize",
    }
}

pub(super) fn int_kind_from_name(name: &str) -> Option<IntKind> {
    match name {
        "u8" => Some(IntKind::U8),
        "i8" => Some(IntKind::I8),
        "u16" => Some(IntKind::U16),
        "i16" => Some(IntKind::I16),
        "u32" => Some(IntKind::U32),
        "i32" => Some(IntKind::I32),
        "u64" => Some(IntKind::U64),
        "i64" => Some(IntKind::I64),
        "u128" => Some(IntKind::U128),
        "i128" => Some(IntKind::I128),
        "usize" => Some(IntKind::Usize),
        "isize" => Some(IntKind::Isize),
        _ => None,
    }
}

// Maximum value that fits in this integer kind. We don't have negative literals,
// so we only care about the non-negative range.
pub(super) fn int_kind_max(k: &IntKind) -> u128 {
    match k {
        IntKind::U8 => u8::MAX as u128,
        IntKind::I8 => i8::MAX as u128,
        IntKind::U16 => u16::MAX as u128,
        IntKind::I16 => i16::MAX as u128,
        IntKind::U32 => u32::MAX as u128,
        IntKind::I32 => i32::MAX as u128,
        IntKind::U64 => u64::MAX as u128,
        IntKind::I64 => i64::MAX as u128,
        IntKind::U128 => u128::MAX,
        IntKind::I128 => i128::MAX as u128,
        // wasm32: usize/isize are 32-bit.
        IntKind::Usize => u32::MAX as u128,
        IntKind::Isize => i32::MAX as u128,
    }
}

#[derive(Clone)]
pub enum RType {
    Int(IntKind),
    Struct {
        path: Vec<String>,
        type_args: Vec<RType>,
        // Lifetimes provided to the struct's `<'a, ...>` params, in order.
        // Empty for non-lifetime-generic structs. Carry-only for now —
        // borrowck reads them via `find_lifetime_source` to propagate
        // borrows when a returned ref's lifetime ties to one of these.
        lifetime_args: Vec<LifetimeRepr>,
    },
    Ref {
        inner: Box<RType>,
        mutable: bool,
        // Phase B: structural carry only. `Named(_)` records a user-written
        // `'a` annotation; `Inferred(_)` is a placeholder for elided refs.
        // Type equality and unification currently ignore this field — Phase C
        // is when lifetimes start participating in any check.
        lifetime: LifetimeRepr,
    },
    RawPtr { inner: Box<RType>, mutable: bool },
    // An opaque type parameter inside a generic body. Carries the param's
    // name. Codegen substitutes these to concrete types during monomorphization;
    // operations needing layout (byte_size_of, flatten_rtype) reject `Param`.
    Param(String),
    Bool,
    // Tuple type. Empty Vec is the unit type `()` — the type of
    // value-less expressions (function bodies without a tail, blocks
    // ending in `;`, ifs without an else).
    Tuple(Vec<RType>),
    // Enum type. Layout is tagged-union: i32 discriminant at offset 0
    // followed by max-payload-byte buffer. Enum *values* live at a
    // memory address (always shadow-stack-resident); the wasm-flat
    // representation is a single `i32` (the address). Function returns
    // of enum type use the sret convention — a leading i32 out-pointer
    // param, no wasm result. See codegen for layout helpers.
    Enum {
        path: Vec<String>,
        type_args: Vec<RType>,
        lifetime_args: Vec<LifetimeRepr>,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub enum LifetimeRepr {
    // A `'name` annotation written in source. Resolution is by-name only:
    // the named lifetime must be in scope at the type's appearance site
    // (Phase C will enforce that; Phase B accepts any name).
    Named(String),
    // A lifetime allocated for an elided / inferred reference. Phase B uses
    // 0 as a placeholder for everything; Phase C allocates fresh ids per
    // function so different elided refs are distinguishable.
    Inferred(u32),
}

// Outermost lifetime of a ref type. Returns None for non-ref types.
pub fn outer_lifetime(rt: &RType) -> Option<LifetimeRepr> {
    match rt {
        RType::Ref { lifetime, .. } => Some(lifetime.clone()),
        _ => None,
    }
}

pub(super) fn rtype_vec_eq(a: &Vec<RType>, b: &Vec<RType>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !rtype_eq(&a[i], &b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

pub fn rtype_eq(a: &RType, b: &RType) -> bool {
    match (a, b) {
        (RType::Bool, RType::Bool) => true,
        (RType::Int(ka), RType::Int(kb)) => ka == kb,
        (
            RType::Struct {
                path: pa,
                type_args: aa,
                ..
            },
            RType::Struct {
                path: pb,
                type_args: ab,
                ..
            },
        ) => pa == pb && rtype_vec_eq(aa, ab),
        (
            RType::Ref {
                inner: ia,
                mutable: ma,
                ..
            },
            RType::Ref {
                inner: ib,
                mutable: mb,
                ..
            },
        ) => ma == mb && rtype_eq(ia, ib),
        (
            RType::RawPtr {
                inner: ia,
                mutable: ma,
            },
            RType::RawPtr {
                inner: ib,
                mutable: mb,
            },
        ) => ma == mb && rtype_eq(ia, ib),
        (RType::Param(a), RType::Param(b)) => a == b,
        (RType::Tuple(a), RType::Tuple(b)) => rtype_vec_eq(a, b),
        (
            RType::Enum {
                path: pa,
                type_args: aa,
                ..
            },
            RType::Enum {
                path: pb,
                type_args: ab,
                ..
            },
        ) => pa == pb && rtype_vec_eq(aa, ab),
        _ => false,
    }
}

pub fn rtype_to_string(t: &RType) -> String {
    match t {
        RType::Bool => "bool".to_string(),
        RType::Int(k) => int_kind_name(k).to_string(),
        RType::Struct { path, type_args, .. } => {
            if type_args.is_empty() {
                place_to_string(path)
            } else {
                let mut s = place_to_string(path);
                s.push('<');
                let mut i = 0;
                while i < type_args.len() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&rtype_to_string(&type_args[i]));
                    i += 1;
                }
                s.push('>');
                s
            }
        }
        RType::Ref { inner, mutable, .. } => {
            if *mutable {
                format!("&mut {}", rtype_to_string(inner))
            } else {
                format!("&{}", rtype_to_string(inner))
            }
        }
        RType::RawPtr { inner, mutable } => {
            if *mutable {
                format!("*mut {}", rtype_to_string(inner))
            } else {
                format!("*const {}", rtype_to_string(inner))
            }
        }
        RType::Param(n) => n.clone(),
        RType::Tuple(elems) => {
            let mut s = String::from("(");
            let mut i = 0;
            while i < elems.len() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&rtype_to_string(&elems[i]));
                i += 1;
            }
            // Trailing comma for 1-tuples (matches Rust output).
            if elems.len() == 1 {
                s.push(',');
            }
            s.push(')');
            s
        }
        RType::Enum { path, type_args, .. } => {
            if type_args.is_empty() {
                place_to_string(path)
            } else {
                let mut s = place_to_string(path);
                s.push('<');
                let mut i = 0;
                while i < type_args.len() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&rtype_to_string(&type_args[i]));
                    i += 1;
                }
                s.push('>');
                s
            }
        }
    }
}

pub fn rtype_size(ty: &RType, structs: &StructTable) -> u32 {
    match ty {
        RType::Bool => 1,
        RType::Int(k) => match k {
            IntKind::U128 | IntKind::I128 => 2,
            _ => 1,
        },
        RType::Struct { path, type_args, .. } => {
            let entry = struct_lookup(structs, path).expect("resolved struct");
            let env = struct_env(&entry.type_params, type_args);
            let mut s: u32 = 0;
            let mut i = 0;
            while i < entry.fields.len() {
                let fty = substitute_rtype(&entry.fields[i].ty, &env);
                s += rtype_size(&fty, structs);
                i += 1;
            }
            s
        }
        RType::Ref { .. } | RType::RawPtr { .. } => 1,
        RType::Param(_) => unreachable!("rtype_size called on unresolved type parameter"),
        RType::Tuple(elems) => {
            let mut s: u32 = 0;
            let mut i = 0;
            while i < elems.len() {
                s += rtype_size(&elems[i], structs);
                i += 1;
            }
            s
        }
        // Enum values are represented on the wasm stack as a single
        // i32 address. The actual bytes (disc + payload) live at that
        // address. See `byte_size_of` for the on-stack-frame size and
        // `flatten_rtype` for the matching wasm shape.
        RType::Enum { .. } => 1,
    }
}

pub(crate) fn struct_env(
    type_params: &Vec<String>,
    type_args: &Vec<RType>,
) -> Vec<(String, RType)> {
    let mut env: Vec<(String, RType)> = Vec::new();
    let n = if type_params.len() < type_args.len() {
        type_params.len()
    } else {
        type_args.len()
    };
    let mut i = 0;
    while i < n {
        env.push((type_params[i].clone(), type_args[i].clone()));
        i += 1;
    }
    env
}

pub fn flatten_rtype(ty: &RType, structs: &StructTable, out: &mut Vec<crate::wasm::ValType>) {
    match ty {
        RType::Bool => out.push(crate::wasm::ValType::I32),
        RType::Int(k) => match k {
            IntKind::U64 | IntKind::I64 => out.push(crate::wasm::ValType::I64),
            IntKind::U128 | IntKind::I128 => {
                out.push(crate::wasm::ValType::I64);
                out.push(crate::wasm::ValType::I64);
            }
            _ => out.push(crate::wasm::ValType::I32),
        },
        RType::Struct { path, type_args, .. } => {
            let entry = struct_lookup(structs, path).expect("resolved struct");
            let env = struct_env(&entry.type_params, type_args);
            let mut i = 0;
            while i < entry.fields.len() {
                let fty = substitute_rtype(&entry.fields[i].ty, &env);
                flatten_rtype(&fty, structs, out);
                i += 1;
            }
        }
        RType::Ref { .. } | RType::RawPtr { .. } => out.push(crate::wasm::ValType::I32),
        RType::Param(_) => unreachable!("flatten_rtype called on unresolved type parameter"),
        RType::Tuple(elems) => {
            let mut i = 0;
            while i < elems.len() {
                flatten_rtype(&elems[i], structs, out);
                i += 1;
            }
        }
        // Enums flatten to a single i32 (the address of the on-shadow-
        // stack disc+payload bytes). Construction allocates the slot
        // and yields its address; reads chase the address.
        RType::Enum { .. } => out.push(crate::wasm::ValType::I32),
    }
}

pub fn byte_size_of(rt: &RType, structs: &StructTable, enums: &EnumTable) -> u32 {
    match rt {
        RType::Bool => 1,
        RType::Int(k) => match k {
            IntKind::U8 | IntKind::I8 => 1,
            IntKind::U16 | IntKind::I16 => 2,
            IntKind::U32 | IntKind::I32 | IntKind::Usize | IntKind::Isize => 4,
            IntKind::U64 | IntKind::I64 => 8,
            IntKind::U128 | IntKind::I128 => 16,
        },
        RType::Ref { .. } | RType::RawPtr { .. } => 4,
        RType::Struct { path, type_args, .. } => {
            let entry = struct_lookup(structs, path).expect("resolved struct");
            let env = struct_env(&entry.type_params, type_args);
            let mut total: u32 = 0;
            let mut i = 0;
            while i < entry.fields.len() {
                let fty = substitute_rtype(&entry.fields[i].ty, &env);
                total += byte_size_of(&fty, structs, enums);
                i += 1;
            }
            total
        }
        RType::Param(_) => unreachable!("byte_size_of called on unresolved type parameter"),
        RType::Tuple(elems) => {
            let mut total: u32 = 0;
            let mut i = 0;
            while i < elems.len() {
                total += byte_size_of(&elems[i], structs, enums);
                i += 1;
            }
            total
        }
        // Tagged-union: 4-byte i32 disc at offset 0, then a buffer of
        // max(payload byte size) for the largest variant. Each variant's
        // payload sits within the buffer in declaration order; smaller
        // variants leave the trailing bytes unused.
        RType::Enum { path, type_args, .. } => {
            let entry = enum_lookup(enums, path).expect("resolved enum");
            let env = struct_env(&entry.type_params, type_args);
            let mut max_payload: u32 = 0;
            let mut i = 0;
            while i < entry.variants.len() {
                let p = variant_payload_byte_size(&entry.variants[i].payload, &env, structs, enums);
                if p > max_payload {
                    max_payload = p;
                }
                i += 1;
            }
            4 + max_payload
        }
    }
}

// Sum-of-bytes for a variant's payload after substituting the enum's
// type-arg env into each field's declared type.
pub fn variant_payload_byte_size(
    payload: &VariantPayloadResolved,
    env: &Vec<(String, RType)>,
    structs: &StructTable,
    enums: &EnumTable,
) -> u32 {
    match payload {
        VariantPayloadResolved::Unit => 0,
        VariantPayloadResolved::Tuple(types) => {
            let mut total: u32 = 0;
            let mut i = 0;
            while i < types.len() {
                let ty = substitute_rtype(&types[i], env);
                total += byte_size_of(&ty, structs, enums);
                i += 1;
            }
            total
        }
        VariantPayloadResolved::Struct(fields) => {
            let mut total: u32 = 0;
            let mut i = 0;
            while i < fields.len() {
                let ty = substitute_rtype(&fields[i].ty, env);
                total += byte_size_of(&ty, structs, enums);
                i += 1;
            }
            total
        }
    }
}

// Substitutes type parameters with their concrete types. `env` maps each
// param name to a concrete RType. Called by codegen during monomorphization.
// If a Param doesn't appear in env, returns it unchanged (for nested-generic
// scenarios where the env is partial).
pub fn substitute_rtype(rt: &RType, env: &Vec<(String, RType)>) -> RType {
    match rt {
        RType::Bool => RType::Bool,
        RType::Int(k) => RType::Int(*k),
        RType::Struct { path, type_args, lifetime_args } => {
            let mut subst_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                subst_args.push(substitute_rtype(&type_args[i], env));
                i += 1;
            }
            RType::Struct {
                path: path.clone(),
                type_args: subst_args,
                lifetime_args: lifetime_args.clone(),
            }
        }
        RType::Ref { inner, mutable, lifetime } => RType::Ref {
            inner: Box::new(substitute_rtype(inner, env)),
            mutable: *mutable,
            lifetime: lifetime.clone(),
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(substitute_rtype(inner, env)),
            mutable: *mutable,
        },
        RType::Param(name) => {
            let mut i = 0;
            while i < env.len() {
                if env[i].0 == *name {
                    return env[i].1.clone();
                }
                i += 1;
            }
            RType::Param(name.clone())
        }
        RType::Tuple(elems) => {
            let mut out: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < elems.len() {
                out.push(substitute_rtype(&elems[i], env));
                i += 1;
            }
            RType::Tuple(out)
        }
        RType::Enum { path, type_args, lifetime_args } => {
            let mut subst_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                subst_args.push(substitute_rtype(&type_args[i], env));
                i += 1;
            }
            RType::Enum {
                path: path.clone(),
                type_args: subst_args,
                lifetime_args: lifetime_args.clone(),
            }
        }
    }
}

// Whether `t` implements `std::Copy`. Built-in types (integers, `&T`,
// `*const T`, `*mut T`) get their `impl Copy` rows from `lib/std/lib.rs`;
// user-written `impl Copy for Foo {}` adds rows too. `&mut T` is
// deliberately *not* Copy.
pub fn is_copy(t: &RType, traits: &TraitTable) -> bool {
    is_copy_with_bounds(t, traits, &Vec::new(), &Vec::new())
}

// Same as `is_copy` but also recognizes a `Param(name)` as Copy when the
// type-param's bounds include `std::Copy`. `type_params` and
// `type_param_bounds` align in length and order — typically threaded
// through from a `GenericTemplate.type_params` / `type_param_bounds`.
pub fn is_copy_with_bounds(
    t: &RType,
    traits: &TraitTable,
    type_params: &Vec<String>,
    type_param_bounds: &Vec<Vec<Vec<String>>>,
) -> bool {
    solve_impl_in_ctx(&copy_trait_path(), t, traits, type_params, type_param_bounds, 0).is_some()
}

pub fn copy_trait_path() -> Vec<String> {
    vec!["std".to_string(), "marker".to_string(), "Copy".to_string()]
}

pub fn drop_trait_path() -> Vec<String> {
    vec!["std".to_string(), "ops".to_string(), "Drop".to_string()]
}

// Whether `t` implements `std::Drop`. Used by codegen to decide whether
// to emit drop calls at scope end and by impl validation to enforce
// Drop/Copy mutual exclusion.
pub fn is_drop(t: &RType, traits: &TraitTable) -> bool {
    let drop_path = drop_trait_path();
    solve_impl(&drop_path, t, traits, 0).is_some()
}

pub fn is_raw_ptr(t: &RType) -> bool {
    matches!(t, RType::RawPtr { .. })
}

pub fn is_ref_mutable(t: &RType) -> bool {
    matches!(t, RType::Ref { mutable: true, .. })
}
