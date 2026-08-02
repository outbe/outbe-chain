//! L2Registry zk signature gate on `offerTribute` (PFS-001-10 / PFS-001-11).
//!
//! The harness plays the L2 network: it generates a BLS MinSig keypair,
//! registers the operator's EOA as the network's L1 address, and drives the
//! `zk_enabled` toggle. With the gate enabled an unsigned offer must revert;
//! a real `outbe.full_proof@1.0.0` whose root is signed with the registered key
//! must pass the full gate and issue the canonical Tribute through the normal
//! enclave path.

use alloy_primitives::{Address, B256};
use ark_bn254::Fr;
use ark_ff::UniformRand;
use bytes::Bytes as CodecBytes;
use commonware_codec::{DecodeExt, Encode};
use commonware_cryptography::bls12381::primitives::{
    group::Private,
    ops::{self, sign_message},
    variant::MinSig,
};
use cucumber::{then, when};
use outbe_protocol::primitive::signature::SignatureScheme;
use outbe_protocol::protocol::imt::Imt;
use outbe_protocol::protocol::key::{NftSecret, Signer};
use outbe_protocol::protocol::zk::{Circuit, ProofGenerator};
use outbe_protocol::{Codec, OutbeV1, Suite};
use outbe_protocol_derive::Entity;
use outbe_zk_backend::barretenberg::Barretenberg;
use outbe_zk_canonical::full::FullProvable;
use outbe_zk_canonical::noir::full_proof::FullProof;
use outbe_zk_canonical::INCLUSION_DEPTH;
use rand::{rngs::StdRng, SeedableRng};

use crate::world::rpc::TributeZkOffer;
use crate::world::World;

/// Namespace must match `outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE`.
/// Kept as a literal so the harness exercises the external signing contract
/// rather than importing the runtime crate.
const ZK_MERKLE_ROOT_NAMESPACE: &[u8] = b"_PSO_CHAIN_COMMITMENT_ROOT";

const L2_CHAIN_ID: u64 = 4242;

#[derive(Entity)]
struct TributeDraftFixture {
    #[outbe(id_seed)]
    id: B256,
    #[outbe(body, owner, pos = 0)]
    derived_owner: B256,
    #[outbe(body, pos = 1)]
    worldwide_day: u64,
    #[outbe(body, pos = 2)]
    currency: u16,
    #[outbe(body, pos = 3)]
    base: u64,
    #[outbe(body, pos = 4)]
    atto: u64,
    #[outbe(body, pos = 5)]
    su_ids: Vec<B256>,
}

struct ZkOfferFixture {
    tribute_draft_id_hex: String,
    su_hash_hex: String,
    merkle_root: [u8; 32],
    proof_hex: String,
}

fn low_b256(last_byte: u8) -> B256 {
    let mut value = [0u8; 32];
    value[31] = last_byte;
    B256::from(value)
}

fn field_bytes(field: &Fr) -> [u8; 32] {
    OutbeV1::field_to_be_bytes(field)
        .try_into()
        .expect("BN254 field encoding is 32 bytes")
}

fn generate_zk_offer_fixture(
    l1_owner: Address,
    chain_id: u64,
    worldwide_day: u64,
) -> ZkOfferFixture {
    // CRS setup uses a blocking download/read path. Generate on a plain thread
    // rather than inside cucumber's Tokio runtime.
    std::thread::spawn(move || {
        outbe_zkproof::init_crs().expect("pinned CRS initializes for e2e proof generation");

        let tribute_draft_id = low_b256(0x11);
        let su_hash = low_b256(0x22);
        let mut rng = StdRng::from_seed([9; 32]);
        let (secret, public_key) = <OutbeV1 as Suite>::Signature::keypair(&mut rng);
        let nonce = Fr::rand(&mut rng);
        let derived_owner = OutbeV1::derive_owner(&public_key, nonce).unwrap();
        let draft = TributeDraftFixture {
            id: tribute_draft_id,
            derived_owner: B256::from(field_bytes(&derived_owner)),
            worldwide_day,
            currency: 840,
            base: 100,
            atto: 0,
            su_ids: vec![su_hash],
        };
        let binding =
            OutbeV1::binding(&l1_owner.into_array(), &tribute_draft_id.0, chain_id).unwrap();
        let signer = Signer::from_secret(NftSecret::new(secret), nonce).unwrap();
        let tree = Imt::<OutbeV1>::new(INCLUSION_DEPTH).unwrap();
        let path = tree.empty_inclusion_path(0);
        let (witness, public) = draft
            .derive_full_witness(&mut rng, &signer, binding, &path)
            .unwrap();
        let proof = ProofGenerator::<OutbeV1, FullProof>::generate(
            &Barretenberg::default(),
            &witness,
            &public,
        )
        .unwrap();

        let public_inputs = <FullProof as Circuit<OutbeV1>>::public_inputs(&public);
        assert_eq!(public_inputs.len(), 4);
        let merkle_root = field_bytes(&public_inputs[3]);
        let mut combined = Vec::with_capacity(outbe_zkproof::FULL_PROOF_COMBINED_LEN);
        combined.extend_from_slice(&(public_inputs.len() as u32).to_be_bytes());
        for value in public_inputs {
            combined.extend_from_slice(&field_bytes(&value));
        }
        for field in proof.proof {
            combined.extend_from_slice(&field);
        }
        assert_eq!(combined.len(), outbe_zkproof::FULL_PROOF_COMBINED_LEN);

        ZkOfferFixture {
            tribute_draft_id_hex: format!("0x{}", hex::encode(tribute_draft_id)),
            su_hash_hex: format!("0x{}", hex::encode(su_hash)),
            merkle_root,
            proof_hex: format!("0x{}", hex::encode(combined)),
        }
    })
    .join()
    .expect("e2e proof generation thread")
}

