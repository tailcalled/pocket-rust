fn main() {
    use pocket_rust::{Library, Vfs, compile};
    let mut std_vfs = Vfs::new();
    load_dir(std::path::Path::new("lib/std"), std::path::Path::new("lib/std"), &mut std_vfs);
    let stdlib = Library { name: "std".to_string(), vfs: std_vfs, entry: "lib.rs".to_string(), prelude: true };
    let mut user = Vfs::new();
    user.insert("lib.rs".to_string(), "pub fn answer() -> u32 { let f = || 7u32; f.call(()) }".to_string());
    match compile(&[stdlib], &user, "lib.rs") {
        Ok(m) => {
            let bytes = m.encode();
            std::fs::write("/tmp/closure.wasm", &bytes).unwrap();
            eprintln!("wrote {} bytes", bytes.len());
        }
        Err(e) => eprintln!("compile err: {}", e),
    }
}

fn load_dir(root: &std::path::Path, dir: &std::path::Path, vfs: &mut pocket_rust::Vfs) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let p = entry.path();
        if p.is_dir() {
            load_dir(root, &p, vfs);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            let key: String = p.strip_prefix(root).unwrap().components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>().join("/");
            vfs.insert(key, std::fs::read_to_string(&p).unwrap());
        }
    }
}
