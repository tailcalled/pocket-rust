---
name: language-syntax
description: Use when working with the surface syntax pocket-rust accepts — statements (let, let-else, assignment, compound assignment), expressions (literals, field access, indexing, blocks, if), control flow (while, for, return, break, continue, ?), operators (binary/unary desugar, precedence, short-circuit), and macros (vec!, matches!, panic!). Comments are also covered here.
---

# language surface syntax

## Functions

`fn NAME(P1: T1, P2: T2, …)` with an optional return type. Optional generic type parameters: `fn NAME<T1, T2>(...)`. May be prefixed with `unsafe` (`unsafe fn`, `pub unsafe fn`) — calls to an unsafe fn must lexically appear inside an `unsafe { … }` block, and the body of an `unsafe fn` is implicitly an unsafe context. No attributes, no `where` clauses, no const generics.

Function body: a block — a sequence of statements followed by an optional tail expression. A tail-less function body (or block) evaluates to `()` (the unit tuple); the function's declared return type — or `()` when no `-> Type` was given — is unified against that.

## Statements

- `let PAT = EXPR;` / `let PAT: TYPE = EXPR;` — pattern binding. PAT may be `name` (binding), `mut name` (mutable binding), `_` (wildcard — value evaluated for side effects then dropped), or any irrefutable destructuring pattern (today: tuples of irrefutable sub-patterns, `&pat` / `&mut pat`, `name @ pat`). Refutable patterns (variant constructors, literals, ranges, or-patterns whose alternatives don't all cover the type) are rejected unless `let-else` is used. Optional annotation unifies with the value's inferred type. Refutability is decided by the same machinery as `match` exhaustiveness (see patterns-and-matching skill).

- `let PAT = EXPR else { DIVERGING };` — let-else. PAT may be refutable; if it doesn't match `EXPR`, the `else` block runs. The else block must diverge: typeck checks that either the block's tail expression has type `!` or some statement in the block does. Pattern bindings are not in scope inside the else block; they scope to the rest of the enclosing block on the match path.

- `let PAT: TYPE;` — declared without initializer. The `=` is optional in `parse_let_stmt`; absence routes to the uninit branch. Typeck requires the type annotation (no value to infer from), the pattern to be a single `Binding` (no destructure / wildcard / refutable), and forbids let-else. The mut-check in `check_assign_stmt` is relaxed for these bindings (`LocalEntry.declared_uninit`) so the deferred initializer assignment goes through without `mut` (pocket-rust accepts repeat assignments to a non-mut uninit binding too — strict superset of Rust). Borrowck wires `CfgStmtKind::Uninit(local)` after `StorageLive` in `lower_let`; `MoveStatus::Uninit` joins like Moved (assignment via `state.init` clears it). Reads before init error with "use of uninitialized binding `x`". At scope-end, `Uninit` maps to `MovedPlace::Moved` so codegen skips Drop. Mono emits `MonoStmt::LetUninit { binding, .. }` (reads the resolved type from `expr_types[pattern.id]`); codegen reserves storage but emits no value. Partial-init flag handling (assigned on some paths, needs flag-init=0 + set-on-assign) is stubbed in codegen but not yet exercised by a test — gap-tested.

- `PLACE = EXPR;` — assignment. The LHS must be a place expression (a `Var` or a `Var`-rooted `FieldAccess`/`TupleIndex` chain). For a whole-binding assignment (`x = …;`) the binding must be declared `mut`. Field/tuple-index assignments are allowed through an owned `mut` binding *or* through a `&mut T` binding (the binding itself need not be `mut` — it's the inner mutability that authorizes the write); through a `&T` binding they're rejected.

- `PLACE OP= EXPR;` — compound assignment for `+= -= *= /= %=`. Parser desugars at the statement level to `Stmt::Expr(MethodCall { receiver: lhs, method: "<op>_assign", args: [rhs] })` — `add_assign` / `sub_assign` / `mul_assign` / `div_assign` / `rem_assign`. Each is provided by the matching `*Assign<Rhs = Self>` trait in `std::ops`; primitive impls for every int kind do `*self = ¤T_op(*self, other);`. Method dispatch autorefs the receiver to `&mut Self`, so the LHS must be a mutable place.

- `EXPR;` — expression statement. Any expression, followed by `;`, is evaluated and its value (if any) is discarded. Brace-block expressions (`{ … }`, `unsafe { … }`, `if …`) may sit as statements without a trailing `;` — their braces already delimit them.

Names are in scope for subsequent statements and the tail expression. No shadowing checks.

## Expressions

- Integer literals (parsed as `u64`, range-checked into the inferred int kind, emitted as `i32.const` / `i64.const`).
- `true` / `false` boolean literals (emitted as `i32.const 1`/`0`).
- Function calls: `path::to::func(arg, arg, …)`.
- Bare-identifier variable references (resolve to enclosing fn's params or `let` bindings; emitted as `local.get`).
- Struct literals: `Path { field: expr, … }`.
- Field access: `expr.field`, chainable.
- Tuple expressions: `()`, `(a,)`, `(a, b, …)` and tuple-index `t.0`, `t.1`, …
- `&expr` / `&mut expr` to take a reference.
- Block expressions: `{ stmts; tail_expr }` whose value is the tail expression (or `()` when tail-less). Each block introduces its own local scope. Borrows created inside a block expression follow the same call-scoped rule as anywhere else.
- `if cond { … } else { … }` — `else` is optional (defaults to an empty block, unit-typed); arms unify to the same type, that's the if's type. The condition disallows bare struct literals (parens to use them); `else if` desugars to a chained nested `if` in the else block. Wasm codegen emits a structured `if/else/end` with a `BlockType` matching the if's result.
- `arr[idx]` indexing — `ExprKind::Index { base, index, bracket_span }`. Typeck reads `idx`'s type and dispatches `std::ops::Index<Idx>` for base's type (autoderef through `&T` first) via `solve_impl_with_args(Index, [idx_ty], base_ty)`. Idx is `usize` for element indexing, `Range<usize>`/`RangeFrom<usize>`/`RangeTo<usize>`/`RangeInclusive<usize>`/`RangeToInclusive<usize>`/`RangeFull` for slicing. Unbound integer-class vars in `idx_ty` (naked `arr[0]` and nested `s[1..4]` whose `Range<?int>` carries unbound int vars) default to `usize` via `default_int_vars_to_usize` before dispatch. The impl's `Output` assoc-type binding is read via `find_assoc_binding` and returned. Codegen branches on enclosing context: value position emits `*base.index(idx)` (loads from the returned `&Output` address); `&base[idx]` emits `base.index(idx)` (returns the address as a borrow); `&mut base[idx]` and `base[idx] = val` emit `base.index_mut(idx)` (the latter then store-throughs the value). The receiver is passed through directly when base is already `&Self`/`&mut Self`, autoref'd otherwise.

- Range expressions — `a..b`, `a..`, `..b`, `..`, `a..=b`, `..=b`. Lowest expression precedence (above logical-or). Non-associative (`a..b..c` errors at the inner parser). Desugars at parse-time to `std::ops::Range*` struct literals (single-segment paths relying on the prelude): `a..b → Range { start: a, end: b }`, `a.. → RangeFrom { start: a }`, `..b → RangeTo { end: b }`, `.. → RangeFull {}`, `a..=b → RangeInclusive { start: a, end: b }`, `..=b → RangeToInclusive { end: b }`. `..=` without a right side is rejected at parse-time. Range types' `start`/`end` are accessible as ordinary fields after construction. Combined with the `Index<Range*<usize>> for [T]/Vec<T>/str` impls in `lib/std/`, this is what makes `&v[1..3]` and `&s[..end]` work.

## Control flow

- `while COND { BODY }` and `'label: while COND { BODY }`. Cond unifies with `bool`; body is unit-typed. Whole expression is `()` — `while` is statement-shaped, used inside blocks. Inside the body, `break` and `break 'label` exit the (named) loop; `continue` and `continue 'label` jump back to the cond. Both type to `!`. Codegen lowers to `Block(Empty) ; Loop(Empty) ; <cond> ; i32.eqz ; BrIf 1 ; <body> ; Br 0 ; End ; End` — outer Block is the break target, inner Loop is the continue target.

- `for PAT in EXPR { BODY }` and `'label: for PAT in EXPR { BODY }`. EXPR's resolved type must impl `std::iter::Iterator { type Item; fn next(&mut self) -> Option<Self::Item>; }`. The loop calls `Iterator::next(&mut __iter)` repeatedly; `Some(PAT)` runs the body with PAT bound, `None` exits. Whole expression is `()`; `break` / `continue` (with optional label) work inside the body. **No AST-level desugar** — for-loops stay first-class through typeck (so errors say "the trait `Iterator` is not implemented for `T` (required by `for` loop)" rather than referring to a synthetic `Iterator::next` call), and are lowered directly in borrowck (`lower_for`) and codegen (`codegen_for_expr`: shadow-stack slot for `__iter`, per-iteration `Option<Item>` sret slot, `BrIf` on disc=0 to exit). Pattern is currently restricted to `Var(name)` or `_`. Pocket-rust doesn't yet auto-call `IntoIterator::into_iter`, so users write `vec.into_iter()` (etc.) explicitly.

- **Block-tail unit-block-likes:** when a `for`/`while`/`if`/`match` is the last expression of a block (no trailing `;`), it becomes the block's `tail`. `codegen_unit_block_stmt` codegens the tail as if it were an expression statement; without this, a tail `for` would silently never run.

- `break` / `continue` walk the `loops` stack to find the matching `LoopCgFrame` (label-matched, or innermost for the unlabelled form), emit drops for any in-loop bindings (locals at indices ≥ `locals_len_at_entry` in reverse order), then emit `Br(N)` where N is computed from the *current* cf-depth minus the frame's recorded depth. Current-cf-depth is read by scanning the instruction stream backward counting structured opens (Block/Loop/If) vs Ends — robust to nested if/match constructs between the loop boundary and the break/continue site.

- `return EXPR;` / `return;` — early-exit expression. Type `!`. The value (or `()` for the bare form) unifies against the enclosing function's declared return type at typeck. Borrowck lowers via `Terminator::Return` after lowering the value's operand. Codegen mirrors the function-end epilogue: codegen the value, stash flat scalars to fresh locals, drop every in-scope binding, then for sret-returning fns memcpy the value bytes to the caller-supplied sret slot and push the sret address (else push the stashed scalars), restore SP from the function-entry-saved local, emit wasm `Return`. Subsequent same-block code becomes wasm-polymorphic dead code.

- `?` operator (try) — postfix on `Result<T, E>` expressions inside a function returning `Result<U, E>` with the same error type `E`. On `Ok(v)` extracts `v` as the expression's value; on `Err(e)` builds `Err(e)` and returns early from the enclosing function. **Not desugared early** — `ExprKind::Try { inner, question_span }` reaches typeck and codegen as a first-class node so error spans point at the `?` token rather than at synthetic match arms. Typeck checks: inner must be a `std::result::Result` enum (currently hardcoded — no general `Try` trait yet); function return must be a `Result` with the same `E` (`?` doesn't yet do `From`-coercion on the error type). Codegen lowers as: evaluate inner (pushes the Result enum's address), stash to local, load disc at offset 0, `if disc == 0 { read Ok payload at +4 } else { allocate fresh enum slot, write disc=Err, memcpy err payload to +4, drop bindings, memcpy slot to sret, restore SP, push sret, Return }`. The if's BlockType is the Ok-payload's flat shape (multi-value when needed). Hardcoded Ok=disc 0, Err=disc 1 (declaration order in `lib/std/result.rs`).

## Operator syntax

`+`, `-`, `*`, `/`, `%`, `==`, `!=`, `<`, `<=`, `>`, `>=`, `&&`, `||`, plus prefix unary `-` and `!`, are parsed as expressions and desugared at parse-time.

**Precedence (lowest → highest):** logical-or (`||`), logical-and (`&&`), comparison, additive, multiplicative, prefix unary `-`/`!`, cast, then postfix.

**Binary desugar:** `a + b` → `a.add(b)`, `a - b` → `a.sub(b)` (additive), `a * b`/`a / b`/`a % b` → `a.mul(b)`/`a.div(b)`/`a.rem(b)` (multiplicative); each method comes from the matching Rust-style operator trait `Add<Rhs = Self>` / `Sub<Rhs = Self>` / `Mul<Rhs = Self>` / `Div<Rhs = Self>` / `Rem<Rhs = Self>` (defined in `std::ops`). Each trait carries an associated `type Output;`; `<T as Add<Rhs>>::Output` is the result type. Default trait params (`Rhs = Self`) let `impl Add for Foo` omit the trait-arg and have it default to the impl's `Self`.

**Comparisons:** `a == b` → `a.eq(&b)`, `a < b` → `a.lt(&b)`, etc. — `eq`/`ne` come from `PartialEq`, `lt`/`le`/`gt`/`ge` from `PartialOrd`; both methods take `(&self, &Self) -> bool`, so the rhs is wrapped in a borrow expression and the receiver autorefs through method dispatch.

**Unary minus** has two parser forms: `-INT_LIT` collapses to a single `NegIntLit(value)` AST node so the literal type can be inferred from context (avoids a method-dispatch-on-unbound-var); `-other_expr` desugars to `other_expr.neg()` via `Neg::neg`. Primitive impls for every int kind lower their bodies to the corresponding `¤T_op` builtin; `Neg::neg` is `0 - self` via `¤T_sub`.

**Prefix `!`** desugars to `expr.not()` via `std::ops::Not` (`type Output; fn not(self) -> Self::Output;`); only `bool`'s impl exists today (lowers to `¤bool_not`), the integer-bitwise-NOT impls are TODO.

**`&&` / `||`** desugar at parse-time to short-circuiting if-else: `a && b` → `if a { b } else { false }`, `a || b` → `if a { true } else { b }` — the rhs is never evaluated when the lhs decides the result, falling out of the existing if-expr code-path semantics.

**Numeric literal overloading is dropped** — integer literals only resolve to built-in `Int(_)` kinds (defaulting to `i32` when unconstrained), never to user types via a `from_i64`-like trait. Use an explicit constructor (`MyType::from(42)`) for custom literal-shaped construction.

## Macros

Invoked as `name!(args)` or `name![args]` (parens or brackets — the lexer's `!` and the parser's macro-detection accept both). The args parse as a comma-separated expression list either way **except** for two parse-time-special-cased macros that have non-expression-shaped args.

- `vec![a, b, c]` is desugared in `parse_path_atom::desugar_vec_macro` to a block expression: `{ let mut __pr_vec_<id> = Vec::new(); __pr_vec_<id>.push(a); …; __pr_vec_<id> }` — element type is inferred from the contents (or the surrounding context for the empty `vec![]` form, which collapses to `Vec::new()`).

- `matches!(scrut, pattern)` and `matches!(scrut, pattern if guard)` are parsed by `parse_matches_macro` (the second arg is a *pattern*, not an expression, so the regular call-args path doesn't fit) and desugar to a 2-arm match: `match scrut { pattern (if guard)? => true, _ => false }`.

- `panic!(msg: &str)` — `ExprKind::MacroCall { name: "panic", name_span, args: [msg] }`. Always parens-form; the bracket form parses but typeck rejects. Typeck verifies the single arg unifies with `&str` and yields `!` (the macro diverges). Codegen pushes the &str fat ref (ptr, len), emits `Call(0)` to the host-imported `env.panic(ptr, len)` function (reserved at wasm function index 0 by `compile()` regardless of whether any panic call is emitted), then `unreachable`. Borrowck terminates the current block with `Terminator::Unreachable` so subsequent same-block code is dead. The wasm `imports` section carries `env.panic` as the first import; module-defined functions occupy wasm idxs starting at `imports.len()` (currently 1). Test harness (`tests/lang/mod.rs` and `tests/std/mod.rs`) registers a `panic` stub via `wasmi::Linker::define` that traps execution; production hosts can print + abort.

Unrecognized macros surface as `MacroCall { name, args }` and `check_macro_call` errors with `unknown macro `X!`` unless special-cased downstream (currently only `panic!` is).

**Token interaction:** in **type position**, the `&&` token splits into two `&`s (`parse_type` rewrites `tokens[pos]`) so `&&str` parses as `& &str` rather than tripping the logical-AND token; the lexer can't tell expression-vs-type context, so this is handled per-recognition-site.

## Comments

Line comments `// ...` (run to end-of-line) and block comments `/* ... */` (with **nesting** — `/* outer /* inner */ outer */` is a single comment) are recognized by the lexer and discarded. Doc-comment syntax (`///`, `//!`) isn't special-cased yet; it parses as a regular line comment whose payload happens to start with `/` or `!`.

## Integer literal type inference

A bare `42` gets a fresh integer-class type variable; the variable unifies with whatever owns it (the let annotation, the param type at a call site, the field type in a struct literal, the function's return type). If no constraint pins the variable down, it defaults to `i32`. Type suffixes (`42u32`, `100i64`, …) pin the literal's type at the source: the parser desugars `42u32` to `(42 as u32)` at parse time. Recognized suffixes are exactly the int-kind names.
