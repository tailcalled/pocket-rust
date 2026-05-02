// Early return inside a loop. Once `i == 7` the function returns
// 42; the surrounding code never reaches the tail expression.
fn find_seven() -> u32 {
    let mut i: u32 = 0;
    while i < 100 {
        if i == 7 {
            return 42;
        }
        i = i + 1;
    }
    0
}

fn answer() -> u32 {
    find_seven()
}
