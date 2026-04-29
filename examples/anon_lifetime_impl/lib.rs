struct Logger<'a> { tag: &'a u32 }

impl Drop for Logger<'_> {
    fn drop(&mut self) {}
}

fn answer() -> u32 {
    let t: u32 = 42;
    let _l: Logger<'_> = Logger { tag: &t };
    *_l.tag
}
