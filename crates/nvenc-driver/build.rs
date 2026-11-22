// SPDX-License-Identifier: MIT
fn main() {
    let mut bindings = bindgen::Builder::default()
        .header("nvenc-wrapper.h")
        .clang_arg("-I../../vendor/nvenc")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    if let Ok(output) = std::process::Command::new("cc")
        .arg("-print-file-name=include")
        .output()
    {
        let dir = String::from_utf8_lossy(&output.stdout);
        if std::path::Path::new(dir.trim()).join("stddef.h").is_file() {
            bindings = bindings.clang_args(["-idirafter", dir.trim()]);
        }
    }
    bindings
        .allowlist_type("NV_ENCODE_API_FUNCTION_LIST")
        .allowlist_type("NV_ENC_PIC_FLAGS")
        .allowlist_var("RUST_.*|NV_ENC_PIC_FLAG_.*")
        .prepend_enum_name(false)
        .layout_tests(false)
        .derive_default(true)
        .generate_comments(false)
        .generate()
        .expect("generate NVENC bindings")
        .write_to_file(
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("nvenc.rs"),
        )
        .expect("write NVENC bindings");
}
