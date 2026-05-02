// Unit struct: declared with `struct Name;` (semicolon body),
// constructed with the empty struct-lit form `Name {}`. Layout is
// zero bytes; flat scalar shape is empty. Used here purely for its
// presence — `_marker` carries no value but satisfies the field
// initializer for the tuple's first element.
struct Marker;

fn answer() -> u32 {
    let _m: Marker = Marker {};
    42u32
}
