// Submodule loaded via `<child>/mod.rs` (Rust 2015 style + still-valid
// 2018+ form). The crate root declares `mod sub;`; the resolver tries
// `sub.rs` first, falls through to `sub/mod.rs`. The `sub` directory
// here has no `sub.rs` sibling, only `sub/mod.rs`.

mod sub;

fn answer() -> u32 {
    sub::value()
}
