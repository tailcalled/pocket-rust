// Sole impl is the blanket `impl<T> Trait for T`. Recv is a Sized
// concrete type (`u32`); blanket binds T = u32, autoref reaches the
// `&self` method at chain level `&u32`.

trait Trait {
    fn m(&self) -> u32;
}

impl<T> Trait for T {
    fn m(&self) -> u32 { 42 }
}

fn answer() -> u32 {
    let x: u32 = 0;
    x.m()
}
