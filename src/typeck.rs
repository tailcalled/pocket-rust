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
    Struct(Vec<String>),
    Ref { inner: Box<RType>, mutable: bool },
    RawPtr { inner: Box<RType>, mutable: bool },
}

pub fn rtype_clone(t: &RType) -> RType {
    match t {
        RType::Int(k) => RType::Int(int_kind_copy(k)),
        RType::Struct(p) => RType::Struct(clone_path(p)),
        RType::Ref { inner, mutable } => RType::Ref {
            inner: Box::new(rtype_clone(inner)),
            mutable: *mutable,
        },
        RType::RawPtr { inner, mutable } => RType::RawPtr {
            inner: Box::new(rtype_clone(inner)),
            mutable: *mutable,
        },
    }
}

pub fn rtype_eq(a: &RType, b: &RType) -> bool {
    match (a, b) {
        (RType::Int(ka), RType::Int(kb)) => int_kind_eq(ka, kb),
        (RType::Struct(pa), RType::Struct(pb)) => path_eq(pa, pb),
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
        _ => false,
    }
}

pub fn rtype_to_string(t: &RType) -> String {
    match t {
        RType::Int(k) => int_kind_name(k).to_string(),
        RType::Struct(p) => place_to_string(p),
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
    }
}

pub fn rtype_size(ty: &RType, structs: &StructTable) -> u32 {
    match ty {
        RType::Int(k) => match k {
            IntKind::U128 | IntKind::I128 => 2,
            _ => 1,
        },
        RType::Struct(p) => {
            let entry = struct_lookup(structs, p).expect("resolved struct");
            let mut s: u32 = 0;
            let mut i = 0;
            while i < entry.fields.len() {
                s += rtype_size(&entry.fields[i].ty, structs);
                i += 1;
            }
            s
        }
        RType::Ref { .. } | RType::RawPtr { .. } => 1,
    }
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
        RType::Struct(p) => {
            let entry = struct_lookup(structs, p).expect("resolved struct");
            let mut i = 0;
            while i < entry.fields.len() {
                flatten_rtype(&entry.fields[i].ty, structs, out);
                i += 1;
            }
        }
        RType::Ref { .. } | RType::RawPtr { .. } => out.push(crate::wasm::ValType::I32),
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
        RType::Struct(p) => {
            let entry = struct_lookup(structs, p).expect("resolved struct");
            let mut total: u32 = 0;
            let mut i = 0;
            while i < entry.fields.len() {
                total += byte_size_of(&entry.fields[i].ty, structs);
                i += 1;
            }
            total
        }
    }
}

pub fn is_copy(t: &RType) -> bool {
    match t {
        RType::Int(_) => true,
        RType::Struct(_) => false,
        RType::Ref { .. } => true,
        RType::RawPtr { .. } => true,
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
    // For each `Deref` expression in the body, in source-DFS order: `true`
    // iff the operand resolved to a raw pointer (`*const T` / `*mut T`).
    // Safeck reads this in lockstep to flag derefs outside `unsafe` blocks.
    pub deref_is_raw: Vec<bool>,
}

pub struct FuncTable {
    pub entries: Vec<FnSymbol>,
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
    file: &str,
) -> Result<RType, Error> {
    match &ty.kind {
        TypeKind::Path(path) => {
            if path.segments.len() == 1 {
                if let Some(k) = int_kind_from_name(&path.segments[0].name) {
                    return Ok(RType::Int(k));
                }
            }
            let mut full = clone_path(current_module);
            let mut i = 0;
            while i < path.segments.len() {
                full.push(path.segments[i].name.clone());
                i += 1;
            }
            if struct_lookup(structs, &full).is_some() {
                Ok(RType::Struct(full))
            } else {
                Err(Error {
                    file: file.to_string(),
                    message: format!("unknown type: {}", segments_to_string(&path.segments)),
                    span: path.span.copy(),
                })
            }
        }
        TypeKind::Ref { inner, mutable } => {
            let r = resolve_type(inner, current_module, structs, file)?;
            Ok(RType::Ref {
                inner: Box::new(r),
                mutable: *mutable,
            })
        }
        TypeKind::RawPtr { inner, mutable } => {
            let r = resolve_type(inner, current_module, structs, file)?;
            Ok(RType::RawPtr {
                inner: Box::new(r),
                mutable: *mutable,
            })
        }
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
                table.entries.push(StructEntry {
                    path: full,
                    name_span: sd.name_span.copy(),
                    file: module.source_file.clone(),
                    fields: Vec::new(),
                });
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_struct_names(m, path, table);
                path.pop();
            }
            Item::Function(_) => {}
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
                let mut resolved: Vec<RTypedField> = Vec::new();
                let mut k = 0;
                while k < sd.fields.len() {
                    let rt = resolve_type(&sd.fields[k].ty, path, table, &module.source_file)?;
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
                let mut full = clone_path(path);
                full.push(f.name.clone());
                let mut param_types: Vec<RType> = Vec::new();
                let mut k = 0;
                while k < f.params.len() {
                    let rt = resolve_type(&f.params[k].ty, path, structs, &module.source_file)?;
                    param_types.push(rt);
                    k += 1;
                }
                let return_type = match &f.return_type {
                    Some(ty) => {
                        let rt = resolve_type(ty, path, structs, &module.source_file)?;
                        if let RType::Ref { .. } = &rt {
                            return Err(Error {
                                file: module.source_file.clone(),
                                message: "functions cannot return reference types".to_string(),
                                span: ty.span.copy(),
                            });
                        }
                        Some(rt)
                    }
                    None => None,
                };
                funcs.entries.push(FnSymbol {
                    path: full,
                    idx: *next_idx,
                    param_types,
                    return_type,
                    let_types: Vec::new(),
                    lit_types: Vec::new(),
                    deref_is_raw: Vec::new(),
                });
                *next_idx += 1;
            }
            Item::Module(m) => {
                path.push(m.name.clone());
                collect_funcs(m, path, funcs, next_idx, structs)?;
                path.pop();
            }
            Item::Struct(_) => {}
        }
        i += 1;
    }
    Ok(())
}

