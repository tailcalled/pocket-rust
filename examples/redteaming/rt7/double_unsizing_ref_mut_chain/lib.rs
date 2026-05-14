// Chained coercion `&mut T → &mut dyn Trait → &dyn Trait`.
//
// `coerce_at` handles each step individually: it accepts
//   `&mut T` → `&mut dyn Trait` (preserves mutability), and
//   `&mut T` → `&dyn Trait`     (downgrades mutability in one step).
// But it does NOT recognize `&mut dyn Trait` → `&dyn Trait` — the
// "source already Dyn" guard added in Phase 9 falls through to
// `unify`, and unify rejects `&mut Dyn` against `&Dyn` as a plain
// mutability mismatch.
//
// Architectural shape: coercion handling assumes the source is a
// concrete `T`. Once you have a `&mut dyn Trait`, downgrading to
// `&dyn Trait` isn't an unsizing — it's a reborrow + mutability
// downgrade — but the coercion path doesn't recognize it. Real Rust
// accepts because reborrowing rules cover the `&mut → &` direction
// for any pointee, including DSTs.
//
// Fix: in `coerce_at`'s "source already Dyn" guard, allow the case
// where the source is `&mut Dyn` and the target is `&Dyn` with the
// SAME bound list — emit no DynCoercion (the data ptr + vtable ptr
// already exist) but accept the unify.

trait Counter { fn read(&self) -> u32; }
struct Ctr { n: u32 }
impl Counter for Ctr { fn read(&self) -> u32 { self.n } }

pub fn answer() -> u32 {
    let mut c = Ctr { n: 42 };
    let m: &mut dyn Counter = &mut c;
    // Real Rust accepts: `&mut dyn Counter` downgrades to `&dyn
    // Counter`. Today's pocket-rust rejects with
    // "expected `&dyn Counter`, got `&mut dyn Counter`".
    let n: &dyn Counter = m;
    n.read()
}
