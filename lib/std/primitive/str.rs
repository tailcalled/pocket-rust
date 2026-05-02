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

use crate::ops::Index;
use crate::ops::IndexMut;
use crate::ops::Range;
use crate::ops::RangeFrom;
use crate::ops::RangeTo;
use crate::ops::RangeInclusive;
use crate::ops::RangeToInclusive;
use crate::ops::RangeFull;

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

    // True iff `idx` falls on a UTF-8 char boundary — that is, the
    // byte at position `idx` is the start of a codepoint (or `idx`
    // is exactly the string length / zero). Slicing helpers below
    // panic if any range boundary fails this check, since splitting
    // mid-codepoint would produce an invalid `&str`.
    //
    // A UTF-8 continuation byte has the bit pattern `10xxxxxx` —
    // `(b & 0xC0) == 0x80`. Anything else is a codepoint start.
    pub fn is_char_boundary(&self, idx: usize) -> bool {
        let len: usize = ¤str_len(self);
        if idx == 0 {
            return true;
        }
        if idx == len {
            return true;
        }
        if idx > len {
            return false;
        }
        let bytes: &[u8] = ¤str_as_bytes(self);
        let b: u8 = bytes[idx];
        // `b & 0xC0` (== 192) tests whether `b` is a UTF-8
        // continuation byte (`10xxxxxx`, mask = 0xC0, value = 0x80).
        // Use the bitwise builtin directly — surface `&` isn't an
        // expression operator yet, and pocket-rust's lexer is
        // decimal-only, so we write the constants in decimal too.
        let masked: u8 = ¤u8_and(b, 192u8);
        masked != 128u8
    }
}

// `s[start..end]` etc. — Range slicing for `str`. Returns a sub-`&str`
// (or `&mut str`) over the same byte range. Bounds-checked: out-of-
// range or reversed indices `panic!`. **Boundary-checked too:** every
// byte index passed in (start and end, with end becoming end+1 for
// inclusive ranges) must fall on a UTF-8 char boundary — see
// `is_char_boundary`. Slicing mid-codepoint would yield an invalid
// `&str`, so we panic at the slicing call rather than letting the
// invalid ref propagate.
//
// Output is `str` (not `[u8]`) so the indexing codegen returns
// `&str` / `&mut str` and downstream code keeps the string-vs-bytes
// distinction. RangeFull skips both the bounds and boundary checks
// — `0..len` is always valid by construction.

impl Index<Range<usize>> for str {
    type Output = str;
    fn index(&self, r: Range<usize>) -> &str {
        let len: usize = ¤str_len(self);
        if r.start > r.end {
            panic!("str range start > end")
        }
        if r.end > len {
            panic!("str range end out of bounds")
        }
        if !self.is_char_boundary(r.start) {
            panic!("str slice start is not a char boundary")
        }
        if !self.is_char_boundary(r.end) {
            panic!("str slice end is not a char boundary")
        }
        let new_len: usize = r.end - r.start;
        unsafe {
            let bytes: &[u8] = ¤str_as_bytes(self);
            let base: *const u8 = bytes.as_ptr();
            let new_ptr: *const u8 = base.byte_add(r.start);
            ¤make_str(new_ptr, new_len)
        }
    }
}

impl IndexMut<Range<usize>> for str {
    fn index_mut(&mut self, r: Range<usize>) -> &mut str {
        let len: usize = ¤str_len(self);
        if r.start > r.end {
            panic!("str range start > end")
        }
        if r.end > len {
            panic!("str range end out of bounds")
        }
        if !self.is_char_boundary(r.start) {
            panic!("str slice start is not a char boundary")
        }
        if !self.is_char_boundary(r.end) {
            panic!("str slice end is not a char boundary")
        }
        let new_len: usize = r.end - r.start;
        unsafe {
            let bytes: &mut [u8] = ¤str_as_mut_bytes(self);
            let base: *mut u8 = bytes.as_mut_ptr();
            let new_ptr: *mut u8 = base.byte_add(r.start);
            ¤make_mut_str(new_ptr, new_len)
        }
    }
}

impl Index<RangeFrom<usize>> for str {
    type Output = str;
    fn index(&self, r: RangeFrom<usize>) -> &str {
        let len: usize = ¤str_len(self);
        if r.start > len {
            panic!("str range start out of bounds")
        }
        if !self.is_char_boundary(r.start) {
            panic!("str slice start is not a char boundary")
        }
        let new_len: usize = len - r.start;
        unsafe {
            let bytes: &[u8] = ¤str_as_bytes(self);
            let base: *const u8 = bytes.as_ptr();
            let new_ptr: *const u8 = base.byte_add(r.start);
            ¤make_str(new_ptr, new_len)
        }
    }
}

impl IndexMut<RangeFrom<usize>> for str {
    fn index_mut(&mut self, r: RangeFrom<usize>) -> &mut str {
        let len: usize = ¤str_len(self);
        if r.start > len {
            panic!("str range start out of bounds")
        }
        if !self.is_char_boundary(r.start) {
            panic!("str slice start is not a char boundary")
        }
        let new_len: usize = len - r.start;
        unsafe {
            let bytes: &mut [u8] = ¤str_as_mut_bytes(self);
            let base: *mut u8 = bytes.as_mut_ptr();
            let new_ptr: *mut u8 = base.byte_add(r.start);
            ¤make_mut_str(new_ptr, new_len)
        }
    }
}

