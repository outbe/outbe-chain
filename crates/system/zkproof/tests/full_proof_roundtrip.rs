use ark_bn254::Fr;
use ark_ff::UniformRand;
use outbe_protocol::error::Error;
use outbe_protocol::primitive::signature::SignatureScheme;
use outbe_protocol::protocol::entity::{Entity, Owned};
use outbe_protocol::protocol::imt::Imt;
use outbe_protocol::protocol::key::{NftSecret, Signer};
use outbe_protocol::protocol::zk::{Circuit, ProofGenerator};
use outbe_protocol::{Codec, OutbeV1, Suite};
use outbe_zk_backend::barretenberg::Barretenberg;
use outbe_zk_canonical::full::{full_circuit_domain, FullProvable};
use outbe_zk_canonical::noir::full_proof::FullProof;
use outbe_zk_canonical::INCLUSION_DEPTH;
use rand::{rngs::StdRng, SeedableRng};

struct TestTributeDraft {
    id: Fr,
    owner: Fr,
    fields: Vec<Fr>,
}

impl Entity<OutbeV1> for TestTributeDraft {
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

impl Owned<OutbeV1> for TestTributeDraft {
    fn owner(&self) -> Result<Fr, Error> {
        Ok(self.owner)
    }
}

/// Expensive real-prover contract test. It is ignored in the ordinary unit
/// suite because it requires the 64 MiB pinned CRS and runs Barretenberg proof
/// generation; CI/release validation should run it explicitly.
#[test]
#[ignore = "requires the pinned Barretenberg CRS and full proof generation"]
fn generated_pinned_full_proof_verifies_through_chain_seam() {
    outbe_zkproof::init_crs().expect("pinned CRS must initialize");

    let mut rng = StdRng::from_seed([7; 32]);
    let (secret, public_key) = <OutbeV1 as Suite>::Signature::keypair(&mut rng);
    let nonce = Fr::rand(&mut rng);
    let owner = OutbeV1::derive_owner(&public_key, nonce).unwrap();
    let binding = OutbeV1::binding(&[1; 20], &[2; 32], 19_280_501).unwrap();
    let draft = TestTributeDraft {
        id: Fr::from(11u64),
        owner,
        fields: vec![Fr::from(20_250_115u64), Fr::from(840u64), Fr::from(100u64)],
    };
    let signer = Signer::from_secret(NftSecret::new(secret), nonce).unwrap();
    let tree = Imt::<OutbeV1>::new(full_circuit_domain(), INCLUSION_DEPTH).unwrap();
    let path = tree.empty_inclusion_path(0);
    let (witness, public) = draft
        .derive_full_witness(&mut rng, &signer, binding, &path)
        .unwrap();
    let proof =
        ProofGenerator::<OutbeV1, FullProof>::generate(&Barretenberg::default(), &witness, &public)
            .unwrap();

    let public_inputs = <FullProof as Circuit<OutbeV1>>::public_inputs(&public);
    let mut combined = Vec::with_capacity(outbe_zkproof::FULL_PROOF_COMBINED_LEN);
    combined.extend_from_slice(&(public_inputs.len() as u32).to_be_bytes());
    for value in public_inputs {
        combined.extend_from_slice(&OutbeV1::field_to_be_bytes(&value));
    }
    for field in proof.proof {
        combined.extend_from_slice(&field);
    }

    assert_eq!(combined.len(), outbe_zkproof::FULL_PROOF_COMBINED_LEN);
    assert!(outbe_zkproof::verify_full_proof(&combined).unwrap());

    combined[4 + 32] ^= 1;
    assert!(!outbe_zkproof::verify_full_proof(&combined).unwrap());
}
