//! Owner-local opening of a padded Fidelity cohort record.

use alloy_primitives::Address;
use outbe_core_common::confidential_tee::LocalCryptoError;

/// Open a `FID2` cohort record with the account view key.
///
/// An empty blob opens to an empty interior. The cipher lives in
/// `outbe_core_common`, so host decrypt and the enclave envelope cannot drift.
pub fn decrypt_fidelity_cohorts(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<Vec<u8>, LocalCryptoError> {
    outbe_core_common::confidential_tee::decrypt_fidelity_cohorts(view_key, account, blob)
}
