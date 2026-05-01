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
