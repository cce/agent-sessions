fn main() {
    // Compile iTerm2 API protobuf definitions
    prost_build::compile_protos(&["proto/api.proto"], &["proto/"])
        .expect("Failed to compile iTerm2 protobuf");

    tauri_build::build()
}
