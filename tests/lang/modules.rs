// Modules (`mod`), `use` declarations, `pub` visibility, `pub use`
// re-exports, and the implicit stdlib prelude.

use super::*;

// `mod NAME;` resolves to `NAME.rs` next to the declaring file. This
// covers the existence-check error path.
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

// Stdlib prelude: `use std::*;` is auto-injected for libraries with
// `prelude: true`. `id` resolves through the prelude glob to
// `std::dummy::id`.
#[test]
fn uses_std_dummy_id_returns_7() {
    expect_answer("lang/modules/uses_std", 7i32);
}

#[test]
fn uses_std_generic_struct_returns_42() {
    expect_answer("lang/modules/uses_std_generic_struct", 42i32);
}

#[test]
fn uses_std_generic_returns_42() {
    expect_answer("lang/modules/uses_std_generic", 42i32);
}

// Basic `use`: bring `std::dummy::id` into scope as `id`.
#[test]
fn use_basic_returns_7() {
    expect_answer("lang/modules/use_basic", 7i32);
}

// Glob: `use std::dummy::*;` brings every item directly under
// `std::dummy` into scope.
#[test]
fn use_glob_returns_42() {
    expect_answer("lang/modules/use_glob", 42i32);
}

// Rename: `use std::dummy::id as identity;`.
#[test]
fn use_rename_returns_99() {
    expect_answer("lang/modules/use_rename", 99i32);
}

// Brace multi-import: `use std::{Drop, dummy};` brings both `Drop`
// (trait, used in an impl block) and `dummy` (module, used as a path
// prefix `dummy::id`) into scope.
#[test]
fn use_brace_returns_42() {
    expect_answer("lang/modules/use_brace", 42i32);
}

// `self` inside a brace group re-imports the prefix path itself:
// `use std::dummy::{self, id};` brings both `dummy` (the module,
// resolvable as `dummy::id(...)`) and `id` (bare-name) into scope.
#[test]
fn use_brace_self_returns_42() {
    expect_answer("lang/modules/use_brace_self", 42u32);
}

// `self as <name>` inside a brace renames the imported module:
// `use std::dummy::{self as d};` makes `d::id(...)` resolve while
// the original name `dummy` does not enter the local scope.
#[test]
fn use_brace_self_rename_returns_42() {
    expect_answer("lang/modules/use_brace_self_rename", 42u32);
}

// Block-scope use: `use std::dummy::id;` inside a block expression
// scopes the import to that block.
#[test]
fn use_block_scope_returns_33() {
    expect_answer("lang/modules/use_block_scope", 33i32);
}

// `use crate::…` resolves through the enclosing crate's root. For
// the user crate (name == ""), `crate::helper::compute` rewrites to
// `helper::compute`.
#[test]
fn use_crate_returns_77() {
    expect_answer("lang/modules/use_crate", 77i32);
}

// `pub use` re-exports. Module `b` has `pub use crate::a::deep;`,
// which makes `b::deep` resolve (from outside `b`) to `a::deep`.
#[test]
fn pub_use_reexport_returns_77() {
    expect_answer("lang/modules/pub_use_reexport", 77i32);
}

// `mod sub;` resolves to `sub/mod.rs` when no `sub.rs` exists. Tests
// the Rust-2015-style directory-with-mod.rs convention. `sub/mod.rs`
// itself anchors the `sub/` directory, so its own `mod inner;`
// resolves to `sub/inner.rs` (a sibling), not `sub/mod/inner.rs`.
#[test]
fn mod_rs_directory_layout_returns_42() {
    expect_answer("lang/modules/mod_rs_dir", 42u32);
}

#[test]
fn private_function_call_from_outside_module_is_rejected() {
    let err = compile_sources(&[
        ("lib.rs", "mod inner;\nfn answer() -> u32 { inner::secret() }"),
        ("inner.rs", "fn secret() -> u32 { 7 }"),
    ]);
    assert!(
        err.contains("private"),
        "expected `private` error, got: {}",
        err
    );
}

#[test]
fn private_struct_field_read_is_rejected() {
    let err = compile_sources(&[
        (
            "lib.rs",
            "mod inner;\nfn answer() -> u32 { let f: inner::Foo = inner::make(); f.value }",
        ),
        (
            "inner.rs",
            "pub struct Foo { value: u32 }\npub fn make() -> Foo { Foo { value: 1 } }",
        ),
    ]);
    assert!(
        err.contains("private"),
        "expected private-field error, got: {}",
        err
    );
}

#[test]
fn private_struct_field_construction_is_rejected() {
    let err = compile_sources(&[
        (
            "lib.rs",
            "mod inner;\nfn answer() -> u32 { let f: inner::Foo = inner::Foo { value: 1 }; 0 }",
        ),
        ("inner.rs", "pub struct Foo { value: u32 }"),
    ]);
    assert!(
        err.contains("private"),
        "expected private-field-construction error, got: {}",
        err
    );
}

