// `let x: u32;` — declared without an initializer, assigned later,
// then read. Borrowck threads the binding through the move-state
// lattice as `Uninit`; the subsequent `x = …;` clears it back to
// `Init`, so the read is valid.
fn answer() -> u32 {
    let x: u32;
    x = 99u32;
    x
}
