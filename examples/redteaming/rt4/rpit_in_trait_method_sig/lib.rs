// `trait Maker { fn make() -> impl Show; }` — an RPIT in a trait
// method **declaration**. `resolve_trait_methods` resolves each
// method's return type via the plain `resolve_type`, which rejects
// `TypeKind::ImplTrait`. The RPIT-aware rewrite is wired only into
// `register_function`, which trait method declarations don't go
// through.
//
// Real Rust supports RPITIT (return-position impl Trait in trait):
// the trait declares an opaque return per impl, and each impl's
// concrete pin can differ (so one impl returns `u32`, another
// returns `bool`, both opaque to the caller).
//
// Expected post-fix: this parses, the trait records a per-method
// rpit-slot shape, and impls supply concrete pins.

trait Show {
    fn show(self) -> u32;
}

impl Show for u32 {
    fn show(self) -> u32 {
        self
    }
}

trait Maker {
    fn make() -> impl Show;
}

struct UnitMaker;

impl Maker for UnitMaker {
    fn make() -> impl Show {
        21u32
    }
}

pub fn answer() -> u32 {
    UnitMaker::make().show()
}
