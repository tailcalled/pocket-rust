fn noop() {
    let _u: () = ();
}

fn answer() -> u32 {
    noop();
    42
}
