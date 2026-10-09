//! Client-local opening of a context-bound Gratis balance with its view key.
use crate::offer_encrypt::hkdf_sha256;
use crate::owner_local_open::{open_local_ciphertext, LocalOpen};
use alloy_primitives::{Address, U256};
use zeroize::Zeroizing;
pub fn decrypt_gratis_balance(
    view_key: &[u8; 32],
    account: Address,
    blob: &[u8],
) -> Result<U256, String> {
    if blob.is_empty() {
        return Ok(U256::ZERO);
    }
    if blob.len() != 92 || &blob[8..12] != b"GRA2" || blob[..8] == [0; 8] {
        return Err("invalid encrypted Gratis balance".into());
    }
    let mut context = account.as_slice().to_vec();
    context.push(0);
    context.extend_from_slice(&blob[..8]);
    context.extend_from_slice(&blob[12..44]);
    let key = Zeroizing::new(hkdf_sha256(
        view_key,
        &context,
        b"outbe/gratis/amount-key/v2",
    )?);
    let opened = open_local_ciphertext(LocalOpen {
        key: &key,
        nonce_context: &context,
        nonce_info: b"outbe/gratis/amount-nonce/v2",
        ciphertext: &blob[44..],
        invalid_key_error: "invalid Gratis view key",
        decryption_error: "Gratis balance decryption failed",
    })?;
    let amount: &[u8; 32] = opened
        .as_slice()
        .try_into()
        .map_err(|_| "invalid Gratis amount length")?;
    Ok(U256::from_be_bytes(*amount))
}
