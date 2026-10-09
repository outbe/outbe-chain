//! Owner-local opening of the padded Fidelity cohort record with a view key.
use crate::offer_encrypt::hkdf_sha256;
use crate::owner_local_open::{open_local_ciphertext, LocalOpen};
use alloy_primitives::Address;
use zeroize::Zeroizing;

pub fn decrypt_fidelity_cohorts(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<Vec<u8>, String> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    if blob.len() < 60 || &blob[8..12] != b"FID2" || blob[..8] == [0; 8] {
        return Err("invalid encrypted Fidelity cohort envelope".into());
    }
    let mut context = account.as_slice().to_vec();
    context.extend_from_slice(&blob[..8]);
    context.extend_from_slice(&blob[12..44]);
    let key = Zeroizing::new(hkdf_sha256(
        view_key,
        &context,
        b"outbe/fidelity/cohort-key/v2",
    )?);
    let opened = open_local_ciphertext(LocalOpen {
        key: &key,
        nonce_context: &context,
        nonce_info: b"outbe/fidelity/cohort-nonce/v2",
        ciphertext: &blob[44..],
        invalid_key_error: "invalid Fidelity view key",
        decryption_error: "Fidelity cohort decryption failed",
    })?;
    Ok(opened.as_slice().to_vec())
}
