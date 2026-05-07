// `fn apply<F: FnOnce() -> u32>(f: F) -> u32 { f() + f() }` — the
// body invokes the FnOnce-bounded `f` twice. FnOnce *consumes*
// `self`; the second call is a use-after-move. Real Rust rejects
// this at borrowck.
//
// Pocket-rust accepts: `check_bare_typeparam_fn_call` records a
// `Fn::call` / `FnMut::call_mut` / `FnOnce::call_once` dispatch
// (rt4#5 picked the right family) but the receiver-adjust for
// FnOnce is `Move` — meaning each call moves `f`. Borrowck should
// then catch the second move. It doesn't, because the
// move-tracking for symbolically-dispatched calls on a Param-typed
// receiver doesn't fire when the dispatch is recorded as a method
// call rather than a value-position move.
//
// Expected post-fix: the second `f()` errors "use of moved value
// `f`".

struct V {
    x: u32,
}

fn apply<F: FnOnce() -> u32>(f: F) -> u32 {
    f() + f()
}

pub fn answer() -> u32 {
    let v = V { x: 21u32 };
    apply(move || v.x)
}
