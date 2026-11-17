//! Helpers for tests in other crates.

use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::time::WorldwideDay;

use crate::schema::TributeFactoryContract;

/// Registers a deterministic fixture L2 network and submits a signed `offer_tribute`
/// targeting `worldwide_day` for cross-crate lifecycle tests.
pub fn submit_signed_test_offer_for_day(
    storage: StorageHandle<'_>,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    use alloy_primitives::{Address, Bytes, U256};
    use outbe_l2registry::L2RegistryContract;
    use outbe_zk::demo_tribute::COMBINED_LEN as DEMO_TRIBUTE_COMBINED_LEN;

    const L2_CHAIN_ID: u64 = 0xdead;
    const PUB_HEX: &str = concat!(
        "000000000000000000000000000000001829f9fd76d079565e7d8a6616bf247a87eceb4531477f0be2e0668479902f6ea5507432d979bcb0f431b1099108693d",
        "0000000000000000000000000000000011697208ae3a8d9be58fb66f912d87f462c5e9054176014a75b834bf42f1f3b2c19df137e4efcfd456d1899b7725bb5f",
        "0000000000000000000000000000000007fa4a9e26691e08d61632d4c7b5bd55fbe7baf448eeb79a70d1b8c0b98c9d81873b6a781402db0863c1c363aff7cbab",
        "0000000000000000000000000000000004facf29f7f5bd15edbe4ca219dc2c0c6d0ad35750842ede67cb83e275ca587ccdc470ae27db4a89f99737cfed8fb9c7"
    );
    const SIG_HEX: &str =
        "a38cbc31b858bc5f6d41adfa7f14be151cff370515dfc788568540e9ee3113a94547b318304270a23d2e4bb9bf247977";

    let caller = Address::repeat_byte(0x77);
    let pub_bytes = hex::decode(PUB_HEX).expect("valid fixture pubkey hex");
    let sig_bytes = hex::decode(SIG_HEX).expect("valid fixture sig hex");
    let root = [0x04u8; 32];

    L2RegistryContract::new(storage.clone())
        .register_network(L2_CHAIN_ID, caller, &pub_bytes)
        .expect("register fixture L2 network");

    let mut proof = Vec::with_capacity(DEMO_TRIBUTE_COMBINED_LEN);
    proof.extend_from_slice(&4u32.to_be_bytes());
    proof.extend_from_slice(&[0x01; 32]);
    proof.extend_from_slice(&[0x02; 32]);
    proof.extend_from_slice(&[0x03; 32]);
    proof.extend_from_slice(&root);
    proof.resize(DEMO_TRIBUTE_COMBINED_LEN, 0);

    let input = crate::runtime::OfferTributeInput {
        caller,
        cipher_text: Bytes::new(),
        nonce: Bytes::new(),
        ephemeral_pubkey: U256::ZERO,
        worldwide_day,
        tribute_currency: 840,
        reference_currency: 840,
        zk_proof: proof.into(),
        l2_chain_id: u32::try_from(L2_CHAIN_ID).expect("u32 chain id"),
        circuit_version: "1.1.0".to_owned(),
        zk_merkle_root: Bytes::copy_from_slice(&root),
        signature: sig_bytes.into(),
    };
    TributeFactoryContract::new(storage)
        .offer_tribute(input)
        .map(|_| ())
}
