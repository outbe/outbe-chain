use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

const PACKAGES: &[(&str, &str)] = &[
    ("libsgx-dcap-quote-verify", "1.26.100.1-noble1"),
    ("libsgx-dcap-quote-verify-dev", "1.26.100.1-noble1"),
    ("libsgx-headers", "2.29.100.1-noble1"),
    ("libstdc++6", "14.2.0-4ubuntu2~24.04.1"),
    ("libgcc-s1", "14.2.0-4ubuntu2~24.04.1"),
];

const ARTIFACTS: &[(&str, u64, &str)] = &[
    (
        "/usr/lib/x86_64-linux-gnu/libsgx_dcap_quoteverify.so.1.13.103.0",
        5_322_424,
        "4745bc5b46cbdc17a78119ae2db08f54b86ff9077c5ab480f378741396365aef",
    ),
    (
        "/usr/lib/x86_64-linux-gnu/libstdc++.so.6.0.33",
        2_592_224,
        "1fd75fe70354a416d75aef22bcae68c47bd25d20e2d0568c30b1a9838cf62f11",
    ),
    (
        "/usr/lib/x86_64-linux-gnu/libgcc_s.so.1",
        183_024,
        "d93224d2b0dab4247598be683adca02f5cf00586f99c187579cd7e92058fb7cb",
    ),
];

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
    for (package, expected_version) in PACKAGES {
        verify_package(package, expected_version);
    }
    for (path, expected_size, expected_sha256) in ARTIFACTS {
        verify_artifact(path, *expected_size, expected_sha256);
        println!("cargo:rerun-if-changed={path}");
    }

    println!("cargo:rerun-if-changed=native/qvl_wrapper.c");
    cc::Build::new()
        .file("native/qvl_wrapper.c")
        .flag_if_supported("-std=c11")
        .warnings_into_errors(true)
        .compile("outbe_native_qvl_wrapper");
    println!("cargo:rustc-link-search=native=/usr/lib/x86_64-linux-gnu");
    println!("cargo:rustc-link-lib=dylib=sgx_dcap_quoteverify");
}

fn verify_package(package: &str, expected_version: &str) {
    let output = Command::new("dpkg-query")
        .args(["--show", "--showformat=${Version}", package])
        .output()
        .unwrap_or_else(|error| panic!("failed to query native-QVL package {package}: {error}"));
    assert!(
        output.status.success(),
        "required native-QVL package is not installed: {package}"
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("dpkg-query version must be UTF-8"),
        expected_version,
        "native-QVL package version mismatch: {package}"
    );
}

fn verify_artifact(path: &str, expected_size: u64, expected_sha256: &str) {
    assert!(
        Path::new(path).is_absolute(),
        "native-QVL artifact path must be absolute"
    );
    let payload = std::fs::read(path)
        .unwrap_or_else(|error| panic!("failed to read native-QVL artifact {path}: {error}"));
    assert_eq!(
        u64::try_from(payload.len()).expect("artifact length must fit u64"),
        expected_size,
        "native-QVL artifact size mismatch: {path}"
    );
    let digest = Sha256::digest(&payload)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        digest, expected_sha256,
        "native-QVL artifact SHA-256 mismatch: {path}"
    );
}
