// Alias used in a struct field type: the struct stores a `Count`,
// callers can read/write it as `u32`. Verifies that the alias-substitution
// pass runs *before* struct-field resolution so the field type is
// already concrete by the time codegen looks at it.

pub type Count = u32;

struct Counter { value: Count }

fn answer() -> u32 {
    let c: Counter = Counter { value: 42u32 };
    c.value
}
