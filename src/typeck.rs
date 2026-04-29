use crate::ast::{
    AssignStmt, Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, Module,
    PathSegment, Stmt, StructLit, Type, TypeKind,
};
use crate::span::{Error, Span};

// ----- Public RType used by borrowck and codegen -----

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

pub fn int_kind_copy(k: &IntKind) -> IntKind {
    match k {
        IntKind::U8 => IntKind::U8,
        IntKind::I8 => IntKind::I8,
        IntKind::U16 => IntKind::U16,
        IntKind::I16 => IntKind::I16,
        IntKind::U32 => IntKind::U32,
        IntKind::I32 => IntKind::I32,
        IntKind::U64 => IntKind::U64,
        IntKind::I64 => IntKind::I64,
        IntKind::U128 => IntKind::U128,
        IntKind::I128 => IntKind::I128,
        IntKind::Usize => IntKind::Usize,
        IntKind::Isize => IntKind::Isize,
    }
}

pub fn int_kind_eq(a: &IntKind, b: &IntKind) -> bool {
    match (a, b) {
        (IntKind::U8, IntKind::U8) => true,
        (IntKind::I8, IntKind::I8) => true,
        (IntKind::U16, IntKind::U16) => true,
        (IntKind::I16, IntKind::I16) => true,
        (IntKind::U32, IntKind::U32) => true,
        (IntKind::I32, IntKind::I32) => true,
        (IntKind::U64, IntKind::U64) => true,
        (IntKind::I64, IntKind::I64) => true,
        (IntKind::U128, IntKind::U128) => true,
        (IntKind::I128, IntKind::I128) => true,
        (IntKind::Usize, IntKind::Usize) => true,
        (IntKind::Isize, IntKind::Isize) => true,
        _ => false,
    }
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

fn int_kind_from_name(name: &str) -> Option<IntKind> {
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
fn int_kind_max(k: &IntKind) -> u128 {
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

pub enum RType {
    Int(IntKind),
    Struct {
        path: Vec<String>,
        type_args: Vec<RType>,
    },
    Ref { inner: Box<RType>, mutable: bool },
    RawPtr { inner: Box<RType>, mutable: bool },
    // An opaque type parameter inside a generic body. Carries the param's
    // name. Codegen substitutes these to concrete types during monomorphization;
    // operations needing layout (byte_size_of, flatten_rtype) reject `Param`.
    Param(String),
}

pub fn rtype_clone(t: &RType) -> RType {
    match t {
        RType::Int(k) => RType::Int(int_kind_copy(k)),
        RType::Struct { path, type_args } => RType::Struct {
            path: clone_path(path),
            type_args: rtype_vec_clone(type_args),
        },
        RType::Ref { inner, mutable } => RType::Ref {
            inner: Box::new(rtype_clone(inner)),
            mutable: *mutable,
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(rtype_clone(inner)),
            mutable: *mutable,
        },
        RType::Param(n) => RType::Param(n.clone()),
    }
}

fn rtype_vec_clone(v: &Vec<RType>) -> Vec<RType> {
    let mut out: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(rtype_clone(&v[i]));
        i += 1;
    }
    out
}

fn rtype_vec_eq(a: &Vec<RType>, b: &Vec<RType>) -> bool {
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
        (RType::Int(ka), RType::Int(kb)) => int_kind_eq(ka, kb),
        (
            RType::Struct {
                path: pa,
                type_args: aa,
            },
            RType::Struct {
                path: pb,
                type_args: ab,
            },
        ) => path_eq(pa, pb) && rtype_vec_eq(aa, ab),
        (
            RType::Ref {
                inner: ia,
                mutable: ma,
            },
            RType::Ref {
                inner: ib,
                mutable: mb,
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
        _ => false,
    }
}

pub fn rtype_to_string(t: &RType) -> String {
    match t {
        RType::Int(k) => int_kind_name(k).to_string(),
        RType::Struct { path, type_args } => {
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
        RType::Ref { inner, mutable } => {
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
    }
}

pub fn rtype_size(ty: &RType, structs: &StructTable) -> u32 {
    match ty {
        RType::Int(k) => match k {
            IntKind::U128 | IntKind::I128 => 2,
            _ => 1,
        },
        RType::Struct { path, type_args } => {
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
    }
}

fn struct_env(type_params: &Vec<String>, type_args: &Vec<RType>) -> Vec<(String, RType)> {
    let mut env: Vec<(String, RType)> = Vec::new();
    let n = if type_params.len() < type_args.len() {
        type_params.len()
    } else {
        type_args.len()
    };
    let mut i = 0;
    while i < n {
        env.push((type_params[i].clone(), rtype_clone(&type_args[i])));
        i += 1;
    }
    env
}

pub fn flatten_rtype(ty: &RType, structs: &StructTable, out: &mut Vec<crate::wasm::ValType>) {
    match ty {
        RType::Int(k) => match k {
            IntKind::U64 | IntKind::I64 => out.push(crate::wasm::ValType::I64),
            IntKind::U128 | IntKind::I128 => {
                out.push(crate::wasm::ValType::I64);
                out.push(crate::wasm::ValType::I64);
            }
            _ => out.push(crate::wasm::ValType::I32),
        },
        RType::Struct { path, type_args } => {
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
    }
}

pub fn byte_size_of(rt: &RType, structs: &StructTable) -> u32 {
    match rt {
        RType::Int(k) => match k {
            IntKind::U8 | IntKind::I8 => 1,
            IntKind::U16 | IntKind::I16 => 2,
            IntKind::U32 | IntKind::I32 | IntKind::Usize | IntKind::Isize => 4,
            IntKind::U64 | IntKind::I64 => 8,
            IntKind::U128 | IntKind::I128 => 16,
        },
        RType::Ref { .. } | RType::RawPtr { .. } => 4,
        RType::Struct { path, type_args } => {
            let entry = struct_lookup(structs, path).expect("resolved struct");
            let env = struct_env(&entry.type_params, type_args);
            let mut total: u32 = 0;
            let mut i = 0;
            while i < entry.fields.len() {
                let fty = substitute_rtype(&entry.fields[i].ty, &env);
                total += byte_size_of(&fty, structs);
                i += 1;
            }
            total
        }
        RType::Param(_) => unreachable!("byte_size_of called on unresolved type parameter"),
    }
}

// Substitutes type parameters with their concrete types. `env` maps each
// param name to a concrete RType. Called by codegen during monomorphization.
// If a Param doesn't appear in env, returns it unchanged (for nested-generic
// scenarios where the env is partial).
pub fn substitute_rtype(rt: &RType, env: &Vec<(String, RType)>) -> RType {
    match rt {
        RType::Int(k) => RType::Int(int_kind_copy(k)),
        RType::Struct { path, type_args } => {
            let mut subst_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                subst_args.push(substitute_rtype(&type_args[i], env));
                i += 1;
            }
            RType::Struct {
                path: clone_path(path),
                type_args: subst_args,
            }
        }
        RType::Ref { inner, mutable } => RType::Ref {
            inner: Box::new(substitute_rtype(inner, env)),
            mutable: *mutable,
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(substitute_rtype(inner, env)),
            mutable: *mutable,
        },
        RType::Param(name) => {
            let mut i = 0;
            while i < env.len() {
                if env[i].0 == *name {
                    return rtype_clone(&env[i].1);
                }
                i += 1;
            }
            RType::Param(name.clone())
        }
    }
}

pub fn is_copy(t: &RType) -> bool {
    match t {
        RType::Int(_) => true,
        RType::Struct { .. } => false,
        RType::Ref { .. } => true,
        RType::RawPtr { .. } => true,
        // Without trait bounds, we can't claim T is Copy. Conservatively false.
        RType::Param(_) => false,
    }
}

pub fn is_raw_ptr(t: &RType) -> bool {
    matches!(t, RType::RawPtr { .. })
}

pub fn is_ref_mutable(t: &RType) -> bool {
    matches!(t, RType::Ref { mutable: true, .. })
}

pub struct RTypedField {
    pub name: String,
    pub name_span: Span,
    pub ty: RType,
}

pub struct StructEntry {
    pub path: Vec<String>,
    pub name_span: Span,
    pub file: String,
    pub type_params: Vec<String>,
    pub fields: Vec<RTypedField>,
}

pub struct StructTable {
    pub entries: Vec<StructEntry>,
}

pub fn struct_lookup<'a>(table: &'a StructTable, path: &Vec<String>) -> Option<&'a StructEntry> {
    let mut i = 0;
    while i < table.entries.len() {
        if path_eq(&table.entries[i].path, path) {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}

pub struct FnSymbol {
    pub path: Vec<String>,
    pub idx: u32,
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    pub let_types: Vec<RType>,
    pub lit_types: Vec<RType>,
    // Per `StructLit` expression in body, in source-DFS order: the final
    // resolved struct type (with concrete `type_args`). Codegen reads these
    // to compute layout for generic struct literals.
    pub struct_lit_types: Vec<RType>,
    // For each `Deref` expression in the body, in source-DFS order: `true`
    // iff the operand resolved to a raw pointer (`*const T` / `*mut T`).
    // Safeck reads this in lockstep to flag derefs outside `unsafe` blocks.
    pub deref_is_raw: Vec<bool>,
    // Lifetime elision: index of the source ref param whose lifetime flows to
    // the output ref. Rule 3 (self) takes precedence over rule 2 (single ref).
    pub ret_ref_source: Option<usize>,
    // Per `MethodCall` expression in body, in source-DFS order: how to lower
    // it. Borrowck and codegen consume this in lockstep.
    pub method_resolutions: Vec<MethodResolution>,
    // Per `Call` expression in body, in source-DFS order: which callee.
    pub call_resolutions: Vec<CallResolution>,
}

// How a `Call` expression resolves to a callee. For non-generic functions
// it's an index into FuncTable.entries. For generic functions, it points to
// a template plus the type arguments at the call site (which may themselves
// contain `Param` if the calling function is also generic — substituted at
// monomorphization).
pub enum CallResolution {
    Direct(usize),
    Generic {
        template_idx: usize,
        type_args: Vec<RType>,
    },
}

// A generic function declaration. Its body is type-checked once,
// polymorphically (so let_types/lit_types/etc. may contain `RType::Param`).
// Codegen monomorphizes lazily per (template_idx, concrete type_args) pair,
// substituting Param → concrete in the recorded artifacts.
pub struct GenericTemplate {
    pub path: Vec<String>,
    pub type_params: Vec<String>,
    pub func: crate::ast::Function,
    pub enclosing_module: Vec<String>,
    pub source_file: String,
    pub param_types: Vec<RType>,
    pub return_type: Option<RType>,
    pub let_types: Vec<RType>,
    pub lit_types: Vec<RType>,
    pub struct_lit_types: Vec<RType>,
    pub deref_is_raw: Vec<bool>,
    pub ret_ref_source: Option<usize>,
    pub method_resolutions: Vec<MethodResolution>,
    pub call_resolutions: Vec<CallResolution>,
}

pub struct MethodResolution {
    // For concrete methods (non-template), this is the WASM idx. For
    // generic-method calls, ignored — see `template_idx`/`type_args` instead.
    pub callee_idx: u32,
    pub callee_path: Vec<String>,
    pub recv_adjust: ReceiverAdjust,
    pub ret_borrows_receiver: bool,
    // When the method is a generic template (impl-generic and/or method-generic),
    // these record the resolution for codegen to monomorphize. type_args has
    // length = template's type_params.len(), in the same order: impl's params
    // first (bound to receiver type_args), then method's own (fresh vars
    // resolved by inference).
    pub template_idx: Option<usize>,
    pub type_args: Vec<RType>,
}

pub enum ReceiverAdjust {
    Move,        // recv is consumed; method takes Self
    BorrowImm,   // recv is owned; method takes &Self → emit &recv
    BorrowMut,   // recv is owned; method takes &mut Self → emit &mut recv
    ByRef,       // recv is &Self/&mut Self; pass i32 directly (incl. mut→imm downgrade)
}

pub struct FuncTable {
    pub entries: Vec<FnSymbol>,
    pub templates: Vec<GenericTemplate>,
}

pub fn template_lookup<'a>(
    table: &'a FuncTable,
    path: &Vec<String>,
) -> Option<(usize, &'a GenericTemplate)> {
    let mut i = 0;
    while i < table.templates.len() {
        if path_eq(&table.templates[i].path, path) {
            return Some((i, &table.templates[i]));
        }
        i += 1;
    }
    None
}

pub fn func_lookup<'a>(table: &'a FuncTable, path: &Vec<String>) -> Option<&'a FnSymbol> {
    let mut i = 0;
    while i < table.entries.len() {
        if path_eq(&table.entries[i].path, path) {
            return Some(&table.entries[i]);
        }
        i += 1;
    }
    None
}

pub fn clone_path(path: &Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < path.len() {
        out.push(path[i].clone());
        i += 1;
    }
    out
}

pub fn path_eq(a: &Vec<String>, b: &Vec<String>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

// Resolve a path expression's segments to an absolute lookup path. Handles
// `Self::…` substitution: replaces a leading `Self` segment with the impl
// target's struct name. Used by both typeck and codegen for call and struct
// literal lookups.
pub fn resolve_full_path(
    current_module: &Vec<String>,
    self_target: Option<&RType>,
    segments: &Vec<PathSegment>,
) -> Vec<String> {
    let mut full = clone_path(current_module);
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
    self_target: Option<&RType>,
    type_params: &Vec<String>,
    file: &str,
) -> Result<RType, Error> {
    match &ty.kind {
        TypeKind::Path(path) => {
            if path.segments.len() == 1 {
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
            let mut full = clone_path(current_module);
            let mut i = 0;
            while i < path.segments.len() {
                full.push(path.segments[i].name.clone());
                i += 1;
            }
            // Generic args attach to the path's last segment.
            let last = &path.segments[path.segments.len() - 1];
            let entry = match struct_lookup(structs, &full) {
                Some(e) => e,
                None => {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!("unknown type: {}", segments_to_string(&path.segments)),
                        span: path.span.copy(),
                    });
                }
            };
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
            let mut type_args: Vec<RType> = Vec::new();
            let mut i = 0;
            while i < last.args.len() {
                let t =
                    resolve_type(&last.args[i], current_module, structs, self_target, type_params, file)?;
                type_args.push(t);
                i += 1;
            }
            Ok(RType::Struct {
                path: full,
                type_args,
            })
        }
        TypeKind::Ref { inner, mutable } => {
            let r = resolve_type(inner, current_module, structs, self_target, type_params, file)?;
            Ok(RType::Ref {
                inner: Box::new(r),
                mutable: *mutable,
            })
        }
        TypeKind::RawPtr { inner, mutable } => {
            let r = resolve_type(inner, current_module, structs, self_target, type_params, file)?;
            Ok(RType::RawPtr {
                inner: Box::new(r),
                mutable: *mutable,
            })
        }
        TypeKind::SelfType => match self_target {
            Some(rt) => Ok(rtype_clone(rt)),
            None => Err(Error {
                file: file.to_string(),
                message: "`Self` is only valid inside an `impl` block".to_string(),
                span: ty.span.copy(),
            }),
        },
    }
}

// ----- Inference machinery -----

pub fn check(
    root: &Module,
    structs: &mut StructTable,
    funcs: &mut FuncTable,
    next_idx: &mut u32,
) -> Result<(), Error> {
    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_struct_names(root, &mut path, structs);

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    resolve_struct_fields(root, &mut path, structs)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    collect_funcs(root, &mut path, funcs, next_idx, structs)?;

    let mut path: Vec<String> = Vec::new();
    push_root_name(&mut path, root);
    let mut current_file = root.source_file.clone();
    check_module(root, &mut path, &mut current_file, structs, funcs)?;

    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

fn collect_struct_names(module: &Module, path: &mut Vec<String>, table: &mut StructTable) {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Struct(sd) => {
                let mut full = clone_path(path);
                full.push(sd.name.clone());
                let type_param_names: Vec<String> = sd
                    .type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                table.entries.push(StructEntry {
                    path: full,
                    name_span: sd.name_span.copy(),
                    file: module.source_file.clone(),
                    type_params: type_param_names,
                    fields: Vec::new(),
                });
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_struct_names(m, path, table);
                path.pop();
            }
            Item::Function(_) => {}
            Item::Impl(_) => {}
        }
        i += 1;
    }
}

fn resolve_struct_fields(
    module: &Module,
    path: &mut Vec<String>,
    table: &mut StructTable,
) -> Result<(), Error> {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Struct(sd) => {
                let mut full = clone_path(path);
                full.push(sd.name.clone());
                let type_param_names: Vec<String> = sd
                    .type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                let mut resolved: Vec<RTypedField> = Vec::new();
                let mut k = 0;
                while k < sd.fields.len() {
                    let rt = resolve_type(
                        &sd.fields[k].ty,
                        path,
                        table,
                        None,
                        &type_param_names,
                        &module.source_file,
                    )?;
                    if let RType::Ref { .. } = &rt {
                        return Err(Error {
                            file: module.source_file.clone(),
                            message: "struct fields cannot have reference types".to_string(),
                            span: sd.fields[k].ty.span.copy(),
                        });
                    }
                    resolved.push(RTypedField {
                        name: sd.fields[k].name.clone(),
                        name_span: sd.fields[k].name_span.copy(),
                        ty: rt,
                    });
                    k += 1;
                }
                let mut e = 0;
                while e < table.entries.len() {
                    if path_eq(&table.entries[e].path, &full) {
                        table.entries[e].fields = resolved;
                        break;
                    }
                    e += 1;
                }
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                resolve_struct_fields(m, path, table)?;
                path.pop();
            }
            Item::Function(_) => {}
            Item::Impl(_) => {}
        }
        i += 1;
    }
    Ok(())
}

