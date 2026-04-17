fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Phase 1: no .proto files yet.
    // When proto/gnmi.proto and proto/gnmi_ext.proto are added,
    // uncomment and extend:
    //
    // tonic_build::configure()
    //     .build_server(false)
    //     .compile_protos(&["proto/gnmi.proto"], &["proto"])?;

    Ok(())
}
