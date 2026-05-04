// Derive on a generic struct: the synthesized impl carries
// `T: Clone` / `T: PartialEq` bounds inherited from each derived trait.

#[deriving(Clone, PartialEq)]
struct Holder<T> { value: T }

fn answer() -> u32 {
    let h: Holder<u32> = Holder { value: 42u32 };
    let g: Holder<u32> = h.clone();
    if h.eq(&g) { g.value } else { 0u32 }
}
