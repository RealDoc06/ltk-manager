fn main() {
    println!("cargo:rerun-if-changed=native/patcher.cpp");
    println!("cargo:rerun-if-changed=native/macho.hpp");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if target_os == "macos" && target_arch == "aarch64" {
        cc::Build::new()
            .cpp(true)
            .std("c++20")
            .warnings(true)
            .file("native/patcher.cpp")
            .compile("ltk_macos_patch_engine");
    }
}
