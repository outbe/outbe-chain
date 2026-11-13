//! Owner-local Tribute decryption using the owner's Tribute view key.

use outbe_core_common::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TRIBUTE_AMOUNT_NONCE_INFO,
};
use outbe_tee::{
    tee_decrypt::{tee_decrypt_counter_slot, CounterSlotContext},
    TeeCryptoError,
};

use super::errors::LocalCryptoError;

const fn map_tribute_error(error: TeeCryptoError) -> LocalCryptoError {
    match error {
        TeeCryptoError::InvalidEnvelope => LocalCryptoError::InvalidTributeEncoding,
        TeeCryptoError::HkdfExpand => LocalCryptoError::HkdfExpand,
        TeeCryptoError::HkdfFill => LocalCryptoError::HkdfFill,
        TeeCryptoError::AeadKey => LocalCryptoError::InvalidTributeAmountKey,
        TeeCryptoError::InvalidPlaintextLength { .. } => {
            LocalCryptoError::InvalidTributeAmountLength
        }
        TeeCryptoError::DecryptionFailed
        | TeeCryptoError::RngFailure
        | TeeCryptoError::AeadSeal => LocalCryptoError::TributeAmountDecryptionFailed,
    }
}

/// Open a public Tribute record with the owner's `Ledger::Tribute` `view_key`.
pub fn decrypt_tribute_for_owner(
    view_key: &[u8; 32],
    tribute: &EncryptedTributeV2,
) -> Result<TributeAmountsV2, LocalCryptoError> {
    if !tribute.has_valid_encoding() {
        return Err(LocalCryptoError::InvalidTributeEncoding);
    }
    let context = &tribute.context;
    let digest = context.digest();
    let info = context.amount_key_info();
    let slot = context.crypto_slot();
    let opened = tee_decrypt_counter_slot(
        view_key,
        CounterSlotContext {
            context_digest: digest.as_slice(),
            key_info: &info,
            crypto_slot: slot.as_slice(),
            nonce_info: TRIBUTE_AMOUNT_NONCE_INFO,
        },
        &tribute.encrypted_amounts,
    )
    .map_err(map_tribute_error)?;
    let amounts = opened.try_into_array::<64>().map_err(map_tribute_error)?;
    Ok(TributeAmountsV2::from_be_bytes(&amounts))
}