// `pub(crate)` widens visibility to the entire crate but not beyond.
// Since the test compiler treats the test program as the user crate,
// `pub(crate) fn` is reachable from any module in that crate.
#[test]
fn pub_crate_function_is_visible_from_sibling_module() {
    let src = "mod a;\nmod b;\nfn answer() -> u32 { b::call_a() }";
    expect_answer_sources(
        &[
            ("lib.rs", src),
            ("a.rs", "pub(crate) fn secret() -> u32 { 42 }"),
            (
                "b.rs",
                "use crate::a::secret;\npub fn call_a() -> u32 { secret() }",
            ),
        ],
        42u32,
    );
}

// `pub(super)` only exposes the item to the parent module. Sibling-of-
// parent access must still go through the parent.
#[test]
fn pub_super_function_is_visible_from_parent() {
    let src = "mod outer;\nfn answer() -> u32 { outer::call() }";
    expect_answer_sources(
        &[
            ("lib.rs", src),
            (
                "outer.rs",
                "mod inner;\npub fn call() -> u32 { inner::secret() }",
            ),
            ("outer/inner.rs", "pub(super) fn secret() -> u32 { 42 }"),
        ],
        42u32,
    );
}

// `pub(super)` is *not* visible from the grandparent (the crate root).
#[test]
fn pub_super_function_not_visible_from_grandparent_is_rejected() {
    let err = compile_sources(&[
        (
            "lib.rs",
            "mod outer;\nfn answer() -> u32 { outer::inner::secret() }",
        ),
        ("outer.rs", "pub mod inner;"),
        ("outer/inner.rs", "pub(super) fn secret() -> u32 { 42 }"),
    ]);
    assert!(
        err.contains("private"),
        "expected `private` error for pub(super) reach, got: {}",
        err
    );
}

// `pub(self)` is identical in effect to no modifier — the item stays
// confined to its defining module. Outside callers see `private`.
#[test]
fn pub_self_function_call_from_outside_is_rejected() {
    let err = compile_sources(&[
        ("lib.rs", "mod inner;\nfn answer() -> u32 { inner::secret() }"),
        ("inner.rs", "pub(self) fn secret() -> u32 { 7 }"),
    ]);
    assert!(
        err.contains("private"),
        "expected `private` error for pub(self) reach, got: {}",
        err
    );
}

// `pub(in crate::a)` exposes the item to a specific ancestor module
// and its descendants only. The ancestor's siblings still see it as
// private.
#[test]
fn pub_in_path_visible_within_named_ancestor() {
    let src = "mod a;\nfn answer() -> u32 { a::call() }";
    expect_answer_sources(
        &[
            ("lib.rs", src),
            (
                "a.rs",
                "mod inner;\npub fn call() -> u32 { inner::secret() }",
            ),
            ("a/inner.rs", "pub(in crate::a) fn secret() -> u32 { 42 }"),
        ],
        42u32,
    );
}

#[test]
fn pub_in_path_not_visible_outside_named_ancestor_is_rejected() {
    let err = compile_sources(&[
        (
            "lib.rs",
            "mod a;\nmod b;\nfn answer() -> u32 { b::call() }",
        ),
        ("a.rs", "pub mod inner;"),
        ("a/inner.rs", "pub(in crate::a) fn secret() -> u32 { 42 }"),
        (
            "b.rs",
            "use crate::a::inner::secret;\npub fn call() -> u32 { secret() }",
        ),
    ]);
    assert!(
        err.contains("private"),
        "expected `private` error for pub(in crate::a) reach from sibling, got: {}",
        err
    );
}

// `pub(in <path>)` requires the path to name an ancestor module. A
// non-ancestor path is a static error at item registration.
#[test]
fn pub_in_path_non_ancestor_is_rejected() {
    let err = compile_sources(&[
        ("lib.rs", "mod a;\nmod b;\nfn answer() -> u32 { 0 }"),
        ("a.rs", "pub(in crate::b) fn nope() -> u32 { 1 }"),
        ("b.rs", "pub fn call() -> u32 { 0 }"),
    ]);
    assert!(
        err.contains("not an ancestor"),
        "expected `not an ancestor` error, got: {}",
        err
    );
}

// `pub(super)` at the crate root has no parent — reject at item
// registration time.
#[test]
fn pub_super_at_crate_root_is_rejected() {
    let err = compile_source("pub(super) fn nope() -> u32 { 0 }\nfn answer() -> u32 { 0 }");
    assert!(
        err.contains("crate root") || err.contains("super"),
        "expected pub(super)-at-root error, got: {}",
        err
    );
}
