// Closure capture detection lives in `check_expr_inner`'s
// `ExprKind::Var(name)` arm — it walks `closure_scopes` and records
// the binding when the lookup crosses a barrier. But there's a
// *parallel* Var lookup in `check_place_inner` (used when a Var
// appears in place position: as a method-call receiver, as the inner
// of a `&` / `&mut` borrow, as the base of a Deref chain) that does
// none of this — it just consults `ctx.locals`. So a closure body
// that uses a captured binding in any place position never records
// the capture at all.
//
// Architectural shape: the capture barrier is a property of the
// binding-resolution operation, but pocket-rust's binding resolution
// happens at TWO sites with separate code paths (value position
// `check_expr_inner::Var` vs place position `check_place_inner::Var`).
// The fix is a single helper used by both — `lookup_local_with_capture`
// that walks `ctx.locals` AND records into the innermost crossed
// `closure_scopes` frame.
//
// Without that, common closure idioms break: any closure body that
// calls a method on a captured binding (`outer.method()`), borrows
// it (`&outer` / `&mut outer`), accesses its fields where the field
// is non-Copy (so the reach goes through place-mode) — all silently
// fail to capture, and the synthesized impl method body errors with
// "unknown variable: `outer`" because it expected a struct field
// rewrite that never happened.
//
// Failure mode here: `read_field(&foo)` inside the closure passes a
// borrow of the captured `foo`. The `&foo` inner is a place
// expression; its Var lookup goes through `check_place_inner` and
// records nothing. Synthesis sees zero captures, the synthesized
// `__closure_0` is a unit struct, and the lifted call-method body
// fails at typeck with "unknown variable: `foo`".
//
// Expected post-fix: program compiles and returns 42.

struct Foo {
    x: u32,
}

fn read_field(f: &Foo) -> u32 {
    f.x
}

pub fn answer() -> u32 {
    let foo = Foo { x: 41u32 };
    let f = |_unit: ()| read_field(&foo) + 1u32;
    f.call(((),))
}
