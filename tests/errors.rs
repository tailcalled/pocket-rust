use pocket_rust::{Library, Vfs, compile};
use std::fs;
use std::path::Path;

fn load_stdlib() -> Library {
    let stdlib_path = Path::new("lib/std");
    let mut vfs = Vfs::new();
    load_dir(stdlib_path, stdlib_path, &mut vfs);
    Library {
        name: "std".to_string(),
        vfs,
        entry: "lib.rs".to_string(),
    }
}

fn load_dir(root: &Path, dir: &Path, vfs: &mut Vfs) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("file_type");
        if file_type.is_dir() {
            load_dir(root, &path, vfs);
        } else if file_type.is_file()
            && path.extension().and_then(|s| s.to_str()) == Some("rs")
        {
            let rel = path.strip_prefix(root).expect("strip_prefix");
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let source = fs::read_to_string(&path).expect("read source");
            vfs.insert(key, source);
        }
    }
}

fn compile_source(source: &str) -> String {
    let mut vfs = Vfs::new();
    vfs.insert("lib.rs".to_string(), source.to_string());
    let libs = vec![load_stdlib()];
    compile(&libs, &vfs, "lib.rs").err().expect("expected error")
}

#[test]
fn lex_error_reports_line_and_column() {
    let err = compile_source("fn answer() -> usize { @ }");
    assert!(
        err.starts_with("lib.rs:1:24:"),
        "expected `lib.rs:1:24:` prefix, got: {}",
        err
    );
}

#[test]
fn parse_error_reports_line_and_column() {
    let err = compile_source("fn ok() -> usize { 42 }\nfn bad)\n");
    assert!(
        err.starts_with("lib.rs:2:7:"),
        "expected `lib.rs:2:7:` prefix, got: {}",
        err
    );
}

#[test]
fn codegen_error_reports_line_and_column() {
    let err = compile_source("fn big() -> usize { 99999999999 }");
    assert!(
        err.starts_with("lib.rs:1:21:"),
        "expected `lib.rs:1:21:` prefix, got: {}",
        err
    );
}

#[test]
fn missing_module_file_reports_decl_site() {
    let err = compile_source("mod nope;\nfn f() {}");
    assert!(
        err.starts_with("lib.rs:1:5:"),
        "expected `lib.rs:1:5:` prefix, got: {}",
        err
    );
    assert!(
        err.contains("nope.rs"),
        "expected message to mention `nope.rs`, got: {}",
        err
    );
}

#[test]
fn unresolved_call_reports_call_site() {
    let err = compile_source("fn main() -> usize { ghost::missing() }");
    assert!(
        err.starts_with("lib.rs:1:22:"),
        "expected `lib.rs:1:22:` prefix, got: {}",
        err
    );
}

#[test]
fn unknown_variable_reports_use_site() {
    let err = compile_source("fn f(a: usize) -> usize { b }");
    assert!(
        err.starts_with("lib.rs:1:27:"),
        "expected `lib.rs:1:27:` prefix, got: {}",
        err
    );
    assert!(
        err.contains("unknown variable"),
        "expected message about unknown variable, got: {}",
        err
    );
}

#[test]
fn arity_mismatch_reports_call_site() {
    let err = compile_source(
        "fn id(x: usize) -> usize { x }\nfn caller() -> usize { id(1, 2) }",
    );
    assert!(
        err.starts_with("lib.rs:2:24:"),
        "expected `lib.rs:2:24:` prefix, got: {}",
        err
    );
    assert!(
        err.contains("expected 1, got 2"),
        "expected arity mismatch detail, got: {}",
        err
    );
}

#[test]
fn unknown_struct_field_reports_use_site() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn f(p: Point) -> usize { p.z }",
    );
    assert!(
        err.starts_with("lib.rs:2:29:"),
        "expected `lib.rs:2:29:` prefix, got: {}",
        err
    );
    assert!(
        err.contains("no field `z`"),
        "expected `no field z` detail, got: {}",
        err
    );
}