fn collect_funcs(
    module: &Module,
    path: &mut Vec<String>,
    funcs: &mut FuncTable,
    next_idx: &mut u32,
    structs: &StructTable,
) -> Result<(), Error> {
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => {
                register_function(
                    f,
                    path,
                    None,
                    &Vec::new(),
                    funcs,
                    next_idx,
                    structs,
                    &module.source_file,
                )?;
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_funcs(m, path, funcs, next_idx, structs)?;
                path.pop();
            }
            Item::Struct(_) => {}
            Item::Impl(ib) => {
                let target_rt = resolve_impl_target(ib, path, structs, &module.source_file)?;
                let impl_type_params: Vec<String> =
                    ib.type_params.iter().map(|p| p.name.clone()).collect();
                let target_name = ib.target.segments[0].name.clone();
                path.push(target_name);
                let mut k = 0;
                while k < ib.methods.len() {
                    register_function(
                        &ib.methods[k],
                        path,
                        Some(&target_rt),
                        &impl_type_params,
                        funcs,
                        next_idx,
                        structs,
                        &module.source_file,
                    )?;
                    k += 1;
                }
                path.pop();
            }
        }
        i += 1;
    }
    Ok(())
}

// Resolves an `impl Path { ... }` target to its struct type. The impl's type
// params must match the target struct's type params 1:1 (e.g., `impl<T, U>
// Pair<T, U>`). Returns the struct type with `Param(...)` type args matching
// the impl's parameter names.
fn resolve_impl_target(
    ib: &crate::ast::ImplBlock,
    current_module: &Vec<String>,
    structs: &StructTable,
    file: &str,
) -> Result<RType, Error> {
    if ib.target.segments.len() != 1 {
        return Err(Error {
            file: file.to_string(),
            message: "impl target must be a single-segment path naming a struct in the current module".to_string(),
            span: ib.target.span.copy(),
        });
    }
    let mut target_path = clone_path(current_module);
    target_path.push(ib.target.segments[0].name.clone());
    let entry = match struct_lookup(structs, &target_path) {
        Some(e) => e,
        None => {
            return Err(Error {
                file: file.to_string(),
                message: format!("unknown struct: {}", ib.target.segments[0].name),
                span: ib.target.span.copy(),
            });
        }
    };
    let target_args = &ib.target.segments[0].args;
    if target_args.len() != entry.type_params.len() {
        return Err(Error {
            file: file.to_string(),
            message: format!(
                "impl target has wrong number of type arguments: expected {}, got {}",
                entry.type_params.len(),
                target_args.len()
            ),
            span: ib.target.span.copy(),
        });
    }
    if entry.type_params.len() != ib.type_params.len() {
        return Err(Error {
            file: file.to_string(),
            message: format!(
                "impl declares {} type parameter(s) but target struct has {}",
                ib.type_params.len(),
                entry.type_params.len()
            ),
            span: ib.target.span.copy(),
        });
    }
    // Each target arg must be a single-segment path matching the impl's type
    // params in order. Concrete instantiations (`impl Foo<u32>`) aren't yet
    // supported.
    let impl_param_names: Vec<String> = ib.type_params.iter().map(|p| p.name.clone()).collect();
    let mut type_args: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < target_args.len() {
        let arg_ty = &target_args[i];
        let ok = match &arg_ty.kind {
            crate::ast::TypeKind::Path(p) => {
                p.segments.len() == 1
                    && p.segments[0].args.is_empty()
                    && p.segments[0].name == impl_param_names[i]
            }
            _ => false,
        };
        if !ok {
            return Err(Error {
                file: file.to_string(),
                message:
                    "impl target's type arguments must match the impl's type parameters in order"
                        .to_string(),
                span: arg_ty.span.copy(),
            });
        }
        type_args.push(RType::Param(impl_param_names[i].clone()));
        i += 1;
    }
    Ok(RType::Struct {
        path: target_path,
        type_args,
    })
}

