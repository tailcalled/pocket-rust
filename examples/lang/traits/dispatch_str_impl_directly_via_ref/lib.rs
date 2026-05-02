// Sole impl is `impl MyTrait for str`. Recv `s: &str` matches the
// method's effective recv type `&str` directly at chain level 0 — no
// autoref, no autoderef. Returns 42.

trait MyTrait {
    fn test(&self) -> u32;
}

impl MyTrait for str {
    fn test(&self) -> u32 { 42 }
}

fn answer() -> u32 {
    let s: &str = "x";
    s.test()
}
