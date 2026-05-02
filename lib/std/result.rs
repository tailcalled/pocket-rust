// `Result<T, E>` — either an `Ok(T)` success value or an `Err(E)`
// error value. The canonical error-handling type; once pocket-rust
// gains the `?` operator, calls returning `Result` will be the way to
// propagate failures.

use crate::option::Option;

pub enum Result<T, E> {
    Ok(T),
    Err(E),
}

impl<T, E> Result<T, E> {
    // Returns true if the result is `Ok`.
    pub fn is_ok(&self) -> bool {
        match self {
            &Result::Ok(_) => true,
            &Result::Err(_) => false,
        }
    }

    // Returns true if the result is `Err`.
    pub fn is_err(&self) -> bool {
        match self {
            &Result::Ok(_) => false,
            &Result::Err(_) => true,
        }
    }

    // Consumes the result and returns the inner Ok value if present,
    // otherwise the supplied `default`.
    pub fn unwrap_or(self, default: T) -> T {
        match self {
            Result::Ok(v) => v,
            Result::Err(_) => default,
        }
    }

    // Converts `Result<T, E>` to `Option<T>`, dropping the Err if any.
    pub fn ok(self) -> Option<T> {
        match self {
            Result::Ok(v) => Option::Some(v),
            Result::Err(_) => Option::None,
        }
    }

    // Converts `Result<T, E>` to `Option<E>`, dropping the Ok if any.
    pub fn err(self) -> Option<E> {
        match self {
            Result::Ok(_) => Option::None,
            Result::Err(e) => Option::Some(e),
        }
    }

    // `self.and(res)` returns `res` when `self` is `Ok`, else
    // `self`'s `Err`. The Ok payload of `self` is dropped. The error
    // type is shared between `self` and `res`; the Ok types differ.
    pub fn and<U>(self, res: Result<U, E>) -> Result<U, E> {
        match self {
            Result::Ok(_) => res,
            Result::Err(e) => Result::Err(e),
        }
    }

    // `self.or(res)` returns `self` if it is `Ok`, otherwise `res`.
    // Mirrors `Option::or` shape but allows the new `Err` type to
    // differ from `self`'s.
    pub fn or<F>(self, res: Result<T, F>) -> Result<T, F> {
        match self {
            Result::Ok(v) => Result::Ok(v),
            Result::Err(_) => res,
        }
    }
}

// `Result<T, !>::into_ok` — when the error type is `!` (uninhabited),
// an `Err` variant can never be constructed. Calling `into_ok`
// extracts the `Ok` payload without needing to handle errors.
// Exhaustiveness sees the Err arm as uninhabited (its payload is `!`)
// and accepts the match without it.
impl<T> Result<T, !> {
    pub fn into_ok(self) -> T {
        match self {
            Result::Ok(v) => v,
        }
    }
}

// `Result<!, E>::into_err` — symmetric: when the ok type is `!`, the
// `Ok` variant can't be constructed, and the match needs only the Err
// arm.
impl<E> Result<!, E> {
    pub fn into_err(self) -> E {
        match self {
            Result::Err(e) => e,
        }
    }
}

// `Result<Result<T, E>, E>::flatten` — `Ok(Ok(x))` → `Ok(x)`,
// `Ok(Err(e))` / `Err(e)` → `Err(e)`. Same shape as
// `Option<Option<T>>::flatten`. The inner and outer `Err` types must
// agree (no closure / collapse-via-conversion yet).
impl<T, E> Result<Result<T, E>, E> {
    pub fn flatten(self) -> Result<T, E> {
        match self {
            Result::Ok(inner) => inner,
            Result::Err(e) => Result::Err(e),
        }
    }
}

// `Result<Option<T>, E>::transpose` — swap the layering:
// `Ok(Some(x))` → `Some(Ok(x))`, `Ok(None)` → `None`, `Err(e)` → `Some(Err(e))`.
// Useful for adapting a fallible producer of optional values into the
// `Option` layer.
impl<T, E> Result<Option<T>, E> {
    pub fn transpose(self) -> Option<Result<T, E>> {
        match self {
            Result::Ok(opt) => match opt {
                Option::Some(v) => Option::Some(Result::Ok(v)),
                Option::None => Option::None,
            },
            Result::Err(e) => Option::Some(Result::Err(e)),
        }
    }
}

// TODOs — methods we'd want eventually but pocket-rust doesn't yet
// have the language features to express. Listed alphabetically. When
// a blocker lands, search this file for the relevant TODO.
//
// TODO: and_then(self, f: F) — needs closures (`F: FnOnce(T) -> Result<U, E>`). The `?` operator desugars to a pattern that's morally `match self { Ok(v) => v, Err(e) => return Err(From::from(e)) }`; if pocket-rust gains `From`/conversions later, threading them through here is the natural extension.
// TODO: as_deref(&self) — needs the `Deref` trait.
// TODO: as_mut(&mut self) -> Result<&mut T, &mut E> — needs ref-pattern bindings inside a match without moving the payload.
// TODO: as_ref(&self) -> Result<&T, &E> — same shape as `as_mut`, shared.
// TODO: cloned(self) / copied(self) — needs `Clone`/`Copy` constraints on inner refs (alongside `as_ref`).
// TODO: expect(self, msg) / expect_err(self, msg) — need `&str` plumbing and a panic primitive (currently no `unreachable!` / `panic!` in pocket-rust).
// TODO: inspect(self, f) / inspect_err(self, f) — need closures.
// TODO: into_iter(self) / iter(&self) / iter_mut(&mut self) — need iterator traits.
// TODO: is_err_and(self, f) / is_ok_and(self, f) — need closures.
// TODO: map(self, f: F) / map_err(self, f) — need closures (`F: FnOnce(T) -> U`).
// TODO: map_or(self, default, f) / map_or_else(self, default_f, f) — need closures.
// TODO: or_else(self, f) — needs closures.
// TODO: unwrap(self) / unwrap_err(self) — need a panic primitive and (for `unwrap_err`) `T: Debug`-equivalent formatting.
// TODO: unwrap_or_default(self) — needs the `Default` trait.
// TODO: unwrap_or_else(self, f) — needs closures.
