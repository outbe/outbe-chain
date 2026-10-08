//! Owner-local opening of the padded Fidelity cohort record with a view key.
use crate::offer_encrypt::hkdf_sha256;
use alloy_primitives::Address;
use ring::aead;
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
    let nonce_material = Zeroizing::new(hkdf_sha256(
        &*key,
        &context,
        b"outbe/fidelity/cohort-nonce/v2",
    )?);
    let mut nonce = [0; 12];
    nonce.copy_from_slice(&nonce_material[..12]);
    let opening = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &*key)
            .map_err(|_| "invalid Fidelity view key")?,
    );
    let mut bytes = Zeroizing::new(blob[44..].to_vec());
    let plaintext = opening
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut bytes,
        )
        .map_err(|_| "Fidelity cohort decryption failed")?;
    Ok(plaintext.to_vec())
}
