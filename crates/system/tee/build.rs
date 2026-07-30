fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_NATIVE_DCAP");

    if std::env::var_os("CARGO_FEATURE_NATIVE_DCAP").is_none() {
        return;
    }

    let target = std::env::var("TARGET").expect("Cargo must set TARGET");
    assert_eq!(
        target, "x86_64-unknown-linux-gnu",
        "native-dcap is pinned to the x86_64 GNU/Linux Intel QVL artifact"
    );

    println!("cargo:rerun-if-changed=native/qvl_wrapper.c");
    cc::Build::new()
        .file("native/qvl_wrapper.c")
        .flag_if_supported("-std=c11")
        .warnings_into_errors(true)
        .compile("outbe_native_qvl_wrapper");
    println!("cargo:rustc-link-lib=dylib=sgx_dcap_quoteverify");
}