fn register_function(
    f: &Function,
    current_module: &Vec<String>,
    self_target: Option<&RType>,
    impl_type_params: &Vec<String>,
    funcs: &mut FuncTable,
    next_idx: &mut u32,
    structs: &StructTable,
    source_file: &str,
) -> Result<(), Error> {
    let mut type_param_names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < impl_type_params.len() {
        type_param_names.push(impl_type_params[i].clone());
        i += 1;
    }
    let mut i = 0;
    while i < f.type_params.len() {
        type_param_names.push(f.type_params[i].name.clone());
        i += 1;
    }
    let is_generic = !type_param_names.is_empty();
    let mut full = clone_path(current_module);
    full.push(f.name.clone());
    let mut param_types: Vec<RType> = Vec::new();
    let mut k = 0;
    while k < f.params.len() {
        let rt = resolve_type(
            &f.params[k].ty,
            current_module,
            structs,
            self_target,
            &type_param_names,
            source_file,
        )?;
        param_types.push(rt);
        k += 1;
    }
    let return_type = match &f.return_type {
        Some(ty) => Some(resolve_type(
            ty,
            current_module,
            structs,
            self_target,
            &type_param_names,
            source_file,
        )?),
        None => None,
    };
    let self_idx = if !f.params.is_empty() && f.params[0].name == "self" {
        Some(0)
    } else {
        None
    };
    let ret_ref_source = match (&return_type, &f.return_type) {
        (Some(rt), Some(ret_ty)) => {
            check_ret_ref_elision(rt, &param_types, self_idx, &ret_ty.span, source_file)?
        }
        _ => None,
    };
    if is_generic {
        funcs.templates.push(GenericTemplate {
            path: full,
            type_params: type_param_names,
            func: f.clone(),
            enclosing_module: clone_path(current_module),
            source_file: source_file.to_string(),
            param_types,
            return_type,
            let_types: Vec::new(),
            lit_types: Vec::new(),
            struct_lit_types: Vec::new(),
            deref_is_raw: Vec::new(),
            ret_ref_source,
            method_resolutions: Vec::new(),
            call_resolutions: Vec::new(),
        });
    } else {
        funcs.entries.push(FnSymbol {
            path: full,
            idx: *next_idx,
            param_types,
            return_type,
            let_types: Vec::new(),
            lit_types: Vec::new(),
            struct_lit_types: Vec::new(),
            deref_is_raw: Vec::new(),
            ret_ref_source,
            method_resolutions: Vec::new(),
            call_resolutions: Vec::new(),
        });
        *next_idx += 1;
    }
    Ok(())
}

// Lifetime elision for ref return types. Rule 3: when a method has `&self` /
// `&mut self`, the output borrow's lifetime is `self`'s, regardless of other
// ref params. Rule 2: otherwise, exactly one input ref param → its lifetime.
// `&mut T -> &U` is allowed (downgrade); `&T -> &mut U` is rejected.
fn check_ret_ref_elision(
    return_type: &RType,
    param_types: &Vec<RType>,
    self_idx: Option<usize>,
    ret_span: &Span,
    file: &str,
) -> Result<Option<usize>, Error> {
    let ret_mutable = match return_type {
        RType::Ref { mutable, .. } => *mutable,
        _ => return Ok(None),
    };
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
            return Ok(Some(idx));
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
    Ok(Some(src_idx))
}

// ----- InferType -----

enum InferType {
    Var(u32),
    Int(IntKind),
    Struct {
        path: Vec<String>,
        type_args: Vec<InferType>,
    },
    Ref { inner: Box<InferType>, mutable: bool },
    RawPtr { inner: Box<InferType>, mutable: bool },
    Param(String),
}

fn infer_clone(t: &InferType) -> InferType {
    match t {
        InferType::Var(v) => InferType::Var(*v),
        InferType::Int(k) => InferType::Int(int_kind_copy(k)),
        InferType::Struct { path, type_args } => InferType::Struct {
            path: clone_path(path),
            type_args: infer_vec_clone(type_args),
        },
        InferType::Ref { inner, mutable } => InferType::Ref {
            inner: Box::new(infer_clone(inner)),
            mutable: *mutable,
        },
        InferType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(infer_clone(inner)),
            mutable: *mutable,
        },
        InferType::Param(n) => InferType::Param(n.clone()),
    }
}

fn infer_vec_clone(v: &Vec<InferType>) -> Vec<InferType> {
    let mut out: Vec<InferType> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        out.push(infer_clone(&v[i]));
        i += 1;
    }
    out
}

// Build a name → InferType env from a generic struct/template's type-param
// names paired with the call site's type arguments. Used to substitute Param
// in field types or method signatures.
fn build_infer_env(type_params: &Vec<String>, type_args: &Vec<InferType>) -> Vec<(String, InferType)> {
    let mut env: Vec<(String, InferType)> = Vec::new();
    let n = if type_params.len() < type_args.len() {
        type_params.len()
    } else {
        type_args.len()
    };
    let mut i = 0;
    while i < n {
        env.push((type_params[i].clone(), infer_clone(&type_args[i])));
        i += 1;
    }
    env
}

fn rtype_to_infer(rt: &RType) -> InferType {
    match rt {
        RType::Int(k) => InferType::Int(int_kind_copy(k)),
        RType::Struct { path, type_args } => {
            let mut infer_args: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                infer_args.push(rtype_to_infer(&type_args[i]));
                i += 1;
            }
            InferType::Struct {
                path: clone_path(path),
                type_args: infer_args,
            }
        }
        RType::Ref { inner, mutable } => InferType::Ref {
            inner: Box::new(rtype_to_infer(inner)),
            mutable: *mutable,
        },
        RType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(rtype_to_infer(inner)),
            mutable: *mutable,
        },
        RType::Param(n) => InferType::Param(n.clone()),
    }
}

// Substitute type parameters in an InferType using a name → InferType env.
// Used at generic call sites to map the callee's `Param("T")` slots to fresh
// inference vars allocated for the call.
fn infer_substitute(t: &InferType, env: &Vec<(String, InferType)>) -> InferType {
    match t {
        InferType::Var(v) => InferType::Var(*v),
        InferType::Int(k) => InferType::Int(int_kind_copy(k)),
        InferType::Struct { path, type_args } => {
            let mut subst_args: Vec<InferType> = Vec::new();
            let mut i = 0;
            while i < type_args.len() {
                subst_args.push(infer_substitute(&type_args[i], env));
                i += 1;
            }
            InferType::Struct {
                path: clone_path(path),
                type_args: subst_args,
            }
        }
        InferType::Ref { inner, mutable } => InferType::Ref {
            inner: Box::new(infer_substitute(inner, env)),
            mutable: *mutable,
        },
        InferType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(infer_substitute(inner, env)),
            mutable: *mutable,
        },
        InferType::Param(name) => {
            let mut i = 0;
            while i < env.len() {
                if env[i].0 == *name {
                    return infer_clone(&env[i].1);
                }
                i += 1;
            }
            InferType::Param(name.clone())
        }
    }
}

fn infer_to_string(t: &InferType) -> String {
    match t {
        InferType::Var(v) => format!("?{}", v),
        InferType::Int(k) => int_kind_name(k).to_string(),
        InferType::Struct { path, type_args } => {
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
                    s.push_str(&infer_to_string(&type_args[i]));
                    i += 1;
                }
                s.push('>');
                s
            }
        }
        InferType::Ref { inner, mutable } => {
            if *mutable {
                format!("&mut {}", infer_to_string(inner))
            } else {
                format!("&{}", infer_to_string(inner))
            }
        }
        InferType::RawPtr { inner, mutable } => {
            if *mutable {
                format!("*mut {}", infer_to_string(inner))
            } else {
                format!("*const {}", infer_to_string(inner))
            }
        }
        InferType::Param(n) => n.clone(),
    }
}

struct Subst {
    bindings: Vec<Option<InferType>>,
    // Per-var "integer class" flag. A var with this flag set must unify with
    // an integer concrete type; trying to unify it with anything else is an
    // immediate error rather than a later "literal couldn't be resolved" check.
    is_integer: Vec<bool>,
}

impl Subst {
    fn fresh_int(&mut self) -> u32 {
        let id = self.bindings.len() as u32;
        self.bindings.push(None);
        self.is_integer.push(true);
        id
    }

    fn fresh_var(&mut self) -> u32 {
        let id = self.bindings.len() as u32;
        self.bindings.push(None);
        self.is_integer.push(false);
        id
    }

    fn substitute(&self, ty: &InferType) -> InferType {
        match ty {
            InferType::Var(v) => match &self.bindings[*v as usize] {
                Some(t) => self.substitute(t),
                None => InferType::Var(*v),
            },
            InferType::Int(k) => InferType::Int(int_kind_copy(k)),
            InferType::Struct { path, type_args } => {
                let mut subst_args: Vec<InferType> = Vec::new();
                let mut i = 0;
                while i < type_args.len() {
                    subst_args.push(self.substitute(&type_args[i]));
                    i += 1;
                }
                InferType::Struct {
                    path: clone_path(path),
                    type_args: subst_args,
                }
            }
            InferType::Ref { inner, mutable } => InferType::Ref {
                inner: Box::new(self.substitute(inner)),
                mutable: *mutable,
            },
            InferType::RawPtr { inner, mutable } => InferType::RawPtr {
                inner: Box::new(self.substitute(inner)),
                mutable: *mutable,
            },
            InferType::Param(n) => InferType::Param(n.clone()),
        }
    }

