// Method-call call sites bypass region inference.
//
// `src/borrowck/build.rs::emit_call_constraints` short-circuits with
// `CallTarget::MethodResolution(_) => return,`. Free-fn calls
// (`CallTarget::Path`) emit per-arg `caller_arg_region :
// callee_param_region_inst` constraints AND instantiate the callee's
// where-clause as edges in the caller's RegionGraph; method calls do
// neither.
//
// Concretely: a generic caller calls a method requiring `'a: 'b`; the
// caller doesn't declare that bound. Free-fn dispatch would catch this
// — the method-call dispatch silently accepts it.
//
// Real Rust rejects: the caller can't prove the callee's where-clause
// holds for the `'a`/`'b` it received from its own caller.
//
// Expected post-fix: emit_call_constraints' MethodResolution arm
// resolves the callee FnSymbol from `MethodResolution.callee_path`,
// instantiates its lifetime params as fresh RegionIds, and emits the
// same per-arg / where-clause / return edges that the Path arm does.

trait Op {
    fn apply<'a, 'b>(&self, x: &'a u32, _y: &'b u32) -> &'b u32;
}

struct Holder;
impl Op for Holder {
    fn apply<'a, 'b>(&self, x: &'a u32, _y: &'b u32) -> &'b u32 {
        // Body's well-formedness depends on the where-clause: returning
        // `&'a u32` as `&'b u32` is sound only if `'a: 'b`. Real Rust
        // rejects this method's body — the impl needs to declare
        // `where 'a: 'b` (and any caller would then have to prove it).
        // We omit the where-clause here so the bug is observable from
        // the call site rather than from the impl-method's body.
        x
    }
}

pub fn answer() -> u32 {
    let a: u32 = 21u32;
    let b: u32 = 21u32;
    let h = Holder {};
    *h.apply(&a, &b) + 21u32
}
