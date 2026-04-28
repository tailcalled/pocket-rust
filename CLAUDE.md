pocket-rust
===

`pocket-rust` is a minimalist compiler for a subset of the Rust programming language. It targets WebAssembly only.

## Why

The real Rust compiler (`rustc`) is too complex to run in WebAssembly. `pocket-rust` is a from-scratch, minimal Rust-subset compiler small enough that its own subset can express it — so it can eventually self-host inside WASM.

## Architecture

- `src/lib.rs` — public surface: `Vfs` and `compile`. Drives the pipeline and resolves modules across files. **No I/O.**
- `src/span.rs` — `Pos { line, col }`, `Span { start, end }` (both `Pos`), `Error { file, message, span }`, and `format_error(&Error) -> String`.
- `src/lexer.rs` — `tokenize(file, source) -> Vec<Token>`. Tokens carry a `Span`; line/column are tracked as the lexer scans, not derived after the fact.
- `src/ast.rs` — resolved AST node types (`Module`, `Item`, `Function`, `StructDef`, `Type`, `Block`, `Stmt`, `LetStmt`, `Expr`, `Path`, `Call`, `StructLit`, `FieldAccess`); a `Module` is recursive (it may contain submodules) and carries its `source_file`. A `Block` is a list of statements (currently just `let`) followed by an optional tail expression.
- `src/parser.rs` — `parse(file, Vec<Token>) -> Vec<RawItem>`. Recursive-descent, owns its tokens by value to avoid lifetime parameters. Emits `RawItem::Function`, `RawItem::Struct`, or `RawItem::ModDecl` for one file's worth of items; module resolution happens above it.
- `src/typeck.rs` — `check(&Module, &mut StructTable, &mut FuncTable, &mut next_idx) -> Result<(), Error>`. Constraint-based type inference. Owns the resolved-type vocabulary: `RType` is `Int(IntKind)` for one of u8/i8/u16/i16/u32/i32/u64/i64/u128/i128/usize/isize, `Struct(absolute_path)`, or `Ref(Box<RType>)`. `StructTable`/`FuncTable`/`FnSymbol` store concrete `RType`s. Inference uses an internal `InferType` (with `Var(u32)`) and a `Subst` whose vars carry an "is_integer" flag; integer literals create fresh integer-class vars. The walk collects unification constraints (`Eq`-style — applied immediately) and per-literal value/range constraints. Unifying an integer-class var with a non-integer concrete type fails immediately with `expected \`X\`, got integer`. After body walk, any still-unbound integer-class var defaults to `I32`; each literal's value is then range-checked against its resolved type. `FnSymbol.let_types` and `lit_types` get the post-solve concrete types, indexed in source-DFS order; codegen consumes them in that same order.
- `src/borrowck.rs` — `check(&Module, &StructTable, &FuncTable) -> Result<(), Error>`. A holder-based borrow checker. Each function-body walk maintains a stack of *holders*: named bindings (params + `let`s) and synthetic call slots. Each holder records which place paths it currently keeps borrowed. Walking an expression returns a `ValueDesc` listing the borrows the expression's value carries; the caller (let, call slot, block tail) decides what to do with them. A borrow stays alive as long as some holder holds it; when the holder is dropped (binding goes out of scope, call slot popped), the borrow goes with it. Move/borrow conflicts use prefix overlap on place paths.
- `src/codegen.rs` — `emit(&mut wasm::Module, &Module, &StructTable, &FuncTable) -> Result<(), Error>`. Appends to an existing `wasm::Module` rather than constructing a fresh one — same accumulating shape as `typeck::check`, so libraries' functions land in the WASM module first and user functions follow. Trusts that `typeck` and `borrowck` have already accepted the program; uses `unreachable!`/`expect` for cases the earlier passes would have caught. Builds each function's locals from the `FuncTable`'s param types, walks the body emitting instructions, and uses a stash-and-restore dance over freshly allocated locals to extract a field's sub-range from a stack-resident struct value. Same `push_root_name` treatment of the crate root, so only user crate-root functions get exported.
- `src/wasm.rs` — structured representation of WASM constructs (`Module`, `FuncType`, `Export`, `FuncBody` with both declared locals and instructions, `Instruction` including `Drop`/`LocalGet`/`LocalSet`/`Call`/`I32Const`) plus their byte encoding (`Module::encode`). Includes uLEB128 / sLEB128 writers and per-section encoders. Pure data + encoding logic.
- `src/main.rs` — I/O shell: reads files, parses argv, writes output. Loads `lib/std/` from disk and passes it to `compile` as a `Library`. Allowed to use any `std` feature; will not run inside WASM.
- `lib/std/` — pocket-rust's own (in-language) standard library. **Not referenced from `src/`.** It's a regular directory of `.rs` files that the host (currently `main.rs` and the test helpers) loads from disk and hands to `compile` as one of its `libraries`. Currently contains `lib/std/lib.rs` (declares submodules) and `lib/std/dummy.rs` (a placeholder `fn id(x: usize) -> usize { x }`).
- `tests/` — integration tests; allowed to do I/O.

