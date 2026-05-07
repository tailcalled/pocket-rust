// `let x;` — no annotation, no initializer. The binding's type is
// inferred from the later assignment via the type-var unification
// the type checker seeds when the annotation is absent.
fn answer() -> u32 {
    let x;
    x = 99u32;
    x
}