impl Index<RangeTo<usize>> for str {
    type Output = str;
    fn index(&self, r: RangeTo<usize>) -> &str {
        let len: usize = ¤str_len(self);
        if r.end > len {
            panic!("str range end out of bounds")
        }
        if !self.is_char_boundary(r.end) {
            panic!("str slice end is not a char boundary")
        }
        unsafe {
            let bytes: &[u8] = ¤str_as_bytes(self);
            let base: *const u8 = bytes.as_ptr();
            ¤make_str(base, r.end)
        }
    }
}

impl IndexMut<RangeTo<usize>> for str {
    fn index_mut(&mut self, r: RangeTo<usize>) -> &mut str {
        let len: usize = ¤str_len(self);
        if r.end > len {
            panic!("str range end out of bounds")
        }
        if !self.is_char_boundary(r.end) {
            panic!("str slice end is not a char boundary")
        }
        unsafe {
            let bytes: &mut [u8] = ¤str_as_mut_bytes(self);
            let base: *mut u8 = bytes.as_mut_ptr();
            ¤make_mut_str(base, r.end)
        }
    }
}

impl Index<RangeInclusive<usize>> for str {
    type Output = str;
    fn index(&self, r: RangeInclusive<usize>) -> &str {
        let len: usize = ¤str_len(self);
        if r.start > r.end {
            panic!("str range start > end")
        }
        if r.end >= len {
            panic!("str range end out of bounds")
        }
        let exclusive_end: usize = r.end + 1;
        if !self.is_char_boundary(r.start) {
            panic!("str slice start is not a char boundary")
        }
        if !self.is_char_boundary(exclusive_end) {
            panic!("str slice end is not a char boundary")
        }
        let new_len: usize = exclusive_end - r.start;
        unsafe {
            let bytes: &[u8] = ¤str_as_bytes(self);
            let base: *const u8 = bytes.as_ptr();
            let new_ptr: *const u8 = base.byte_add(r.start);
            ¤make_str(new_ptr, new_len)
        }
    }
}

impl IndexMut<RangeInclusive<usize>> for str {
    fn index_mut(&mut self, r: RangeInclusive<usize>) -> &mut str {
        let len: usize = ¤str_len(self);
        if r.start > r.end {
            panic!("str range start > end")
        }
        if r.end >= len {
            panic!("str range end out of bounds")
        }
        let exclusive_end: usize = r.end + 1;
        if !self.is_char_boundary(r.start) {
            panic!("str slice start is not a char boundary")
        }
        if !self.is_char_boundary(exclusive_end) {
            panic!("str slice end is not a char boundary")
        }
        let new_len: usize = exclusive_end - r.start;
        unsafe {
            let bytes: &mut [u8] = ¤str_as_mut_bytes(self);
            let base: *mut u8 = bytes.as_mut_ptr();
            let new_ptr: *mut u8 = base.byte_add(r.start);
            ¤make_mut_str(new_ptr, new_len)
        }
    }
}

impl Index<RangeToInclusive<usize>> for str {
    type Output = str;
    fn index(&self, r: RangeToInclusive<usize>) -> &str {
        let len: usize = ¤str_len(self);
        if r.end >= len {
            panic!("str range end out of bounds")
        }
        let exclusive_end: usize = r.end + 1;
        if !self.is_char_boundary(exclusive_end) {
            panic!("str slice end is not a char boundary")
        }
        unsafe {
            let bytes: &[u8] = ¤str_as_bytes(self);
            let base: *const u8 = bytes.as_ptr();
            ¤make_str(base, exclusive_end)
        }
    }
}

impl IndexMut<RangeToInclusive<usize>> for str {
    fn index_mut(&mut self, r: RangeToInclusive<usize>) -> &mut str {
        let len: usize = ¤str_len(self);
        if r.end >= len {
            panic!("str range end out of bounds")
        }
        let exclusive_end: usize = r.end + 1;
        if !self.is_char_boundary(exclusive_end) {
            panic!("str slice end is not a char boundary")
        }
        unsafe {
            let bytes: &mut [u8] = ¤str_as_mut_bytes(self);
            let base: *mut u8 = bytes.as_mut_ptr();
            ¤make_mut_str(base, exclusive_end)
        }
    }
}

impl Index<RangeFull> for str {
    type Output = str;
    fn index(&self, _r: RangeFull) -> &str {
        let len: usize = ¤str_len(self);
        unsafe {
            let bytes: &[u8] = ¤str_as_bytes(self);
            let base: *const u8 = bytes.as_ptr();
            ¤make_str(base, len)
        }
    }
}

impl IndexMut<RangeFull> for str {
    fn index_mut(&mut self, _r: RangeFull) -> &mut str {
        let len: usize = ¤str_len(self);
        unsafe {
            let bytes: &mut [u8] = ¤str_as_mut_bytes(self);
            let base: *mut u8 = bytes.as_mut_ptr();
            ¤make_mut_str(base, len)
        }
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