#[test]
fn missing_struct_field_in_literal() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn f() -> Point { Point { x: 1 } }",
    );
    assert!(
        err.contains("missing field `y`"),
        "expected missing field detail, got: {}",
        err
    );
}

#[test]
fn duplicate_partial_move_is_rejected() {
    // `o.p` is non-Copy (struct); moving it twice is an error.
    let err = compile_source(
        "struct Inner { v: usize }\n\
         struct Outer { p: Inner, q: Inner }\n\
         fn f(o: Outer) -> Outer { Outer { p: o.p, q: o.p } }",
    );
    assert!(
        err.contains("already moved"),
        "expected move error, got: {}",
        err
    );
}

#[test]
fn whole_after_partial_move_is_rejected() {
    // `p.a` is non-Copy; moving it then re-using it errors.
    let err = compile_source(
        "struct Inner { v: usize }\n\
         struct Pair { a: Inner, b: Inner }\n\
         fn use_pair(p: Pair) -> usize { p.a.v }\n\
         fn f(p: Pair) -> usize { use_pair(Pair { a: p.a, b: p.a }) }",
    );
    assert!(
        err.contains("already moved"),
        "expected move error, got: {}",
        err
    );
}

#[test]
fn arg_type_mismatch_struct_for_usize() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn id(n: usize) -> usize { n }\nfn f(p: Point) -> usize { id(p) }",
    );
    assert!(
        err.contains("expected `usize`, got `Point`"),
        "expected arg-type mismatch detail, got: {}",
        err
    );
}

#[test]
fn arg_type_mismatch_usize_for_struct() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn use_point(p: Point) -> usize { p.x }\nfn f() -> usize { use_point(7) }",
    );
    assert!(
        err.contains("expected `Point`"),
        "expected arg-type mismatch mentioning `Point`, got: {}",
        err
    );
}

#[test]
fn struct_field_init_type_mismatch() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nstruct Pair { a: Point, b: Point }\nfn f() -> Pair { Pair { a: 1, b: 2 } }",
    );
    assert!(
        err.contains("expected `Point`, got integer"),
        "expected field-type mismatch, got: {}",
        err
    );
}

#[test]
fn return_type_mismatch() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn make() -> Point { Point { x: 1, y: 2 } }\nfn f() -> usize { make() }",
    );
    assert!(
        err.contains("expected `usize`, got `Point`"),
        "expected return-type mismatch, got: {}",
        err
    );
}

#[test]
fn field_access_on_usize_is_rejected() {
    let err = compile_source(
        "fn id(n: usize) -> usize { n }\nfn f() -> usize { id(7).x }",
    );
    assert!(
        err.contains("non-struct"),
        "expected non-struct field-access error, got: {}",
        err
    );
}

#[test]
fn move_while_borrowed_is_rejected() {
    // Two args of the same call: a borrow of `o` and a move out of `o.y`
    // (non-Copy). The borrow is still active when the second arg is evaluated,
    // so the move conflicts.
    let err = compile_source(
        "struct Inner { v: usize }\n\
         struct Outer { x: Inner, y: Inner }\n\
         fn use_borrow(o: &Outer, q: Inner) -> usize { q.v }\n\
         fn bad(o: Outer) -> usize { use_borrow(&o, o.y) }",
    );
    assert!(
        err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

#[test]
fn borrow_after_move_is_rejected() {
    // Move `o.x` (non-Copy) in arg 0, then try to borrow `&o` in arg 1 —
    // `o` is partially moved, so the borrow errors.
    let err = compile_source(
        "struct Inner { v: usize }\n\
         struct Outer { x: Inner, y: usize }\n\
         fn y_of(o: &Outer) -> usize { o.y }\n\
         fn first(a: Inner, b: usize) -> usize { a.v }\n\
         fn bad(o: Outer) -> usize { first(o.x, y_of(&o)) }",
    );
    assert!(
        err.contains("moved") || err.contains("borrowed"),
        "expected borrow-after-move error, got: {}",
        err
    );
}

#[test]
fn move_out_of_borrow_is_rejected() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nstruct Rect { tl: Point, br: Point }\nfn whoops(r: &Rect) -> Point { r.tl }",
    );
    assert!(
        err.contains("cannot move out of borrow"),
        "expected move-out-of-borrow error, got: {}",
        err
    );
}