    fn bind_var(
        &mut self,
        v: u32,
        other: InferType,
        span: &Span,
        file: &str,
    ) -> Result<(), Error> {
        if self.is_integer[v as usize] {
            match &other {
                InferType::Int(_) => {}
                InferType::Var(other_v) => {
                    self.is_integer[*other_v as usize] = true;
                }
                _ => {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got integer",
                            infer_to_string(&other)
                        ),
                        span: span.copy(),
                    });
                }
            }
        }
        self.bindings[v as usize] = Some(other);
        Ok(())
    }


    fn unify(&mut self, a: &InferType, b: &InferType, span: &Span, file: &str) -> Result<(), Error> {
        let a = self.substitute(a);
        let b = self.substitute(b);
        match (a, b) {
            (InferType::Var(va), InferType::Var(vb)) => {
                if va == vb {
                    Ok(())
                } else {
                    self.bind_var(va, InferType::Var(vb), span, file)
                }
            }
            (InferType::Var(v), other) => self.bind_var(v, other, span, file),
            (other, InferType::Var(v)) => self.bind_var(v, other, span, file),
            (InferType::Int(ka), InferType::Int(kb)) => {
                if int_kind_eq(&ka, &kb) {
                    Ok(())
                } else {
                    Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            int_kind_name(&kb),
                            int_kind_name(&ka)
                        ),
                        span: span.copy(),
                    })
                }
            }
            (
                InferType::Struct {
                    path: pa,
                    type_args: aa,
                },
                InferType::Struct {
                    path: pb,
                    type_args: ab,
                },
            ) => {
                if !path_eq(&pa, &pb) {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            place_to_string(&pb),
                            place_to_string(&pa)
                        ),
                        span: span.copy(),
                    });
                }
                if aa.len() != ab.len() {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: `{}` has {} type arguments, expected {}",
                            place_to_string(&pa),
                            aa.len(),
                            ab.len()
                        ),
                        span: span.copy(),
                    });
                }
                let mut i = 0;
                while i < aa.len() {
                    self.unify(&aa[i], &ab[i], span, file)?;
                    i += 1;
                }
                Ok(())
            }
            (
                InferType::Ref {
                    inner: ia,
                    mutable: ma,
                },
                InferType::Ref {
                    inner: ib,
                    mutable: mb,
                },
            ) => {
                if ma != mb {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            if mb {
                                format!("&mut {}", infer_to_string(&ib))
                            } else {
                                format!("&{}", infer_to_string(&ib))
                            },
                            if ma {
                                format!("&mut {}", infer_to_string(&ia))
                            } else {
                                format!("&{}", infer_to_string(&ia))
                            }
                        ),
                        span: span.copy(),
                    });
                }
                self.unify(&ia, &ib, span, file)
            }
            (
                InferType::RawPtr {
                    inner: ia,
                    mutable: ma,
                },
                InferType::RawPtr {
                    inner: ib,
                    mutable: mb,
                },
            ) => {
                if ma != mb {
                    return Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            if mb {
                                format!("*mut {}", infer_to_string(&ib))
                            } else {
                                format!("*const {}", infer_to_string(&ib))
                            },
                            if ma {
                                format!("*mut {}", infer_to_string(&ia))
                            } else {
                                format!("*const {}", infer_to_string(&ia))
                            }
                        ),
                        span: span.copy(),
                    });
                }
                self.unify(&ia, &ib, span, file)
            }
            (InferType::Param(a), InferType::Param(b)) => {
                if a == b {
                    Ok(())
                } else {
                    Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            b, a
                        ),
                        span: span.copy(),
                    })
                }
            }
            (a, b) => Err(Error {
                file: file.to_string(),
                message: format!(
                    "type mismatch: expected `{}`, got `{}`",
                    infer_to_string(&b),
                    infer_to_string(&a)
                ),
                span: span.copy(),
            }),
        }
    }

    fn finalize(&self, ty: &InferType) -> RType {
        match self.substitute(ty) {
            InferType::Var(_) => RType::Int(IntKind::I32),
            InferType::Int(k) => RType::Int(k),
            InferType::Struct { path, type_args } => {
                let mut concrete: Vec<RType> = Vec::new();
                let mut i = 0;
                while i < type_args.len() {
                    concrete.push(self.finalize(&type_args[i]));
                    i += 1;
                }
                RType::Struct {
                    path,
                    type_args: concrete,
                }
            }
            InferType::Param(n) => RType::Param(n),
            InferType::Ref { inner, mutable } => RType::Ref {
                inner: Box::new(self.finalize(&inner)),
                mutable,
            },
            InferType::RawPtr { inner, mutable } => RType::RawPtr {
                inner: Box::new(self.finalize(&inner)),
                mutable,
            },
        }
    }
}

struct LitConstraint {
    var: u32,
    value: u64,
    span: Span,
}

struct LocalEntry {
    name: String,
    ty: InferType,
    mutable: bool,
}

struct CheckCtx<'a> {
    locals: Vec<LocalEntry>,
    let_vars: Vec<InferType>,
    lit_vars: Vec<u32>,
    lit_constraints: Vec<LitConstraint>,
    struct_lit_vars: Vec<InferType>,
    in_borrow: u32,
    deref_is_raw: Vec<bool>,
    method_resolutions: Vec<PendingMethodCall>,
    call_resolutions: Vec<PendingCall>,
    subst: Subst,
    current_module: &'a Vec<String>,
    current_file: &'a str,
    structs: &'a StructTable,
    funcs: &'a FuncTable,
    self_target: Option<&'a RType>,
    type_params: &'a Vec<String>,
}

fn check_module(
    module: &Module,
    path: &mut Vec<String>,
    current_file: &mut String,
    structs: &StructTable,
    funcs: &mut FuncTable,
) -> Result<(), Error> {
    let saved = current_file.clone();
    *current_file = module.source_file.clone();
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => check_function(f, path, path, None, current_file, structs, funcs)?,
            Item::Module(m) => {
                path.push(m.name.clone());
                check_module(m, path, current_file, structs, funcs)?;
                path.pop();
            }
            Item::Struct(_) => {}
            Item::Impl(ib) => {
                let target_rt = resolve_impl_target(ib, path, structs, current_file)?;
                let mut method_prefix = clone_path(path);
                method_prefix.push(ib.target.segments[0].name.clone());
                let mut k = 0;
                while k < ib.methods.len() {
                    check_function(
                        &ib.methods[k],
                        path,
                        &method_prefix,
                        Some(&target_rt),
                        current_file,
                        structs,
                        funcs,
                    )?;
                    k += 1;
                }
            }
        }
        i += 1;
    }
    *current_file = saved;
    Ok(())
}

fn check_function(
    func: &Function,
    current_module: &Vec<String>,
    path_prefix: &Vec<String>,
    self_target: Option<&RType>,
    current_file: &str,
    structs: &StructTable,
    funcs: &mut FuncTable,
) -> Result<(), Error> {
    // Look up the registered template to derive the full type-param list
    // (impl's params + method's own params, for generic impl methods).
    let lookup_path = {
        let mut p = clone_path(path_prefix);
        p.push(func.name.clone());
        p
    };
    let type_param_names: Vec<String> = match template_lookup(funcs, &lookup_path) {
        Some((_, t)) => t.type_params.clone(),
        None => Vec::new(),
    };
    // Build initial locals from params (params are immutable bindings in our subset).
    let mut locals: Vec<LocalEntry> = Vec::new();
    let mut k = 0;
    while k < func.params.len() {
        let rt = resolve_type(
            &func.params[k].ty,
            current_module,
            structs,
            self_target,
            &type_param_names,
            current_file,
        )?;
        locals.push(LocalEntry {
            name: func.params[k].name.clone(),
            ty: rtype_to_infer(&rt),
            mutable: false,
        });
        k += 1;
    }
    let return_rt: Option<RType> = match &func.return_type {
        Some(ty) => Some(resolve_type(
            ty,
            current_module,
            structs,
            self_target,
            &type_param_names,
            current_file,
        )?),
        None => None,
    };

    let (
        let_vars,
        lit_vars,
        lit_constraints,
        struct_lit_vars,
        deref_is_raw,
        method_resolutions,
        pending_calls,
        subst,
    ) = {
        let mut ctx = CheckCtx {
            locals,
            let_vars: Vec::new(),
            lit_vars: Vec::new(),
            lit_constraints: Vec::new(),
            struct_lit_vars: Vec::new(),
            in_borrow: 0,
            deref_is_raw: Vec::new(),
            method_resolutions: Vec::new(),
            call_resolutions: Vec::new(),
            subst: Subst {
                bindings: Vec::new(),
                is_integer: Vec::new(),
            },
            current_module,
            current_file,
            structs,
            funcs: &*funcs,
            self_target,
            type_params: &type_param_names,
        };
        check_block(&mut ctx, &func.body, &return_rt)?;
        (
            ctx.let_vars,
            ctx.lit_vars,
            ctx.lit_constraints,
            ctx.struct_lit_vars,
            ctx.deref_is_raw,
            ctx.method_resolutions,
            ctx.call_resolutions,
            ctx.subst,
        )
    };

    // Range-check each integer literal against its (now resolved) type.
    let mut i = 0;
    while i < lit_constraints.len() {
        let lc = &lit_constraints[i];
        let resolved = subst.substitute(&InferType::Var(lc.var));
        let kind = match resolved {
            InferType::Var(_) => IntKind::I32,
            InferType::Int(k) => k,
            _ => {
                return Err(Error {
                    file: current_file.to_string(),
                    message: "integer literal could not be resolved to an integer type"
                        .to_string(),
                    span: lc.span.copy(),
                });
            }
        };
        if (lc.value as u128) > int_kind_max(&kind) {
            return Err(Error {
                file: current_file.to_string(),
                message: format!(
                    "integer literal `{}` does not fit in `{}`",
                    lc.value,
                    int_kind_name(&kind)
                ),
                span: lc.span.copy(),
            });
        }
        i += 1;
    }

    // Finalize types.
    let mut let_types: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < let_vars.len() {
        let_types.push(subst.finalize(&let_vars[i]));
        i += 1;
    }
    let mut lit_types: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < lit_vars.len() {
        lit_types.push(subst.finalize(&InferType::Var(lit_vars[i])));
        i += 1;
    }
    let mut struct_lit_types: Vec<RType> = Vec::new();
    let mut i = 0;
    while i < struct_lit_vars.len() {
        struct_lit_types.push(subst.finalize(&struct_lit_vars[i]));
        i += 1;
    }
    // Finalize pending method calls into MethodResolution.
    let mut method_resolutions_final: Vec<MethodResolution> = Vec::new();
    let mut i = 0;
    while i < method_resolutions.len() {
        let p = &method_resolutions[i];
        let mut type_args: Vec<RType> = Vec::new();
        let mut j = 0;
        while j < p.type_arg_infers.len() {
            type_args.push(subst.finalize(&p.type_arg_infers[j]));
            j += 1;
        }
        method_resolutions_final.push(MethodResolution {
            callee_idx: p.callee_idx,
            callee_path: clone_path(&p.callee_path),
            recv_adjust: copy_recv_adjust_local(&p.recv_adjust),
            ret_borrows_receiver: p.ret_borrows_receiver,
            template_idx: p.template_idx,
            type_args,
        });
        i += 1;
    }
    let method_resolutions = method_resolutions_final;
    // Resolve pending generic call sites by finalizing each fresh type-arg var.
    let mut call_resolutions: Vec<CallResolution> = Vec::new();
    let mut i = 0;
    while i < pending_calls.len() {
        match &pending_calls[i] {
            PendingCall::Direct(idx) => call_resolutions.push(CallResolution::Direct(*idx)),
            PendingCall::Generic { template_idx, type_var_ids } => {
                let mut concrete: Vec<RType> = Vec::new();
                let mut j = 0;
                while j < type_var_ids.len() {
                    concrete.push(subst.finalize(&InferType::Var(type_var_ids[j])));
                    j += 1;
                }
                call_resolutions.push(CallResolution::Generic {
                    template_idx: *template_idx,
                    type_args: concrete,
                });
            }
        }
        i += 1;
    }

    // Store on the FnSymbol or GenericTemplate.
    let mut full = clone_path(path_prefix);
    full.push(func.name.clone());
    let mut entry_idx: Option<usize> = None;
    let mut e = 0;
    while e < funcs.entries.len() {
        if path_eq(&funcs.entries[e].path, &full) {
            entry_idx = Some(e);
            break;
        }
        e += 1;
    }
    if let Some(e) = entry_idx {
        funcs.entries[e].let_types = let_types;
        funcs.entries[e].lit_types = lit_types;
        funcs.entries[e].struct_lit_types = struct_lit_types;
        funcs.entries[e].deref_is_raw = deref_is_raw;
        funcs.entries[e].method_resolutions = method_resolutions;
        funcs.entries[e].call_resolutions = call_resolutions;
    } else {
        let mut t = 0;
        while t < funcs.templates.len() {
            if path_eq(&funcs.templates[t].path, &full) {
                funcs.templates[t].let_types = let_types;
                funcs.templates[t].lit_types = lit_types;
                funcs.templates[t].struct_lit_types = struct_lit_types;
                funcs.templates[t].deref_is_raw = deref_is_raw;
                funcs.templates[t].method_resolutions = method_resolutions;
                funcs.templates[t].call_resolutions = call_resolutions;
                break;
            }
            t += 1;
        }
    }
    Ok(())
}

