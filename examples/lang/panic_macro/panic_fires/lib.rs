// `panic!` actually fires — the function unconditionally calls it
// with a known message. The host stub reads the message out of
// memory and surfaces it in the trap.
fn answer() -> u32 {
    panic!("custom message at line 5")
}