#[test]
fn ref_in_struct_field_without_lifetime_is_rejected() {
    // Phase D allows refs in struct fields, but their lifetimes must be
    // explicit and declared on the struct (Rust's standard requirement).
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nstruct Bad { p: &Point }",
    );
    assert!(
        err.contains("missing lifetime specifier"),
        "expected missing-lifetime-specifier error, got: {}",
        err
    );
}

#[test]
fn ref_return_with_zero_ref_params_is_rejected() {
    // Lifetime elision rule 2 only kicks in with exactly one ref param;
    // zero ref params + ref return has no source lifetime to inherit.
    let err = compile_source(
        "fn whoops() -> &u32 { let x: u32 = 1; &x }",
    );
    assert!(
        err.contains("exactly one reference parameter"),
        "expected zero-ref-params error, got: {}",
        err
    );
}

#[test]
fn ref_return_with_two_ref_params_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         fn whoops(a: &Point, b: &Point) -> &Point { a }",
    );
    assert!(
        err.contains("exactly one reference parameter"),
        "expected two-ref-params error, got: {}",
        err
    );
}

#[test]
fn ref_return_mut_from_shared_param_is_rejected() {
    // `&T -> &mut U` would forge mutability — rejected.
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         fn whoops(p: &Point) -> &mut u32 { &mut p.x }",
    );
    assert!(
        err.contains("cannot return `&mut` from a `&` parameter"),
        "expected mut-from-shared error, got: {}",
        err
    );
}

#[test]
fn mut_method_through_shared_ref_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         impl Point { fn set(&mut self, v: u32) -> u32 { self.x = v; self.x } }\n\
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             let r: &Point = &pt; \
             r.set(99) \
         }",
    );
    assert!(
        err.contains("&mut self") && err.contains("shared"),
        "expected mut-method-through-shared error, got: {}",
        err
    );
}

#[test]
fn mut_method_on_immutable_owned_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         impl Point { fn set(&mut self, v: u32) -> u32 { self.x = v; self.x } }\n\
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             pt.set(99) \
         }",
    );
    assert!(
        err.contains("immutable receiver"),
        "expected immutable-receiver error, got: {}",
        err
    );
}

#[test]
fn no_method_on_struct_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             pt.missing() \
         }",
    );
    assert!(
        err.contains("no method `missing`"),
        "expected no-method error, got: {}",
        err
    );
}

#[test]
fn method_call_borrow_outlives_source_is_rejected() {
    // Borrow returned by `&self -> &u32` method should propagate the
    // receiver's borrow, blocking subsequent moves of the receiver.
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         impl Point { fn x_ref(&self) -> &u32 { &self.x } }\n\
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             let r: &u32 = pt.x_ref(); \
             let q = pt; \
             *r \
         }",
    );
    assert!(
        err.contains("cannot move") && err.contains("borrowed"),
        "expected propagated-method-borrow error, got: {}",
        err
    );
}

#[test]
fn wrong_struct_type_arg_count_is_rejected() {
    let err = compile_source(
        "struct Pair<T, U> { first: T, second: U }\n\
         fn answer() -> u32 { let p: Pair<u32> = Pair { first: 1, second: 2 }; p.first }",
    );
    assert!(
        err.contains("type arguments"),
        "expected wrong-struct-type-arg-count error, got: {}",
        err
    );
}

