//! L2Registry zk gate on `offerTribute` (PFS-001-10 / PFS-001-11).
//!
//! The harness plays the L2 network: it registers the operator's EOA through
//! validator governance under a deterministic fixture key, and every offer must
//! carry a real FullProof under the circuit version enabled for the registered
//! L2 chain whose root is signed with exactly that registered key.

use alloy_primitives::{Address, B256};
use cucumber::{then, when};

use crate::internal::l2_fixture::{self, TributeOfferStatement, TributeOfferZk};
use crate::world::rpc::TributeZkOffer;
use crate::world::World;

/// The existing canonical test-chain binding used by this basic scenario.
const L2_CHAIN_ID: u64 = 0xdead;

/// The same chain id as the `uint32` circuit selector argument of `offerTribute`.
const L2_CHAIN_ID_SELECTOR: u32 = 0xdead;

fn low_b256(last_byte: u8) -> B256 {
    let mut value = [0u8; 32];
    value[31] = last_byte;
    B256::from(value)
}

/// The governed L2 network this scenario registered, read back from the chain.
fn registered_network(world: &World) -> (Address, Vec<u8>) {
    world
        .rpc
        .l2_network(L2_CHAIN_ID)
        .expect("registered L2 network")
}

/// A real offer proof for one statement of this scenario's L2 network.
fn offer_proof(
    world: &World,
    l1_owner: Address,
    draft_id: B256,
    su_hash: B256,
    worldwide_day: u64,
) -> TributeOfferZk {
    l2_fixture::prove_tribute_offer(TributeOfferStatement {
        host_chain_id: world
            .rpc
            .chain_id(world.validators.primary_port())
            .expect("chain id the offer executes on"),
        caller: l1_owner,
        l2_chain_id: L2_CHAIN_ID,
        worldwide_day,
        tribute_currency: 840,
        amount_base: "100",
        amount_micro: "0",
        draft_id,
        su_hash,
    })
}

/// Mix one statement's public inputs with another statement's proof words, so
/// the result is well formed but cryptographically invalid for its statement.
fn proof_from_other_statement(original: &TributeOfferZk, donor: &TributeOfferZk) -> Vec<u8> {
    use outbe_zk_backend::barretenberg::verify_circuit;
    use outbe_zk_canonical::full_proof::{decode_public_inputs, PUBLIC_INPUT_COUNT};
    use outbe_zk_canonical::noir::full_proof::FullProof;

    let original = original.proof.clone();
    let donor = donor.proof.clone();
    std::thread::spawn(move || {
        let public = decode_public_inputs(&original).expect("original public inputs");
        let other = decode_public_inputs(&donor).expect("donor public inputs");
        assert_ne!(
            public, other,
            "proof donor must represent a different statement"
        );
        assert!(verify_circuit::<FullProof>(&original).expect("original proof verification"));
        assert!(verify_circuit::<FullProof>(&donor).expect("donor proof verification"));
        let prefix = 4 + PUBLIC_INPUT_COUNT * 32;
        let mut tampered = original;
        tampered[prefix..].copy_from_slice(&donor[prefix..]);
        assert_ne!(tampered, donor, "mixed proof must differ from its donor");
        assert_eq!(
            decode_public_inputs(&tampered).expect("tampered public inputs"),
            public
        );
        assert!(
            !verify_circuit::<FullProof>(&tampered).expect("well-formed mixed proof verification"),
            "mixed proof must fail cryptographic verification"
        );
        tampered
    })
    .join()
    .expect("proof control verifier thread")
}

fn operator_key(world: &World) -> String {
    world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key")
}

#[when("an L2 network is registered for the operator")]
fn register_l2_network(world: &mut World) {
    let key = operator_key(world);
    let l1_address = super::l2_registration::operator_address(world, &key);
    let public = l2_fixture::root_signing_public_key(L2_CHAIN_ID);

    super::l2_registration::ensure_tribute_offer_operator(world, &key);
    assert_eq!(registered_network(world), (l1_address, public));
}

#[then("the governed L2 network is registered")]
fn governed_l2_is_registered(world: &mut World) {
    let (owner, public_key) = registered_network(world);
    assert_eq!(
        owner,
        super::l2_registration::operator_address(world, &operator_key(world))
    );
    assert_eq!(
        public_key,
        l2_fixture::root_signing_public_key(L2_CHAIN_ID),
        "the registered key must be the fixture key that signs offer roots"
    );
}

#[when("the operator submits an encrypted tribute offer without an L2 signature")]
fn offer_without_signature(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    super::tribute_projection::wait_for_offering(world, &wwd);
    let key = operator_key(world);
    let tx_hash = world
        .rpc
        .tribute_offer_with_zk(
            &key,
            &wwd,
            TributeZkOffer {
                tribute_draft_id_hex: &format!("{:#x}", low_b256(0x11)),
                su_hash_hex: &format!("{:#x}", low_b256(0x22)),
                merkle_root_hex: &format!("{:#x}", low_b256(0x33)),
                proof_hex: "0x",
                l2_chain_id: L2_CHAIN_ID_SELECTOR,
                circuit_version: l2_fixture::FIXTURE_CIRCUIT_VERSION,
                signature_hex: "0x",
            },
        )
        .expect("outbe-cli returned offerTribute transaction hash");
    world.state.l2_rejected_offer_tx_hash = Some(tx_hash);
}

