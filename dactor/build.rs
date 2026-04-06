fn main() {
    prost_build::compile_protos(&["proto/system.proto"], &["proto/"])
        .expect("Failed to compile system.proto");
}
