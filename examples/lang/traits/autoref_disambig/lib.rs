trait Show {
    fn show(&self) -> u32;
}

impl Show for u32 {
    fn show(&self) -> u32 { 1 }
}

impl<T> Show for &T {
    fn show(&self) -> u32 { 2 }
}

fn through_ref() -> u32 {
    let x: u32 = 5;
    let r: &u32 = &x;
    r.show()
}

fn through_owned() -> u32 {
    let x: u32 = 5;
    x.show()
}