// ----- InferType -----

enum InferType {
    Var(u32),
    Int(IntKind),
    Struct(Vec<String>),
    Ref { inner: Box<InferType>, mutable: bool },
    RawPtr { inner: Box<InferType>, mutable: bool },
}

fn infer_clone(t: &InferType) -> InferType {
    match t {
        InferType::Var(v) => InferType::Var(*v),
        InferType::Int(k) => InferType::Int(int_kind_copy(k)),
        InferType::Struct(p) => InferType::Struct(clone_path(p)),
        InferType::Ref { inner, mutable } => InferType::Ref {
            inner: Box::new(infer_clone(inner)),
            mutable: *mutable,
        },
        InferType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(infer_clone(inner)),
            mutable: *mutable,
        },
    }
}

fn rtype_to_infer(rt: &RType) -> InferType {
    match rt {
        RType::Int(k) => InferType::Int(int_kind_copy(k)),
        RType::Struct(p) => InferType::Struct(clone_path(p)),
        RType::Ref { inner, mutable } => InferType::Ref {
            inner: Box::new(rtype_to_infer(inner)),
            mutable: *mutable,
        },
        RType::RawPtr { inner, mutable } => InferType::RawPtr {
            inner: Box::new(rtype_to_infer(inner)),
            mutable: *mutable,
        },
    }
}

