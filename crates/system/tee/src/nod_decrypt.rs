//! Owner-local NOD decryption using the owner's X25519 private key.
use crate::owner_local_open::{open_owner_amount_blob, owner_x25519_shared, OwnerAmountBlob};
use alloy_primitives::U256;
use outbe_primitives::nod_encryption::{EncryptedNodV2, NOD_AMOUNT_NONCE_INFO};
pub fn decrypt_nod_for_owner(
    secret: &[u8; 32],
    enclave_public: &[u8; 32],
    nod: &EncryptedNodV2,
) -> Result<U256, String> {
    if !nod.has_valid_encoding() {
        return Err("invalid encrypted NOD encoding".into());
    }
    let (public, shared) = owner_x25519_shared(secret, enclave_public)?;
    let opened = open_owner_amount_blob(
        &shared,
        OwnerAmountBlob {
            context_digest: nod.context_digest().as_slice(),
            key_info: &nod.amount_key_info(enclave_public, &public),
            crypto_slot: nod.crypto_slot().as_slice(),
            nonce_info: NOD_AMOUNT_NONCE_INFO,
            blob: &nod.encrypted_gratis_amount,
            invalid_key_error: "invalid NOD amount key",
            decryption_error: "NOD amount decryption failed",
        },
    )?;
    let amount: &[u8; 32] = opened
        .as_slice()
        .try_into()
        .map_err(|_| "invalid NOD amount length")?;
    Ok(U256::from_be_bytes(*amount))
}
