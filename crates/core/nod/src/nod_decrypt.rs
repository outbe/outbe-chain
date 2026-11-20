//! Owner-local NOD decryption.
//!
//! The openers live in `outbe_core_common::confidential_tee`. This module is the
//! NOD crate's public name for them.

pub use outbe_core_common::confidential_tee::{
    decrypt_nod_for_owner, decrypt_nod_quote_for_owner, LocalCryptoError as NodDecryptError,
};

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, U256};
    use outbe_core_common::nod_encryption::{NodQuoteContextV1, NOD_QUOTE_NONCE_INFO};
    use outbe_tee::tee_decrypt::counter_slot_nonce_context;
    use outbe_tee::tee_encrypt::hkdf_sha256;
    use ring::aead;

    use super::decrypt_nod_quote_for_owner;
    use crate::nod_decrypt::NodDecryptError;

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
        assert!(matches!(
            wrong_key_err,
            NodDecryptError::NodAmountDecryptionFailed(_)
        ));

        let mut tampered_snapshot = context.clone();
        tampered_snapshot.snapshot_id = U256::from(6u64);
        let tampered_snapshot_err =
            decrypt_nod_quote_for_owner(&owner_view_key, &tampered_snapshot, &encrypted)
                .unwrap_err();
        assert!(matches!(
            tampered_snapshot_err,
            NodDecryptError::NodAmountDecryptionFailed(_)
        ));
    }
}
