// pocket-rust skips drop glue for structs / enums / tuples whose
// own type doesn't directly implement `Drop`.
//
// `compute_drop_action` (src/layout.rs) early-outs to `Skip` whenever
// `is_drop(ty, traits)` returns false. `is_drop` (src/typeck/types.rs)
// only checks for a *direct* `impl Drop for T` — it doesn't recurse
// into struct fields, enum variant payloads, or tuple elements. So
// any aggregate that contains `Drop` types but doesn't itself
// implement `Drop` (which is the common case — programs don't usually
// `impl Drop` for their data structures) gets *no* destruction at
// scope end, even when the inner fields desperately need to run their
// destructors.
//
// Consequences span the language: a `Vec<T>` field never frees its
// allocation when the wrapping struct goes out of scope; a
// `(File, Mutex)` pair never closes its file handle nor releases the
// lock; an `enum Foo { Bar(Tracker) }` ignores Tracker's Drop. Real
// Rust calls this "drop glue" — synthesized code the compiler emits
// to recursively drop every field/element of an aggregate, in
// declaration order, before the aggregate's storage is reclaimed.
// pocket-rust simply doesn't synthesize it.
//
// This example uses a raw-pointer log to make Drop calls observable.
// Each `Tracker::drop` appends its `id` to the log via base-10
// shifting. Real Rust drops `a` (id=4) then `b` (id=2):
//   log = ((0 * 10) + 4) * 10 + 2 = 42
// pocket-rust drops nothing (Pair has no `impl Drop`):
//   log = 0
//
// Why architectural: drop glue is a load-bearing piece of language
// semantics — the lifecycle of every owning aggregate depends on it.
// Adding drop glue requires touching three layers: `is_drop` (or a
// new `needs_drop`) must walk into fields/variants/elements;
// `compute_drop_action` must return non-Skip whenever any sub-place
// needs dropping; `emit_drop_call_for_local` must be augmented (or a
// sibling added) that, instead of calling `Drop::drop` on the whole,
// walks the aggregate's structure and drops each leaf in source
// order.
//
// Expected (post-fix): 42.

struct Tracker {
    id: u32,
    log: *mut u32,
}

impl Drop for Tracker {
    fn drop(&mut self) {
        unsafe {
            *self.log = (*self.log) * 10u32 + self.id;
        }
    }
}

struct Pair {
    a: Tracker,
    b: Tracker,
}

fn answer() -> u32 {
    let mut log: u32 = 0;
    let log_ptr: *mut u32 = &mut log as *mut u32;
    {
        let _p = Pair {
            a: Tracker { id: 4u32, log: log_ptr },
            b: Tracker { id: 2u32, log: log_ptr },
        };
    }
    log
}
