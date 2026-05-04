// Symbolic-dispatch bug: calling a `self`-by-value trait method on a
// `&T` receiver where `T: SomeTrait` is silently accepted, even
// though the call would move out of a shared reference.
//
// `dispatch_method_through_trait` (src/typeck/methods.rs) only
// rejects the `Move` receiver shape when the recv was `&mut T`
// (`recv_through_mut_ref`); it falls through to
// `ReceiverAdjust::Move` for `&T` (`recv_through_shared_ref`). The
// trace through the warning chain is direct: `recv_through_shared_ref`
// is computed but never read in the `Move` arm — the dead-variable
// warning marks the missing rejection branch.
//
// Real Rust rejects this with E0507: `cannot move out of `*r` which
// is behind a shared reference`. pocket-rust accepts the program,
// codegens the call with `recv_adjust = Move`, and at runtime passes
// the `&T` pointer value (an i32 shadow-stack address) where the
// callee expects `T`. So `take(self) -> u32 { self }` returns the
// address, not 42.
//
// Expected (post-fix): the program is REJECTED at typeck. The test
// asserts compilation fails with a move-through-shared-ref-style
// diagnostic.

trait Take {
    fn take(self) -> u32;
}

impl Take for u32 {
    fn take(self) -> u32 {
        self
    }
}

fn through_ref<T: Take>(r: &T) -> u32 {
    r.take()
}

fn answer() -> u32 {
    let x: u32 = 42;
    through_ref(&x)
}
