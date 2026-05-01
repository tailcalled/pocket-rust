fn answer() -> u32 {
    let p: (u32,) = (40,);
    match p {
        (mut n,) => {
            let r: &mut u32 = &mut n;
            *r = 42;
            n
        }
    }
}