fn copy_recv_adjust_local(r: &ReceiverAdjust) -> ReceiverAdjust {
    match r {
        ReceiverAdjust::Move => ReceiverAdjust::Move,
        ReceiverAdjust::BorrowImm => ReceiverAdjust::BorrowImm,
        ReceiverAdjust::BorrowMut => ReceiverAdjust::BorrowMut,
        ReceiverAdjust::ByRef => ReceiverAdjust::ByRef,
    }
}

// Per-call recording during body check; resolved at end-of-fn into `CallResolution`.
enum PendingCall {
    Direct(usize),
    Generic { template_idx: usize, type_var_ids: Vec<u32> },
}

// Like `MethodResolution`, but records type-arg InferTypes instead of
// finalized RTypes. End-of-fn finalizes via `subst.finalize`.
struct PendingMethodCall {
    callee_idx: u32,
    callee_path: Vec<String>,
    recv_adjust: ReceiverAdjust,
    ret_borrows_receiver: bool,
    template_idx: Option<usize>,
    // For template methods: one InferType per template type_param. Order:
    // impl's params first (bound to receiver type_args), then method's own
    // params (fresh inference vars, possibly unified by turbofish/inference).
    type_arg_infers: Vec<InferType>,
}

fn check_block(
    ctx: &mut CheckCtx,
    block: &Block,
    return_type: &Option<RType>,
) -> Result<(), Error> {
    let actual = check_block_inner(ctx, block)?;
    match (actual, return_type) {
        (Some(a), Some(e)) => {
            let expected_infer = rtype_to_infer(e);
            ctx.subst
                .unify(&a, &expected_infer, &tail_span_or_block(block), ctx.current_file)?;
            Ok(())
        }
        (None, None) => Ok(()),
        (Some(_), None) => Err(Error {
            file: ctx.current_file.to_string(),
            message: "function returns unit but body has a tail expression".to_string(),
            span: tail_span_or_block(block),
        }),
        (None, Some(_)) => Err(Error {
            file: ctx.current_file.to_string(),
            message: "function expects a return value but body is empty".to_string(),
            span: block.span.copy(),
        }),
    }
}

fn check_block_inner(ctx: &mut CheckCtx, block: &Block) -> Result<Option<InferType>, Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => check_let_stmt(ctx, let_stmt)?,
            Stmt::Assign(assign) => check_assign_stmt(ctx, assign)?,
            Stmt::Expr(expr) => check_expr_stmt(ctx, expr)?,
        }
        i += 1;
    }
    match &block.tail {
        Some(expr) => Ok(Some(check_expr(ctx, expr)?)),
        None => Ok(None),
    }
}

// Statement-position check for block-like expressions (`unsafe { … }`,
// `{ … }`) that don't carry a tail value. Walks the inner stmts but skips
// the "must end with a tail" check that `check_block_expr` enforces.
fn check_expr_stmt(ctx: &mut CheckCtx, expr: &Expr) -> Result<(), Error> {
    let block = match &expr.kind {
        ExprKind::Block(b) | ExprKind::Unsafe(b) => b.as_ref(),
        _ => unreachable!("parser guarantees Stmt::Expr is a block-like"),
    };
    let mark = ctx.locals.len();
    let _ = check_block_inner(ctx, block)?;
    ctx.locals.truncate(mark);
    Ok(())
}

fn check_block_expr(ctx: &mut CheckCtx, block: &Block) -> Result<InferType, Error> {
    let mark = ctx.locals.len();
    let result = check_block_inner(ctx, block)?;
    ctx.locals.truncate(mark);
    match result {
        Some(ty) => Ok(ty),
        None => Err(Error {
            file: ctx.current_file.to_string(),
            message: "block expression must end with an expression that produces a value"
                .to_string(),
            span: block.span.copy(),
        }),
    }
}

fn tail_span_or_block(block: &Block) -> Span {
    match &block.tail {
        Some(expr) => expr.span.copy(),
        None => block.span.copy(),
    }
}

fn check_let_stmt(ctx: &mut CheckCtx, let_stmt: &LetStmt) -> Result<(), Error> {
    let value_ty = check_expr(ctx, &let_stmt.value)?;
    let final_ty = match &let_stmt.ty {
        Some(annotation) => {
            let annot_rt = resolve_type(
                annotation,
                ctx.current_module,
                ctx.structs,
                ctx.self_target,
                ctx.type_params,
                ctx.current_file,
            )?;
            let annot_infer = rtype_to_infer(&annot_rt);
            ctx.subst.unify(
                &value_ty,
                &annot_infer,
                &let_stmt.value.span,
                ctx.current_file,
            )?;
            annot_infer
        }
        None => value_ty,
    };
    ctx.locals.push(LocalEntry {
        name: let_stmt.name.clone(),
        ty: infer_clone(&final_ty),
        mutable: let_stmt.mutable,
    });
    ctx.let_vars.push(final_ty);
    Ok(())
}

fn check_assign_stmt(ctx: &mut CheckCtx, assign: &AssignStmt) -> Result<(), Error> {
    // Two flavors of LHS:
    //   1. Var-rooted chain: `x` or `x.f.g.h`.
    //   2. Deref-rooted chain: `*p` or `(*p).f.g.h`.
    if let Some((root_expr, fields)) = extract_deref_rooted_chain(&assign.lhs) {
        return check_deref_rooted_assign(ctx, root_expr, &fields, assign);
    }
    // LHS must be a place expression (Var or Var-rooted FieldAccess chain).
    let chain = match extract_place_for_assign(&assign.lhs) {
        Some(c) => c,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "left-hand side of assignment must be a place expression".to_string(),
                span: assign.lhs.span.copy(),
            });
        }
    };
    // Find root binding (search reverse for innermost shadow).
    let mut found_idx: Option<usize> = None;
    let mut i = ctx.locals.len();
    while i > 0 {
        i -= 1;
        if ctx.locals[i].name == chain[0] {
            found_idx = Some(i);
            break;
        }
    }
    let idx = match found_idx {
        Some(i) => i,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("unknown variable: `{}`", chain[0]),
                span: assign.lhs.span.copy(),
            });
        }
    };
    let root_resolved = ctx.subst.substitute(&ctx.locals[idx].ty);
    let root_is_mut_ref = matches!(root_resolved, InferType::Ref { mutable: true, .. });
    let root_is_shared_ref = matches!(root_resolved, InferType::Ref { mutable: false, .. });
    if chain.len() == 1 {
        if !ctx.locals[idx].mutable {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "cannot assign to `{}`: not declared as `mut`",
                    chain[0]
                ),
                span: assign.lhs.span.copy(),
            });
        }
    } else if root_is_shared_ref {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "cannot assign through `{}`: shared reference is not mutable",
                chain[0]
            ),
            span: assign.lhs.span.copy(),
        });
    } else if !root_is_mut_ref && !ctx.locals[idx].mutable {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "cannot assign to field of `{}`: not declared as `mut`",
                chain[0]
            ),
            span: assign.lhs.span.copy(),
        });
    }
    // Walk the chain to determine the LHS type.
    let lhs_ty = walk_chain_type(
        &ctx.locals[idx].ty,
        &chain,
        ctx.structs,
        ctx.current_file,
        &assign.lhs.span,
    )?;
    let rhs_ty = check_expr(ctx, &assign.rhs)?;
    ctx.subst
        .unify(&rhs_ty, &lhs_ty, &assign.rhs.span, ctx.current_file)?;
    Ok(())
}

// Returns (deref_target, field_chain) if the LHS is `*expr` or
// `(*expr).field…`. The deref_target is the expression being dereferenced
// (typically a Var bound to a `&mut T` / `*mut T`); the field_chain is the
// list of fields walked after the deref.
fn extract_deref_rooted_chain<'a>(expr: &'a Expr) -> Option<(&'a Expr, Vec<String>)> {
    let mut fields: Vec<String> = Vec::new();
    let mut current = expr;
    loop {
        match &current.kind {
            ExprKind::Deref(inner) => {
                let mut reversed: Vec<String> = Vec::new();
                let mut i = fields.len();
                while i > 0 {
                    i -= 1;
                    reversed.push(fields[i].clone());
                }
                return Some((inner.as_ref(), reversed));
            }
            ExprKind::FieldAccess(fa) => {
                fields.push(fa.field.clone());
                current = &fa.base;
            }
            _ => return None,
        }
    }
}

fn check_deref_rooted_assign(
    ctx: &mut CheckCtx,
    root_expr: &Expr,
    fields: &Vec<String>,
    assign: &AssignStmt,
) -> Result<(), Error> {
    // The root must type as `&mut T` or `*mut T` — otherwise the deref isn't
    // assignable. (We don't allow whole-place assignment through `*const T`
    // or `&T`, matching Rust.)
    let root_infer = check_expr(ctx, root_expr)?;
    let resolved = ctx.subst.substitute(&root_infer);
    let inner_infer = match resolved {
        InferType::Ref { inner, mutable: true } => {
            ctx.deref_is_raw.push(false);
            *inner
        }
        InferType::RawPtr { inner, mutable: true } => {
            ctx.deref_is_raw.push(true);
            *inner
        }
        InferType::Ref { mutable: false, .. } => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "cannot assign through a shared reference".to_string(),
                span: assign.lhs.span.copy(),
            });
        }
        InferType::RawPtr { mutable: false, .. } => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "cannot assign through a `*const T` raw pointer".to_string(),
                span: assign.lhs.span.copy(),
            });
        }
        other => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "cannot dereference `{}` for assignment",
                    infer_to_string(&other)
                ),
                span: assign.lhs.span.copy(),
            });
        }
    };
    // Walk fields starting from the pointed-at type to find the LHS type.
    let mut current = inner_infer;
    let mut i = 0;
    while i < fields.len() {
        let (struct_path, type_args) = match &current {
            InferType::Struct { path, type_args } => (clone_path(path), infer_vec_clone(type_args)),
            _ => {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: "field assignment on non-struct value".to_string(),
                    span: assign.lhs.span.copy(),
                });
            }
        };
        let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
        let mut found = false;
        let mut k = 0;
        while k < entry.fields.len() {
            if entry.fields[k].name == fields[i] {
                let field_infer = rtype_to_infer(&entry.fields[k].ty);
                let env = build_infer_env(&entry.type_params, &type_args);
                current = infer_substitute(&field_infer, &env);
                found = true;
                break;
            }
            k += 1;
        }
        if !found {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "no field `{}` on `{}`",
                    fields[i],
                    place_to_string(&struct_path)
                ),
                span: assign.lhs.span.copy(),
            });
        }
        i += 1;
    }
    let rhs_ty = check_expr(ctx, &assign.rhs)?;
    ctx.subst
        .unify(&rhs_ty, &current, &assign.rhs.span, ctx.current_file)?;
    Ok(())
}

