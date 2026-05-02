// `Option<T>` — a value that may or may not be present. `Some(T)`
// carries a value; `None` carries nothing.

pub enum Option<T> {
    None,
    Some(T),
}

impl<T> Option<T> {
    // Returns true if the option is a `Some`.
    pub fn is_some(&self) -> bool {
        match self {
            &Option::Some(_) => true,
            &Option::None => false,
        }
    }

    // Returns true if the option is a `None`.
    pub fn is_none(&self) -> bool {
        match self {
            &Option::Some(_) => false,
            &Option::None => true,
        }
    }

    // Consumes the option and returns the inner value if `Some`,
    // otherwise returns `default`.
    pub fn unwrap_or(self, default: T) -> T {
        match self {
            Option::Some(v) => v,
            Option::None => default,
        }
    }

    // `self.and(optb)` returns `optb` when `self` is `Some`, else `None`.
    // The Some payload of `self` is dropped.
    pub fn and<U>(self, optb: Option<U>) -> Option<U> {
        match self {
            Option::Some(_) => optb,
            Option::None => Option::None,
        }
    }

    // `self.or(optb)` returns `self` if it is `Some`, otherwise `optb`.
    pub fn or(self, optb: Option<T>) -> Option<T> {
        match self {
            Option::Some(v) => Option::Some(v),
            Option::None => optb,
        }
    }

    // Returns `Some` if exactly one of `self`/`optb` is `Some`,
    // otherwise `None`.
    pub fn xor(self, optb: Option<T>) -> Option<T> {
        match self {
            Option::Some(v) => match optb {
                Option::Some(_) => Option::None,
                Option::None => Option::Some(v),
            },
            Option::None => match optb {
                Option::Some(v) => Option::Some(v),
                Option::None => Option::None,
            },
        }
    }
}

// `Option<Option<T>>::flatten` — `Some(Some(x))` → `Some(x)`, otherwise
// `None`. A second impl block restricts `T` to `Option<U>` so the
// flatten op is only available on doubly-wrapped options.
impl<T> Option<Option<T>> {
    pub fn flatten(self) -> Option<T> {
        match self {
            Option::Some(inner) => inner,
            Option::None => Option::None,
        }
    }
}

// TODOs — methods we'd want eventually but pocket-rust doesn't yet
// have the language features to express. Listed alphabetically. When
// a blocker lands, search this file for the relevant TODO.
//
// TODO: and_then(self, f) — needs closures (`F: FnOnce(T) -> Option<U>`).
// TODO: as_deref(&self) — needs the `Deref` trait.
// TODO: as_mut(&mut self) -> Option<&mut T> — needs ref-pattern bindings inside `match *self` (or equivalent) to take a `&mut T` to the payload without moving it.
// TODO: as_ref(&self) -> Option<&T> — same as `as_mut`, but for shared refs.
// TODO: cloned(self) — needs the `Clone` trait.
// TODO: copied(self) — needs an `impl<T: Copy> Option<&T>` second impl block; in turn needs the `as_ref`-shaped ref-pattern binding to be expressible.
// TODO: expect(self, msg) — needs `&str` and a panic primitive.
// TODO: filter(self, f) — needs closures.
// TODO: get_or_insert(&mut self, value) — needs `mem::replace` (or a `¤replace` intrinsic) to swap the slot.
// TODO: into_iter(self) / iter(&self) — needs the iterator traits.
// TODO: is_none_or(self, f) / is_some_and(self, f) — needs closures.
// TODO: map(self, f) — needs closures.
// TODO: ok_or(self, err) / ok_or_else(self, f) — needs `Result<T, E>` (and closures for the `_else` form).
// TODO: or_else(self, f) — needs closures.
// TODO: replace(&mut self, value) — needs `mem::replace`.
// TODO: take(&mut self) — needs `mem::replace`.
// TODO: transpose(self) — needs `Result<T, E>`.
// TODO: unwrap(self) / unwrap_or_else(self, f) — `unwrap` needs a panic primitive (or a `¤unreachable::<T>()` builtin to type the None arm; the never type itself is now in place, what's missing is a user-callable intrinsic that emits the wasm `unreachable` instruction with a typed return); `_or_else` additionally needs closures.
// TODO: unwrap_or_default(self) — needs the `Default` trait.
