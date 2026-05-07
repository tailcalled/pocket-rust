// `use std::dummy::{self, id};` — the `self` member re-imports
// `dummy` itself, so both `dummy::id(...)` and the bare `id(...)`
// resolve. Verifies the brace-group `self` parses and feeds the
// use-scope as the prefix path.
use std::dummy::{self, id};

fn answer() -> u32 {
    let viaself: u32 = dummy::id(40) as u32;
    let direct: u32 = id(2) as u32;
    viaself + direct
}
