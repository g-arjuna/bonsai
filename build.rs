use std::fs;
use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: build scripts are single-threaded at this point
    unsafe { std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap()) };

    // gNMI: client-only (we are the gNMI client)
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(&["proto/gnmi.proto"], &["proto"])?;

    // Bonsai API: server + client. Collector mode uses the Rust client to
    // stream decoded telemetry into a remote core.
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/bonsai_service.proto"], &["proto"])?;

    copy_lbug_shared_dll();

    Ok(())
}

fn copy_lbug_shared_dll() {
    println!("cargo:rerun-if-env-changed=LBUG_SHARED");

    if !cfg!(windows) || std::env::var_os("LBUG_SHARED").is_none() {
        return;
    }

    let Some(profile_dir) = cargo_profile_dir() else {
        println!("cargo:warning=unable to locate Cargo profile directory for lbug_shared.dll copy");
        return;
    };

    let Some(dll_path) = newest_lbug_shared_dll(&profile_dir) else {
        println!(
            "cargo:warning=lbug_shared.dll was not found under {}; standalone release binaries may need a second cargo build after lbug finishes",
            profile_dir.display()
        );
        return;
    };

    let destination = profile_dir.join("lbug_shared.dll");
    if let Err(error) = fs::copy(&dll_path, &destination) {
        println!(
            "cargo:warning=failed to copy {} to {}: {error}",
            dll_path.display(),
            destination.display()
        );
    }
}

fn cargo_profile_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR")?);
    out_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

fn newest_lbug_shared_dll(profile_dir: &Path) -> Option<PathBuf> {
    let build_dir = profile_dir.join("build");
    let entries = fs::read_dir(build_dir).ok()?;
    let mut candidates: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            entry
                .path()
                .join("out")
                .join("build")
                .join("src")
                .join("lbug_shared.dll")
        })
        .filter(|path| path.exists())
        .collect();

    candidates.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    candidates.pop()
}