#[test]
fn field_access_on_generic_param_is_rejected() {
    // Polymorphic body check: `t.field` where `t: T` has no shape — reject.
    let err = compile_source(
        "fn bad<T>(t: T) -> u32 { t.field }",
    );
    assert!(
        err.contains("non-struct"),
        "expected field-access-on-T error, got: {}",
        err
    );
}

#[test]
fn turbofish_on_non_generic_is_rejected() {
    let err = compile_source(
        "fn plain() -> u32 { 7 }\n\
         fn answer() -> u32 { plain::<u32>() }",
    );
    assert!(
        err.contains("not a generic function") || err.contains("turbofish"),
        "expected turbofish-on-non-generic error, got: {}",
        err
    );
}

#[test]
fn wrong_type_arg_count_is_rejected() {
    let err = compile_source(
        "fn id<T>(x: T) -> T { x }\n\
         fn answer() -> u32 { id::<u32, u64>(5) }",
    );
    assert!(
        err.contains("type arguments"),
        "expected wrong-type-arg-count error, got: {}",
        err
    );
}

#[test]
fn self_outside_impl_is_rejected() {
    let err = compile_source(
        "fn answer() -> u32 { let x: Self = 0; x }",
    );
    assert!(
        err.contains("`Self` is only valid inside an `impl` block"),
        "expected Self-outside-impl error, got: {}",
        err
    );
}

#[test]
fn returned_borrow_outlives_source_is_rejected() {
    // Borrowck propagates the input borrow through the call: `r` carries a
    // borrow on `pt`, so moving `pt` afterward must conflict.
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\n\
         fn borrow_field(pt: &Point) -> &u32 { &pt.x }\n\
         fn answer() -> u32 { \
             let pt = Point { x: 1, y: 2 }; \
             let r: &u32 = borrow_field(&pt); \
             let q = pt; \
             *r \
         }",
    );
    assert!(
        err.contains("cannot move") && err.contains("borrowed"),
        "expected propagated-borrow error, got: {}",
        err
    );
}

#[test]
fn let_annotation_type_mismatch_is_rejected() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn f() -> usize { let x: usize = Point { x: 1, y: 2 }; x }",
    );
    assert!(
        err.contains("expected `usize`, got `Point`"),
        "expected let-annotation mismatch, got: {}",
        err
    );
}

#[test]
fn let_then_use_after_move_is_rejected() {
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn use_point(p: Point) -> usize { p.x }\nfn f() -> usize { let p = Point { x: 1, y: 2 }; let q = use_point(p); p.y }",
    );
    assert!(
        err.contains("already moved"),
        "expected use-after-move error, got: {}",
        err
    );
}

#[test]
fn block_expr_without_tail_is_rejected() {
    let err = compile_source("fn f() -> usize { let x = { let y = 5; }; x }");
    assert!(
        err.contains("block expression must end with"),
        "expected tailless block-expr error, got: {}",
        err
    );
}

#[test]
fn let_out_of_scope_after_block_is_rejected() {
    let err = compile_source("fn f() -> usize { let x = { let y = 7; y }; y }");
    assert!(
        err.contains("unknown variable: `y`"),
        "expected out-of-scope error, got: {}",
        err
    );
}

#[test]
fn assignment_to_immutable_binding_is_rejected() {
    let err = compile_source("fn f() -> u32 { let x = 5; x = 6; x }");
    assert!(
        err.contains("not declared as `mut`"),
        "expected mut-required error, got: {}",
        err
    );
}

#[test]
fn field_assignment_to_immutable_record_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn f() -> u32 { let p = Point { x: 1, y: 2 }; p.x = 99; p.x }",
    );
    assert!(
        err.contains("not declared as `mut`"),
        "expected mut-required error, got: {}",
        err
    );
}

