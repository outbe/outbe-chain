//! Owner view-key derivation for reproducible E2E fixtures.

use alloy_primitives::Address;
use outbe_primitives::entity_codec::{
    decode_stored_tribute_v1, decode_stored_tribute_v2, TributeBodyV1,
};
use outbe_tee::protocol::Ledger;

use crate::internal::eth;
use crate::world::World;

pub(crate) const CREATOR_KEY: &str =
    "0x7979797979797979797979797979797979797979797979797979797979797979";

const NOD_HOLDER_SEED: u64 = 0x0e0d_0000;

pub(crate) fn owner_private_key(world: &World, owner: Address) -> String {
    if eth::address_of(CREATOR_KEY) == Some(owner) {
        return CREATOR_KEY.to_owned();
    }
    if eth::address_of(crate::world::forge::DEPLOYER_KEY) == Some(owner) {
        return crate::world::forge::DEPLOYER_KEY.to_owned();
    }
    for index in 0..world.validators.size() {
        if let Ok(key) = world.validators.get(index).evm_key() {
            if eth::address_of(&key) == Some(owner) {
                return key;
            }
        }
    }
    for index in 0..16 {
        let key = crate::features::entity_lifecycle::holders::key(NOD_HOLDER_SEED, index);
        if eth::address_of(&key) == Some(owner) {
            return key;
        }
    }
    panic!("no E2E private key registered for owner {owner:#x}");
}

pub(crate) fn tribute_view_key(world: &World, owner: Address) -> [u8; 32] {
    let url = world.rpc.url(world.validators.primary_port());
    let key = owner_private_key(world, owner);
    eth::derive_account_keys(&url, &key, Ledger::Tribute)
        .expect("derive owner Tribute view key")
        .view
}

pub(crate) fn nod_view_key(world: &World, owner: Address) -> [u8; 32] {
    let url = world.rpc.url(world.validators.primary_port());
    let key = owner_private_key(world, owner);
    eth::derive_account_keys(&url, &key, Ledger::Nod)
        .expect("derive owner Nod view key")
        .view
}

/// Authenticate the canonical body before using this local numerical view.
/// Uses the owner's derived `view_key` to open the ciphertext client-side.
pub(crate) fn calculation_view(world: &World, stored: &[u8]) -> Result<TributeBodyV1, String> {
    let encrypted = match decode_stored_tribute_v2(stored) {
        Ok(body) => body,
        Err(_) => return decode_stored_tribute_v1(stored).map_err(|error| error.to_string()),
    };
    let context = &encrypted.context;
    let view_key = tribute_view_key(world, context.owner);
    let amounts = outbe_tee::tribute_decrypt::decrypt_tribute_for_owner(&view_key, &encrypted)
        .map_err(|error| error.to_string())?;
    Ok(TributeBodyV1 {
        tribute_id: context.tribute_id,
        owner: context.owner,
        worldwide_day: context.worldwide_day,
        issuance_amount_minor: amounts.issuance_amount_minor,
        issuance_currency: context.issuance_currency,
        nominal_amount_minor: amounts.nominal_amount_minor,
        reference_currency: context.reference_currency,
        tribute_price_minor: context.tribute_price_minor,
    })
}
