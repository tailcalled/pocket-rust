---
name: testing-conventions
description: Use when adding/restructuring tests, examples, or test helpers. Covers the `examples/` and `tests/` directory layout, the two-level Cargo wrapper trick, the `expect_answer`/`compile_source`/`compile_sources` helpers, error-message assertion conventions, and naming rules.
---

# testing conventions

Examples and integration tests are organized by **target** (language intrinsic vs. stdlib feature) and within that by **feature**. Each feature is one directory of examples + one test file pair.

## Layout

- `examples/lang/<feature>/<example>/lib.rs` — example code that exercises a language-intrinsic feature (functions, structs, enums, references, traits, patterns, while loops, the `¤` builtin family, …). Compiled and executed via wasmi.
- `examples/std/<feature>/<example>/lib.rs` — example code that exercises an in-language stdlib type/trait (`Copy`, `Drop`, `Num`, the `cmp` traits and operator desugar).
- `tests/lang.rs` (binary) → `tests/lang/mod.rs` — driver + shared helpers; declares `mod basics; mod structs; mod enums; ...`. Each `tests/lang/<feature>.rs` file holds positive and negative tests for that feature: positive tests call `expect_answer("lang/<feature>/<example>", expected)`; negative tests call `compile_source(inline_src)` and assert the error message contains the expected substring.
- `tests/std.rs` (binary) → `tests/std/mod.rs` — same shape, with `mod copy; mod drop; mod num; mod cmp;`.

## Two-level wrapper rationale

The two-level wrapper (`tests/lang.rs` containing only `#[path = "lang/mod.rs"] mod lang;`) is needed because Cargo only auto-discovers `tests/*.rs` files as binaries, but mod-path resolution from a crate root looks for `tests/<name>.rs` rather than `tests/lang/<name>.rs`. Reaching into `tests/lang/mod.rs` via `#[path]` lets the inner `mod foo;` declarations resolve relatively to `tests/lang/`, which is the natural layout.

## Naming

Test file names avoid Rust keywords — `let_stmts.rs` (not `let.rs`), `if_exprs.rs`, `while_loops.rs`, `enums.rs`, `structs.rs`. The `std` wrapper module imports as `mod stdlib` so the `tests/std.rs` binary's submodule isn't named `std_`.

## Helpers

- `expect_answer(dir, expected)` — compiles the example at `examples/<dir>/lib.rs`, runs `answer()` via wasmi, asserts the result equals `expected`. Used for positive tests.
- `compile_source(source)` — compiles a single inline source string as `main.rs`, returns the formatted error string (or panics on success). Used for negative tests; assert the error message contains the expected substring.
- `compile_sources(files)` — same but for multi-file sources (when testing module/use behaviour). Pass `&[(&str, &str)]` of `(path, source)` pairs.

## Positive + negative pairing

Every feature change adds both:
- A positive test (`expect_answer(...)` against an example) — proves the feature works at runtime.
- A negative test (`compile_source(...)` returning an error string, asserted via substring match) — proves the compiler rejects misuse with a sensible message.

When the misuse case has *several* shapes (wrong arity, wrong type, wrong position, …), pick the shapes most likely to be written by a confused user and pin those error messages down.

The positive tests live in `examples/<area>/<feature>/<example>/lib.rs`; the negative tests are inline `compile_source(...)` calls in the same `tests/<area>/<feature>.rs` file.

## Panic stub

The test harness (`tests/lang/mod.rs` and `tests/std/mod.rs`) registers a `panic` stub via `wasmi::Linker::define` that traps execution; production hosts can print + abort. This is what makes `expect_answer` on a panicking example surface as a wasmi runtime trap.
