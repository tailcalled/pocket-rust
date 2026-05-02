// `&v[idx]` and `&mut v[idx]` — borrow contexts that route to
// Index::index and IndexMut::index_mut respectively. The borrow's
// type is `&T` / `&mut T` to the element in place.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(0);
    v.push(0);
    let r: &mut u32 = &mut v[0];
    *r = 42;
    let s: &u32 = &v[0];
    *s
}
