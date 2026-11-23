//! Offer decryption, the nominal of an offer and its public ZK hashes.

pub mod compute;
pub mod errors;
pub mod issue;
pub mod payload;
pub mod protocol;
pub mod zk_claim;

use outbe_core_common::confidential_tee::ecdhe_tribute_offer_decrypt;
use outbe_tee::resident::OfferKeyHandle;
use zeroize::Zeroizing;

use errors::{OfferResult, PayloadError, ProcessError};
use payload::TributeInputPayload;
use protocol::EncryptedTributeOffer;

/// The resident offer key and the HKDF salt an offer is sealed with.
pub struct OfferKeyMaterial<'a> {
    pub offer_private_key: &'a OfferKeyHandle,
    pub salt: &'a [u8; 32],
}

/// Decrypt and validate the confidential payload of `offer`.
pub fn open_offer(
    key: &OfferKeyMaterial<'_>,
    offer: &EncryptedTributeOffer,
) -> OfferResult<TributeInputPayload> {
    let plaintext = Zeroizing::new(
        ecdhe_tribute_offer_decrypt(
            key.offer_private_key,
            key.salt,
            &offer.ephemeral_pubkey.to_be_bytes::<32>(),
            &offer.nonce,
            &offer.cipher_text,
        )
        .map_err(ProcessError::DecryptionFailed)?,
    );
    let payload: TributeInputPayload =
        serde_json::from_slice(&plaintext).map_err(PayloadError::InvalidEncryptedTributePayload)?;
    payload::validate(&payload)?;
    Ok(payload)
}
