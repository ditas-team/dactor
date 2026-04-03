fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Ensure protoc is found. prost-build can auto-detect protoc but sometimes
    // finds a directory instead of the binary. We check known locations first.
    if std::env::var("PROTOC").is_err() {
        let candidates = [
            // Chocolatey (Windows)
            "C:\\ProgramData\\chocolatey\\lib\\protoc\\tools\\bin\\protoc.exe",
            // Homebrew (macOS)
            "/opt/homebrew/bin/protoc",
            "/usr/local/bin/protoc",
            // Linux
            "/usr/bin/protoc",
        ];
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                std::env::set_var("PROTOC", path);
                break;
            }
        }
    }
    tonic_build::compile_protos("proto/test_node.proto")?;
    Ok(())
}
