//! Creator-local decryption. No enclave secret is used or exported here.

use outbe_primitives::tribute_encryption::{
    EncryptedTributeV2, TributeAmountsV2, TRIBUTE_AMOUNT_NONCE_INFO,
};

use crate::{
    errors::LocalCryptoError,
    owner_local_open::{open_owner_amount_blob, owner_x25519_shared, OwnerAmountBlob},
};

/// Open a public Tribute record with the creator's own X25519 private key.
pub fn decrypt_tribute_for_creator(
    creator_secret: &[u8; 32],
    enclave_public: &[u8; 32],
    tribute: &EncryptedTributeV2,
) -> Result<TributeAmountsV2, LocalCryptoError> {
    if !tribute.has_valid_encoding() {
        return Err(LocalCryptoError::InvalidTributeEncoding);
    }
    let (creator_public, shared) = owner_x25519_shared(creator_secret, enclave_public)?;
    let context = &tribute.context;
    let info = context.amount_key_info(
        enclave_public,
        &creator_public,
        &tribute.encrypted_creator_public_key,
    );
    let opened = open_owner_amount_blob(
        &shared,
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