/// Submit one offer carrying a real proof for the operator's own draft, signed
/// by the registered network key. Expected to be admitted.
fn submit_proven_offer(world: &mut World, tag: &str) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    let worldwide_day = wwd.parse::<u64>().expect("worldwide-day number");
    let key = operator_key(world);
    let l1_owner = super::l2_registration::operator_address(world, &key);
    let (draft_id, su_hash) = l2_fixture::offer_identifiers(tag, l1_owner, worldwide_day as u32);
    let zk = offer_proof(world, l1_owner, draft_id, su_hash, worldwide_day);

    super::tribute_projection::wait_for_offering(world, &wwd);
    let tx_hash = world
        .rpc
        .tribute_offer_with_zk(
            &key,
            &wwd,
            TributeZkOffer {
                tribute_draft_id_hex: &zk.tribute_draft_id_hex,
                su_hash_hex: &zk.su_hash_hex,
                merkle_root_hex: &zk.merkle_root_hex(),
                proof_hex: &zk.proof_hex(),
                l2_chain_id: zk.l2_chain_id,
                circuit_version: zk.circuit_version,
                signature_hex: &zk.signature_hex(),
            },
        )
        .expect("outbe-cli returned the proven offerTribute transaction hash");
    world.state.tribute_tx_hash = Some(tx_hash);
}

#[when("the operator submits a valid FullProof offer for one encrypted tribute")]
fn offer_with_valid_full_proof(world: &mut World) {
    submit_proven_offer(world, "zk-gate-valid");
}

#[when("the operator proves a signed tampered proof is rejected then submits the valid FullProof")]
fn offer_with_valid_zk_proof(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    let worldwide_day = wwd.parse::<u64>().expect("worldwide-day number");
    let key = operator_key(world);
    let l1_owner = super::l2_registration::operator_address(world, &key);
    let (draft_id, su_hash) =
        l2_fixture::offer_identifiers("zk-gate-tampered", l1_owner, worldwide_day as u32);
    let fixture = offer_proof(world, l1_owner, draft_id, su_hash, worldwide_day);
    let (donor_draft_id, donor_su_hash) =
        l2_fixture::offer_identifiers("zk-gate-donor", l1_owner, worldwide_day as u32);
    let donor = offer_proof(
        world,
        l1_owner,
        donor_draft_id,
        donor_su_hash,
        worldwide_day,
    );
    let tampered = proof_from_other_statement(&fixture, &donor);

    let (registered_owner, public_key) = registered_network(world);
    assert_eq!(registered_owner, l1_owner);
    assert!(
        l2_fixture::verify_merkle_root(&public_key, &fixture.merkle_root, &fixture.signature),
        "positive control: the fixture signature must verify against the registered key"
    );
    assert_eq!(
        world
            .rpc
            .l2_chain_by_l1_address(l1_owner)
            .expect("read the operator's L2Registry mapping"),
        L2_CHAIN_ID
    );

    super::tribute_projection::wait_for_offering(world, &wwd);
    let rejected = world
        .rpc
        .tribute_offer_with_zk(
            &key,
            &wwd,
            TributeZkOffer {
                tribute_draft_id_hex: &fixture.tribute_draft_id_hex,
                su_hash_hex: &fixture.su_hash_hex,
                merkle_root_hex: &fixture.merkle_root_hex(),
                proof_hex: &format!("0x{}", hex::encode(&tampered)),
                l2_chain_id: fixture.l2_chain_id,
                circuit_version: fixture.circuit_version,
                signature_hex: &fixture.signature_hex(),
            },
        )
        .expect("submit well-formed tampered proof with a valid signature");
    super::tribute_negatives::assert_rejection(
        world,
        &rejected,
        &key,
        super::tribute_negatives::Rejection::InvalidProof,
    );
    super::tribute_negatives::assert_supply(world, 0);
    super::tribute_projection::wait_for_offering(world, &wwd);

    submit_proven_offer(world, "zk-gate-tampered-valid");
}

#[then("the offer is rejected and tribute supply stays zero")]
fn offer_rejected_desis_limit_minor_zero(world: &mut World) {
    let tx_hash = world
        .state
        .l2_rejected_offer_tx_hash
        .as_deref()
        .expect("rejected offer tx");
    super::tribute_negatives::assert_rejection(
        world,
        tx_hash,
        &operator_key(world),
        super::tribute_negatives::Rejection::MissingSignature,
    );
    super::tribute_negatives::assert_supply(world, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_l2registry::api as l2_api;

    #[test]
    fn scenario_uses_the_preexisting_circuit_binding() {
        assert_eq!(L2_CHAIN_ID_SELECTOR, L2_CHAIN_ID as u32);
        let bound = outbe_zk_canonical::l2_circuits(L2_CHAIN_ID);
        assert!(
            bound
                .iter()
                .any(|entry| entry.version == l2_fixture::FIXTURE_CIRCUIT_VERSION),
            "the gate scenario's chain must be bound to the fixture circuit version"
        );
        assert_eq!(
            l2_api::ZK_MERKLE_ROOT_NAMESPACE,
            l2_fixture::ZK_MERKLE_ROOT_NAMESPACE
        );
    }
}
