fn answer() -> u32 {
    let v: u32 = {
        use std::dummy::id;
        id(33) as u32
    };
    v
}