fn extract_place_for_assign(expr: &Expr) -> Option<Vec<String>> {
    let mut chain: Vec<String> = Vec::new();
    let mut current = expr;
    loop {
        match &current.kind {
            ExprKind::Var(name) => {
                chain.push(name.clone());
                let mut reversed: Vec<String> = Vec::new();
                let mut i = chain.len();
                while i > 0 {
                    i -= 1;
                    reversed.push(chain[i].clone());
                }
                return Some(reversed);
            }
            ExprKind::FieldAccess(fa) => {
                chain.push(fa.field.clone());
                current = &fa.base;
            }
            _ => return None,
        }
    }
}

fn walk_chain_type(
    start: &InferType,
    chain: &Vec<String>,
    structs: &StructTable,
    file: &str,
    span: &Span,
) -> Result<InferType, Error> {
    let mut current = infer_clone(start);
    let mut i = 1;
    while i < chain.len() {
        let (struct_path, type_args) = match &current {
            InferType::Struct { path, type_args } => (clone_path(path), infer_vec_clone(type_args)),
            InferType::Ref { inner, .. } => match inner.as_ref() {
                InferType::Struct { path, type_args } => {
                    (clone_path(path), infer_vec_clone(type_args))
                }
                _ => {
                    return Err(Error {
                        file: file.to_string(),
                        message: "field assignment on non-struct value".to_string(),
                        span: span.copy(),
                    });
                }
            },
            _ => {
                return Err(Error {
                    file: file.to_string(),
                    message: "field assignment on non-struct value".to_string(),
                    span: span.copy(),
                });
            }
        };
        let entry = struct_lookup(structs, &struct_path).expect("resolved struct");
        let mut found = false;
        let mut k = 0;
        while k < entry.fields.len() {
            if entry.fields[k].name == chain[i] {
                let field_infer = rtype_to_infer(&entry.fields[k].ty);
                let env = build_infer_env(&entry.type_params, &type_args);
                current = infer_substitute(&field_infer, &env);
                found = true;
                break;
            }
            k += 1;
        }
        if !found {
            return Err(Error {
                file: file.to_string(),
                message: format!(
                    "no field `{}` on `{}`",
                    chain[i],
                    place_to_string(&struct_path)
                ),
                span: span.copy(),
            });
        }
        i += 1;
    }
    Ok(current)
}

fn check_expr(ctx: &mut CheckCtx, expr: &Expr) -> Result<InferType, Error> {
    match &expr.kind {
        ExprKind::IntLit(n) => {
            let v = ctx.subst.fresh_int();
            ctx.lit_vars.push(v);
            ctx.lit_constraints.push(LitConstraint {
                var: v,
                value: *n,
                span: expr.span.copy(),
            });
            Ok(InferType::Var(v))
        }
        ExprKind::Var(name) => {
            let mut i = ctx.locals.len();
            while i > 0 {
                i -= 1;
                if ctx.locals[i].name == *name {
                    return Ok(infer_clone(&ctx.locals[i].ty));
                }
            }
            Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("unknown variable: `{}`", name),
                span: expr.span.copy(),
            })
        }
        ExprKind::Call(call) => check_call(ctx, call, expr),
        ExprKind::StructLit(lit) => check_struct_lit(ctx, lit, expr),
        ExprKind::FieldAccess(fa) => check_field_access(ctx, fa, expr),
        ExprKind::Borrow { inner, mutable } => {
            // Inside a Borrow's place chain, suppress the "non-Copy through ref"
            // rejection — borrowing a place doesn't move out of it.
            ctx.in_borrow += 1;
            let inner_ty = check_expr(ctx, inner)?;
            ctx.in_borrow -= 1;
            Ok(InferType::Ref {
                inner: Box::new(inner_ty),
                mutable: *mutable,
            })
        }
        ExprKind::Cast { inner, ty } => check_cast(ctx, inner, ty, expr),
        ExprKind::Deref(inner) => check_deref(ctx, inner, expr),
        ExprKind::Unsafe(block) => check_block_expr(ctx, block.as_ref()),
        ExprKind::Block(block) => check_block_expr(ctx, block.as_ref()),
        ExprKind::MethodCall(mc) => check_method_call(ctx, mc, expr),
    }
}

// `recv.method(args)` resolution. Type-check the receiver, peel one layer of
// ref to find the underlying struct, look up `[StructPath, method_name]` in
// FuncTable, derive a `ReceiverAdjust` from the recv type vs the method's
// receiver type, type-check remaining args, and record the resolution for
// borrowck/codegen consumption.
fn check_method_call(
    ctx: &mut CheckCtx,
    mc: &crate::ast::MethodCall,
    call_expr: &Expr,
) -> Result<InferType, Error> {
    let recv_ty = check_expr(ctx, &mc.receiver)?;
    let resolved_recv = ctx.subst.substitute(&recv_ty);
    // Determine the receiver's struct path AND its type args (so we can bind
    // them to the impl's type params when the method is generic).
    let (recv_kind, struct_path, recv_type_args): (RecvShape, Vec<String>, Vec<InferType>) =
        match resolved_recv {
            InferType::Struct { path, type_args } => (RecvShape::Owned, path, type_args),
            InferType::Ref { inner, mutable } => match *inner {
                InferType::Struct { path, type_args } => (
                    if mutable {
                        RecvShape::MutRef
                    } else {
                        RecvShape::SharedRef
                    },
                    path,
                    type_args,
                ),
                _ => {
                    return Err(Error {
                        file: ctx.current_file.to_string(),
                        message: "method calls require a struct receiver".to_string(),
                        span: mc.receiver.span.copy(),
                    });
                }
            },
            _ => {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: "method calls require a struct receiver".to_string(),
                    span: mc.receiver.span.copy(),
                });
            }
        };
    let mut method_path = clone_path(&struct_path);
    method_path.push(mc.method.clone());
    // Try entries (concrete methods) first, then templates (generic methods).
    let from_entry = func_lookup(ctx.funcs, &method_path).is_some();
    let from_template = template_lookup(ctx.funcs, &method_path).is_some();
    if !from_entry && !from_template {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "no method `{}` on `{}`",
                mc.method,
                place_to_string(&struct_path)
            ),
            span: mc.method_span.copy(),
        });
    }
    // Snapshot the method's signature data; for templates, also build a
    // type-arg env mapping the method's type_params to fresh inference vars
    // (with the impl's params bound to the receiver's type_args first).
    let (mp_param_types, mp_return_type, mp_type_params, mp_callee_idx, mp_ret_ref_source, mp_is_template, mp_template_idx) =
        if from_entry {
            let entry = func_lookup(ctx.funcs, &method_path).unwrap();
            (
                rtype_vec_clone(&entry.param_types),
                entry.return_type.as_ref().map(rtype_clone),
                Vec::new(),
                entry.idx,
                entry.ret_ref_source,
                false,
                0usize,
            )
        } else {
            let (idx, t) = template_lookup(ctx.funcs, &method_path).unwrap();
            (
                rtype_vec_clone(&t.param_types),
                t.return_type.as_ref().map(rtype_clone),
                t.type_params.clone(),
                0u32,
                t.ret_ref_source,
                true,
                idx,
            )
        };
    if mp_param_types.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "function `{}` is not a method (no `self` receiver)",
                place_to_string(&method_path)
            ),
            span: mc.method_span.copy(),
        });
    }
    // Build env: for templates, the leading type_params correspond to the
    // impl's params (from the receiver type_args), and any trailing entries
    // are the method's own type_params (fresh vars, may be unified by
    // turbofish).
    let mut env: Vec<(String, InferType)> = Vec::new();
    let mut method_type_var_ids: Vec<u32> = Vec::new();
    if mp_is_template {
        let mut i = 0;
        while i < mp_type_params.len() {
            if i < recv_type_args.len() {
                // Bind impl's param to the receiver's corresponding type arg.
                env.push((mp_type_params[i].clone(), infer_clone(&recv_type_args[i])));
                method_type_var_ids.push(0);
            } else {
                let v = ctx.subst.fresh_var();
                env.push((mp_type_params[i].clone(), InferType::Var(v)));
                method_type_var_ids.push(v);
            }
            i += 1;
        }
        // Apply method-call turbofish (`.foo::<T1, T2>(...)`) by unifying each
        // explicit arg with the corresponding fresh var (in the trailing
        // slots, after impl-bound ones).
        if !mc.turbofish_args.is_empty() {
            let method_own_count = mp_type_params.len() - recv_type_args.len();
            if mc.turbofish_args.len() != method_own_count {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "wrong number of type arguments to method `{}`: expected {}, got {}",
                        mc.method,
                        method_own_count,
                        mc.turbofish_args.len()
                    ),
                    span: mc.method_span.copy(),
                });
            }
            let mut k = 0;
            while k < mc.turbofish_args.len() {
                let user_rt = resolve_type(
                    &mc.turbofish_args[k],
                    ctx.current_module,
                    ctx.structs,
                    ctx.self_target,
                    ctx.type_params,
                    ctx.current_file,
                )?;
                let user_infer = rtype_to_infer(&user_rt);
                let var_id = method_type_var_ids[recv_type_args.len() + k];
                ctx.subst.unify(
                    &InferType::Var(var_id),
                    &user_infer,
                    &mc.turbofish_args[k].span,
                    ctx.current_file,
                )?;
                k += 1;
            }
        }
    } else if !mc.turbofish_args.is_empty() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!("method `{}` is not generic", mc.method),
            span: mc.method_span.copy(),
        });
    }
    let recv_param = &mp_param_types[0];
    let recv_adjust = derive_recv_adjust(&recv_kind, recv_param, ctx, &mc.receiver, &mc.method_span)?;
    let expected_arg_count = mp_param_types.len() - 1;
    if mc.args.len() != expected_arg_count {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of arguments to `{}`: expected {}, got {}",
                mc.method,
                expected_arg_count,
                mc.args.len()
            ),
            span: call_expr.span.copy(),
        });
    }
    let callee_idx = mp_callee_idx;
    let callee_ret_source = mp_ret_ref_source;
    let mut method_param_infer: Vec<InferType> = Vec::new();
    let mut k = 0;
    while k < mp_param_types.len() {
        let raw = rtype_to_infer(&mp_param_types[k]);
        let subst = if mp_is_template {
            infer_substitute(&raw, &env)
        } else {
            raw
        };
        method_param_infer.push(subst);
        k += 1;
    }
    let return_infer: Option<InferType> = match &mp_return_type {
        Some(rt) => {
            let raw = rtype_to_infer(rt);
            Some(if mp_is_template {
                infer_substitute(&raw, &env)
            } else {
                raw
            })
        }
        None => None,
    };
    // Build the type-arg env (only meaningful for templates).
    let type_arg_infers: Vec<InferType> = if mp_is_template {
        let mut v: Vec<InferType> = Vec::new();
        let mut i = 0;
        while i < mp_type_params.len() {
            v.push(infer_clone(&env[i].1));
            i += 1;
        }
        v
    } else {
        Vec::new()
    };
    // Reserve the resolution slot in source-DFS order.
    let resolution_idx = ctx.method_resolutions.len();
    let template_idx_opt = if mp_is_template { Some(mp_template_idx) } else { None };
    ctx.method_resolutions.push(PendingMethodCall {
        callee_idx,
        callee_path: clone_path(&method_path),
        recv_adjust,
        ret_borrows_receiver: false,
        template_idx: template_idx_opt,
        type_arg_infers,
    });
    // Type-check remaining args against method's params[1..].
    let mut i = 0;
    while i < mc.args.len() {
        let arg_ty = check_expr(ctx, &mc.args[i])?;
        ctx.subst.unify(
            &arg_ty,
            &method_param_infer[i + 1],
            &mc.args[i].span,
            ctx.current_file,
        )?;
        i += 1;
    }
    // Record whether this call's result borrow should be attributed to the
    // receiver place (for borrowck propagation through ref-returning methods).
    let ret_borrows_recv = matches!(callee_ret_source, Some(0))
        && matches!(
            ctx.method_resolutions[resolution_idx].recv_adjust,
            ReceiverAdjust::BorrowImm
                | ReceiverAdjust::BorrowMut
                | ReceiverAdjust::ByRef
        );
    ctx.method_resolutions[resolution_idx].ret_borrows_receiver = ret_borrows_recv;
    match return_infer {
        Some(rt) => Ok(rt),
        None => Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "method `{}` returns unit and can't be used as a value",
                mc.method
            ),
            span: call_expr.span.copy(),
        }),
    }
}

