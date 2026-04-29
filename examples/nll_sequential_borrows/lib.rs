struct Point { x: u32, y: u32 }

fn read_x(p: &Point) -> u32 { p.x }
fn set_x(p: &mut Point, v: u32) -> u32 { p.x = v; p.x }

fn answer() -> u32 {
    let mut pt = Point { x: 1, y: 2 };
    let _r: &Point = &pt;
    let _v = read_x(&pt);
    let m: &mut Point = &mut pt;
    set_x(m, 7)
}
