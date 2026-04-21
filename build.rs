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

    Ok(())
}
