//! Tribute encryption under the `TRIBUTE` confidential domain.

use alloy_primitives::{Address, B256};
use outbe_core_common::confidential_tee::{SlotCipherDomain, TeeError, TRIBUTE};
use outbe_core_common::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TributeContextV2, TRIBUTE_AMOUNT_NONCE_INFO,
};
use outbe_tee::resident::{ResidentOfferMaterial, StateKeyHandle};
use zeroize::Zeroizing;

const AMOUNTS: SlotCipherDomain = SlotCipherDomain {
    nonce_info: TRIBUTE_AMOUNT_NONCE_INFO,
};

/// Derive the resident Tribute state key from the DKG group signature.
pub fn derive_tribute_state_key(
    material: &ResidentOfferMaterial,
    chain_id: B256,
    epoch: u64,
) -> Result<StateKeyHandle, TeeError> {
    TRIBUTE.derive_state_key(material, chain_id, epoch)
}

/// Derive the per-owner Tribute view key from `tribute_state_key`.
pub fn derive_view_key(
    state_key: &StateKeyHandle,
    owner: Address,
) -> Result<StateKeyHandle, TeeError> {
    TRIBUTE.account_keys.derive_view_key(state_key, owner)
}

/// Encrypt the validated amounts of one offer.
pub fn encrypt_tribute(
    tribute_state_key: &StateKeyHandle,
    context: TributeContextV2,
    amounts: &TributeAmountsV2,
) -> Result<EncryptedTributeV2, TeeError> {
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

/// Private read of one encrypted Tribute record.
pub fn decrypt_tribute(
    tribute_state_key: &StateKeyHandle,
    tribute: &EncryptedTributeV2,
) -> Result<TributeAmountsV2, TeeError> {
    if !tribute.has_valid_encoding() {
        return Err(TeeError::DecryptFailed(None));
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
        .map_err(|_| TeeError::DecryptFailed(None))?;
    Ok(TributeAmountsV2::from_be_bytes(amounts))
}

fn derive_amount_key(
    tribute_state_key: &StateKeyHandle,
    context: &TributeContextV2,
) -> Result<StateKeyHandle, TeeError> {
    let view_key = derive_view_key(tribute_state_key, context.owner)?;
    let info = context.amount_key_info();
    Ok(view_key.derive_with_salt(context.digest().as_slice(), &info)?)
}
