trait Animal {
    fn legs(&self) -> u32;
}

trait Dog: Animal {
    fn bark(&self) -> u32;
}

struct Pug { age: u32 }

impl Animal for Pug {
    fn legs(&self) -> u32 { 4 }
}

impl Dog for Pug {
    fn bark(&self) -> u32 { 7 }
}

fn count<T: Dog>(t: T) -> u32 {
    t.legs() + t.bark()
}

fn answer() -> u32 {
    let p: Pug = Pug { age: 3 };
    count(p) * 2
}
