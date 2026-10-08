//! Client-local opening of a context-bound Gratis balance with its view key.
use crate::offer_encrypt::hkdf_sha256;
use alloy_primitives::{Address, U256};
use ring::aead;
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
    let nonce_bytes = hkdf_sha256(&*key, &context, b"outbe/gratis/amount-nonce/v2")?;
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_bytes[..12]);
    let opening = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &*key)
            .map_err(|_| "invalid Gratis view key")?,
    );
    let mut bytes = Zeroizing::new(blob[44..].to_vec());
    let plaintext = opening
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut bytes,
        )
        .map_err(|_| "Gratis balance decryption failed")?;
    let amount: &[u8; 32] = (&*plaintext)
        .try_into()
        .map_err(|_| "invalid Gratis amount length")?;
    Ok(U256::from_be_bytes(*amount))
}
