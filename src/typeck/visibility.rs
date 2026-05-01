// Visibility checks for items within and across modules. The basic rule:
// a `pub` item is visible from anywhere; a non-pub item is visible only
// from its defining module and that module's descendants.
//
// Callers pass `defining_module` explicitly so the rule applies
// uniformly to free functions, structs, traits, and methods —
// methods nest under their impl target's name in the path, but the
// defining module is still the enclosing module, not the struct.
pub fn is_visible_from(
    defining_module: &Vec<String>,
    is_pub: bool,
    accessor_module: &Vec<String>,
) -> bool {
    if is_pub {
        return true;
    }
    if accessor_module.len() < defining_module.len() {
        return false;
    }
    let mut i = 0;
    while i < defining_module.len() {
        if accessor_module[i] != defining_module[i] {
            return false;
        }
        i += 1;
    }
    true
}

// Defining module for a function-table path: free functions live at
// `[mod..., name]` (drop one), inherent/trait-impl methods live at
// `[mod..., StructName, method_name]` (drop two). The
// `is_method_path` flag is computed from `FnSymbol.impl_target`.
pub fn fn_defining_module(item_path: &Vec<String>, is_method: bool) -> Vec<String> {
    let drop = if is_method { 2 } else { 1 };
    let n = if item_path.len() >= drop {
        item_path.len() - drop
    } else {
        0
    };
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < n {
        out.push(item_path[i].clone());
        i += 1;
    }
    out
}

// Defining module for a struct/trait at `[mod..., name]`.
pub fn type_defining_module(item_path: &Vec<String>) -> Vec<String> {
    if item_path.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 1 < item_path.len() {
        out.push(item_path[i].clone());
        i += 1;
    }
    out
}

// Field-level visibility: a non-pub struct field is only accessible
// from inside the struct's defining module (or any descendant).
pub fn field_visible_from(
    struct_path: &Vec<String>,
    field_is_pub: bool,
    accessor_module: &Vec<String>,
) -> bool {
    is_visible_from(
        &type_defining_module(struct_path),
        field_is_pub,
        accessor_module,
    )
}
