fn side_effect_returns(n: u32) -> u32 { n }
fn answer() -> u32 {
    let _ = side_effect_returns(99);
    42
}
