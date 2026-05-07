// Forward-reference a (later-declared) RPIT fn AND bind its result
// to a local before calling a method on the local. With rt4#3's
// fix, `make().show()` works even when `make`'s pin isn't set
// yet — `check_method_call_opaque` routes through the slot bounds.
// But `let r = make(); r.show()` is different: the local `r` is
// recorded in `FnSymbol.expr_types` with type `RType::Opaque{make,
// 0}`. After typeck, `finalize_rpit_substitutions` rewrites
// FnSymbol return_types and `MethodResolution.trait_dispatch.recv_type`
// — but NOT the per-NodeId `expr_types` table. The local's
// recorded type stays `Opaque`. Codegen reads `expr_types[r]` to
// allocate the binding's storage and hits an `Opaque` node, which
// the layout helpers (`byte_size_of`, `flatten_rtype`,
// `collect_leaves`) panic on.
//
// Architectural shape: finalize substitution missed `expr_types`.
// Any bind-and-method-on-RPIT pattern in a forward-reference
// crashes mono/codegen.
//
// Fix shape: extend `finalize_rpit_substitutions` to also walk
// every `FnSymbol.expr_types` and `Template.expr_types` and
// substitute `Opaque{fn, slot} → pin`.

trait Show {
    fn show(self) -> u32;
}

impl Show for u32 {
    fn show(self) -> u32 {
        self
    }
}

pub fn answer() -> u32 {
    use_make_via_local()
}

fn use_make_via_local() -> u32 {
    let r = make();
    r.show()
}

fn make() -> impl Show {
    42u32
}
