//! Typed canary decrypt of one Tribute offer over a caller-supplied context.

use alloy_primitives::B256;
use outbe_core_common::confidential_tee::{
    attestation, ecdhe_tribute_offer_decrypt, verify_response_attestation, OFFER_HKDF_SALT,
};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::TransportError;

use super::super::protocol::{inputs_canonical_hash, EncryptedTributeOffer};
use super::call::{self, auth_spec};

/// Decrypt one canary offer with `context`. Does not read the process-global runtime.
pub fn open_canary_tribute_offer(
    context: &InProcessTeeContext,
    offer: &EncryptedTributeOffer,
) -> Result<Vec<u8>, TransportError> {
    let expected = inputs_canonical_hash(std::slice::from_ref(offer));
    execute(context, offer, expected)
}

/// Session wrapper. The inputs hash is fixed before the call.
pub fn decrypt_canary_tribute_offer(
    offer: EncryptedTributeOffer,
) -> Result<Vec<u8>, TransportError> {
    let expected = inputs_canonical_hash(std::slice::from_ref(&offer));
    call::session("tribute_canary_offer", move |context| {
        execute(context, &offer, expected)
    })
}

fn execute(
    context: &InProcessTeeContext,
    offer: &EncryptedTributeOffer,
    expected: B256,
) -> Result<Vec<u8>, TransportError> {
    let plaintext = call::with_offer(context, |handle| {
        ecdhe_tribute_offer_decrypt(
            handle,
            &OFFER_HKDF_SALT,
            &offer.ephemeral_pubkey.to_be_bytes::<32>(),
            &offer.nonce,
            &offer.cipher_text,
        )
        .map_err(call::reject)
    })?;
    let preimage = attestation(b"outbe/tribute/canary-offer/v1", expected, &plaintext)
        .map_err(call::reject)?;
    let tag = call::sign(context, &preimage);
    verify_response_attestation(
        &auth_spec(&context.attestation_public(), expected, expected, &tag),
        &preimage,
    )?;
    Ok(plaintext)
}
