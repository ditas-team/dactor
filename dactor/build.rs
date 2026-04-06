fn main() {
    prost_build::compile_protos(&["proto/system.proto"], &["proto/"]).unwrap_or_else(|e| {
        panic!(
            "Failed to compile proto/system.proto: {e}\n\
             \n\
             This requires the `protoc` compiler. Install it via:\n\
             - macOS:   brew install protobuf\n\
             - Ubuntu:  apt install -y protobuf-compiler\n\
             - Windows: choco install protoc\n\
             - Or set PROTOC env var to the protoc binary path.\n"
        );
    });
}
