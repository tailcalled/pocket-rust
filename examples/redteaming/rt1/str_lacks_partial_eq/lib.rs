// String literals are typed `&'static str` and are the only way to
// construct strings today, but `lib/std/cmp.rs` doesn't declare
// `impl PartialEq for str` (or for `&str`). So `"a" == "b"` errors
// "no method `eq` on `&str`" — there's effectively no string
// equality at all in the language right now.
//
// Expected: 42.

fn answer() -> u32 {
    let s: &str = "hello";
    let t: &str = "world";
    if s == t { 0 } else { 42 }
}
