//! Owner-local opening of the padded Fidelity cohort record with a view key.

use alloy_primitives::Address;
use outbe_tee::{
    tee_decrypt::{tee_decrypt_v2_envelope, V2EnvelopeDomain},
    TeeCryptoError,
};

use super::errors::LocalCryptoError;

const FIDELITY_V2_DOMAIN: V2EnvelopeDomain<'static> = V2EnvelopeDomain {
    scope: &[],
    marker: b"FID2",
    key_info: b"outbe/fidelity/cohort-key/v2",
    nonce_info: b"outbe/fidelity/cohort-nonce/v2",
};

const fn map_fidelity_error(error: TeeCryptoError) -> LocalCryptoError {
    match error {
        TeeCryptoError::InvalidEnvelope => LocalCryptoError::InvalidFidelityCohortEnvelope,
        TeeCryptoError::HkdfExpand => LocalCryptoError::HkdfExpand,
        TeeCryptoError::HkdfFill => LocalCryptoError::HkdfFill,
        TeeCryptoError::AeadKey => LocalCryptoError::InvalidFidelityViewKey,
        TeeCryptoError::DecryptionFailed
        | TeeCryptoError::RngFailure
        | TeeCryptoError::AeadSeal
        | TeeCryptoError::InvalidPlaintextLength { .. } => {
            LocalCryptoError::FidelityCohortDecryptionFailed
        }
    }
}

/// Open a `FID2` Fidelity cohort record with the account's Fidelity view key.
pub fn decrypt_fidelity_cohorts(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    let opened = tee_decrypt_v2_envelope(view_key, account, FIDELITY_V2_DOMAIN, blob)
        .map_err(map_fidelity_error)?;
    Ok(opened.as_slice().to_vec())
}