fn infer_to_string(t: &InferType) -> String {
    match t {
        InferType::Var(v) => format!("?{}", v),
        InferType::Int(k) => int_kind_name(k).to_string(),
        InferType::Struct(p) => place_to_string(p),
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

    fn substitute(&self, ty: &InferType) -> InferType {
        match ty {
            InferType::Var(v) => match &self.bindings[*v as usize] {
                Some(t) => self.substitute(t),
                None => InferType::Var(*v),
            },
            InferType::Int(k) => InferType::Int(int_kind_copy(k)),
            InferType::Struct(p) => InferType::Struct(clone_path(p)),
            InferType::Ref { inner, mutable } => InferType::Ref {
                inner: Box::new(self.substitute(inner)),
                mutable: *mutable,
            },
            InferType::RawPtr { inner, mutable } => InferType::RawPtr {
                inner: Box::new(self.substitute(inner)),
                mutable: *mutable,
            },
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
            (InferType::Struct(pa), InferType::Struct(pb)) => {
                if path_eq(&pa, &pb) {
                    Ok(())
                } else {
                    Err(Error {
                        file: file.to_string(),
                        message: format!(
                            "type mismatch: expected `{}`, got `{}`",
                            place_to_string(&pb),
                            place_to_string(&pa)
                        ),
                        span: span.copy(),
                    })
                }
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
            InferType::Struct(p) => RType::Struct(p),
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
    deref_is_raw: Vec<bool>,
    subst: Subst,
    current_module: &'a Vec<String>,
    current_file: &'a str,
    structs: &'a StructTable,
    funcs: &'a FuncTable,
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
            Item::Function(f) => check_function(f, path, current_file, structs, funcs)?,
            Item::Module(m) => {
                path.push(m.name.clone());
                check_module(m, path, current_file, structs, funcs)?;
                path.pop();
            }
            Item::Struct(_) => {}
        }
        i += 1;
    }
    *current_file = saved;
    Ok(())
}

fn check_function(
    func: &Function,
    current_module: &Vec<String>,
    current_file: &str,
    structs: &StructTable,
    funcs: &mut FuncTable,
) -> Result<(), Error> {
    // Build initial locals from params (params are immutable bindings in our subset).
    let mut locals: Vec<LocalEntry> = Vec::new();
    let mut k = 0;
    while k < func.params.len() {
        let rt = resolve_type(&func.params[k].ty, current_module, structs, current_file)?;
        locals.push(LocalEntry {
            name: func.params[k].name.clone(),
            ty: rtype_to_infer(&rt),
            mutable: false,
        });
        k += 1;
    }
    let return_rt: Option<RType> = match &func.return_type {
        Some(ty) => Some(resolve_type(ty, current_module, structs, current_file)?),
        None => None,
    };

    let (let_vars, lit_vars, lit_constraints, deref_is_raw, subst) = {
        let mut ctx = CheckCtx {
            locals,
            let_vars: Vec::new(),
            lit_vars: Vec::new(),
            lit_constraints: Vec::new(),
            deref_is_raw: Vec::new(),
            subst: Subst {
                bindings: Vec::new(),
                is_integer: Vec::new(),
            },
            current_module,
            current_file,
            structs,
            funcs: &*funcs,
        };
        check_block(&mut ctx, &func.body, &return_rt)?;
        (
            ctx.let_vars,
            ctx.lit_vars,
            ctx.lit_constraints,
            ctx.deref_is_raw,
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

    // Store on the FnSymbol.
    let mut full = clone_path(current_module);
    full.push(func.name.clone());
    let mut e = 0;
    while e < funcs.entries.len() {
        if path_eq(&funcs.entries[e].path, &full) {
            funcs.entries[e].let_types = let_types;
            funcs.entries[e].lit_types = lit_types;
            funcs.entries[e].deref_is_raw = deref_is_raw;
            break;
        }
        e += 1;
    }
    Ok(())
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
        let struct_path = match &current {
            InferType::Struct(p) => clone_path(p),
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
                current = rtype_to_infer(&entry.fields[k].ty);
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
        let struct_path = match &current {
            InferType::Struct(p) => clone_path(p),
            InferType::Ref { inner, .. } => match inner.as_ref() {
                InferType::Struct(p) => clone_path(p),
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
                current = rtype_to_infer(&entry.fields[k].ty);
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
            let inner_ty = check_expr(ctx, inner)?;
            Ok(InferType::Ref {
                inner: Box::new(inner_ty),
                mutable: *mutable,
            })
        }
        ExprKind::Cast { inner, ty } => check_cast(ctx, inner, ty, expr),
        ExprKind::Deref(inner) => check_deref(ctx, inner, expr),
        ExprKind::Unsafe(block) => check_block_expr(ctx, block.as_ref()),
        ExprKind::Block(block) => check_block_expr(ctx, block.as_ref()),
    }
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
    let target = resolve_type(ty, ctx.current_module, ctx.structs, ctx.current_file)?;
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
    let mut full = clone_path(ctx.current_module);
    let mut i = 0;
    while i < call.callee.segments.len() {
        full.push(call.callee.segments[i].name.clone());
        i += 1;
    }
    let entry = match func_lookup(ctx.funcs, &full) {
        Some(e) => e,
        None => {
            return Err(Error {
                file: ctx.current_file.to_string(),
                message: format!(
                    "unresolved function: {}",
                    segments_to_string(&call.callee.segments)
                ),
                span: call.callee.span.copy(),
            });
        }
    };
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

    match return_infer {
        Some(rt) => Ok(rt),
        None => Err(Error {
            file: ctx.current_file.to_string(),
            message: format!(
                "function `{}` returns unit and can't be used as a value",
                segments_to_string(&call.callee.segments)
            ),
            span: call_expr.span.copy(),
        }),
    }
}

fn check_struct_lit(
    ctx: &mut CheckCtx,
    lit: &StructLit,
    lit_expr: &Expr,
) -> Result<InferType, Error> {
    let mut full = clone_path(ctx.current_module);
    let mut i = 0;
    while i < lit.path.segments.len() {
        full.push(lit.path.segments[i].name.clone());
        i += 1;
    }

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
    let mut def_field_names: Vec<String> = Vec::new();
    let mut def_field_types: Vec<RType> = Vec::new();
    let mut k = 0;
    while k < entry.fields.len() {
        def_field_names.push(entry.fields[k].name.clone());
        def_field_types.push(rtype_clone(&entry.fields[k].ty));
        k += 1;
    }

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

    // Type-check inits in source order.
    let mut i = 0;
    while i < lit.fields.len() {
        let init = &lit.fields[i];
        let init_ty = check_expr(ctx, &init.value)?;
        let mut k = 0;
        while k < def_field_names.len() {
            if def_field_names[k] == init.name {
                let expected = rtype_to_infer(&def_field_types[k]);
                ctx.subst
                    .unify(&init_ty, &expected, &init.value.span, ctx.current_file)?;
                break;
            }
            k += 1;
        }
        i += 1;
    }

    Ok(InferType::Struct(full))
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
    let (struct_path, through_ref) = match resolved {
        InferType::Struct(p) => (p, through_explicit_deref),
        InferType::Ref { inner, .. } => match *inner {
            InferType::Struct(p) => (p, true),
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
            let field_ty = rtype_clone(&entry.fields[i].ty);
            if through_ref && !is_copy(&field_ty) {
                return Err(Error {
                    file: ctx.current_file.to_string(),
                    message: format!(
                        "cannot move out of borrow: field `{}` of `{}` has non-Copy type `{}`",
                        fa.field,
                        place_to_string(&struct_path),
                        rtype_to_string(&field_ty)
                    ),
                    span: fa.field_span.copy(),
                });
            }
            return Ok(rtype_to_infer(&field_ty));
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
