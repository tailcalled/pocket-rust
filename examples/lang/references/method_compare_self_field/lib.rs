// Comparison of `self.field` against another value through a method
// receiver. The `<` desugars to `self.field.lt(&other)`, where the
// lt method's recv_adjust is BorrowImm — so codegen must take the
// address of the *non-first* field of the pointee. Used to silently
// miscompile in the Mono path: lower_expr's value-context FieldAccess
// produced `Field { base: Local(self_ref), byte_offset: 0 }` (without
// auto-deref), and emit_place_address pushed `self_addr + 0` instead
// of `self_addr + offset_of_field`. First-field reads happened to
// work by coincidence (offset 0); non-first fields read garbage.

struct Window { lo: u32, hi: u32 }

impl Window {
    fn in_range(&self, x: u32) -> u32 {
        if self.lo < x {
            if x < self.hi { 1 } else { 0 }
        } else {
            0
        }
    }
}

fn answer() -> u32 {
    let r: Window = Window { lo: 5, hi: 10 };
    r.in_range(7) * 100 + r.in_range(20) * 10 + r.in_range(3)
}
