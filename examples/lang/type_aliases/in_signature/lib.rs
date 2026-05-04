// Aliases in function signatures: param + return-type positions resolve
// transparently. Caller passes a `u32`, callee param's `Count` accepts
// it without conversion.

pub type Count = u32;

fn double(c: Count) -> Count {
    c + c
}

fn answer() -> u32 {
    double(21u32)
}
