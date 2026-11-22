fn main() {
    let header = "../../vendor/nvenc/nvEncodeAPI.h";
    println!("cargo:rerun-if-changed={header}");
    // Expose the header's function-like version macros as integer constants.
    let mut wrapper = format!("#include \"{header}\"\n");
    for name in [
        "NVENCAPI_VERSION",
        "NV_ENCODE_API_FUNCTION_LIST_VER",
        "NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER",
        "NV_ENC_PRESET_CONFIG_VER",
        "NV_ENC_CONFIG_VER",
        "NV_ENC_INITIALIZE_PARAMS_VER",
        "NV_ENC_CREATE_INPUT_BUFFER_VER",
        "NV_ENC_CREATE_BITSTREAM_BUFFER_VER",
        "NV_ENC_LOCK_INPUT_BUFFER_VER",
        "NV_ENC_PIC_PARAMS_VER",
        "NV_ENC_LOCK_BITSTREAM_VER",
        "NV_ENC_REGISTER_RESOURCE_VER",
        "NV_ENC_MAP_INPUT_RESOURCE_VER",
    ] {
        wrapper.push_str(&format!(
            "static const unsigned int RUST_{name} = {name};\n"
        ));
    }
    let mut builder = bindgen::Builder::default();
    // Some distributions install libclang without its builtin stddef.h. Fall
    // back to the system compiler's include directory for standard headers.
    if let Ok(output) = std::process::Command::new("cc")
        .arg("-print-file-name=include")
        .output()
    {
        let include = String::from_utf8_lossy(&output.stdout);
        if std::path::Path::new(include.trim())
            .join("stddef.h")
            .is_file()
        {
            builder = builder.clang_args(["-idirafter", include.trim()]);
        }
    }
    builder
        .header_contents("nvenc-wrapper.h", &wrapper)
        .allowlist_type("NV_ENCODE_API_FUNCTION_LIST")
        .allowlist_var("RUST_.*")
        .prepend_enum_name(false)
        .layout_tests(false)
        .derive_default(true)
        .generate_comments(false)
        .generate()
        .expect("generate NVIDIA SDK bindings")
        .write_to_file(
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("nvenc.rs"),
        )
        .unwrap();
}
