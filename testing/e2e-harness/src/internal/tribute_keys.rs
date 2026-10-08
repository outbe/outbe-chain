//! Independent creator encryption keys for reproducible E2E fixtures only.

use alloy_primitives::Address;
use outbe_compressed_entities::{
    decode_stored_tribute_v1, decode_stored_tribute_v2, TributeBodyV1,
};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

pub(crate) fn secret(creator: Address) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"outbe-e2e-creator-encryption-key-v2");
    hash.update(creator);
    hash.finalize().into()
}

pub(crate) fn public_hex(creator: Address) -> String {
    let public = PublicKey::from(&StaticSecret::from(secret(creator)));
    format!("0x{}", hex::encode(public.as_bytes()))
}

/// Authenticate the canonical body before using this local numerical view.
/// This helper never calls a node or enclave to obtain decrypted amounts.
pub(crate) fn calculation_view(
    stored: &[u8],
    network_public: &[u8; 32],
) -> Result<TributeBodyV1, String> {
    let encrypted = match decode_stored_tribute_v2(stored) {
        Ok(body) => body,
        Err(_) => return decode_stored_tribute_v1(stored).map_err(|error| error.to_string()),
    };
    let context = &encrypted.context;
    let amounts = outbe_tee::tribute_decrypt::decrypt_tribute_for_creator(
        &secret(context.owner),
        network_public,
        &encrypted,
    )?;
    Ok(TributeBodyV1 {
        tribute_id: context.tribute_id,
        owner: context.owner,
        worldwide_day: context.worldwide_day,
        issuance_amount_minor: amounts.issuance_amount_minor,
        issuance_currency: context.issuance_currency,
        nominal_amount_minor: amounts.nominal_amount_minor,
        reference_currency: context.reference_currency,
        tribute_price_minor: context.tribute_price_minor,
        exclude_from_intex_issuance: context.exclude_from_intex_issuance,
    })
}
