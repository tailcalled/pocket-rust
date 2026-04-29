trait Show { fn show(self) -> u32; }

impl Show for u32 { fn show(self) -> u32 { self } }

fn answer() -> u32 {
    let x: u32 = 42;
    x.show()
}