Pipeline: `main` populates a `Vfs` per crate (virtual filesystem: `Vec<File>` keyed by forward-slash relative path) and calls `compile(libraries, &user_vfs, user_entry) -> Result<wasm::Module, String>`. The `libraries` slice is a list of `Library { name, vfs, entry }` values — pre-existing crates that the user crate can reference. `compile` processes each library in order, then the user crate. For each crate it: resolves modules (following `mod NAME;` declarations to siblings in that crate's VFS), runs typeck (extending shared `StructTable`/`FuncTable`), borrowck, and codegen (appending to the shared `wasm::Module`). The final `wasm::Module` is returned; the caller turns it into bytes via `module.encode()`. The library system is fully generic: `lib.rs` doesn't know anything about `std` specifically — `main.rs` is the one place that decides to load `lib/std/` and pass it as a library. Other hosts could pass different libraries, multiple libraries, or none.

The crate root's `name` drives its path prefix: a library is created with `name = "std"` so its items live at `["std", ...]`, while the user crate has `name = ""` so its items live at the empty prefix. The "export iff `current_module.is_empty()`" rule in codegen then naturally exports user crate-root functions and never library functions (libraries' top-level functions sit at `["std"]`, etc., so they're not exported even though they're emitted into the WASM module). Errors in library code are attributed to the file paths the library's VFS was populated with (e.g. `lib.rs`, `dummy.rs`) — not synthetic `<std>/...` paths.

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
- Function body: a block — a sequence of `let` statements followed by an optional tail expression. (No expression statements yet.)
- Statements: `let NAME = EXPR;` and `let NAME: TYPE = EXPR;`. The optional type annotation is checked against the value's inferred type. The bound name is in scope for subsequent statements and the tail expression. No `mut`, no patterns, no shadowing checks (a duplicate name within a scope shadows incompletely — move tracking can't tell two same-named bindings apart).
- Expressions: usize integer literals (parsed as `u64`, range-checked into `u32`, emitted as `i32.const`); function calls of the form `path::to::func(arg, arg, …)`; bare-identifier variable references that resolve to the enclosing function's parameters or `let` bindings (emitted as `local.get`); struct literals `Path { field: expr, … }`; field access `expr.field`, chainable; `&expr` to take a shared reference; block expressions `{ stmts; tail_expr }` whose value is the tail expression.
- Block expressions: same shape as a function body block, but the tail expression is **required** (we don't have a unit value yet). Each block introduces its own local scope — `let` bindings inside don't escape. Borrows created inside a block expression follow the same call-scoped rule as anywhere else.
- Types: integers (u8, i8, u16, i16, u32, i32, u64, i64, u128, i128, usize, isize), structs, and `&T`. WASM layout: ≤32-bit integers (and usize/isize, since we target wasm32) flatten to 1 `i32`; 64-bit integers flatten to 1 `i64`; 128-bit integers flatten to two `i64`s (low half then high half). Structs flatten in declaration order, recursively — a `Point { x: u32, y: u64 }` is `[i32, i64]`. `&T` has the same layout as `T` (refs are a pure compile-time concept; we have no linear memory). All function params and return values that flatten to more than one WASM scalar become multi-value signatures.
- Integer literals are inferred from context. A bare `42` gets a fresh integer-class type variable; the variable unifies with whatever owns it (the let annotation, the param type at a call site, the field type in a struct literal, the function's return type, …). If no constraint pins the variable down, it defaults to `i32` (Rust's convention) and is then range-checked. So `fn answer() -> u8 { 42 }` puts `42` into u8; `fn answer() -> i64 { 9_000_000_000 }` puts it into i64; `let x = 5` with no other use defaults `x` to i32.
- Modules: `mod NAME;` at any module scope. The compiler looks up `NAME.rs` in the same directory as the declaring file. No inline `mod NAME { … }` syntax yet, no nested-directory resolution beyond same-dir siblings, no `use`/`pub`/`super::`/`crate::`.
- Structs: `struct NAME { field: Type, … }`. No tuple structs, no unit structs, no methods, no `impl` blocks, no generics, no derive. Struct fields cannot be reference types.
- References: shared references `&T` are allowed in parameter types and as `&expr` expressions. They are forbidden in struct fields and return types (sidesteps lifetime annotations). Field access through `&T` is allowed only for `Copy` fields (`usize`, `&U`); accessing a struct field through a reference is rejected as "cannot move out of borrow". There is no explicit dereference operator (`*r`); auto-deref handles `r.field`.

Path resolution: every path in an expression is interpreted relative to the module containing the call. Single-segment identifiers without `(...)`/`{...}` are variable references; with `(...)` they're calls; with `{...}` they're struct literals. Multi-segment paths must be calls or struct literals. Only top-level (crate-root) functions are exported under their bare name.

Move and borrow tracking (in `borrowck.rs`): walk every function body with a stack of holders.

- A *holder* is either a named binding (a function parameter or a `let`) or a synthetic *call slot* pushed for the duration of a function call's argument evaluation. Each holder records the list of place paths it currently keeps borrowed.
- Walking an expression returns a `ValueDesc { borrows: Vec<Path> }`. `&place` produces a desc carrying that place. Reading a ref-typed `Var` produces the desc of borrows the binding currently holds (refs are `Copy`, so reads don't move). Everything else produces an empty desc — calls and struct literals can't return references, so their values never carry borrows out.
- The caller of `walk_expr` chooses what to do with the desc: a `let` makes a new binding holder absorb the desc's borrows; a call's argument absorbs them into the synthetic call holder; a block-expression's tail returns its desc up to whatever consumes the block. When a holder is dropped (block scope ends, call returns, function ends), the borrows it kept alive die with it.
- A move of place `P` is a conflict if any prior move or any *currently held* borrow shares a path prefix with `P`. A `&P` is a conflict if `P` has been moved.
- Reads of owned-type locals are tracked as moves (current overstrictness — `usize` should be `Copy` but we don't honor that yet).

Concretely:

- `f(&p, p.y)` rejects: while evaluating arg 2, the call slot still holds `[p]` from arg 1, so the partial-move on `[p, y]` conflicts.
- `Pair { first: x_of(&p), second: p.y }` is accepted: `x_of(&p)` is itself a call, and *its* call slot holds `[p]` only until `x_of` returns, after which the call slot is popped and the borrow is gone before `p.y` is evaluated.
- `let pt2 = { let pt3 = &pt1; pt3 }` keeps the borrow alive past the inner block: `pt3`'s holder is dropped when the inner scope ends, but the block's tail desc carries `[pt1]` up to the outer `let pt2`, which then becomes a holder. A subsequent `let invalid = pt1;` correctly rejects.
- `let v = { let r = &pt1; r.x }` accepts a subsequent `let q = pt1;`: the inner block's tail is `r.x` (a `usize`, copied through the ref), so its desc is empty. When the inner scope ends, `r` is dropped, the `[pt1]` borrow has no holder left, and the move is allowed.

Local types from `let` statements are computed during typeck (in source-DFS order across the whole function body, including lets nested inside block expressions and struct-literal field values) and stored on `FnSymbol.let_types`. Borrowck reads them in lock-step using a `let_idx` counter that walks the AST in the same order. Codegen recomputes the types as it walks (since `codegen_expr` already returns the type of each expression) and uses them to size the WASM locals it allocates for each binding. Because typeck and borrowck must agree on this order, struct-literal fields are type-checked in *source* order (matching borrowck), even though codegen emits them in *definition* order to keep the WASM stack layout right.

Each pass scopes a block expression by saving `locals.len()` (typeck/codegen) or `holders.len()` (borrowck) before entering and truncating back on exit, so let bindings inside a block aren't visible outside. The WASM locals allocated for those bindings remain in the function (we don't reuse local slots across scopes), but they're harmlessly unreferenced after the block ends.

The remaining looseness vs Rust: reads of `Copy`-typed owned locals (any integer type, plus refs) are still tracked as moves; only refs currently get the "no-move" treatment. So `f(a, a)` where `a: u32` rejects in pocket-rust though Rust would copy. Fixing this is a one-line widening of `is_ref_holder` to `is_copy_holder` (using `typeck::is_copy`) — held off because several existing error tests assert the strict behavior on owned-int double-uses.

Reads of `&T`-typed locals don't record moves (refs are `Copy`), and field-access chains rooted in a `&T` local are likewise treated as non-moving. So `Rect { top_left: d.primary.top_left, bottom_right: d.secondary.bottom_right }` is fine (disjoint paths), `Rect { top_left: d.primary, bottom_right: d.primary.top_left }` errors at the second use.

Field access codegen: for `expr.field`, the base is fully evaluated onto the stack (always — there's no place-expression optimisation yet), then a stash-and-restore over freshly allocated locals extracts the desired range. Each `FieldAccess` allocates new temp locals; we don't reuse. Chains like `expr.a.b.c` produce one stash-restore per `.`; an obvious future optimisation is to fold a chain into a single extraction at the cumulative offset/size.

Type checking (in `typeck.rs`): every call's arguments must match the callee's parameter types, every struct-literal field initializer must match its declared field type, and a function's tail expression must match the declared return type. Field access on a non-struct value (`expr.field` where `expr` is `usize`) is rejected. Duplicate function/struct paths aren't detected; the relevant lookup returns the first match.
