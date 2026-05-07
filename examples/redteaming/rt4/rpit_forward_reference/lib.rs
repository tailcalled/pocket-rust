// `use_make` is declared BEFORE `make` in this module. `make` uses
// RPIT — its `FnSymbol.return_type` is `Opaque{make, 0}` at setup
// time, with `rpit_slots[0].pin = None` until its body is checked.
//
// Pocket-rust's typeck walks bodies in declaration order: `use_make`'s
// body is checked first. At that point `make`'s pin is still None,
// so the call `make()` resolves to type `Opaque{make, 0}`, and
// `.show()` looks for an impl on `Opaque{...}` — none exists, so
// method dispatch fails. By the time `make`'s body is later checked
// and the pin is filled in, the caller's type errors have already
// fired.
//
// Real Rust handles this fine: forward references to RPIT functions
// work the same as forward references to ordinary fns. The opaque
// type's bounds are visible from the signature alone, so `.show()`
// dispatches via the `Show` bound recorded on the slot.
//
// Expected post-fix: this compiles cleanly. Either body-check RPIT
// fns first (topological), or make trait dispatch on `Opaque` consult
// the slot's bounds rather than treating it as a structureless type.

trait Show {
    fn show(self) -> u32;
}

impl Show for u32 {
    fn show(self) -> u32 {
        self
    }
}

pub fn answer() -> u32 {
    use_make()
}

fn use_make() -> u32 {
    make().show()
}

fn make() -> impl Show {
    42u32
}
