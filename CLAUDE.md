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
- `src/borrowck.rs` — `check(&Module, &StructTable, &FuncTable) -> Result<(), Error>`. A holder-based borrow checker. Each function-body walk maintains a stack of *holders*: named bindings (params + `let`s) and synthetic call slots. Each holder records which place paths it currently keeps borrowed. Walking an expression returns a `ValueDesc` listing the borrows the expression's value carries; the caller (let, call slot, block tail) decides what to do with them. A borrow stays alive as long as some holder holds it; when the holder is dropped (binding goes out of scope, call slot popped), the borrow goes with it. Move/borrow conflicts use prefix overlap on place paths. Casts to raw pointer types drop borrow tracking (raw pointers carry no compile-time lifetime). Deref-rooted assignments (`*p = …;`, `(*p).f = …;`) skip the conflict scan — typeck's exclusivity invariant on `&mut T` covers them, and `*mut T` is unsafe and out of scope for borrowck.
- `src/safeck.rs` — `check(&Module, &FuncTable) -> Result<(), Error>`. Single rule: dereferencing a raw pointer (`*const T` / `*mut T`) requires being inside an `unsafe { … }` block. Doesn't redo type analysis — typeck records, per `Deref` expression in source-DFS order, whether the operand was a raw pointer (`FnSymbol.deref_is_raw`); safeck walks the AST in lockstep advancing a `deref_idx` and tracks an `in_unsafe` boolean across `unsafe` boundaries.
- `src/codegen.rs` — `emit(&mut wasm::Module, &Module, &StructTable, &FuncTable) -> Result<(), Error>`. Appends to an existing `wasm::Module` rather than constructing a fresh one — same accumulating shape as `typeck::check`, so libraries' functions land in the WASM module first and user functions follow. Trusts that `typeck` and `borrowck` have already accepted the program; uses `unreachable!`/`expect` for cases the earlier passes would have caught. Each function gets a per-call frame on a *shadow stack* (a region of linear memory tracked by a global SP) for any binding whose address is taken; everything else lives in WASM locals as flat scalars. Field access on stack-resident struct values still uses a stash-and-restore dance over freshly allocated locals; field access through references becomes a direct `iN.load` against the ref's i32 with the field offset folded into the load's immediate.
- `src/wasm.rs` — structured representation of WASM constructs (`Module`, `FuncType`, `Memory`, `Global`, `Export`, `FuncBody`, `Instruction` covering const/get/set/call/drop, arithmetic on `i32`, and load/store with `align`/`offset` immediates) plus their byte encoding (`Module::encode`). Includes uLEB128 / sLEB128 writers and per-section encoders for type / function / memory / global / export / code sections. Pure data + encoding logic.
- `src/main.rs` — I/O shell: reads files, parses argv, writes output. Loads `lib/std/` from disk and passes it to `compile` as a `Library`. Allowed to use any `std` feature; will not run inside WASM.
- `lib/std/` — pocket-rust's own (in-language) standard library. **Not referenced from `src/`.** It's a regular directory of `.rs` files that the host (currently `main.rs` and the test helpers) loads from disk and hands to `compile` as one of its `libraries`. Currently contains `lib/std/lib.rs` (declares submodules) and `lib/std/dummy.rs` (a placeholder `fn id(x: usize) -> usize { x }`).
- `tests/` — integration tests; allowed to do I/O.

