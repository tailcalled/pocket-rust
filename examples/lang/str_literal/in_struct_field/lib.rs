// Literal in a struct-field initializer.
struct Greet<'a> { msg: &'a str, weight: u32 }

fn answer() -> u32 {
    let g: Greet = Greet { msg: "hello", weight: 32 };
    (g.msg.len() as u32) + g.weight + 5
}
