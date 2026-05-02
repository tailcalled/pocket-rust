// Attempt to compile pocket-rust's own `src/` with pocket-rust.
//
// Loads `src/` (excluding `main.rs`, which uses host `std::env`/`std::fs`
// and isn't part of the self-host target) into a Vfs, with the
// in-language stdlib loaded as a prelude library, and asks `compile`
// to start at `lib.rs`.
//
// This isn't expected to succeed yet — the point is to surface the
// next bootstrap blocker. Run with `cargo run --example selfhost`.

fn main() {
    use pocket_rust::{Library, Vfs, compile};
    use std::path::Path;

    let stdlib_path = Path::new("lib/std");
    let mut std_vfs = Vfs::new();
    load_dir(stdlib_path, stdlib_path, &mut std_vfs, &[]);
    let stdlib = Library {
        name: "std".to_string(),
        vfs: std_vfs,
        entry: "lib.rs".to_string(),
        prelude: true,
    };

    let src_path = Path::new("src");
    let mut user_vfs = Vfs::new();
    // `main.rs` uses host-only `std::env` / `std::fs` / `std::process`
    // — explicitly out of scope for the self-host target. Skip it.
    load_dir(src_path, src_path, &mut user_vfs, &["main.rs"]);

    eprintln!("loaded {} files from src/:", user_vfs.files.len());
    for f in &user_vfs.files {
        eprintln!("  {}", f.path);
    }
    eprintln!();

    match compile(&[stdlib], &user_vfs, "lib.rs") {
        Ok(_module) => {
            println!("OK: pocket-rust self-compiled.");
        }
        Err(e) => {
            println!("FAIL: {}", e);
            std::process::exit(1);
        }
    }
}

fn load_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    vfs: &mut pocket_rust::Vfs,
    skip: &[&str],
) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            load_dir(root, &path, vfs, skip);
        } else if file_type.is_file() && path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let rel = path.strip_prefix(root).unwrap();
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if skip.contains(&key.as_str()) {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            vfs.insert(key, source);
        }
    }
}
