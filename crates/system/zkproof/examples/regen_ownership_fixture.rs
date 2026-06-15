//! Regenerate the canonical OWNERSHIP proof fixture consumed by
//! `tests::zk_verify_ownership_fixture_returns_one` and
//! `tests::dispatch_groth16_ownership_fixture_verifies`.
//!
//! Run from the repo root:
//!
//!   cargo run -p outbe-zkproof --example regen_ownership_fixture -- \
//!       --out crates/system/zkproof/tests/fixtures/ownership.bin
//!
//! Output format matches what the tests read:
//!   `OWNERSHIP.circuit_hash[32] || NoirProof::to_combined()`.
//!
//! Re-run whenever an `outbe-circuits` bump changes the OWNERSHIP
//! circuit_hash / VK or the underlying Noir + Barretenberg toolchain
//! changes the combined-proof byte layout.

use std::path::PathBuf;

use ark_bn254::Fr;
use ark_ff::UniformRand;
use rand::rngs::OsRng;

use outbe_crypto_common::witness::{HashableNFT, OwnableNFT};
use outbe_zk_canonical::OWNERSHIP;
use outbe_zk_circuit_noir::{
    circuit_loader, testing, NoirCircuitConfig, NoirOwnershipCircuit, ZKCircuit, ZKMode,
    OWNERSHIP_PROOF_JSON,
};

struct OwnershipNft {
    ownership: Fr,
    nft_hash: Fr,
}

impl OwnableNFT for OwnershipNft {
    fn ownership(&self) -> Fr {
        self.ownership
    }
}

impl HashableNFT for OwnershipNft {
    fn hash(&self) -> Result<Fr, outbe_crypto_common::OutbeCryptoError> {
        Ok(self.nft_hash)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let out_path: PathBuf = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .map(PathBuf::from)
        .ok_or("expected: --out <path/to/ownership.bin>")?;

    // Brings up the Barretenberg CRS the prover needs (same path the
    // node uses at startup); panics from the underlying setup are
    // caught so a missing/invalid network env doesn't tear the binary
    // down silently.
    outbe_zkproof::init_crs();

    let key = testing::random_grumpkin_key()?;
    let nonce = Fr::rand(&mut OsRng);
    let ownership = testing::ownership_from_grumpkin_key(&key, nonce)?;
    let nft_hash = Fr::rand(&mut OsRng);
    let nft = OwnershipNft {
        ownership,
        nft_hash,
    };

    let witness = testing::build_ownership_witness(&nft, &key, nonce)?;
    let bytecode = circuit_loader::load_circuit_from_str(OWNERSHIP_PROOF_JSON)?;
    let config = NoirCircuitConfig {
        circuit: bytecode,
        version: "0.10.0",
        low_memory: false,
        scheme: ZKMode::UltraHonkKeccak,
    };
    let circuit = NoirOwnershipCircuit::setup(config)?;
    let proof = circuit.prove(witness)?;
    let combined = proof.to_combined();

    let mut out = Vec::with_capacity(32 + combined.len());
    out.extend_from_slice(&OWNERSHIP.circuit_hash);
    out.extend_from_slice(&combined);
    std::fs::write(&out_path, &out)?;

    eprintln!(
        "wrote {} bytes ({} combined proof) to {}",
        out.len(),
        combined.len(),
        out_path.display()
    );
    Ok(())
}
