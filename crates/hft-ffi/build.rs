fn main() {
    println!("cargo::rustc-check-cfg=cfg(native_shim)");
    #[cfg(feature = "vendor-sdk")]
    compile_shim();
}

#[cfg(feature = "vendor-sdk")]
fn compile_shim() {
    match cc::Build::new()
        .file("native/test_shim.c")
        .flag_if_supported("-std=c11")
        .flag_if_supported("/std:c11")
        .try_compile("hft_ffi_test_shim")
    {
        Ok(()) => println!("cargo::rustc-cfg=native_shim"),
        Err(error) => println!("cargo::warning=native test shim skipped: {error}"),
    }
}
