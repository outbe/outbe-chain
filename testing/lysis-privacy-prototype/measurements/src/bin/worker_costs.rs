// Component benchmark: production coefficient kernel plus an explicitly proposed Nod encoding.
// Does not execute an OCOMP worker, production serialization, storage, consensus or certificates.
extern crate self as outbe_primitives;
pub mod error {
    #[derive(Debug)]
    pub enum PrecompileError {
        Revert(String),
    }
    pub type Result<T> = std::result::Result<T, PrecompileError>;
}
pub mod units {
    pub const SCALE_1E6_U128: u128 = 1_000_000;
    pub const SCALE_1E6_U256: alloy_primitives::U256 =
        alloy_primitives::U256::from_limbs([1_000_000, 0, 0, 0]);
}
mod algorithm {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../crates/core/lysis/src/algorithm.rs"
    ));
}
use alloy_primitives::{keccak256, U256};
use serde_json::json;
use std::{hint::black_box, time::Instant};
fn median(mut x: Vec<f64>) -> f64 {
    x.sort_by(f64::total_cmp);
    (x[(x.len() - 1) / 2] + x[x.len() / 2]) / 2.
}
fn main() {
    let mut coefficient_profiles = Vec::new();
    for k in [1usize, 8, 32] {
        let m = U256::from(1_000_000u64);
        let mut y = vec![m / U256::from(k); k];
        let sum: U256 = y.iter().copied().sum();
        y[k - 1] += m - sum;
        let populations = vec![32u64; k];
        let mut times = Vec::new();
        for _ in 0..8 {
            let t = Instant::now();
            for _ in 0..100 {
                black_box(
                    algorithm::calc_fraction_distribution_fp(
                        black_box(&y),
                        black_box(&populations),
                        k * 32,
                        black_box(U256::from(320_000u64)),
                        black_box(U256::from(640_000u64)),
                    )
                    .unwrap(),
                );
            }
            times.push(t.elapsed().as_secs_f64() * 1000. / 100.);
        }
        coefficient_profiles.push(json!({"leagues":k,"kernel_median_ms":median(times)}));
    }
    // Illustrative fixed-width layout (NOT the current Nod ABI):
    // version 1; nod/source id 32; owner 20; day 4; league 2;
    // four amount commitments 128; fraction 32; entry price 32; floor price 32;
    // reference currency 20; expiry 8; params/root 32. Total 343 bytes.
    const SIZE: usize = 343;
    let mut batch_times = Vec::new();
    let mut checksum = [0u8; 32];
    for sample in 0..8u64 {
        let t = Instant::now();
        for batch in 0..256u64 {
            let mut encoded = Vec::with_capacity(256 * SIZE);
            for row in 0..256u64 {
                let id = sample * 65_536 + batch * 256 + row;
                let mut record = [0u8; SIZE];
                record[0] = 1;
                record[1..9].copy_from_slice(&id.to_le_bytes());
                record[33..53].copy_from_slice(&[7u8; 20]);
                record[53..57].copy_from_slice(&10u32.to_le_bytes());
                record[57..59].copy_from_slice(&((row % 8) as u16).to_le_bytes());
                record[59..187].copy_from_slice(black_box(&[13u8; 128]));
                record[187..219].copy_from_slice(&U256::from(320_000u64).to_le_bytes::<32>());
                record[219..251].copy_from_slice(&U256::from(1_700_000u64).to_le_bytes::<32>());
                record[251..283].copy_from_slice(&U256::from(1_836_000u64).to_le_bytes::<32>());
                record[283..303].copy_from_slice(&[17u8; 20]);
                record[303..311].copy_from_slice(&123456u64.to_le_bytes());
                record[311..343].copy_from_slice(&[19u8; 32]);
                checksum = keccak256(black_box(&record)).0;
                encoded.extend_from_slice(&record);
            }
            black_box(encoded);
        }
        batch_times.push(t.elapsed().as_secs_f64() * 1000. / 256.);
    }
    black_box(checksum);
    let batch_ms = median(batch_times);
    println!("{}",serde_json::to_string_pretty(&json!({
        "kind":"kernel_and_proposed_encoding_only_not_worker_tps",
        "coefficients":coefficient_profiles,
        "nod_encoding":{"record_bytes":SIZE,"defined_fields_bytes":343,
          "records_per_batch":256,"batch_median_ms":batch_ms,"serialization_and_one_keccak_each_only_per_second":256000./batch_ms,
          "not_covered":["real commitments or curve validation","real Nod ABI","Merkle proofs and tree construction","input validation","source/data availability","disk/network","OCOMP execution and certification"]}
    })).unwrap());
}