#[test]
fn assignment_while_borrowed_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn x_of(p: &Point) -> u32 { p.x }\nfn use_borrow(p: &Point, q: u32) -> u32 { q }\nfn f() -> u32 { let mut p = Point { x: 1, y: 2 }; let r = &p; use_borrow(r, { p.x = 99; p.x }) }",
    );
    assert!(
        err.contains("borrowed"),
        "expected borrow-conflict error, got: {}",
        err
    );
}

#[test]
fn integer_literal_too_big_for_u8() {
    let err = compile_source("fn f() -> u8 { 300 }");
    assert!(
        err.contains("does not fit"),
        "expected fit-check error, got: {}",
        err
    );
}

#[test]
fn integer_literal_defaults_to_i32() {
    // `x` is never used, so its type variable is unconstrained and defaults to
    // i32. 4_000_000_000 doesn't fit in i32, so the post-solve range check
    // catches it — proving the default fired.
    let err = compile_source("fn f() -> u32 { let x = 4000000000; 0 }");
    assert!(
        err.contains("does not fit"),
        "expected default-overflow error, got: {}",
        err
    );
}

#[test]
fn borrow_through_inner_block_blocks_outer_move() {
    // The borrow `&pt1` is created inside the inner block, but the block returns
    // it (as `pt3`) so the borrow ends up bound to `pt2`. A subsequent move of
    // `pt1` must be rejected — `pt2` would otherwise be a dangling reference.
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn x_of(p: &Point) -> usize { p.x }\nfn f() -> usize { let pt1 = Point { x: 42, y: 0 }; let pt2 = { let pt3 = &pt1; pt3 }; let invalid = pt1; x_of(pt2) }",
    );
    assert!(
        err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

#[test]
fn borrow_of_subfield_blocks_parent_move() {
    // `&p.x` borrows the subfield, leaving `p` with a borrowed sub-place.
    // Trying to move `p` whole while `r` is still live then has to fail.
    let err = compile_source(
        "struct Point { x: usize, y: usize }\nfn f(p: Point) -> usize { let r = &p.x; let q = p; *r }",
    );
    assert!(
        err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

#[test]
fn two_mut_borrows_of_same_place_conflict() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn take(a: &mut Point, b: &mut Point) -> u32 { a.x }\nfn f() -> u32 { let mut p = Point { x: 1, y: 2 }; take(&mut p, &mut p) }",
    );
    assert!(
        err.contains("already borrowed") || err.contains("borrowed"),
        "expected mut/mut borrow conflict, got: {}",
        err
    );
}

#[test]
fn shared_and_mut_borrow_conflict() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn take(a: &mut Point, b: &Point) -> u32 { a.x }\nfn f() -> u32 { let mut p = Point { x: 1, y: 2 }; take(&mut p, &p) }",
    );
    assert!(
        err.contains("already borrowed") || err.contains("borrowed"),
        "expected mut/shared borrow conflict, got: {}",
        err
    );
}

#[test]
fn assign_through_shared_ref_is_rejected() {
    let err = compile_source(
        "struct Point { x: u32, y: u32 }\nfn f(p: &Point) -> u32 { p.x = 7; p.x }",
    );
    assert!(
        err.contains("shared reference") || err.contains("not mutable"),
        "expected shared-ref assignment rejection, got: {}",
        err
    );
}

// Phase E error tests — lifetimes.

#[test]
fn undeclared_lifetime_is_rejected() {
    // `'a` not declared in the fn's `<'a, ...>` params.
    let err = compile_source(
        "fn bad(x: &'a u32) -> &'a u32 { x }",
    );
    assert!(
        err.contains("undeclared lifetime"),
        "expected undeclared-lifetime error, got: {}",
        err
    );
}

#[test]
fn lifetime_param_after_type_param_is_rejected() {
    // Lifetimes must come before type params (Rust convention).
    let err = compile_source(
        "fn bad<T, 'a>(x: &'a T) -> &'a T { x }",
    );
    assert!(
        err.contains("lifetime parameters must come before"),
        "expected lifetime-after-type rejection, got: {}",
        err
    );
}

