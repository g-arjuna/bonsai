fn main() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: build scripts are single-threaded at this point
    unsafe { std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap()) };

    tonic_build::configure()
        .build_server(false)
        .compile_protos(&["proto/gnmi.proto"], &["proto"])?;

    Ok(())
}
