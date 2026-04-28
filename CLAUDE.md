pocket-rust
===

`pocket-rust` is a minimalist compiler for a subset of the Rust programming language. It targets WebAssembly only.

## Why

The real Rust compiler (`rustc`) is too complex to run in WebAssembly. `pocket-rust` is a from-scratch, minimal Rust-subset compiler small enough that its own subset can express it — so it can eventually self-host inside WASM.

## Architecture

- `src/lib.rs` — public surface: `Vfs` and `compile`. Drives the pipeline and resolves modules across files. **No I/O.**
- `src/span.rs` — `Pos { line, col }`, `Span { start, end }` (both `Pos`), `Error { file, message, span }`, and `format_error(&Error) -> String`.
- `src/lexer.rs` — `tokenize(file, source) -> Vec<Token>`. Tokens carry a `Span`; line/column are tracked as the lexer scans, not derived after the fact.
- `src/ast.rs` — resolved AST node types (`Module`, `Item`, `Function`, `StructDef`, `Type`, `Block`, `Expr`, `Path`, `Call`, `StructLit`, `FieldAccess`); a `Module` is recursive (it may contain submodules) and carries its `source_file`.
- `src/parser.rs` — `parse(file, Vec<Token>) -> Vec<RawItem>`. Recursive-descent, owns its tokens by value to avoid lifetime parameters. Emits `RawItem::Function`, `RawItem::Struct`, or `RawItem::ModDecl` for one file's worth of items; module resolution happens above it.
- `src/typeck.rs` — `check(&Module) -> Result<(StructTable, FuncTable), Error>`. Owns the resolved-type vocabulary: `RType` (`Usize`, `Struct(absolute_path)`, or `Ref(Box<RType>)`), the `StructTable`, the `FuncTable`, and the path/type helpers (`rtype_clone`, `rtype_eq`, `rtype_size`, `flatten_rtype`, `resolve_type`, `is_copy`, `clone_path`, `path_eq`, `place_to_string`, `segments_to_string`). Builds the tables (struct-name pass → struct-field-type pass → function-collection pass; rejects `&T` in struct fields and return types), then walks every function body validating types end-to-end (variable scope, call arity + arg types, struct-literal completeness/duplicates/types, return-type match, field access on structs or `&Struct`, no move-out-of-borrow for non-`Copy` fields, usize literal range).
- `src/borrowck.rs` — `check(&Module, &StructTable, &FuncTable) -> Result<(), Error>`. Tracks moves and shared borrows for every function body. Place expressions rooted in an owned (non-`Ref`) local record moves; place expressions rooted in a `Ref` local don't track (refs are `Copy`). `&place` expressions record borrows. A move conflicts with any prior move *or* borrow on an overlapping path (prefix); a borrow conflicts only with prior moves (shared borrows can stack). Borrows of non-place expressions (e.g. `&fresh_struct_lit()`) need no tracking.
- `src/codegen.rs` — `codegen(&Module, &StructTable, &FuncTable) -> Result<wasm::Module, Error>`. Trusts that `typeck` and `borrowck` have already accepted the program; uses `unreachable!`/`expect` for cases the earlier passes would have caught. Builds each function's locals from the `FuncTable`'s param types, walks the body emitting instructions, and uses a stash-and-restore dance over freshly allocated locals to extract a field's sub-range from a stack-resident struct value.
- `src/wasm.rs` — structured representation of WASM constructs (`Module`, `FuncType`, `Export`, `FuncBody` with both declared locals and instructions, `Instruction` including `Drop`/`LocalGet`/`LocalSet`/`Call`/`I32Const`) plus their byte encoding (`Module::encode`). Includes uLEB128 / sLEB128 writers and per-section encoders. Pure data + encoding logic.
- `src/main.rs` — I/O shell: reads files, parses argv, writes output. Allowed to use any `std` feature; will not run inside WASM.
- `lib/std/` — pocket-rust's own (in-language) standard library. Files here are embedded into the compiler binary via `include_str!` from `src/lib.rs` and injected into every program as a top-level module named `std`. Currently contains `lib/std/lib.rs` (declares submodules) and `lib/std/dummy.rs` (a placeholder `fn id(x: usize) -> usize { x }`).
- `tests/` — integration tests; allowed to do I/O.

