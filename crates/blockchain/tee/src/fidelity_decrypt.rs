//! Owner-local opening of the padded Fidelity cohort record with a view key.
use crate::errors::LocalCryptoError;
use crate::owner_local_open::{open_v2_envelope_blob, FIDELITY_V2_SPEC};
use alloy_primitives::Address;

pub fn decrypt_fidelity_cohorts(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    Ok(
        open_v2_envelope_blob(view_key, account, &[], blob, FIDELITY_V2_SPEC)?
            .as_slice()
            .to_vec(),
    )
}
