// Inherent methods on the UTF-8 string DST `str`. Mirrors the surface
// of Rust's `std::primitive::str` module. `str` is layout-identical
// to `[u8]` (a fat ref of (data ptr, length) over u8 bytes), but kept
// as its own type so users get `&str` in error messages and so future
// UTF-8 invariants and string-specific methods can attach here.
//
// Construction goes through `¤make_str(ptr, len)` for the unsafe raw-
// parts route, and string literals (once landed) compile directly to
// (data_offset, byte_len). The methods here only consume an existing
// `&str` reference.

impl str {
    // Length in bytes (not chars). `&str`'s fat ref carries this
    // value as its second i32; the intrinsic drops the data ptr and
    // returns the length scalar — pure metadata access.
    pub fn len(&self) -> usize {
        ¤str_len(self)
    }

    // True iff the string contains zero bytes. Same fat-ref length
    // read as `len`, compared against zero.
    pub fn is_empty(&self) -> bool {
        ¤str_len(self) == 0
    }

    // Reinterpret as `&[u8]`. The fat-ref representations are bit-
    // identical, so codegen passes the (ptr, len) pair through
    // unchanged. Always safe (UTF-8 is a strict subset of arbitrary
    // bytes; readers that don't need the UTF-8 invariant can use this
    // freely).
    pub fn as_bytes(&self) -> &[u8] {
        ¤str_as_bytes(self)
    }
}

// TODOs — methods we'd want eventually but pocket-rust doesn't yet
// have the language features to express. Listed alphabetically. When
// a blocker lands, search this file for the relevant TODO.
//
// TODO: as_ptr(&self) -> *const u8 — needs a `¤str_ptr` intrinsic to extract the data half of the fat ref; trivial follow-up.
// TODO: bytes(&self) — needs iterator traits.
// TODO: chars(&self) — needs iterator traits + UTF-8 decoding.
// TODO: contains(&self, needle: &str) — needs substring search; expressible once `as_bytes` callers prove the byte-level path.
// TODO: ends_with(&self, suffix: &str) / starts_with — needs byte-level prefix/suffix comparison; expressible today over `as_bytes`.
// TODO: eq_ignore_ascii_case / make_ascii_lowercase / make_ascii_uppercase / to_ascii_lowercase / to_ascii_uppercase — needs `&mut str` mutation + ASCII helpers.
// TODO: find(&self, pat) / rfind / split / split_whitespace / lines — needs string-pattern infrastructure (Pattern trait + iterator traits).
// TODO: get(&self, range) -> Option<&str> — needs ranges and the UTF-8 char-boundary check.
// TODO: parse::<T>() — needs the FromStr trait.
// TODO: repeat(&self, n) -> String — needs `String` (heap-owned).
// TODO: replace(&self, from: &str, to: &str) -> String — needs `String`.
// TODO: split_at(&self, mid) -> (&str, &str) — needs returning a 4-i32 tuple ABI (already supported via tuple flattening, just hadn't a use case).
// TODO: to_string(&self) -> String — needs `String`.
// TODO: trim(&self) / trim_start / trim_end — expressible over byte indexing once char-boundary helpers exist.