#[test]
fn struct_field_ref_without_lifetime_is_rejected_already_listed() {
    // (Covered above by `ref_in_struct_field_without_lifetime_is_rejected`.)
    // Spot-check that a *typed* lifetime works for the same shape:
    let err = compile_source(
        "struct Inner { x: u32 }\nstruct Bad { p: &Inner }",
    );
    assert!(
        err.contains("missing lifetime specifier"),
        "expected missing-lifetime error, got: {}",
        err
    );
}

#[test]
fn wrong_struct_lifetime_arg_count_is_rejected() {
    // `Holder<'a>` declared, used with two lifetime args.
    let err = compile_source(
        "struct Holder<'a> { r: &'a u32 }\nfn bad<'a, 'b>(h: Holder<'a, 'b>) -> u32 { 0 }",
    );
    assert!(
        err.contains("lifetime arguments"),
        "expected wrong-lifetime-arg-count error, got: {}",
        err
    );
}

#[test]
fn move_through_combined_borrow_is_rejected() {
    // `longer<'a>(x, y)` ties both args to the result; moving either while
    // the result is live conflicts.
    let err = compile_source(
        "struct B { v: u32 }\nfn longer<'a>(x: &'a u32, y: &'a u32) -> &'a u32 { x }\nfn f() -> u32 { let a: B = B { v: 1 }; let b: B = B { v: 2 }; let r: &u32 = longer(&a.v, &b.v); let b2: B = b; *r }",
    );
    assert!(
        err.contains("while it is borrowed") || err.contains("borrowed"),
        "expected move-while-borrowed error, got: {}",
        err
    );
}

#[test]
fn ambiguous_impl_method_dispatch_is_rejected() {
    // Two impls' targets both match `Pair<u32, u32>` and both define `get`.
    let err = compile_source(
        "struct Pair<T, U> { first: T, second: U }\n\
         impl<T> Pair<u32, T> { fn get(self) -> u32 { self.first } }\n\
         impl<U> Pair<U, u32> { fn get(self) -> u32 { self.second } }\n\
         fn f() -> u32 { let p: Pair<u32, u32> = Pair { first: 1, second: 2 }; p.get() }",
    );
    assert!(
        err.contains("ambiguous method"),
        "expected ambiguous-method error, got: {}",
        err
    );
}

#[test]
fn duplicate_impl_block_is_rejected() {
    // Two impls of the exact same target define the same method name.
    let err = compile_source(
        "struct Foo { x: u32 }\n\
         impl Foo { fn get(self) -> u32 { self.x } }\n\
         impl Foo { fn get(self) -> u32 { self.x } }\n\
         fn f() -> u32 { let f: Foo = Foo { x: 7 }; f.get() }",
    );
    assert!(
        err.contains("ambiguous method") || err.contains("duplicate"),
        "expected duplicate/ambiguous-impl error, got: {}",
        err
    );
}

// Trait-level error tests (T1).

#[test]
fn missing_trait_method_in_impl_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(self) -> u32; }\n\
         struct Foo { x: u32 }\n\
         impl Show for Foo {}\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("missing trait method"),
        "expected missing-method error, got: {}",
        err
    );
}

#[test]
fn extra_method_in_trait_impl_is_rejected() {
    let err = compile_source(
        "trait Show {}\n\
         struct Foo { x: u32 }\n\
         impl Show for Foo { fn extra(self) -> u32 { self.x } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("not a member of trait"),
        "expected extra-method error, got: {}",
        err
    );
}

#[test]
fn duplicate_trait_impl_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(self) -> u32; }\n\
         struct Foo { x: u32 }\n\
         impl Show for Foo { fn show(self) -> u32 { self.x } }\n\
         impl Show for Foo { fn show(self) -> u32 { self.x } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("duplicate impl"),
        "expected duplicate-impl error, got: {}",
        err
    );
}

