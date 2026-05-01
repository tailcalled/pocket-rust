fn pick_first(a: usize, b: usize) -> usize { a }
fn pick_second(a: usize, b: usize) -> usize { b }
fn pick_third(a: usize, b: usize, c: usize) -> usize { c }

fn helper(x: usize, y: usize) -> usize {
    pick_first(y, x)
}

fn answer() -> usize {
    pick_third(
        10,
        20,
        helper(
            pick_second(100, 200),
            pick_first(300, 400)
        )
    )
}
