//! These tests generate and verify real Barretenberg proofs.
//!
//! They generate UltraHonkKeccak proofs from genuine witnesses and verify them through the FFI backend.
//! They execute the complete proof flow on the device.
//! Run `cargo test -p outbe-zk-backend --test barretenberg`.

use ark_ff::UniformRand;

use outbe_zk::demo_tribute::{self, demo_tribute_domain, DemoTributeProvable};
use outbe_zk::error::Error;
use outbe_zk::noir::demo_tribute::DemoTribute;
use outbe_zk::primitive::{hash, signature};
use outbe_zk::protocol::entity::{Entity, Owned};
use outbe_zk::protocol::imt::Imt;
use outbe_zk::protocol::key::{NftSecret, Signer};
use outbe_zk::protocol::zk::{ProofGenerator, ProofVerifier};
use outbe_zk::Fr;
use outbe_zk::INCLUSION_DEPTH;
use outbe_zk_backend::barretenberg::{init_crs, verify_circuit, Barretenberg, BarretenbergHandler};

struct TestNft {
    id: Fr,
    owner: Fr,
    fields: Vec<Fr>,
}

impl Entity for TestNft {
    fn id_seed(&self) -> Result<Fr, Error> {
        Ok(self.id)
    }
    fn encode_id_body(&self, _out: &mut Vec<Fr>) -> Result<(), Error> {
        Ok(())
    }
    fn encode_body(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
        out.extend_from_slice(&self.fields);
        Ok(())
    }
}
impl Owned for TestNft {
    fn owner(&self) -> Result<Fr, Error> {
        Ok(self.owner)
    }
}

#[test]
fn demo_tribute_prove_verify_round_trip() {
    init_crs().expect("launch initializes the handler before proof generation");
    let mut rng = ark_std::test_rng();
    let (sk, pk) = signature::keypair(&mut rng);
    let nonce = Fr::rand(&mut rng);
    let owner = hash::derive_owner(&pk, nonce).unwrap();
    let binding = hash::binding(&[3u8; 20], &[4u8; 32], 99, 0xdead).unwrap();
    let td = TestNft {
        id: owner,
        owner,
        fields: vec![Fr::from(978u64), Fr::from(100u64)],
    };
    let signer = Signer::from_secret(NftSecret::new(sk), nonce).unwrap();
    let tree = Imt::new(demo_tribute_domain(), Fr::from(0u64), INCLUSION_DEPTH).unwrap();
    let path = tree.empty_inclusion_path(0);
    let (witness, public) = td
        .derive_demo_tribute_witness(&mut rng, &signer, binding, &path)
        .unwrap();

    let proof =
        ProofGenerator::<DemoTribute>::generate(&Barretenberg::default(), &witness, &public)
            .expect("bb prove");
    assert!(
        ProofVerifier::<DemoTribute>::verify(&Barretenberg::default(), &public, &proof).unwrap(),
        "valid demo tribute proof must verify"
    );

    // Call native verification and statistics concurrently through the same handler.
    let acir = {
        use base64::Engine;
        use outbe_zk::CircuitId;
        use std::io::Read;
        let compressed = base64::engine::general_purpose::STANDARD
            .decode(DemoTribute::BYTECODE_B64)
            .unwrap();
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(compressed.as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        raw
    };
    std::thread::scope(|scope| {
        let verify = scope.spawn(|| {
            for _ in 0..2 {
                assert!(ProofVerifier::<DemoTribute>::verify(
                    &Barretenberg::default(),
                    &public,
                    &proof
                )
                .unwrap());
            }
        });
        let stats = scope.spawn(|| {
            let info = BarretenbergHandler::global()
                .unwrap()
                .circuit_stats(
                    barretenberg_rs::generated_types::CircuitInput {
                        name: String::new(),
                        bytecode: acir,
                        verification_key: Vec::new(),
                    },
                    false,
                    barretenberg_rs::generated_types::ProofSystemSettings {
                        ipa_accumulation: false,
                        oracle_hash_type: "keccak".into(),
                        disable_zk: false,
                        optimized_solidity_verifier: false,
                    },
                )
                .unwrap();
            assert!(info.num_gates_dyadic > 0);
        });
        verify.join().unwrap();
        stats.join().unwrap();
    });
    init_crs().unwrap();
    let combined = demo_tribute::encode_combined_proof(public.clone(), proof.proof).unwrap();
    assert!(verify_circuit::<DemoTribute>(&combined).unwrap());
    let mut tampered = combined.clone();
    tampered[4..36].fill(0);
    assert!(!verify_circuit::<DemoTribute>(&tampered).unwrap());
    assert_eq!(combined.len(), demo_tribute::COMBINED_LEN);
    let decoded = demo_tribute::decode_public_inputs(&combined).unwrap();
    assert_eq!(decoded.derived_owner, owner);
    assert_eq!(decoded.binding_hash, binding);
}
