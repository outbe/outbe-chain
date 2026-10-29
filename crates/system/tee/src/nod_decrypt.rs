//! Owner-local NOD decryption using the owner's NOD view key.
use crate::errors::LocalCryptoError;
use crate::owner_local_open::{open_owner_amount_blob, OwnerAmountBlob};
use alloy_primitives::U256;
use outbe_primitives::nod_encryption::{EncryptedNodV1, NOD_AMOUNT_NONCE_INFO};

pub fn decrypt_nod_for_owner(
    view_key: &[u8; 32],
    nod: &EncryptedNodV1,
) -> Result<U256, LocalCryptoError> {
    if !nod.has_valid_encoding() {
        return Err(LocalCryptoError::InvalidNodEncoding);
    }
    let opened = open_owner_amount_blob(
        view_key,
        OwnerAmountBlob {
            context_digest: nod.context_digest().as_slice(),
            key_info: &nod.amount_key_info(),
            crypto_slot: nod.crypto_slot().as_slice(),
            nonce_info: NOD_AMOUNT_NONCE_INFO,
            blob: &nod.encrypted_gratis_amount,
            invalid_key_error: LocalCryptoError::InvalidNodAmountKey,
            decryption_error: LocalCryptoError::NodAmountDecryptionFailed,
        },
    )?;
    let amount: &[u8; 32] = opened
        .as_slice()
        .try_into()
        .map_err(LocalCryptoError::invalid_nod_amount_length)?;
    Ok(U256::from_be_bytes(*amount))
}
