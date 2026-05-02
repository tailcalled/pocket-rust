pocket-rust
===

A from-scratch, minimal Rust-subset compiler that targets WebAssembly. Small enough that its own subset can express it, so it can eventually self-host inside WASM.

## Project rules

- **No silent deferrals.** Every existing first-class feature must keep working end-to-end with each new one. If an interaction is genuinely out of scope, name it explicitly in the plan *before implementing* and get user approval. "Threading X through is annoying" is not a reason to defer.
- **Maintain the relevant skill in the same change.** Feature-specific knowledge lives in `.claude/skills/<name>/SKILL.md` (see Skills index). When a decision/invariant/layout for one of those areas changes, edit the matching SKILL.md in the same turn. CLAUDE.md stays small: only project-wide rules and the high-level architecture. New features that don't fit any existing skill ship with a new SKILL.md (added to the index).
- **Tests reveal flaws — never restructure tests to avoid gaps.** Fix the gap; expand the test if anything. If a failure is "unrelated" to the feature under test, it's still a real bug — fix it, or at minimum keep the failing assertion in place so the conversation gets forced.
- **Every feature change adds both positive *and* negative tests.** Positive: `expect_answer(...)` against an example. Negative: `compile_source(...)` with a substring assertion on the error. See `testing-conventions` skill for layout and helpers.
- **Stdlib parity TODOs.** When adding to `lib/std/`, walk the matching `std::` API and either implement each method or leave a `// TODO: <method-name> — <missing-feature>` so a `grep -r "TODO" lib/std/` finds everything that becomes implementable when a given feature lands.

## Architecture

- `src/lib.rs` — public surface: `Vfs` and `compile`. Drives the pipeline. **No I/O.**
- `src/span.rs` — `Pos`, `Span`, `Error`, `format_error`. Lexer-tracked line/col.
- `src/lexer.rs` — `tokenize(file, source) -> Vec<Token>`.
- `src/parser.rs` — `parse(file, Vec<Token>) -> Vec<RawItem>`. Recursive-descent.
- `src/ast.rs` — resolved AST. `Module` recursive (carries `source_file`). Each `Expr` and `Pattern` carries a per-fn `id: NodeId`; `Function.node_count` exposes the count.
- `src/typeck/` — type checking. → `typeck-pipeline` skill.
- `src/borrowck/` — CFG-based borrow checker. → `borrowck-pipeline` skill.
- `src/safeck.rs` — enforces unsafe-block requirement for raw-ptr deref + unsafe-fn calls. Reads typeck artifacts; doesn't re-type.
- `src/codegen.rs` — `emit(&mut wasm::Module, …)`. Appends to existing module so libraries land first. → `codegen-machinery` skill.
- `src/wasm.rs` — structured WASM repr + byte encoder. → `wasm-encoding` skill.
- `src/main.rs` — I/O shell. Loads `lib/std/` from disk and passes it as a `Library`.
- `lib/std/` — pocket-rust's own (in-language) stdlib. **Not referenced from `src/`.** → `stdlib-layout` skill.
- `tests/` — integration tests. → `testing-conventions` skill.

## Pipeline

`main` populates a `Vfs` per crate and calls `compile(libraries, &user_vfs, user_entry) -> Result<wasm::Module, String>`. `compile` processes each library in order, then the user crate. Per crate: resolve modules (following `mod NAME;` declarations to siblings), run typeck (extending shared `StructTable`/`FuncTable`), borrowck, safeck, and codegen (appending to the shared `wasm::Module`). The library system is fully generic — `lib.rs` doesn't know about `std`; `main.rs` is the one place that loads `lib/std/`.

The crate root's `name` drives its path prefix: a library's items live at `["std", ...]`; the user crate has `name = ""` so its items live at the empty prefix. The "export iff `current_module.is_empty()`" rule in codegen exports user crate-root functions and never library functions.

Errors flow through `span::Error { file, message, span }`, formatted as `<file>:<line>:<col>: <message>`. Each pass walks the AST independently and reads typeck's per-`Expr.id` artifacts (`expr_types`, `method_resolutions`, `call_resolutions`) — no source-DFS lockstep counters between passes.

## CLI

```
pocket-rust <input-dir> <output.wasm>
```

Walks `<input-dir>` recursively for `*.rs` files, populates a `Vfs`, calls `compile`, writes the bytes.

## Skills index

Feature-specific knowledge under `.claude/skills/<name>/SKILL.md`, loaded on-demand. Keep in sync as part of the same change.

- `typeck-pipeline` — typeck submodules, `RType` vocabulary, `InferType`/`Subst`, integer-literal defaulting.
- `borrowck-pipeline` — CFG submodules, build → moves → liveness → borrows, NLL, reborrow patterns.
- `codegen-machinery` — shadow-stack, escape analysis, frame layout, `Storage`/`BaseAddr`, monomorphization, string pool.
- `trait-system` — declarations, dispatch (receiver-type chain), supertraits, AssocProj, default & generic trait params, `Copy`.
- `drop-and-destructors` — `Drop` machinery, drop flags, partial-move rejection, pattern-binding interactions.
- `patterns-and-matching` — pattern AST, refutability, exhaustiveness, `match`/`if let`/`let-else`.
- `references-and-lifetimes` — `&T`/`&mut T`, lifetimes, raw pointers, `unsafe`, smart-pointer deref, reborrow.
- `types-and-layout` — int kinds, `bool`, `char`, structs, tuples, enums (sret), DSTs, never, `byte_size_of`/`flatten_rtype`.
- `modules-paths-visibility` — `mod`, `use`, prelude, `pub`, `pub use` re-exports, path resolution.
- `builtin-intrinsics` — `¤` intrinsic catalog.
- `language-syntax` — surface syntax: statements, expressions, control flow, operator desugar, macros.
- `stdlib-layout` — `lib/std/` contents.
- `wasm-encoding` — `src/wasm.rs` sections + helpers.
- `testing-conventions` — `examples/`/`tests/` layout, `expect_answer`/`compile_source` helpers, naming.
