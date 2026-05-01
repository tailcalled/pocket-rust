struct Pt { x: u32, y: u32 }

impl Copy for Pt {}

fn answer() -> u32 {
    let p: Pt = Pt { x: 42, y: 7 };
    let q: Pt = p;
    p.x
}
