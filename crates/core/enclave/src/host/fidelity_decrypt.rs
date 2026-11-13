//! Owner-local opening of the padded Fidelity cohort record with a view key.

use alloy_primitives::Address;
use outbe_tee::tee_decrypt::{tee_decrypt_v2_envelope, V2EnvelopeInput};

use super::errors::LocalCryptoError;

/// Open a `FID2` Fidelity cohort record with the account's Fidelity view key.
pub fn decrypt_fidelity_cohorts(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    let opened = tee_decrypt_v2_envelope(
        view_key,
        V2EnvelopeInput {
            account,
            scope: &[],
            marker: b"FID2",
            key_info: b"outbe/fidelity/cohort-key/v2",
            nonce_info: b"outbe/fidelity/cohort-nonce/v2",
            blob,
        },
    )
    .map_err(LocalCryptoError::from_fidelity_v2)?;
    Ok(opened.as_slice().to_vec())
}
