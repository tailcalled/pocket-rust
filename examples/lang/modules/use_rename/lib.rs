use std::dummy::id as identity;

fn answer() -> u32 {
    identity(99) as u32
}
