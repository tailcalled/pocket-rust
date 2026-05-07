// Bare call `f()` against a type-param bounded by `FnOnce`. The
// closure passed to `apply` is `move || v.take()` — it consumes the
// non-Copy `v` capture, so the synthesized closure type impls
// `FnOnce` (and only `FnOnce`; the mutating-by-consuming nature
// excludes the `Fn`/`FnMut` impls).
//
// Pocket-rust's `check_bare_typeparam_fn_call` records
// `trait_path = std::ops::Fn` regardless of the matched bound, so
// dispatch tries to find a `Fn`-impl on the closure type at impl-
// resolution time. None exists. Real Rust dispatches as
// `FnOnce::call_once` and the call resolves cleanly.

struct V {
    x: u32,
}

impl V {
    fn take(self) -> u32 {
        self.x
    }
}

fn apply<F: FnOnce() -> u32>(f: F) -> u32 {
    f()
}

pub fn answer() -> u32 {
    let v = V { x: 21u32 };
    apply(move || v.take()) + 21u32
}
