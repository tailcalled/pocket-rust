use super::{RType, int_kind_from_name};

pub struct BuiltinSig {
    pub params: Vec<RType>,
    pub result: RType,
}

// Recognized builtins. The name's first segment is the type (one of
// `bool`, `u8`, `i8`, `u16`, `i16`, `u32`, `i32`, `u64`, `i64`,
// `usize`, `isize`); the rest is the operation. Returns `None` for
// any name we don't recognize.
pub fn builtin_signature(name: &str) -> Option<BuiltinSig> {
    // Split on the last `_` to separate type prefix from op suffix.
    // The op is one of a small fixed set; everything before the op
    // is the type name (which contains no `_`).
    let ops = [
        "add", "sub", "mul", "div", "rem", "eq", "ne", "lt", "le",
        "gt", "ge", "and", "or", "not", "xor",
    ];
    let mut found_op: Option<&str> = None;
    let mut k = 0;
    while k < ops.len() {
        let op = ops[k];
        if name.len() > op.len() + 1 {
            let prefix_end = name.len() - op.len();
            if name.as_bytes()[prefix_end - 1] == b'_'
                && &name[prefix_end..] == op
            {
                found_op = Some(op);
                break;
            }
        }
        k += 1;
    }
    let op = found_op?;
    let ty_name = &name[..name.len() - op.len() - 1];
    let ty = match ty_name {
        "bool" => RType::Bool,
        _ => RType::Int(int_kind_from_name(ty_name)?),
    };
    if matches!(ty, RType::Bool) {
        match op {
            "and" | "or" | "xor" | "eq" | "ne" => {
                return Some(BuiltinSig {
                    params: vec![RType::Bool, RType::Bool],
                    result: RType::Bool,
                });
            }
            "not" => {
                return Some(BuiltinSig {
                    params: vec![RType::Bool],
                    result: RType::Bool,
                });
            }
            _ => return None,
        }
    }
    let is_arith = matches!(op, "add" | "sub" | "mul" | "div" | "rem" | "and" | "or" | "xor");
    let is_cmp = matches!(op, "eq" | "ne" | "lt" | "le" | "gt" | "ge");
    if is_arith {
        return Some(BuiltinSig {
            params: vec![ty.clone(), ty.clone()],
            result: ty,
        });
    }
    if is_cmp {
        return Some(BuiltinSig {
            params: vec![ty.clone(), ty],
            result: RType::Bool,
        });
    }
    None
}

