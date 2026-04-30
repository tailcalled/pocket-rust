fn main() {
    use pocket_rust::{Library, Vfs, compile};
    use std::path::Path;
    let stdlib_path = Path::new("lib/std");
    let mut std_vfs = Vfs::new();
    load_dir(stdlib_path, stdlib_path, &mut std_vfs);
    let stdlib = Library {
        name: "std".to_string(),
        vfs: std_vfs,
        entry: "lib.rs".to_string(),
        prelude: true,
    };
    let dir_path = "examples/generic_pair";
    let root = Path::new(dir_path);
    let mut vfs = Vfs::new();
    load_dir(root, root, &mut vfs);
    let module = compile(&[stdlib], &vfs, "lib.rs").expect("compile failed");
    let bytes = module.encode();
    std::fs::write("/tmp/out.wasm", &bytes).unwrap();
    println!("Wrote {} bytes", bytes.len());
}

fn load_dir(root: &std::path::Path, dir: &std::path::Path, vfs: &mut pocket_rust::Vfs) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            load_dir(root, &path, vfs);
        } else if file_type.is_file() && path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let rel = path.strip_prefix(root).unwrap();
            let key = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            let source = std::fs::read_to_string(&path).unwrap();
            vfs.insert(key, source);
        }
    }
}
