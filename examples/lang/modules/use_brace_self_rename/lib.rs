// `use std::dummy::{self as d};` — the `self` member is re-imported
// under the explicit rename `d`, so `d::id(...)` resolves while the
// original name `dummy` does not.
use std::dummy::{self as d};

fn answer() -> u32 {
    d::id(42) as u32
}
