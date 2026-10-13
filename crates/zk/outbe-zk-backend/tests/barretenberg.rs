//! Real barretenberg prove→verify round-trips.
//!
//! Generates actual UltraHonkKeccak proofs from genuine witnesses and verifies
//! them through the FFI backend — the full on-device path. Run with
//! `cargo test -p outbe-zk-backend --test barretenberg`.

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
use outbe_zk_backend::barretenberg::Barretenberg;

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

    let combined = demo_tribute::encode_combined_proof(public.clone(), proof.proof).unwrap();
    assert_eq!(combined.len(), demo_tribute::COMBINED_LEN);
    let decoded = demo_tribute::decode_public_inputs(&combined).unwrap();
    assert_eq!(decoded.derived_owner, owner);
    assert_eq!(decoded.binding_hash, binding);
}
