// Push past the initial capacity of 4, forcing `grow`. Pop should
// still return the values in LIFO order, summing back to the input.
fn answer() -> u32 {
    let mut v: Vec<u32> = Vec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    v.push(5);
    v.push(6);
    let mut sum: u32 = 0;
    while v.len() > 0 {
        let x: u32 = match v.pop() {
            Option::Some(n) => n,
            Option::None => 0,
        };
        sum = sum + x;
    }
    sum
}
