//! Enclave-resident Tribute encryption under the `TRIBUTE` confidential domain.

use alloy_primitives::{Address, B256};
use outbe_primitives::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TributeContextV2, TRIBUTE_AMOUNT_NONCE_INFO,
};
use zeroize::Zeroizing;

use crate::{
    confidential::{SlotCipherDomain, TRIBUTE},
    crypto::hkdf_sha256,
    errors::{Result, TeeError},
};

const AMOUNTS: SlotCipherDomain = SlotCipherDomain {
    nonce_info: TRIBUTE_AMOUNT_NONCE_INFO,
};

/// Derive the resident Tribute state key from the DKG group signature.
pub fn derive_tribute_state_key(
    group_sig: &[u8],
    chain_id: B256,
    epoch: u64,
) -> Result<Zeroizing<[u8; 32]>> {
    TRIBUTE.derive_state_key(group_sig, chain_id, epoch)
}

/// Derive the per-owner Tribute view key from `tribute_state_key`.
pub fn derive_view_key(state_key: &[u8; 32], owner: Address) -> Result<Zeroizing<[u8; 32]>> {
    TRIBUTE.account_keys.derive_view_key(state_key, owner)
}

/// Encrypt the validated, deterministically calculated amounts of one offer.
/// The caller must bind context.offer_input_hash to that exact encrypted offer.
pub fn encrypt_tribute(
    tribute_state_key: &[u8; 32],
    context: TributeContextV2,
    amounts: &TributeAmountsV2,
) -> Result<EncryptedTributeV2> {
    let slot = context.crypto_slot();
    let amount_key = derive_amount_key(tribute_state_key, &context)?;
    let plaintext = Zeroizing::new(amounts.to_be_bytes());
    let encrypted_amounts = AMOUNTS
        .slot(&amount_key, slot, 0)
        .write_blob(0, plaintext.as_ref())?;
    Ok(EncryptedTributeV2 {
        context,
        encrypted_amounts,
    })
}

/// Private read bridge. The view key and the amount key are not retained.
pub fn decrypt_tribute(
    tribute_state_key: &[u8; 32],
    tribute: &EncryptedTributeV2,
) -> Result<TributeAmountsV2> {
    if !tribute.has_valid_encoding() {
        return Err(TeeError::DecryptFailed);
    }
    let slot = tribute.context.crypto_slot();
    let key = derive_amount_key(tribute_state_key, &tribute.context)?;
    let (_, plaintext) = AMOUNTS
        .slot(&key, slot, 0)
        .read_blob(&tribute.encrypted_amounts)?;
    let plaintext = Zeroizing::new(plaintext);
    let amounts = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed)?;
    Ok(TributeAmountsV2::from_be_bytes(amounts))
}

fn derive_amount_key(
    tribute_state_key: &[u8; 32],
    context: &TributeContextV2,
) -> Result<Zeroizing<[u8; 32]>> {
    let view_key = derive_view_key(tribute_state_key, context.owner)?;
    let info = context.amount_key_info();
    let key = hkdf_sha256(context.digest().as_slice(), &*view_key, &info)?;
    Ok(Zeroizing::new(key))
}
