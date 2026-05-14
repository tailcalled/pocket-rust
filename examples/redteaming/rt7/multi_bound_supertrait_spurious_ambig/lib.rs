// Multi-bound dyn with shared supertrait → spurious ambiguity.
//
// `dyn Show + Tag` where `trait Show: Tag {}`. The vtable walker
// `dyn_vtable_methods` walks each principal's transitive supertrait
// closure independently. For the principal `Show`, the closure
// includes Tag.tag (because Show: Tag); for the principal `Tag`,
// the closure includes Tag.tag (its own method). Method dispatch
// then sees `tag` declared twice — once via Show's closure, once via
// Tag's direct methods — and emits "ambiguous method `tag`".
//
// Architectural shape: the multi-bound walker doesn't de-duplicate
// across principals. Two principals' supertrait closures are
// independent sets; their UNION is what the vtable should expose, but
// `check_dyn_method_call` treats overlapping entries as conflicting
// candidates. Real Rust de-dupes by `(trait_path, method_name)`:
// the same declaration via two routes is one candidate, not two.
//
// Fix: when building `found` in `check_dyn_method_call`, dedupe by
// the declaring trait path + method name. The current dedup-by-
// absolute-slot only catches per-trait duplicates, not cross-
// principal duplicates that resolve to the SAME trait.

trait Tag { fn tag(&self) -> u32; }
trait Show: Tag {}

struct Foo { v: u32 }
impl Tag for Foo { fn tag(&self) -> u32 { self.v + 7 } }
impl Show for Foo {}

pub fn answer() -> u32 {
    let f = Foo { v: 11 };
    // `dyn Show + Tag` — both principals reach the same `Tag::tag`.
    // Real Rust accepts and dispatches via Tag.tag.
    // Today's pocket-rust rejects with "ambiguous method `tag` on
    // multi-bound `dyn` type: declared by both `Tag` and `Tag`".
    let s: &dyn Show + Tag = &f;
    s.tag()
}