enum RecvShape {
    Owned,
    SharedRef,
    MutRef,
}

fn derive_recv_adjust(
    recv_kind: &RecvShape,
    recv_param: &RType,
    ctx: &CheckCtx,
    recv_expr: &Expr,
    method_span: &Span,
) -> Result<ReceiverAdjust, Error> {
    match recv_param {
        RType::Struct { .. } => {
            // Method takes `Self` (owned).
            match recv_kind {
                RecvShape::Owned => Ok(ReceiverAdjust::Move),
                _ => Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "cannot move out of borrow to call `{}` (which takes `self` by value)",
                        token_method_name(recv_expr)
                    ),
                    span: method_span.copy(),
                }),
            }
        }
        RType::Ref {
            mutable: param_mut,
            ..
        } => {
            // Method takes `&Self` or `&mut Self`.
            match (recv_kind, param_mut) {
                (RecvShape::Owned, false) => Ok(ReceiverAdjust::BorrowImm),
                (RecvShape::Owned, true) => {
                    if !is_mutable_place(ctx, recv_expr) {
                        return Err(Error {
                            file: ctx.current_file.to_string(),
                            message:
                                "cannot call `&mut self` method on an immutable receiver"
                                    .to_string(),
                            span: method_span.copy(),
                        });
                    }
                    Ok(ReceiverAdjust::BorrowMut)
                }
                (RecvShape::SharedRef, false) => Ok(ReceiverAdjust::ByRef),
                (RecvShape::SharedRef, true) => Err(Error {
                    file: ctx.current_file.to_string(),
                    message:
                        "cannot call `&mut self` method through a shared reference"
                            .to_string(),
                    span: method_span.copy(),
                }),
                (RecvShape::MutRef, false) => Ok(ReceiverAdjust::ByRef),
                (RecvShape::MutRef, true) => Ok(ReceiverAdjust::ByRef),
            }
        }
        _ => unreachable!("method receiver must be Self or &Self/&mut Self"),
    }
}

fn token_method_name(_recv: &Expr) -> &'static str {
    // Placeholder: we only use this in an error message that's about the
    // receiver, not the method itself.
    "this method"
}

// A receiver is a "mutable place" if it's a Var bound to a `mut` local, or a
// FieldAccess chain rooted at one — same rule as for `&mut place` borrows.
fn is_mutable_place(ctx: &CheckCtx, expr: &Expr) -> bool {
    let chain = match extract_place_for_assign(expr) {
        Some(c) => c,
        None => return false,
    };
    let mut i = ctx.locals.len();
    while i > 0 {
        i -= 1;
        if ctx.locals[i].name == chain[0] {
            // Owned `mut` binding, or a `&mut T` binding.
            if ctx.locals[i].mutable {
                return true;
            }
            let resolved = ctx.subst.substitute(&ctx.locals[i].ty);
            return matches!(resolved, InferType::Ref { mutable: true, .. });
        }
    }
    false
}

// `*p` reads the pointed-to value. Result type = inner of the ref/raw-ptr.
// We don't enforce Copy here — that check kicks in only when the deref is
// USED as a value (the caller can decide). `(*p).field` access still applies
// the Copy rule via `check_field_access`'s "through reference" branch.
fn check_deref(ctx: &mut CheckCtx, inner: &Expr, deref_expr: &Expr) -> Result<InferType, Error> {
    let inner_ty = check_expr(ctx, inner)?;
    let resolved = ctx.subst.substitute(&inner_ty);
    match resolved {
        InferType::Ref { inner, .. } => {
            ctx.deref_is_raw.push(false);
            Ok(*inner)
        }
        InferType::RawPtr { inner, .. } => {
            ctx.deref_is_raw.push(true);
            Ok(*inner)
        }
        other => Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "cannot dereference `{}` — only references and raw pointers can be dereferenced",
                infer_to_string(&other)
            ),
            span: deref_expr.span.copy(),
        }),
    }
}

// Casts in our subset: any of {integer, &T, &mut T, *const T, *mut T} can be
// cast to any of {*const T, *mut T}. Integer-class type vars get pinned to
// usize at the cast site (matches Rust's "integers cast to ptr-sized") so the
// underlying ABI is i32. Everything else is a type-level reinterpret.
fn check_cast(
    ctx: &mut CheckCtx,
    inner: &Expr,
    ty: &Type,
    cast_expr: &Expr,
) -> Result<InferType, Error> {
    let target = resolve_type(
        ty,
        ctx.current_module,
        ctx.structs,
        ctx.self_target,
        ctx.type_params,
        ctx.current_file,
    )?;
    if !is_raw_ptr(&target) {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "casts are only allowed to raw pointer types, got `{}`",
                rtype_to_string(&target)
            ),
            span: ty.span.copy(),
        });
    }
    let src_ty = check_expr(ctx, inner)?;
    let resolved_src = ctx.subst.substitute(&src_ty);
    let ok = matches!(
        &resolved_src,
        InferType::Ref { .. } | InferType::RawPtr { .. } | InferType::Int(_) | InferType::Var(_)
    );
    if !ok {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "cannot cast `{}` to a raw pointer",
                infer_to_string(&resolved_src)
            ),
            span: cast_expr.span.copy(),
        });
    }
    if let InferType::Var(v) = &resolved_src {
        // Pin an unresolved integer literal to usize so the runtime ABI is i32.
        if ctx.subst.is_integer[*v as usize] {
            ctx.subst
                .unify(
                    &InferType::Var(*v),
                    &InferType::Int(IntKind::Usize),
                    &cast_expr.span,
                    ctx.current_file,
                )?;
        }
    }
    Ok(rtype_to_infer(&target))
}

