/// Validates that bonsai.toml.example parses without error.
/// If this test fails, the example file has drifted from config.rs.
#[tokio::test]
async fn bonsai_toml_example_parses() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/bonsai.toml.example");
    bonsai::config::load(path)
        .await
        .expect("bonsai.toml.example failed to parse — did config.rs change a field name or type?");
}
