fn main() {
    println!("cargo::rerun-if-changed=native/hft_vendor_api.h");
    println!("cargo::rerun-if-changed=native/test_shim.c");
    println!("cargo::rerun-if-changed=native/header_cpp_test.cpp");
    #[cfg(feature = "vendor-sdk")]
    compile_shim();
}

#[cfg(feature = "vendor-sdk")]
fn compile_shim() {
    cc::Build::new()
        .file("native/test_shim.c")
        .flag_if_supported("-std=c11")
        .flag_if_supported("/std:c11")
        .compile("hft_ffi_test_shim");

    cc::Build::new()
        .cpp(true)
        .cpp_link_stdlib(None)
        .file("native/header_cpp_test.cpp")
        .flag_if_supported("-std=c++11")
        .flag_if_supported("/std:c++14")
        .compile("hft_ffi_header_cpp_test");
}