fn check_call(ctx: &mut CheckCtx, call: &Call, call_expr: &Expr) -> Result<InferType, Error> {
    let full = resolve_full_path(ctx.current_module, ctx.self_target, &call.callee.segments);
    // Generic args attach to the last segment of the callee path.
    let last_seg_args = if call.callee.segments.is_empty() {
        Vec::new()
    } else {
        let last = &call.callee.segments[call.callee.segments.len() - 1];
        let mut v: Vec<crate::ast::Type> = Vec::new();
        let mut i = 0;
        while i < last.args.len() {
            v.push(last.args[i].clone());
            i += 1;
        }
        v
    };
    // Try non-generic first.
    if let Some(entry_idx) = funcs_entry_index(ctx.funcs, &full) {
        let entry = &ctx.funcs.entries[entry_idx];
        if !last_seg_args.is_empty() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "`{}` is not a generic function — turbofish is not allowed",
                    segments_to_string(&call.callee.segments)
                ),
                span: call_expr.span.copy(),
            });
        }
        if call.args.len() != entry.param_types.len() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "wrong number of arguments to `{}`: expected {}, got {}",
                    segments_to_string(&call.callee.segments),
                    entry.param_types.len(),
                    call.args.len()
                ),
                span: call_expr.span.copy(),
            });
        }
        let mut param_infer: Vec<InferType> = Vec::new();
        let mut k = 0;
        while k < entry.param_types.len() {
            param_infer.push(rtype_to_infer(&entry.param_types[k]));
            k += 1;
        }
        let return_infer: Option<InferType> = match &entry.return_type {
            Some(rt) => Some(rtype_to_infer(rt)),
            None => None,
        };
        ctx.call_resolutions.push(PendingCall::Direct(entry_idx));
        let mut i = 0;
        while i < call.args.len() {
            let arg_ty = check_expr(ctx, &call.args[i])?;
            ctx.subst.unify(
                &arg_ty,
                &param_infer[i],
                &call.args[i].span,
                ctx.current_file,
            )?;
            i += 1;
        }
        return match return_infer {
            Some(rt) => Ok(rt),
            None => Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "function `{}` returns unit and can't be used as a value",
                    segments_to_string(&call.callee.segments)
                ),
                span: call_expr.span.copy(),
            }),
        };
    }
    // Try a generic template.
    if let Some((template_idx, _)) = template_lookup(ctx.funcs, &full) {
        // Snapshot the template's data we need (clone vectors so we don't keep
        // a borrow into ctx.funcs across the upcoming ctx.subst mutations).
        let tmpl_type_params: Vec<String> = ctx.funcs.templates[template_idx].type_params.clone();
        let tmpl_param_types: Vec<RType> = {
            let mut v: Vec<RType> = Vec::new();
            let mut k = 0;
            while k < ctx.funcs.templates[template_idx].param_types.len() {
                v.push(rtype_clone(&ctx.funcs.templates[template_idx].param_types[k]));
                k += 1;
            }
            v
        };
        let tmpl_return_type: Option<RType> = ctx.funcs.templates[template_idx]
            .return_type
            .as_ref()
            .map(rtype_clone);
        if !last_seg_args.is_empty() && last_seg_args.len() != tmpl_type_params.len() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "wrong number of type arguments to `{}`: expected {}, got {}",
                    segments_to_string(&call.callee.segments),
                    tmpl_type_params.len(),
                    last_seg_args.len()
                ),
                span: call_expr.span.copy(),
            });
        }
        if call.args.len() != tmpl_param_types.len() {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "wrong number of arguments to `{}`: expected {}, got {}",
                    segments_to_string(&call.callee.segments),
                    tmpl_param_types.len(),
                    call.args.len()
                ),
                span: call_expr.span.copy(),
            });
        }
        let mut env: Vec<(String, InferType)> = Vec::new();
        let mut var_ids: Vec<u32> = Vec::new();
        let mut k = 0;
        while k < tmpl_type_params.len() {
            let v = ctx.subst.fresh_var();
            var_ids.push(v);
            env.push((tmpl_type_params[k].clone(), InferType::Var(v)));
            k += 1;
        }
        // Apply explicit turbofish args by unifying.
        let mut k = 0;
        while k < last_seg_args.len() {
            let user_rt = resolve_type(
                &last_seg_args[k],
                ctx.current_module,
                ctx.structs,
                ctx.self_target,
                ctx.type_params,
                ctx.current_file,
            )?;
            let user_infer = rtype_to_infer(&user_rt);
            ctx.subst.unify(
                &InferType::Var(var_ids[k]),
                &user_infer,
                &last_seg_args[k].span,
                ctx.current_file,
            )?;
            k += 1;
        }
        let mut param_infer: Vec<InferType> = Vec::new();
        let mut k = 0;
        while k < tmpl_param_types.len() {
            param_infer.push(infer_substitute(&rtype_to_infer(&tmpl_param_types[k]), &env));
            k += 1;
        }
        let return_infer: Option<InferType> = tmpl_return_type
            .as_ref()
            .map(|rt| infer_substitute(&rtype_to_infer(rt), &env));
        ctx.call_resolutions.push(PendingCall::Generic {
            template_idx,
            type_var_ids: var_ids,
        });
        let mut i = 0;
        while i < call.args.len() {
            let arg_ty = check_expr(ctx, &call.args[i])?;
            ctx.subst.unify(
                &arg_ty,
                &param_infer[i],
                &call.args[i].span,
                ctx.current_file,
            )?;
            i += 1;
        }
        return match return_infer {
            Some(rt) => Ok(rt),
            None => Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "function `{}` returns unit and can't be used as a value",
                    segments_to_string(&call.callee.segments)
                ),
                span: call_expr.span.copy(),
            }),
        };
    }
    Err(Error {
        file: ctx.current_file.to_string(),
        message: format!(
            "unresolved function: {}",
            segments_to_string(&call.callee.segments)
        ),
        span: call.callee.span.copy(),
    })
}

fn funcs_entry_index(funcs: &FuncTable, path: &Vec<String>) -> Option<usize> {
    let mut i = 0;
    while i < funcs.entries.len() {
        if path_eq(&funcs.entries[i].path, path) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn check_struct_lit(
    ctx: &mut CheckCtx,
    lit: &StructLit,
    lit_expr: &Expr,
) -> Result<InferType, Error> {
    let full = resolve_full_path(ctx.current_module, ctx.self_target, &lit.path.segments);

    let entry = match struct_lookup(ctx.structs, &full) {
        Some(e) => e,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "unknown struct: {}",
                    segments_to_string(&lit.path.segments)
                ),
                span: lit.path.span.copy(),
            });
        }
    };
    let struct_type_params: Vec<String> = entry.type_params.clone();
    let mut def_field_names: Vec<String> = Vec::new();
    let mut def_field_types: Vec<RType> = Vec::new();
    let mut k = 0;
    while k < entry.fields.len() {
        def_field_names.push(entry.fields[k].name.clone());
        def_field_types.push(rtype_clone(&entry.fields[k].ty));
        k += 1;
    }
    // Allocate fresh type-arg vars for this struct's params. If the path's
    // last segment carried turbofish args, unify them.
    let last_seg = &lit.path.segments[lit.path.segments.len() - 1];
    if !last_seg.args.is_empty() && last_seg.args.len() != struct_type_params.len() {
        return Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "wrong number of type arguments for `{}`: expected {}, got {}",
                place_to_string(&full),
                struct_type_params.len(),
                last_seg.args.len()
            ),
            span: lit.path.span.copy(),
        });
    }
    let mut env: Vec<(String, InferType)> = Vec::new();
    let mut type_arg_infers: Vec<InferType> = Vec::new();
    let mut i = 0;
    while i < struct_type_params.len() {
        let v = ctx.subst.fresh_var();
        type_arg_infers.push(InferType::Var(v));
        env.push((struct_type_params[i].clone(), InferType::Var(v)));
        i += 1;
    }
    let mut k = 0;
    while k < last_seg.args.len() {
        let user_rt = resolve_type(
            &last_seg.args[k],
            ctx.current_module,
            ctx.structs,
            ctx.self_target,
            ctx.type_params,
            ctx.current_file,
        )?;
        let user_infer = rtype_to_infer(&user_rt);
        ctx.subst.unify(
            &type_arg_infers[k],
            &user_infer,
            &last_seg.args[k].span,
            ctx.current_file,
        )?;
        k += 1;
    }
    // Record this struct lit's type in source-DFS visit-first order: push
    // BEFORE checking field initializers so nested struct lits get later
    // indices, matching codegen's outer-first traversal.
    let result = InferType::Struct {
        path: clone_path(&full),
        type_args: infer_vec_clone(&type_arg_infers),
    };
    let struct_lit_idx = ctx.struct_lit_vars.len();
    ctx.struct_lit_vars.push(infer_clone(&result));
    let _ = struct_lit_idx;

    // Validate field shape.
    let mut i = 0;
    while i < lit.fields.len() {
        let mut found = false;
        let mut j = 0;
        while j < def_field_names.len() {
            if lit.fields[i].name == def_field_names[j] {
                found = true;
                break;
            }
            j += 1;
        }
        if !found {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "struct `{}` has no field `{}`",
                    segments_to_string(&lit.path.segments),
                    lit.fields[i].name
                ),
                span: lit.fields[i].name_span.copy(),
            });
        }
        let mut k = i + 1;
        while k < lit.fields.len() {
            if lit.fields[k].name == lit.fields[i].name {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!("field `{}` is initialized twice", lit.fields[i].name),
                    span: lit.fields[k].name_span.copy(),
                });
            }
            k += 1;
        }
        i += 1;
    }
    let mut i = 0;
    while i < def_field_names.len() {
        let mut present = false;
        let mut j = 0;
        while j < lit.fields.len() {
            if lit.fields[j].name == def_field_names[i] {
                present = true;
                break;
            }
            j += 1;
        }
        if !present {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!("missing field `{}`", def_field_names[i]),
                span: lit_expr.span.copy(),
            });
        }
        i += 1;
    }

    // Type-check inits in source order. Each declared field type is
    // substituted via the struct's type-arg env so Param("T") in field types
    // unifies with whatever the type-arg var resolves to.
    let mut i = 0;
    while i < lit.fields.len() {
        let init = &lit.fields[i];
        let init_ty = check_expr(ctx, &init.value)?;
        let mut k = 0;
        while k < def_field_names.len() {
            if def_field_names[k] == init.name {
                let expected_raw = rtype_to_infer(&def_field_types[k]);
                let expected = infer_substitute(&expected_raw, &env);
                ctx.subst
                    .unify(&init_ty, &expected, &init.value.span, ctx.current_file)?;
                break;
            }
            k += 1;
        }
        i += 1;
    }

    Ok(InferType::Struct {
        path: full,
        type_args: type_arg_infers,
    })
}

fn check_field_access(
    ctx: &mut CheckCtx,
    fa: &FieldAccess,
    _fa_expr: &Expr,
) -> Result<InferType, Error> {
    // Field access through a deref expression — `(*p).field` — applies the
    // same "Copy fields only" rule as auto-deref `r.field` does. Detect it
    // syntactically before walking the base.
    let through_explicit_deref = matches!(&fa.base.kind, ExprKind::Deref(_));
    let base_ty = check_expr(ctx, &fa.base)?;
    let resolved = ctx.subst.substitute(&base_ty);
    let (struct_path, struct_type_args, through_ref) = match resolved {
        InferType::Struct { path, type_args } => (path, type_args, through_explicit_deref),
        InferType::Ref { inner, .. } => match *inner {
            InferType::Struct { path, type_args } => (path, type_args, true),
            _ => {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: "field access on non-struct value".to_string(),
                    span: fa.base.span.copy(),
                });
            }
        },
        _ => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: "field access on non-struct value".to_string(),
                span: fa.base.span.copy(),
            });
        }
    };
    let entry = struct_lookup(ctx.structs, &struct_path).expect("resolved struct");
    let mut i = 0;
    while i < entry.fields.len() {
        if entry.fields[i].name == fa.field {
            // Substitute the field's declared type with the struct's type args
            // (e.g., `pair.first` where pair: Pair<u32, u64> and field declared
            // as T → resolves to u32).
            let env = build_infer_env(&entry.type_params, &struct_type_args);
            let field_ty_raw = rtype_clone(&entry.fields[i].ty);
            let field_infer_raw = rtype_to_infer(&field_ty_raw);
            let field_infer = infer_substitute(&field_infer_raw, &env);
            // Copy check: a non-Copy field accessed through a ref is a move
            // out of borrow. Suppress when we're inside a `&...` (place
            // borrow doesn't move).
            if through_ref && ctx.in_borrow == 0 && !is_copy(&field_ty_raw) {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "cannot move out of borrow: field `{}` of `{}` has non-Copy type `{}`",
                        fa.field,
                        place_to_string(&struct_path),
                        rtype_to_string(&field_ty_raw)
                    ),
                    span: fa.field_span.copy(),
                });
            }
            return Ok(field_infer);
        }
        i += 1;
    }
    Err(Error {
        file: ctx.current_file.to_string(),
        message: format!(
            "no field `{}` on `{}`",
            fa.field,
            place_to_string(&struct_path)
        ),
        span: fa.field_span.copy(),
    })
}
