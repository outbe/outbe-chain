//! Owner-local Tribute decryption using the owner's Tribute view key.

use outbe_primitives::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TRIBUTE_AMOUNT_NONCE_INFO,
};

use crate::{
    errors::LocalCryptoError,
    owner_local_open::{open_owner_amount_blob, OwnerAmountBlob},
};

/// Open a public Tribute record with the owner's `Ledger::Tribute` `view_key`.
pub fn decrypt_tribute_for_owner(
    view_key: &[u8; 32],
    tribute: &EncryptedTributeV2,
) -> Result<TributeAmountsV2, LocalCryptoError> {
    if !tribute.has_valid_encoding() {
        return Err(LocalCryptoError::InvalidTributeEncoding);
    }
    let context = &tribute.context;
    let info = context.amount_key_info();
    let opened = open_owner_amount_blob(
        view_key,
        OwnerAmountBlob {
            context_digest: context.digest().as_slice(),
            key_info: &info,
            crypto_slot: context.crypto_slot().as_slice(),
            nonce_info: TRIBUTE_AMOUNT_NONCE_INFO,
            blob: &tribute.encrypted_amounts,
            invalid_key_error: LocalCryptoError::InvalidTributeAmountKey,
            decryption_error: LocalCryptoError::TributeAmountDecryptionFailed,
        },
    )?;
    let amounts = opened
        .as_slice()
        .try_into()
        .map_err(LocalCryptoError::invalid_tribute_amount_length)?;
    Ok(TributeAmountsV2::from_be_bytes(amounts))
}
