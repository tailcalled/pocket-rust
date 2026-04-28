use pocket_rust::{Library, Vfs, compile};
use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        let prog = if args.is_empty() { "pocket-rust" } else { args[0].as_str() };
        eprintln!("usage: {} <input-dir> <output.wasm>", prog);
        return ExitCode::from(2);
    }
    let input_dir = Path::new(&args[1]);
    let output_path = Path::new(&args[2]);

    let mut user_vfs = Vfs::new();
    if let Err(e) = load_dir(input_dir, input_dir, &mut user_vfs) {
        eprintln!("error: {}", e);
        return ExitCode::from(1);
    }

    let stdlib_path = Path::new("lib/std");
    let mut stdlib_vfs = Vfs::new();
    if let Err(e) = load_dir(stdlib_path, stdlib_path, &mut stdlib_vfs) {
        eprintln!("error loading stdlib at {}: {}", stdlib_path.display(), e);
        return ExitCode::from(1);
    }
    let std_lib = Library {
        name: "std".to_string(),
        vfs: stdlib_vfs,
        entry: "lib.rs".to_string(),
    };

    match compile(&[std_lib], &user_vfs, "main.rs") {
        Ok(module) => match fs::write(output_path, &module.encode()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error writing {}: {}", output_path.display(), e);
                ExitCode::from(1)
            }
        },
        Err(msg) => {
            eprintln!("compile error: {}", msg);
            ExitCode::from(1)
        }
    }
}

fn load_dir(root: &Path, dir: &Path, vfs: &mut Vfs) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("read_dir {}: {}", dir.display(), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry in {}: {}", dir.display(), e))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("file_type {}: {}", path.display(), e))?;
        if file_type.is_dir() {
            load_dir(root, &path, vfs)?;
        } else if file_type.is_file() && path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| format!("strip_prefix {}: {}", path.display(), e))?;
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let source = fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {}", path.display(), e))?;
            vfs.insert(key, source);
        }
    }
    Ok(())
}
