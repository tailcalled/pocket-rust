use super::{
    EnumTable, StructTable, TraitTable, VariantPayloadResolved, enum_lookup, place_to_string,
    solve_impl, struct_lookup,
};
use super::traits::solve_impl_in_ctx;

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

pub(crate) fn int_kind_from_name(name: &str) -> Option<IntKind> {
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

// Magnitude of the most-negative value representable in this kind:
// for signed types `2^(bits-1)` (so `-2^(bits-1)` is in range);
// `0` for unsigned types (no negative range). Used to range-check
// `-N` literals against the resolved type.
pub(super) fn int_kind_neg_magnitude(k: &IntKind) -> u128 {
    match k {
        IntKind::U8 | IntKind::U16 | IntKind::U32 | IntKind::U64 | IntKind::U128
        | IntKind::Usize => 0,
        IntKind::I8 => 1u128 << 7,
        IntKind::I16 => 1u128 << 15,
        IntKind::I32 => 1u128 << 31,
        IntKind::I64 => 1u128 << 63,
        IntKind::I128 => 1u128 << 127,
        IntKind::Isize => 1u128 << 31,
    }
}

pub(super) fn int_kind_signed(k: &IntKind) -> bool {
    matches!(
        k,
        IntKind::I8
            | IntKind::I16
            | IntKind::I32
            | IntKind::I64
            | IntKind::I128
            | IntKind::Isize
    )
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
    // `[T]` — the dynamically-sized slice type. Bare `Slice` is
    // unsized: `byte_size_of` and `flatten_rtype` panic on it. Valid
    // only as the inner of a `Ref { inner: Slice(_), .. }` — a fat
    // pointer that flattens to `[I32, I32]` (data ptr + length).
    Slice(Box<RType>),
    // `str` — UTF-8 string DST. Layout-identical to `Slice(Box::new(Int(U8)))`
    // (a fat ref over u8 bytes), but kept distinct from `Slice<u8>` at the
    // type level so users get `&str` in error messages and so future UTF-8
    // invariants can attach here. Same DST rules as `Slice`.
    Str,
    // `char` — Unicode scalar value (0..=0x10FFFF excluding surrogates).
    // Stored as a 4-byte u32 in memory; flattens to one `i32` in wasm.
    // Distinct from `u32` at the type level: `'X' as u32` is required
    // to convert. Codegen treats char-as-int (and int-as-char with
    // range check skipped at the moment — relying on the lexer's
    // codepoint validation upstream).
    Char,
    // `!` — the never type. Has no inhabitants. Coerces freely to any
    // other type at unification time so `break`/`continue`/`return`
    // (and calls to functions returning `!`) can sit as one arm of an
    // `if`/`match` whose other arm yields a real value, with the
    // construct typed as the real value's type. Codegen treats Never
    // as zero-sized / no-flat-scalars (its expressions never produce
    // a wasm value — they emit `br` / `unreachable` / etc.).
    Never,
    // Associated-type projection: `Self::Item` or `T::Item` in source.
    // `base` is `Param("Self")` / `Param("T")` until substitution lands
    // a concrete type; `concretize_assoc_proj` then resolves to the
    // impl's binding. `trait_path` is the trait that declares this
    // assoc — populated when uniquely determined from the param's
    // bounds (or always at trait-method-sig time when Self refers to
    // the enclosing trait). `name` is the assoc-type name.
    AssocProj {
        base: Box<RType>,
        trait_path: Vec<String>,
        name: String,
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
        (RType::Slice(a), RType::Slice(b)) => rtype_eq(a, b),
        (RType::Str, RType::Str) => true,
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
        (
            RType::AssocProj { base: ba, trait_path: ta, name: na },
            RType::AssocProj { base: bb, trait_path: tb, name: nb },
        ) => ta == tb && na == nb && rtype_eq(ba, bb),
        (RType::Never, RType::Never) => true,
        (RType::Char, RType::Char) => true,
        _ => false,
    }
}

// True iff `t` (or any nested element) is `Param(_)`. Used at typeck
// finalize to skip impl-resolution validation when a trait_dispatch
// still depends on an outer generic that won't be pinned until
// monomorphization.
pub fn rtype_contains_param(t: &RType) -> bool {
    match t {
        RType::Param(_) => true,
        RType::Struct { type_args, .. } | RType::Enum { type_args, .. } => {
            type_args.iter().any(rtype_contains_param)
        }
        RType::Tuple(elems) => elems.iter().any(rtype_contains_param),
        RType::Ref { inner, .. } | RType::RawPtr { inner, .. } | RType::Slice(inner) => {
            rtype_contains_param(inner)
        }
        RType::AssocProj { base, .. } => rtype_contains_param(base),
        RType::Bool | RType::Int(_) | RType::Str | RType::Never | RType::Char => false,
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
        RType::Slice(inner) => format!("[{}]", rtype_to_string(inner)),
        RType::Str => "str".to_string(),
        RType::AssocProj { base, name, .. } => {
            format!("<{} as ?>::{}", rtype_to_string(base), name)
        }
        RType::Never => "!".to_string(),
        RType::Char => "char".to_string(),
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
        RType::Ref { inner, .. } => match inner.as_ref() {
            // Fat ref to a slice: 2 wasm scalars (ptr + len).
            RType::Slice(_) | RType::Str => 2,
            _ => 1,
        },
        RType::RawPtr { .. } => 1,
        RType::Param(_) => unreachable!("rtype_size called on unresolved type parameter"),
        RType::Slice(_) | RType::Str => unreachable!("`[T]` / `str` is unsized — only valid behind a reference"),
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
        RType::AssocProj { .. } => unreachable!(
            "rtype_size called on unresolved associated-type projection"
        ),
        // `!` has no inhabitants; a value of type `!` never exists at
        // runtime. Treat as zero-sized for the rare case it appears in
        // a layout (e.g. `(u32, !)` is theoretically size 4); in
        // practice expressions of type `!` divert via br / unreachable
        // before any storage layout matters.
        RType::Never => 0,
        RType::Char => 1,
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
        RType::Ref { inner, .. } => match inner.as_ref() {
            // Fat ref to a DST slice or str: (data ptr, length) — both
            // i32 on wasm32.
            RType::Slice(_) | RType::Str => {
                out.push(crate::wasm::ValType::I32);
                out.push(crate::wasm::ValType::I32);
            }
            _ => out.push(crate::wasm::ValType::I32),
        },
        RType::RawPtr { .. } => out.push(crate::wasm::ValType::I32),
        RType::Param(_) => unreachable!("flatten_rtype called on unresolved type parameter"),
        RType::Slice(_) | RType::Str => unreachable!("`[T]` / `str` is unsized — only valid behind a reference"),
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
        RType::AssocProj { .. } => unreachable!(
            "flatten_rtype called on unresolved associated-type projection"
        ),
        // No flat scalars — `!` has no inhabitants, so a function that
        // claims to return `!` produces no wasm result. Same shape as
        // a 0-tuple from the wasm-ABI's perspective.
        RType::Never => {}
        // `char` flattens to a single i32 (the codepoint as a 4-byte
        // value).
        RType::Char => out.push(crate::wasm::ValType::I32),
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
        RType::Ref { inner, .. } => match inner.as_ref() {
            // Fat ref: 8 bytes (4 ptr + 4 len) on wasm32.
            RType::Slice(_) | RType::Str => 8,
            _ => 4,
        },
        RType::RawPtr { .. } => 4,
        RType::Slice(_) | RType::Str => unreachable!("`[T]` / `str` is unsized — only valid behind a reference"),
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
        RType::AssocProj { .. } => unreachable!(
            "byte_size_of called on unresolved associated-type projection"
        ),
        RType::Never => 0,
        RType::Char => 4,
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
        RType::Slice(inner) => RType::Slice(Box::new(substitute_rtype(inner, env))),
        RType::Str => RType::Str,
        RType::AssocProj { base, trait_path, name } => RType::AssocProj {
            base: Box::new(substitute_rtype(base, env)),
            trait_path: trait_path.clone(),
            name: name.clone(),
        },
        RType::Never => RType::Never,
        RType::Char => RType::Char,
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

// Given a method name, return the trait paths that an unbound
// integer-literal var should consult for dispatch — namely every
// trait that declares the method *and* has at least one Int-target
// impl. This is the dynamic replacement for the old hardcoded list
// (which only covered the std arith / cmp / `*Assign` traits and
// silently failed for user traits with int impls). User-defined
// traits with int impls are now first-class for num-lit dispatch:
// e.g. `trait Halver { type Out; fn halve(self) -> Self::Out; }`
// with `impl Halver for u32` makes `42.halve()` dispatch through
// Halver, with the result type pinned by surrounding context.
pub fn numeric_lit_op_traits_for_method(
    traits: &TraitTable,
    method: &str,
) -> Vec<Vec<String>> {
    let mut result: Vec<Vec<String>> = Vec::new();
    let mut t = 0;
    while t < traits.entries.len() {
        let entry = &traits.entries[t];
        let mut declares_method = false;
        let mut m = 0;
        while m < entry.methods.len() {
            if entry.methods[m].name == method {
                declares_method = true;
                break;
            }
            m += 1;
        }
        if declares_method {
            // Trait declares the method — does it have any Int-target
            // impl that a num-lit Var could plausibly select?
            let mut has_int_impl = false;
            let mut i = 0;
            while i < traits.impls.len() {
                let row = &traits.impls[i];
                if row.trait_path == entry.path
                    && matches!(&row.target, RType::Int(_))
                {
                    has_int_impl = true;
                    break;
                }
                i += 1;
            }
            if has_int_impl {
                result.push(entry.path.clone());
            }
        }
        t += 1;
    }
    result
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

// Whether `t` has no inhabitants — i.e. no value of type `t` can ever
// exist. `!` is the canonical uninhabited type; a tuple/struct with
// any uninhabited field is uninhabited (constructing it would require
// a value of that field's type); an enum is uninhabited iff *every*
// variant's payload is uninhabited (no variant can be constructed).
// References and raw pointers are treated as inhabited regardless of
// pointee — practically irrelevant for our use cases (`&!` doesn't
// arise in valid programs). Used by match-exhaustiveness to skip
// uninconstructable variants like `Err(!)` in `Result<T, !>`.
pub fn is_uninhabited(
    t: &RType,
    structs: &StructTable,
    enums: &EnumTable,
) -> bool {
    match t {
        RType::Never => true,
        RType::Tuple(elems) => elems.iter().any(|e| is_uninhabited(e, structs, enums)),
        RType::Struct { path, type_args, .. } => {
            let entry = match struct_lookup(structs, path) {
                Some(e) => e,
                None => return false,
            };
            let env = struct_env(&entry.type_params, type_args);
            entry.fields.iter().any(|f| {
                let fty = substitute_rtype(&f.ty, &env);
                is_uninhabited(&fty, structs, enums)
            })
        }
        RType::Enum { path, type_args, .. } => {
            let entry = match enum_lookup(enums, path) {
                Some(e) => e,
                None => return false,
            };
            let env = struct_env(&entry.type_params, type_args);
            // Enum is uninhabited iff every variant is unconstructable.
            entry.variants.iter().all(|v| {
                is_variant_payload_uninhabited(&v.payload, &env, structs, enums)
            })
        }
        // Refs / raw pointers: nominally inhabited (pointer-to-? is
        // an i32). Other leaf types: inhabited.
        _ => false,
    }
}

// A variant's payload is uninhabited if any field/element is
// uninhabited. A `Unit` variant is *constructable* (no payload to
// produce), so always inhabited.
pub fn is_variant_payload_uninhabited(
    payload: &VariantPayloadResolved,
    env: &Vec<(String, RType)>,
    structs: &StructTable,
    enums: &EnumTable,
) -> bool {
    match payload {
        VariantPayloadResolved::Unit => false,
        VariantPayloadResolved::Tuple(types) => types.iter().any(|t| {
            let ty = substitute_rtype(t, env);
            is_uninhabited(&ty, structs, enums)
        }),
        VariantPayloadResolved::Struct(fields) => fields.iter().any(|f| {
            let ty = substitute_rtype(&f.ty, env);
            is_uninhabited(&ty, structs, enums)
        }),
    }
}

// Whether `t` is `Sized` — i.e. has a known compile-time size. The two
// DSTs in pocket-rust are `str` and `[T]`; everything else is Sized
// (refs/pointers to DSTs are Sized — a `&str` is two i32s). A `Param`
// is conservatively treated as Sized: every type-param implicitly
// carries a `T: Sized` bound, so a binding to a non-Sized type is
// rejected at unification (see `bind_var`). `!` is Sized (zero-sized).
pub fn is_sized(t: &RType) -> bool {
    !matches!(t, RType::Slice(_) | RType::Str)
}

pub fn is_ref_mutable(t: &RType) -> bool {
    matches!(t, RType::Ref { mutable: true, .. })
}
