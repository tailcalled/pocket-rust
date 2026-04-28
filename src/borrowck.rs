use crate::ast::{
    Block, Call, Expr, ExprKind, FieldAccess, Function, Item, LetStmt, Module, Stmt, StructLit,
};
use crate::span::{Error, Span};
use crate::typeck::{FuncTable, RType, StructTable, clone_path, func_lookup, rtype_clone};

pub fn check(
    root: &Module,
    structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut current_file = root.source_file.clone();
    let mut current_module: Vec<String> = Vec::new();
    push_root_name(&mut current_module, root);
    check_module(root, &mut current_module, &mut current_file, structs, funcs)?;
    Ok(())
}

fn push_root_name(path: &mut Vec<String>, root: &Module) {
    if !root.name.is_empty() {
        path.push(root.name.clone());
    }
}

fn check_module(
    module: &Module,
    current_module: &mut Vec<String>,
    current_file: &mut String,
    structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let saved = current_file.clone();
    *current_file = module.source_file.clone();
    let mut i = 0;
    while i < module.items.len() {
        match &module.items[i] {
            Item::Function(f) => {
                check_function(f, current_module, current_file, structs, funcs)?
            }
            Item::Module(m) => {
                current_module.push(m.name.clone());
                check_module(m, current_module, current_file, structs, funcs)?;
                current_module.pop();
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
    _structs: &StructTable,
    funcs: &FuncTable,
) -> Result<(), Error> {
    let mut full = clone_path(current_module);
    full.push(func.name.clone());
    let entry = func_lookup(funcs, &full).expect("typeck registered this function");

    let mut let_types: Vec<RType> = Vec::new();
    let mut k = 0;
    while k < entry.let_types.len() {
        let_types.push(rtype_clone(&entry.let_types[k]));
        k += 1;
    }

    let mut state = BorrowState {
        holders: Vec::new(),
        moved: Vec::new(),
        let_types,
        let_idx: 0,
        file: current_file.to_string(),
    };

    let mut k = 0;
    while k < func.params.len() {
        state.holders.push(Holder {
            name: Some(func.params[k].name.clone()),
            rtype: Some(rtype_clone(&entry.param_types[k])),
            holds: Vec::new(),
        });
        k += 1;
    }

    walk_stmts_and_tail(&mut state, &func.body)?;
    Ok(())
}

// ---------- State ----------

struct BorrowState {
    // Stack of holders. A holder either names a let/param binding (Some name)
    // or is a synthetic call slot (None name). Each holder records the
    // borrows it currently keeps alive (a list of place paths).
    holders: Vec<Holder>,
    // Permanent set of moved places (function-wide).
    moved: Vec<Vec<String>>,
    // Types of let-introduced bindings, in source-DFS order. Populated by typeck.
    let_types: Vec<RType>,
    let_idx: usize,
    file: String,
}

struct Holder {
    name: Option<String>,
    rtype: Option<RType>,
    holds: Vec<Vec<String>>,
}

// A descriptor of the borrows a value carries forward — i.e. which places this
// expression's value, if it's a reference, refers to. For non-reference values,
// always empty. The caller decides what to do with these (absorb into a binding,
// attach to a call slot, drop on the floor).
struct ValueDesc {
    borrows: Vec<Vec<String>>,
}

fn empty_desc() -> ValueDesc {
    ValueDesc {
        borrows: Vec::new(),
    }
}

// ---------- Walk ----------

fn walk_stmts_and_tail(state: &mut BorrowState, block: &Block) -> Result<ValueDesc, Error> {
    let mut i = 0;
    while i < block.stmts.len() {
        match &block.stmts[i] {
            Stmt::Let(let_stmt) => walk_let_stmt(state, let_stmt)?,
        }
        i += 1;
    }
    match &block.tail {
        Some(tail) => walk_expr(state, tail),
        None => Ok(empty_desc()),
    }
}

fn walk_let_stmt(state: &mut BorrowState, let_stmt: &LetStmt) -> Result<(), Error> {
    let desc = walk_expr(state, &let_stmt.value)?;
    let ty = rtype_clone(&state.let_types[state.let_idx]);
    state.let_idx += 1;
    state.holders.push(Holder {
        name: Some(let_stmt.name.clone()),
        rtype: Some(ty),
        holds: desc.borrows,
    });
    Ok(())
}

fn walk_expr(state: &mut BorrowState, expr: &Expr) -> Result<ValueDesc, Error> {
    match &expr.kind {
        ExprKind::IntLit(_) => Ok(empty_desc()),
        ExprKind::Var(name) => walk_var(state, name, expr),
        ExprKind::Call(call) => walk_call(state, call),
        ExprKind::StructLit(lit) => walk_struct_lit(state, lit),
        ExprKind::FieldAccess(fa) => walk_field_access(state, fa, expr),
        ExprKind::Borrow(_) => walk_borrow(state, expr),
        ExprKind::Block(block) => walk_block_expr(state, block.as_ref()),
    }
}

fn walk_var(state: &mut BorrowState, name: &str, expr: &Expr) -> Result<ValueDesc, Error> {
    let idx = find_binding(state, name).expect("typeck verified the variable exists");
    if is_ref_holder(&state.holders[idx]) {
        // Reading a ref: refs are Copy. The value carries the same borrows
        // the binding holds; the caller may decide to add another holder for them.
        let holds = clone_places(&state.holders[idx].holds);
        Ok(ValueDesc { borrows: holds })
    } else {
        // Owned read: this is a move.
        let mut place: Vec<String> = Vec::new();
        place.push(name.to_string());
        try_move(state, place, expr.span.copy())?;
        Ok(empty_desc())
    }
}

fn walk_call(state: &mut BorrowState, call: &Call) -> Result<ValueDesc, Error> {
    // Push a synthetic call holder. Borrows produced by argument expressions
    // become its holds for the duration of the call, then the holder is popped.
    state.holders.push(Holder {
        name: None,
        rtype: None,
        holds: Vec::new(),
    });
    let call_idx = state.holders.len() - 1;
    let mut i = 0;
    while i < call.args.len() {
        let desc = walk_expr(state, &call.args[i])?;
        let mut k = 0;
        while k < desc.borrows.len() {
            state.holders[call_idx]
                .holds
                .push(clone_path(&desc.borrows[k]));
            k += 1;
        }
        i += 1;
    }
    state.holders.truncate(call_idx);
    // Functions can't return references (typeck-rejected), so result carries no borrows.
    Ok(empty_desc())
}

fn walk_struct_lit(state: &mut BorrowState, lit: &StructLit) -> Result<ValueDesc, Error> {
    // Struct fields can't be references (typeck-rejected), so the constructed
    // struct value carries no borrows. We still walk each field to surface
    // moves / borrow registrations inside their initializer expressions.
    let mut i = 0;
    while i < lit.fields.len() {
        walk_expr(state, &lit.fields[i].value)?;
        i += 1;
    }
    Ok(empty_desc())
}

fn walk_field_access(
    state: &mut BorrowState,
    fa: &FieldAccess,
    expr: &Expr,
) -> Result<ValueDesc, Error> {
    match extract_place(expr) {
        Some(place) => {
            let root_idx =
                find_binding(state, &place[0]).expect("typeck verified the variable exists");
            if is_ref_holder(&state.holders[root_idx]) {
                // Navigation through a reference. Field is Copy (typeck), so
                // the result is plain data with no carried borrows; the ref
                // itself is unchanged.
                Ok(empty_desc())
            } else {
                // Partial-move out of an owned struct.
                try_move(state, place, expr.span.copy())?;
                Ok(empty_desc())
            }
        }
        None => {
            // Field access on a non-place base (e.g. a call result). Walk the
            // base for its side effects; the field result is Copy.
            walk_expr(state, &fa.base)?;
            Ok(empty_desc())
        }
    }
}

fn walk_borrow(state: &mut BorrowState, expr: &Expr) -> Result<ValueDesc, Error> {
    let inner = match &expr.kind {
        ExprKind::Borrow(i) => i,
        _ => unreachable!("walk_borrow called on non-Borrow"),
    };
    match extract_place(inner) {
        Some(place) => {
            // Refuse to borrow a place whose root has already been moved.
            let mut i = 0;
            while i < state.moved.len() {
                if paths_share_prefix(&state.moved[i], &place) {
                    return Err(Error {
                        file: state.file.clone(),
                        message: format!(
                            "cannot borrow `{}`: it has been moved",
                            place_to_string(&place)
                        ),
                        span: expr.span.copy(),
                    });
                }
                i += 1;
            }
            let mut borrows = Vec::new();
            borrows.push(place);
            Ok(ValueDesc { borrows })
        }
        None => {
            // Borrowing a non-place expression (e.g. `&fresh_struct_lit()`).
            // We still walk inner for its move-tracking side effects, but
            // don't track the borrow (no place to point at).
            walk_expr(state, inner)?;
            Ok(empty_desc())
        }
    }
}

fn walk_block_expr(state: &mut BorrowState, block: &Block) -> Result<ValueDesc, Error> {
    // The block introduces a fresh local scope. Any holders pushed inside
    // (let bindings) are dropped when the block ends. The block's tail value
    // descriptor is returned to the caller — its borrows survive the scope
    // because the *caller* will turn them into a holder of its own.
    let mark = state.holders.len();
    let desc = walk_stmts_and_tail(state, block)?;
    state.holders.truncate(mark);
    Ok(desc)
}

// ---------- Helpers ----------

fn find_binding(state: &BorrowState, name: &str) -> Option<usize> {
    let mut i = state.holders.len();
    while i > 0 {
        i -= 1;
        if let Some(n) = &state.holders[i].name {
            if n == name {
                return Some(i);
            }
        }
    }
    None
}

fn is_ref_holder(h: &Holder) -> bool {
    matches!(h.rtype, Some(RType::Ref(_)))
}

fn extract_place(expr: &Expr) -> Option<Vec<String>> {
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

fn try_move(state: &mut BorrowState, place: Vec<String>, span: Span) -> Result<(), Error> {
    let mut i = 0;
    while i < state.moved.len() {
        if paths_share_prefix(&state.moved[i], &place) {
            return Err(Error {
                file: state.file.clone(),
                message: format!("`{}` was already moved", place_to_string(&place)),
                span,
            });
        }
        i += 1;
    }
    let mut h = 0;
    while h < state.holders.len() {
        let mut k = 0;
        while k < state.holders[h].holds.len() {
            if paths_share_prefix(&state.holders[h].holds[k], &place) {
                return Err(Error {
                    file: state.file.clone(),
                    message: format!(
                        "cannot move `{}` while it is borrowed",
                        place_to_string(&place)
                    ),
                    span,
                });
            }
            k += 1;
        }
        h += 1;
    }
    state.moved.push(place);
    Ok(())
}

fn paths_share_prefix(a: &Vec<String>, b: &Vec<String>) -> bool {
    let m = if a.len() < b.len() { a.len() } else { b.len() };
    let mut i = 0;
    while i < m {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn place_to_string(p: &Vec<String>) -> String {
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

fn clone_places(places: &Vec<Vec<String>>) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < places.len() {
        out.push(clone_path(&places[i]));
        i += 1;
    }
    out
}