Pipeline: `main` populates a `Vfs` (virtual filesystem: `Vec<File>` keyed by forward-slash relative path) and calls `compile(&vfs, entry) -> Result<wasm::Module, String>`. `compile` recursively resolves modules — for each `mod NAME;` it sees in a file, it looks up `NAME.rs` in the same directory of the VFS, parses it, and links it into the AST as a submodule. After resolving the user crate, `compile` builds the standard library from its own embedded VFS (the files under `lib/std/`) using the same resolver, names the resulting `Module` `std`, and pushes it into the user crate root's items so user code can reach it as `std::dummy::id(...)` etc. With the augmented AST in hand, `compile` runs **typeck → borrowck → codegen** in that order, failing fast on the first error. The caller turns the resulting module into bytes via `module.encode()`. Keeping `compile` returning a structured value (instead of `Vec<u8>`) means tests and other consumers can inspect the WASM tree without re-parsing, and the encoder is the single place that knows the binary format.

Standard library errors are attributed to a synthetic file path of the form `<std>/<file>.rs` (e.g. `<std>/lib.rs:3:5: ...`) so they're distinguishable from errors in the user's own files. Stdlib functions are compiled into every program; only crate-root user functions are exported, so `std::dummy::id` lives in the WASM module but isn't reachable by name from the host.

The pass split is meant to keep responsibilities clean as the language grows: type-shape questions live in `typeck.rs`, ownership/borrow questions in `borrowck.rs`, and `codegen.rs` is allowed to assume the AST it sees is well-formed. Each phase walks the AST independently — codegen recomputes types as it walks for layout decisions but never reports type errors; if it would, that's a missing check in `typeck`.

## Error reporting

Errors flow through a structured `span::Error { file, message, span }`. `Span` is built from `Pos { line, col }` pairs that the lexer tracks while scanning — no after-the-fact byte-offset → line/col conversion. The lexer/parser embed the `file` into errors directly; codegen tracks the current source file as it walks the AST so cross-module errors are attributed to the right file. `compile` formats errors in the standard `<file>:<line>:<col>: <message>` shape so editors can jump to the location. Integration tests assert the prefix of each kind of error (lex, parse, codegen, missing module file, unresolved call) to keep the wiring honest.

## Bootstrapping discipline

Every data structure or language feature used inside `lib.rs` becomes something pocket-rust-the-language must eventually support. In `lib.rs`, prefer:

- `Vec<Entry>` with linear scan over `HashMap` / `BTreeMap`.
- Plain structs and enums over trait-heavy abstractions.
- `while`-with-index over iterator chains when it's a wash.

Performance for small N is not a reason to reach for complex collections. This applies to `lib.rs` only — `main.rs`, tests, and *user code being compiled by pocket-rust* are unconstrained.

## CLI

```
pocket-rust <input-dir> <output.wasm>
```

Walks `<input-dir>` recursively for `*.rs` files, populates the `Vfs`, calls `compile(&vfs, "main.rs")`, writes the bytes.

## Tests

Examples live in `examples/<name>/`. Integration tests live in `tests/`, split by what they're checking:

- `tests/examples.rs` — positive tests. Read an example dir into a `Vfs`, call `compile`, and validate the output by handing it to a real WASM engine (`wasmi`) — never by byte-for-byte comparison. For functions, instantiate and invoke them and check return values.
- `tests/errors.rs` — error-shape tests. Feed inline source through `compile`, assert the returned message starts with the expected `<file>:<line>:<col>:` prefix.

## Status

The Rust subset currently supported:

