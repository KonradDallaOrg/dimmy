fn main() {
    // When both local-stt (whisper-rs) and local-llm (llama-cpp-2) are enabled,
    // both crates compile their own copy of ggml from source. The ggml symbols
    // (quantize_*, ggml_*) collide at link time.
    //
    // Both crates use recent, compatible ggml versions so allowing multiple
    // definitions is safe — the linker picks the first definition which is
    // functionally identical in both copies.
    #[cfg(all(feature = "local-stt", feature = "local-llm"))]
    {
        #[cfg(target_os = "windows")]
        println!("cargo:rustc-link-arg=/FORCE:MULTIPLE");

        #[cfg(target_os = "linux")]
        println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");

        #[cfg(target_os = "macos")]
        println!("cargo:rustc-link-arg=-Wl,-multiply_defined,suppress");
    }
}
