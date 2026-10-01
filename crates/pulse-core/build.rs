use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    let files = ["article.proto", "story.proto", "embedder.proto"]
        .map(|f| proto_root.join("pulse/v1").join(f));

    println!("cargo:rerun-if-changed={}", proto_root.display());

    prost_build::Config::new()
        .protoc_executable(protoc_bin_vendored::protoc_bin_path()?)
        .compile_protos(&files, &[&proto_root])?;
    Ok(())
}