- Functions: `fn NAME(P1: T1, P2: T2, …)` with an optional return type. No visibility modifiers, no attributes, no generics.
- Function body: a block whose only contents are an optional tail expression.
- Expressions: usize integer literals (parsed as `u64`, range-checked into `u32`, emitted as `i32.const`); function calls of the form `path::to::func(arg, arg, …)`; bare-identifier variable references that resolve to the enclosing function's parameters (emitted as `local.get`); struct literals `Path { field: expr, … }`; field access `expr.field`, chainable; `&expr` to take a shared reference.
- Types: `usize` (1 i32), structs (recursively flattened — a `Point { x: usize, y: usize }` is 2 i32s; a `Diagram` containing two `Rect`s containing two `Point`s each is 8 i32s), and `&T` (same WASM layout as `T` — references are a pure compile-time concept since we have no linear memory). All function params and return values that contain structs become multi-value WASM signatures.
- Modules: `mod NAME;` at any module scope. The compiler looks up `NAME.rs` in the same directory as the declaring file. No inline `mod NAME { … }` syntax yet, no nested-directory resolution beyond same-dir siblings, no `use`/`pub`/`super::`/`crate::`.
- Structs: `struct NAME { field: Type, … }`. No tuple structs, no unit structs, no methods, no `impl` blocks, no generics, no derive. Struct fields cannot be reference types.
- References: shared references `&T` are allowed in parameter types and as `&expr` expressions. They are forbidden in struct fields and return types (sidesteps lifetime annotations). Field access through `&T` is allowed only for `Copy` fields (`usize`, `&U`); accessing a struct field through a reference is rejected as "cannot move out of borrow". There is no explicit dereference operator (`*r`); auto-deref handles `r.field`.

Path resolution: every path in an expression is interpreted relative to the module containing the call. Single-segment identifiers without `(...)`/`{...}` are variable references; with `(...)` they're calls; with `{...}` they're struct literals. Multi-segment paths must be calls or struct literals. Only top-level (crate-root) functions are exported under their bare name.

Move and borrow tracking (in `borrowck.rs`): a pre-pass over each function body records every move and every `&place`. Two paths conflict if one is a prefix of the other. A move conflicts with any prior move *or* still-active borrow on an overlapping path; a borrow conflicts only with prior moves (shared borrows stack).

Borrow scope is *per `Call`*: borrows added while evaluating a call's arguments are dropped after the call (a call-level approximation of Rust's "borrow ends at last use"). Moves are permanent for the function body. So `f(&p, p.y)` rejects (the borrow and the move are siblings in the same arg list, both alive at once), but `Pair { first: x_of(&p), second: p.y }` is accepted: the `&p` borrow dies inside `x_of`'s call before `p.y` is evaluated. Multiple shared borrows of the same place across separate calls are also fine.

Reads of `&T`-typed locals don't record moves (refs are `Copy`), and field-access chains rooted in a `&T` local are likewise treated as non-moving. So `Rect { top_left: d.primary.top_left, bottom_right: d.secondary.bottom_right }` is fine (disjoint paths), `Rect { top_left: d.primary, bottom_right: d.primary.top_left }` errors at the second use.

Field access codegen: for `expr.field`, the base is fully evaluated onto the stack (always — there's no place-expression optimisation yet), then a stash-and-restore over freshly allocated locals extracts the desired range. Each `FieldAccess` allocates new temp locals; we don't reuse. Chains like `expr.a.b.c` produce one stash-restore per `.`; an obvious future optimisation is to fold a chain into a single extraction at the cumulative offset/size.

Type checking (in `typeck.rs`): every call's arguments must match the callee's parameter types, every struct-literal field initializer must match its declared field type, and a function's tail expression must match the declared return type. Field access on a non-struct value (`expr.field` where `expr` is `usize`) is rejected. Duplicate function/struct paths aren't detected; the relevant lookup returns the first match.