#[test]
fn integer_literal_on_non_num_type_is_rejected() {
    let err = compile_source(
        "struct NotNum { x: u32 }\n\
         fn f() -> u32 { let n: NotNum = 5; 0 }",
    );
    assert!(
        err.contains("expected `NotNum`, got integer"),
        "expected literal-non-Num rejection, got: {}",
        err
    );
}

#[test]
fn integer_literal_in_unbounded_generic_is_rejected() {
    let err = compile_source(
        "fn make<T>() -> T { 42 }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("expected `T`, got integer"),
        "expected literal-unbounded-T rejection, got: {}",
        err
    );
}

#[test]
fn drop_and_copy_are_mutually_exclusive() {
    let err = compile_source(
        "struct Foo { x: u32 }\n\
         impl Copy for Foo {}\n\
         impl Drop for Foo { fn drop(&mut self) {} }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("cannot be implemented") && err.contains("already implements"),
        "expected drop/copy conflict error, got: {}",
        err
    );
}

#[test]
fn trait_impl_concretizes_method_param_is_rejected() {
    // T2.5b: when the trait method has its own type-param `<U>`, the
    // impl must declare a matching one and use it polymorphically.
    // Pre-fix this compiled silently; the validator skipped methods
    // with type-params. Now it requires α-equivalent signatures.
    let err = compile_source(
        "trait Foo { fn bar<U>(self, u: U) -> U; }\n\
         struct X {}\n\
         impl Foo for X { fn bar(self, u: u32) -> u32 { u } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("type parameters"),
        "expected method type-param arity error, got: {}",
        err
    );
}

#[test]
fn nested_borrow_blocks_conflicting_mut_borrow() {
    // Lifetime cleanup: nested per-slot tracking. The inner borrow of
    // `x` lives in `o`'s field_holds at path `["i","r"]`. Taking
    // `&mut x` while `o` is still live (the tail reads `o.i.r`) must
    // conflict. Pre-fix, the nested borrow was dropped during the
    // `let o = Outer { i: ... }` move into `o`, so the conflict was
    // silently missed.
    let err = compile_source(
        "struct Inner<'a> { r: &'a u32 }\n\
         struct Outer<'a> { i: Inner<'a> }\n\
         fn f() -> u32 {\n\
             let mut x: u32 = 5;\n\
             let o: Outer = Outer { i: Inner { r: &x } };\n\
             let _m: &mut u32 = &mut x;\n\
             *o.i.r\n\
         }",
    );
    assert!(
        err.contains("already borrowed"),
        "expected borrow-conflict error, got: {}",
        err
    );
}

#[test]
fn partial_move_of_drop_value_is_rejected() {
    // T4.6: whole-binding moves of a Drop value are now allowed (codegen
    // skips the implicit drop on the moved-from slot). Partial moves
    // remain rejected — Drop's destructor runs over the whole value, so
    // there's no sound way to drop a value with a hole punched in it.
    let err = compile_source(
        "struct Inner { x: u32 }\n\
         struct Outer { i: Inner }\n\
         impl Drop for Outer { fn drop(&mut self) {} }\n\
         fn f() -> u32 {\n\
             let o: Outer = Outer { i: Inner { x: 1 } };\n\
             let i: Inner = o.i;\n\
             0\n\
         }",
    );
    assert!(
        err.contains("type implements `Drop`"),
        "expected drop partial-move error, got: {}",
        err
    );
}

#[test]
fn trait_impl_method_return_type_mismatch_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(self) -> u32; }\n\
         struct Foo { x: u32 }\n\
         impl Show for Foo { fn show(self) -> u64 { 0 } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("wrong return type"),
        "expected return-type-mismatch error, got: {}",
        err
    );
}

