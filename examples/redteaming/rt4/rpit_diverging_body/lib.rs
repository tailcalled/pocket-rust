// RPIT body that diverges via `panic!()`. The body's actual return
// type is `!` (Never). Real Rust accepts this — `!` is uninhabited
// so any trait obligation is vacuously true, and the function's
// abstract return type is fine for callers that never actually
// reach the panic.
//
// Pocket-rust currently rejects: `check_block`'s post-unify
// validation walks each Opaque slot and calls
// `solve_impl_in_ctx_with_args(trait_path, trait_args, &Never, ...)`
// which returns None, producing
// "RPIT body return type `!` does not satisfy bound `Show`".
//
// Expected post-fix: this compiles cleanly. The validation should
// short-circuit on `RType::Never`.

trait Show {
    fn show(self) -> u32;
}

impl Show for u32 {
    fn show(self) -> u32 {
        self
    }
}

fn make_or_die(b: bool) -> impl Show {
    if b {
        panic!("never")
    } else {
        42u32
    }
}

pub fn answer() -> u32 {
    make_or_die(false).show()
}
