// `const` items at module scope, with the value inlined at each
// reference site by codegen.

const ANSWER: u32 = 42u32;

pub fn answer() -> u32 {
    ANSWER
}
