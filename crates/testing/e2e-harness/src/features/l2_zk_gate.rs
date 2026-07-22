//! L2Registry zk signature gate on `offerTribute` (PFS-001-10 / PFS-001-11).
//!
//! The harness plays the L2 network: it generates a BLS MinPk keypair,
//! registers the operator's EOA as the network's L1 address, and drives the
//! `zk_enabled` toggle. With the gate enabled an unsigned offer must revert;
//! a `zkMerkleRoot` signed with the registered key must pass the gate and
//! issue the canonical Tribute through the normal enclave path.

use alloy_primitives::Address;
use bytes::Bytes as CodecBytes;
use commonware_codec::{DecodeExt, Encode};
use commonware_cryptography::{bls12381, Signer as _};
use commonware_math::algebra::Random;
use cucumber::{then, when};

use crate::world::World;

/// Namespace must match `outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE`.
/// Kept as a literal so the harness exercises the external signing contract
/// rather than importing the runtime crate.
const ZK_MERKLE_ROOT_NAMESPACE: &[u8] = b"_OUTBE_L2_ZK_MERKLE_ROOT";

const L2_CHAIN_ID: u64 = 4242;

/// The `zkMerkleRoot` bytes the "L2 network" commits to in these scenarios.
const ZK_MERKLE_ROOT: &[u8] = b"pfs-001-l2-zk-merkle-root";

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

    let private = bls12381::PrivateKey::random(rand_core::OsRng);
    let public = private.public_key().encode().to_vec();
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

#[when("the operator submits an encrypted tribute offer with a valid L2 signature")]
fn offer_with_valid_signature(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    let key = operator_key(world);
    let private_hex = world
        .state
        .l2_bls_private_hex
        .as_deref()
        .expect("registered L2 BLS key");
    let private = <bls12381::PrivateKey as DecodeExt<()>>::decode(CodecBytes::from(
        hex::decode(private_hex).expect("stored key hex"),
    ))
    .expect("decode stored BLS key");
    let signature = private
        .sign(ZK_MERKLE_ROOT_NAMESPACE, ZK_MERKLE_ROOT)
        .encode()
        .to_vec();

    let tx_hash = world
        .rpc
        .tribute_offer_with_zk(
            &key,
            &wwd,
            &format!("0x{}", hex::encode(ZK_MERKLE_ROOT)),
            &format!("0x{}", hex::encode(signature)),
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
