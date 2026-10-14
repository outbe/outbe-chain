#![allow(clippy::print_stdout)] // diagnostic #[ignore]d tests that print pin/complexity tables
//! Regenerate the `PINNED_G1_SHA256` table in `barretenberg/srs.rs`.
//!
//! For each canonical circuit it computes the SRS point count bb actually needs
//! (`circuit_stats` → dyadic domain `+ 1`; no proving, so it's fast) and the
//! SHA-256 of that G1 prefix from the local Aztec CRS, printing ready-to-paste
//! pin entries. Ignored by default (needs the local CRS + libbb); run with:
//!   cargo test -p outbe-zk-backend --test srs_pins -- --ignored --nocapture
//! then paste the printed `(points, [..])` rows into `PINNED_G1_SHA256`.

use std::io::Read;

use barretenberg_rs::generated_types::{CircuitInput, ProofSystemSettings};
use base64::Engine;
use flate2::read::GzDecoder;
use outbe_barretenberg::{with_api, NativeApi, RuntimeError};
use sha2::{Digest, Sha256};

use outbe_zk::noir::demo_tribute::DemoTribute;
use outbe_zk::noir::niflheim_tribute::NiflheimTribute;
use outbe_zk::CircuitId;

const G1_POINT_SIZE: usize = 64;

/// UltraHonk + keccak, ZK on — matches `Barretenberg::default()` / the prover,
/// so the gate count (and thus point count) matches what proving actually sizes.
fn keccak_settings() -> ProofSystemSettings {
    ProofSystemSettings {
        ipa_accumulation: false,
        oracle_hash_type: "keccak".to_string(),
        disable_zk: false,
        optimized_solidity_verifier: false,
    }
}

fn acir(b64: &str) -> Vec<u8> {
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap();
    let mut raw = Vec::new();
    GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut raw)
        .unwrap();
    raw
}

fn next_pow2(n: u32) -> u32 {
    2u32.pow((n as f64).log2().ceil() as u32)
}

fn cache_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("BB_CRS_PATH") {
        return p.into();
    }
    std::path::Path::new(&std::env::var("HOME").unwrap_or_default()).join(".bb-crs/bn254_g1.dat")
}

fn num_points(api: &mut NativeApi, acir: &[u8]) -> u32 {
    let circuit = CircuitInput {
        name: String::new(),
        bytecode: acir.to_vec(),
        verification_key: Vec::new(),
    };
    let info = api
        .circuit_stats(circuit, false, keccak_settings())
        .unwrap();
    info.num_gates_dyadic.max(next_pow2(info.num_gates)) + 1
}

fn pin_row(label: &str, n: u32) -> String {
    let bytes = std::fs::read(cache_path()).unwrap();
    let need = n as usize * G1_POINT_SIZE;
    assert!(
        bytes.len() >= need,
        "local CRS too small for {label} ({need} bytes)"
    );
    let digest = Sha256::digest(&bytes[..need]);
    let body: Vec<String> = digest.iter().map(|b| format!("0x{b:02x}")).collect();
    format!("    // {label}\n    ({n}, [{}]),", body.join(", "))
}

#[test]
#[ignore = "regen helper: needs local CRS + libbb; run with --ignored --nocapture"]
fn print_srs_pins() {
    with_api(|api| {
        let circuits: &[(&str, &str)] = &[
            ("demo tribute", DemoTribute::BYTECODE_B64),
            ("niflheim tribute", NiflheimTribute::BYTECODE_B64),
        ];
        println!("\n--- paste into PINNED_G1_SHA256 ---");
        for (label, b64) in circuits {
            let n = num_points(api, &acir(b64));
            println!("{}", pin_row(label, n));
        }
        println!("--- end ---\n");
        Ok::<_, RuntimeError>(())
    })
    .unwrap();
}

fn gate_counts(api: &mut NativeApi, acir: &[u8]) -> (u32, u32) {
    let circuit = CircuitInput {
        name: String::new(),
        bytecode: acir.to_vec(),
        verification_key: Vec::new(),
    };
    let info = api
        .circuit_stats(circuit, false, keccak_settings())
        .unwrap();
    (info.num_gates, info.num_gates_dyadic)
}

/// Print a markdown table of each circuit's gate count (UltraHonkKeccak, ZK on).
/// Capture before/after a hash swap to measure the complexity delta:
///   cargo test -p outbe-zk-backend --test srs_pins print_circuit_complexity -- --ignored --nocapture
#[test]
#[ignore = "complexity capture: needs libbb; run with --ignored --nocapture"]
fn print_circuit_complexity() {
    with_api(|api| {
        let circuits: &[(&str, &str)] = &[
            ("demo tribute", DemoTribute::BYTECODE_B64),
            ("niflheim tribute", NiflheimTribute::BYTECODE_B64),
        ];
        println!("\n--- circuit complexity ---");
        println!("| circuit | num_gates | dyadic (2^k) |");
        println!("|---|---|---|");
        for (label, b64) in circuits {
            let (g, d) = gate_counts(api, &acir(b64));
            println!("| {label} | {g} | {d} |");
        }
        println!("--- end ---\n");
        Ok::<_, RuntimeError>(())
    })
    .unwrap();
}