fn operator_key(world: &World) -> String {
    world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key")
}

fn operator_address(world: &World, key: &str) -> Address {
    world
        .rpc
        .address_of(key)
        .expect("operator address")
        .parse()
        .expect("operator address hex")
}

#[when("an L2 network is registered for the operator with zk enabled")]
fn register_l2_network_with_zk(world: &mut World) {
    let key = operator_key(world);
    let l1_address = operator_address(world, &key);

    let (private, public) = ops::keypair::<_, MinSig>(&mut rand_core::OsRng);
    let public = public.encode().to_vec();
    world.state.l2_bls_private_hex = Some(hex::encode(private.encode()));
    world.state.l2_chain_id = Some(L2_CHAIN_ID);

    world
        .rpc
        .l2_register_network(&key, L2_CHAIN_ID, l1_address, &public)
        .expect("registerNetwork succeeds");
    world
        .rpc
        .l2_set_zk_enabled(&key, L2_CHAIN_ID, true)
        .expect("setZkEnabled(true) succeeds");
}

#[when("zk verification is disabled for the registered L2 network")]
fn disable_l2_zk(world: &mut World) {
    let key = operator_key(world);
    let chain_id = world.state.l2_chain_id.expect("registered L2 chain id");
    world
        .rpc
        .l2_set_zk_enabled(&key, chain_id, false)
        .expect("setZkEnabled(false) succeeds");
}

#[when("the operator submits an encrypted tribute offer without an L2 signature")]
fn offer_without_signature(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    let key = operator_key(world);
    let tx_hash = world
        .rpc
        .tribute_offer(&key, &wwd)
        .expect("outbe-cli returned offerTribute transaction hash");
    world.state.l2_rejected_offer_tx_hash = Some(tx_hash);
}

#[when("the operator submits an encrypted tribute offer with a valid ZK proof and L2 signature")]
fn offer_with_valid_zk_proof(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    let key = operator_key(world);
    let l1_owner = operator_address(world, &key);
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("chain id");
    let fixture = generate_zk_offer_fixture(
        l1_owner,
        chain_id,
        wwd.parse().expect("worldwide-day number"),
    );
    let private_hex = world
        .state
        .l2_bls_private_hex
        .as_deref()
        .expect("registered L2 BLS key");
    let private = <Private as DecodeExt<()>>::decode(CodecBytes::from(
        hex::decode(private_hex).expect("stored key hex"),
    ))
    .expect("decode stored BLS key");
    let signature =
        sign_message::<MinSig>(&private, ZK_MERKLE_ROOT_NAMESPACE, &fixture.merkle_root)
            .encode()
            .to_vec();

    let tx_hash = world
        .rpc
        .tribute_offer_with_zk(
            &key,
            &wwd,
            TributeZkOffer {
                tribute_draft_id_hex: &fixture.tribute_draft_id_hex,
                su_hash_hex: &fixture.su_hash_hex,
                merkle_root_hex: &format!("0x{}", hex::encode(fixture.merkle_root)),
                proof_hex: &fixture.proof_hex,
                signature_hex: &format!("0x{}", hex::encode(signature)),
            },
        )
        .expect("outbe-cli returned signed offerTribute transaction hash");
    world.state.tribute_tx_hash = Some(tx_hash);
}

#[then("the offer is rejected and tribute supply stays zero")]
fn offer_rejected_supply_zero(world: &mut World) {
    let tx_hash = world
        .state
        .l2_rejected_offer_tx_hash
        .as_deref()
        .expect("rejected offer tx");
    assert!(
        world.rpc.wait_receipt_status(tx_hash, false, 240),
        "unsigned offer under an enabled zk gate did not revert: {tx_hash}"
    );
    let primary = world.validators.primary_port();
    assert_eq!(
        world.rpc.supply(primary).as_deref(),
        Some("0"),
        "rejected offer changed Tribute total supply"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "generates and verifies a real Barretenberg FullProof"]
    fn generated_zk_offer_fixture_contains_a_valid_full_proof() {
        let fixture = generate_zk_offer_fixture(Address::repeat_byte(0x44), 19_280_501, 20_260_729);
        let proof = hex::decode(
            fixture
                .proof_hex
                .strip_prefix("0x")
                .expect("fixture proof has 0x prefix"),
        )
        .expect("fixture proof is hex");

        assert!(outbe_zkproof::verify_full_proof(&proof).expect("proof verifier succeeds"));
        let public =
            outbe_zkproof::decode_full_proof_public_inputs(&proof).expect("public inputs decode");
        assert_eq!(public.merkle_root, fixture.merkle_root);
    }
}