#[test]
fn trait_impl_method_param_type_mismatch_is_rejected() {
    let err = compile_source(
        "trait Show { fn show(self, n: u32) -> u32; }\n\
         struct Foo { x: u32 }\n\
         impl Show for Foo { fn show(self, n: u64) -> u32 { 0 } }\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("wrong parameter type"),
        "expected param-type-mismatch error, got: {}",
        err
    );
}

#[test]
fn generic_copy_impl_without_bound_is_rejected() {
    let err = compile_source(
        "struct Wrap<T> { inner: T }\n\
         impl<T> Copy for Wrap<T> {}\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("the trait `Copy` is not implemented"),
        "expected non-Copy field error, got: {}",
        err
    );
}

#[test]
fn impl_copy_for_struct_with_non_copy_field_is_rejected() {
    let err = compile_source(
        "struct Inner { x: u32 }\n\
         struct Outer { i: Inner }\n\
         impl Copy for Outer {}\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("the trait `Copy` is not implemented"),
        "expected non-Copy-field error, got: {}",
        err
    );
}

#[test]
fn trait_method_without_bound_is_rejected() {
    // Calling `t.show()` inside `fn f<T>(t: T)` (no `T: Show` bound)
    // should be rejected.
    let err = compile_source(
        "trait Show { fn show(self) -> u32; }\n\
         fn f<T>(t: T) -> u32 { t.show() }",
    );
    assert!(
        err.contains("no method `show`") || err.contains("no trait bound"),
        "expected missing-bound error, got: {}",
        err
    );
}

#[test]
fn unknown_trait_in_impl_is_rejected() {
    let err = compile_source(
        "struct Foo { x: u32 }\n\
         impl Bogus for Foo {}\n\
         fn f() -> u32 { 0 }",
    );
    assert!(
        err.contains("unknown trait"),
        "expected unknown-trait error, got: {}",
        err
    );
}

#[test]
fn move_through_struct_field_borrow_is_rejected() {
    // Moving the place borrowed by a struct's ref field is rejected as long
    // as the wrapper is still live.
    let err = compile_source(
        "struct Inner { x: u32 }\nstruct Wrapper<'a> { r: &'a Inner }\nfn f() -> u32 { let i: Inner = Inner { x: 1 }; let w: Wrapper<'_> = Wrapper { r: &i }; let i2: Inner = i; let r: &Inner = w.r; r.x }",
    );
    assert!(
        err.contains("while it is borrowed") || err.contains("borrowed"),
        "expected move-while-field-borrowed error, got: {}",
        err
    );
}

#[test]
fn if_condition_must_be_bool() {
    let err = compile_source("fn answer() -> u32 { if 1 { 1 } else { 2 } }");
    assert!(
        err.contains("bool") || err.contains("type mismatch"),
        "expected non-bool condition error, got: {}",
        err
    );
}

#[test]
fn if_arms_must_unify() {
    let err = compile_source(
        "fn answer() -> u32 { if true { 1u32 as u32 } else { 0u64 as u64 } }",
    );
    assert!(
        err.contains("type mismatch") || err.contains("expected"),
        "expected arm-mismatch error, got: {}",
        err
    );
}

#[test]
fn read_after_maybe_moved_in_if_is_rejected() {
    // Drop binding moved in then-arm only; reading after the if is a
    // read of a MaybeMoved place, which borrowck rejects.
    let err = compile_source(
        "struct L { p: *mut u32 }\n\
         impl Drop for L { fn drop(&mut self) { unsafe { *self.p = 1; } } }\n\
         fn take(_l: L) -> u32 { 0 }\n\
         fn answer() -> u32 {\n\
             let mut c: u32 = 5;\n\
             let l: L = L { p: &mut c as *mut u32 };\n\
             let _v: u32 = if true { take(l) } else { 0 };\n\
             let _x: L = l;\n\
             0\n\
         }",
    );
    assert!(
        err.contains("moved") || err.contains("already"),
        "expected read-after-MaybeMoved error, got: {}",
        err
    );
}

