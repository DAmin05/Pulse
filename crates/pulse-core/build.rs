use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    let files = ["article.proto", "story.proto", "embedder.proto"]
        .map(|f| proto_root.join("pulse/v1").join(f));

    println!("cargo:rerun-if-changed={}", proto_root.display());

    let mut config = prost_build::Config::new();
    config.protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    // Messages plus gRPC clients; servers live in other languages (the Embedder is Python).
    tonic_build::configure()
        .build_server(false)
        .compile_protos_with_config(config, &files, &[&proto_root])?;
    Ok(())
}
