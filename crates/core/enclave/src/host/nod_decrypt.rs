//! Owner-local NOD decryption using the owner's NOD view key.

use alloy_primitives::U256;
use outbe_core_common::nod_encryption::{
    EncryptedNodV1, NodQuoteContextV1, NOD_AMOUNT_NONCE_INFO, NOD_BLOB_LEN, NOD_QUOTE_NONCE_INFO,
};
use outbe_tee::{
    tee_decrypt::{tee_decrypt_counter_slot, CounterSlotContext},
    TeeCryptoError,
};

use super::errors::LocalCryptoError;

const fn map_nod_error(error: TeeCryptoError) -> LocalCryptoError {
    match error {
        TeeCryptoError::InvalidEnvelope => LocalCryptoError::InvalidNodEncoding,
        TeeCryptoError::HkdfExpand => LocalCryptoError::HkdfExpand,
        TeeCryptoError::HkdfFill => LocalCryptoError::HkdfFill,
        TeeCryptoError::AeadKey => LocalCryptoError::InvalidNodAmountKey,
        TeeCryptoError::InvalidPlaintextLength { .. } => LocalCryptoError::InvalidNodAmountLength,
        TeeCryptoError::DecryptionFailed
        | TeeCryptoError::RngFailure
        | TeeCryptoError::AeadSeal => LocalCryptoError::NodAmountDecryptionFailed,
    }
}

/// Open an [`EncryptedNodV1`] amount with the owner's `Ledger::Nod` view key.
pub fn decrypt_nod_for_owner(
    view_key: &[u8; 32],
    nod: &EncryptedNodV1,
) -> Result<U256, LocalCryptoError> {
    if !nod.has_valid_encoding() {
        return Err(LocalCryptoError::InvalidNodEncoding);
    }
    let context_digest = nod.context_digest();
    let key_info = nod.amount_key_info();
    let crypto_slot = nod.crypto_slot();
    let opened = tee_decrypt_counter_slot(
        view_key,
        CounterSlotContext {
            context_digest: context_digest.as_slice(),
            key_info: &key_info,
            crypto_slot: crypto_slot.as_slice(),
            nonce_info: NOD_AMOUNT_NONCE_INFO,
        },
        &nod.encrypted_gratis_amount,
    )
    .map_err(map_nod_error)?;
    let amount = opened.try_into_array::<32>().map_err(map_nod_error)?;
    Ok(U256::from_be_bytes(amount))
}

/// Alias preserved for callers matching NOD decryption errors.
pub type NodDecryptError = LocalCryptoError;

/// Open an encrypted NOD settlement quote payment with the owner's `Ledger::Nod` view key.
pub fn decrypt_nod_quote_for_owner(
    view_key: &[u8; 32],
    context: &NodQuoteContextV1,
    encrypted_payment: &[u8],
) -> Result<U256, NodDecryptError> {
    if encrypted_payment.len() != NOD_BLOB_LEN || encrypted_payment[..8] != 1u64.to_be_bytes() {
        return Err(LocalCryptoError::InvalidNodEncoding);
    }
    let context_digest = context.context_digest();
    let key_info = context.quote_key_info();
    let crypto_slot = context.crypto_slot();
    let opened = tee_decrypt_counter_slot(
        view_key,
        CounterSlotContext {
            context_digest: context_digest.as_slice(),
            key_info: &key_info,
            crypto_slot: crypto_slot.as_slice(),
            nonce_info: NOD_QUOTE_NONCE_INFO,
        },
        encrypted_payment,
    )
    .map_err(map_nod_error)?;
    let amount = opened.try_into_array::<32>().map_err(map_nod_error)?;
    Ok(U256::from_be_bytes(amount))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use outbe_tee::tee_decrypt::counter_slot_nonce_context;
    use ring::aead;

    use super::*;
    use outbe_tee::tee_encrypt::hkdf_sha256;

    fn seal_test_quote(
        view_key: &[u8; 32],
        context: &NodQuoteContextV1,
        payment_minor: U256,
    ) -> Vec<u8> {
        let key = hkdf_sha256(
            context.context_digest().as_slice(),
            view_key,
            &context.quote_key_info(),
        )
        .unwrap();
        let nonce_context = counter_slot_nonce_context(context.crypto_slot().as_slice(), 0, 1);
        let nonce_material = hkdf_sha256(&key, &nonce_context, NOD_QUOTE_NONCE_INFO).unwrap();
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&nonce_material[..12]);
        let sealing =
            aead::LessSafeKey::new(aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &key).unwrap());
        let mut ciphertext = payment_minor.to_be_bytes::<32>().to_vec();
        sealing
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::empty(),
                &mut ciphertext,
            )
            .unwrap();
        let mut blob = 1u64.to_be_bytes().to_vec();
        blob.extend_from_slice(&ciphertext);
        blob
    }

    #[test]
    fn owner_decrypts_quote_and_rejects_wrong_key_or_modified_snapshot_id() {
        let owner_view_key = [0x42u8; 32];
        let stranger_view_key = [0x99u8; 32];
        let context = NodQuoteContextV1 {
            chain_id: 1,
            nod_id: U256::from(77u64),
            owner: Address::repeat_byte(0x11),
            asset: Address::repeat_byte(0x22),
            settlement_currency: 978,
            snapshot_id: U256::from(5u64),
        };
        let expected_payment = U256::from(123_456_789u64);
        let encrypted = seal_test_quote(&owner_view_key, &context, expected_payment);

        let opened = decrypt_nod_quote_for_owner(&owner_view_key, &context, &encrypted).unwrap();
        assert_eq!(opened, expected_payment);

        let wrong_key_err =
            decrypt_nod_quote_for_owner(&stranger_view_key, &context, &encrypted).unwrap_err();
        assert_eq!(wrong_key_err, LocalCryptoError::NodAmountDecryptionFailed);

        let mut tampered_snapshot = context.clone();
        tampered_snapshot.snapshot_id = U256::from(6u64);
        let tampered_snapshot_err =
            decrypt_nod_quote_for_owner(&owner_view_key, &tampered_snapshot, &encrypted)
                .unwrap_err();
        assert_eq!(
            tampered_snapshot_err,
            LocalCryptoError::NodAmountDecryptionFailed
        );
    }
}