Pipeline: `main` populates a `Vfs` per crate (virtual filesystem: `Vec<File>` keyed by forward-slash relative path) and calls `compile(libraries, &user_vfs, user_entry) -> Result<wasm::Module, String>`. The `libraries` slice is a list of `Library { name, vfs, entry }` values — pre-existing crates that the user crate can reference. `compile` processes each library in order, then the user crate. For each crate it: resolves modules (following `mod NAME;` declarations to siblings in that crate's VFS), runs typeck (extending shared `StructTable`/`FuncTable`), borrowck, safeck, and codegen (appending to the shared `wasm::Module`). The final `wasm::Module` is returned; the caller turns it into bytes via `module.encode()`. The library system is fully generic: `lib.rs` doesn't know anything about `std` specifically — `main.rs` is the one place that decides to load `lib/std/` and pass it as a library. Other hosts could pass different libraries, multiple libraries, or none.

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
- Function body: a block — a sequence of `let` and assignment statements followed by an optional tail expression. (No bare expression statements yet.)
- Statements:
  - `let NAME = EXPR;` / `let NAME: TYPE = EXPR;` — immutable binding. Optional annotation is unified with the value's inferred type.
  - `let mut NAME = EXPR;` / `let mut NAME: TYPE = EXPR;` — mutable binding (eligible for assignment).
  - `PLACE = EXPR;` — assignment. The LHS must be a place expression (a `Var` or a `Var`-rooted `FieldAccess` chain). For a whole-binding assignment (`x = …;`) the binding must be declared `mut`. Field assignments are allowed through an owned `mut` binding *or* through a `&mut T` binding (the binding itself need not be `mut` — it's the inner mutability that authorizes the write); through a `&T` binding they're rejected.
  Names are in scope for subsequent statements and the tail expression. No patterns, no shadowing checks.
- Expressions: usize integer literals (parsed as `u64`, range-checked into `u32`, emitted as `i32.const`); function calls of the form `path::to::func(arg, arg, …)`; bare-identifier variable references that resolve to the enclosing function's parameters or `let` bindings (emitted as `local.get`); struct literals `Path { field: expr, … }`; field access `expr.field`, chainable; `&expr` to take a shared reference; block expressions `{ stmts; tail_expr }` whose value is the tail expression.
- Block expressions: same shape as a function body block, but the tail expression is **required** (we don't have a unit value yet). Each block introduces its own local scope — `let` bindings inside don't escape. Borrows created inside a block expression follow the same call-scoped rule as anywhere else.
- Types: integers (u8, i8, u16, i16, u32, i32, u64, i64, u128, i128, usize, isize), structs, `&T`, `&mut T`, `*const T`, `*mut T`. WASM layout: ≤32-bit integers (and usize/isize, since we target wasm32) flatten to 1 `i32`; 64-bit integers flatten to 1 `i64`; 128-bit integers flatten to two `i64`s (low half then high half). Structs flatten in declaration order, recursively — a `Point { x: u32, y: u64 }` is `[i32, i64]`. References and raw pointers are byte addresses into linear memory and flatten to a single `i32` regardless of the pointee's shape. Memory layout (used when a value lives on the shadow stack) is tightly packed in declaration order: `byte_size_of(struct)` is the sum of its fields' byte sizes, with no alignment padding. All function params and return values that flatten to more than one WASM scalar become multi-value signatures.
- Integer literals are inferred from context. A bare `42` gets a fresh integer-class type variable; the variable unifies with whatever owns it (the let annotation, the param type at a call site, the field type in a struct literal, the function's return type, …). If no constraint pins the variable down, it defaults to `i32` (Rust's convention) and is then range-checked. So `fn answer() -> u8 { 42 }` puts `42` into u8; `fn answer() -> i64 { 9_000_000_000 }` puts it into i64; `let x = 5` with no other use defaults `x` to i32.
- Modules: `mod NAME;` at any module scope. The compiler looks up `NAME.rs` in the same directory as the declaring file. No inline `mod NAME { … }` syntax yet, no nested-directory resolution beyond same-dir siblings, no `use`/`pub`/`super::`/`crate::`.
- Structs: `struct NAME { field: Type, … }`. No tuple structs, no unit structs, no methods, no `impl` blocks, no generics, no derive. Struct fields cannot be reference types.
- References: shared `&T` and unique `&mut T` references are allowed in parameter types and as `&expr` / `&mut expr` expressions. They are forbidden in struct fields and return types (sidesteps lifetime annotations); raw pointers `*const T` / `*mut T` fill that gap. Field access through a reference is allowed only for `Copy` fields (any integer, `&U`, `&mut U`, `*const U`, `*mut U`); accessing a non-Copy struct field through a reference is rejected as "cannot move out of borrow". The same Copy rule applies to explicit deref-and-field (`(*p).field`). Borrow conflicts (two `&mut`, or `&mut` + `&` on overlapping places) are rejected at borrowck.

Raw pointers and `unsafe`: `*const T` / `*mut T` are unrestricted compile-time citizens — they may appear in struct fields, return types, parameter types, and locals; they enable recursive types like `struct Node { next: *const Node }`. Cast syntax `expr as Type` is the only way to produce a raw pointer: `&x as *const T` (and `&mut x as *mut T`) for safe-ref → raw-ptr coercion, `*const T as *mut T` (and vice versa) for kind switching, and `0 as *const T` for null. Unary `*` is the deref operator (read or `*p = …;` write); `unsafe { … }` blocks open an unsafe context. The `safeck.rs` pass enforces that any deref of a raw-pointer-typed operand is lexically inside an `unsafe` block — derefs of `&T` / `&mut T` are always safe. Raw pointers are Copy, carry no compile-time lifetime, and don't participate in borrow tracking (the cast-to-raw-pointer site drops the inner borrow).

Reference codegen — real pointers via a shadow stack. A reference value is an `i32` byte address into the module's single linear memory (1 page = 64 KiB, fixed). A `mut i32` global at index 0 is the stack pointer (`__sp`), initialized to 65536; the shadow stack grows downward. Per function:

1. **Escape analysis.** A pre-pass over the body marks each binding (param or `let`) as *addressed* if any `&binding…` / `&mut binding…` chain takes its address.
2. **Frame layout.** Addressed bindings get fixed byte offsets within the function's frame; `frame_size` is the sum of their `byte_size_of`s.
3. **Prologue / epilogue.** If `frame_size > 0`: `__sp -= frame_size` on entry, `__sp += frame_size` on exit. Spilled params are also copied from their incoming WASM-local slots into the frame at this point.
4. **Spilled bindings.** Live in memory at `__sp + frame_offset`. Reads emit per-leaf `iN.load` ops; writes emit per-leaf `iN.store` ops, with the byte offset folded into the load/store immediate. `&binding.field…` evaluates to `__sp + frame_offset + chain_byte_offset` (an i32).
5. **Non-spilled bindings.** Stay in WASM locals as flat scalars, exactly as before. References themselves are non-spilled (just an i32 in a WASM local).
6. **Call sites.** Reference params are passed as a single i32; no out-parameter rewriting, no writeback dance — mutation through `&mut r` is a real `iN.store` against the address `r` holds.

Layout helpers in `typeck.rs`: `byte_size_of(rtype, structs)` returns the byte size used both for `frame_size` accounting and for chain offsets (1/2/4/8/16 bytes for ints, 4 for refs, sum-of-fields for structs). `flatten_rtype` returns flat WASM scalars for non-spilled bindings — refs flatten to `[I32]` (ABI), not the pointee's shape.

Borrow tracking still treats `&mut T` as non-Copy: reading a `&mut` binding *moves* its borrow handle into the consumer (call slot or new binding) and marks the binding itself as moved. This keeps `let r = &mut p; f(r); p.x` accepted without needing NLL — `r`'s borrow ends with the call, `p`'s direct access afterward is fine.

Path resolution: every path in an expression is interpreted relative to the module containing the call. Single-segment identifiers without `(...)`/`{...}` are variable references; with `(...)` they're calls; with `{...}` they're struct literals. Multi-segment paths must be calls or struct literals. Only top-level (crate-root) functions are exported under their bare name.

Move and borrow tracking (in `borrowck.rs`): walk every function body with a stack of holders.

- A *holder* is either a named binding (a function parameter or a `let`) or a synthetic *call slot* pushed for the duration of a function call's argument evaluation. Each holder records the list of place paths it currently keeps borrowed.
- Walking an expression returns a `ValueDesc { borrows: Vec<Path> }`. `&place` produces a desc carrying that place. Reading a ref-typed `Var` produces the desc of borrows the binding currently holds (refs are `Copy`, so reads don't move). Everything else produces an empty desc — calls and struct literals can't return references, so their values never carry borrows out.
- The caller of `walk_expr` chooses what to do with the desc: a `let` makes a new binding holder absorb the desc's borrows; a call's argument absorbs them into the synthetic call holder; a block-expression's tail returns its desc up to whatever consumes the block. When a holder is dropped (block scope ends, call returns, function ends), the borrows it kept alive die with it.
- A move of place `P` is a conflict if any prior move or any *currently held* borrow shares a path prefix with `P`. A `&P` is a conflict if `P` has been moved.
- An assignment to place `P` (LHS of `P = EXPR;`) is a conflict if any holder still keeps a borrow that overlaps `P`. On success, every entry in `moved` whose path has `P` as a prefix is purged — the spot has a fresh value, so any sub-paths are valid again. Whole-binding reassignments (`x = …;`) thus "reset" `x` even if it had been moved.
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
