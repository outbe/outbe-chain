//! Fresh-genesis cohort envelopes with independent keys for divergent transitions.
use crate::{
    crypto::{chacha20poly1305_decrypt, chacha20poly1305_encrypt, hkdf_sha256},
    errors::{Result, TeeError},
};
use alloy_primitives::Address;
use ring::hmac;
use zeroize::Zeroizing;

const MARKER: &[u8; 4] = b"FID2";
const HEADER_LEN: usize = 8 + 4 + 32;
const TAG_LEN: usize = 16;
pub struct CohortTransition<'a> {
    pub account: Address,
    pub previous_version: u64,
    pub previous_blob: &'a [u8],
    pub padded_state: &'a [u8],
}

pub fn read_blob(view_key: &[u8; 32], account: Address, blob: &[u8]) -> Result<(u64, Vec<u8>)> {
    if blob.is_empty() {
        return Ok((0, Vec::new()));
    }
    if blob.len() < HEADER_LEN + TAG_LEN || &blob[8..12] != MARKER {
        return Err(TeeError::DecryptFailed);
    }
    let version = u64::from_be_bytes(blob[..8].try_into().map_err(|_| TeeError::DecryptFailed)?);
    if version == 0 {
        return Err(TeeError::DecryptFailed);
    }
    let (key, nonce) = opening_material(view_key, account, version, &blob[12..HEADER_LEN])?;
    let plaintext = chacha20poly1305_decrypt(&key, &nonce, &blob[HEADER_LEN..])?;
    Ok((version, plaintext))
}

pub fn write_blob(view_key: &[u8; 32], transition: CohortTransition<'_>) -> Result<Vec<u8>> {
    let version = transition
        .previous_version
        .checked_add(1)
        .ok_or(TeeError::EncryptFailed)?;
    let previous_len =
        u64::try_from(transition.previous_blob.len()).map_err(|_| TeeError::EncryptFailed)?;
    let state_len =
        u64::try_from(transition.padded_state.len()).map_err(|_| TeeError::EncryptFailed)?;
    let mut input = b"outbe/fidelity/cohort-transition-binding/v2".to_vec();
    input.extend_from_slice(transition.account.as_slice());
    input.extend_from_slice(&version.to_be_bytes());
    input.extend_from_slice(&previous_len.to_be_bytes());
    input.extend_from_slice(transition.previous_blob);
    input.extend_from_slice(&state_len.to_be_bytes());
    input.extend_from_slice(transition.padded_state);
    let input = Zeroizing::new(input);
    let binding = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, view_key), &input);
    let (key, nonce) = opening_material(view_key, transition.account, version, binding.as_ref())?;
    let ciphertext = chacha20poly1305_encrypt(&key, &nonce, transition.padded_state)?;
    let mut blob = version.to_be_bytes().to_vec();
    blob.extend_from_slice(MARKER);
    blob.extend_from_slice(binding.as_ref());
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}
fn opening_material(
    view_key: &[u8; 32],
    account: Address,
    version: u64,
    binding: &[u8],
) -> Result<(Zeroizing<[u8; 32]>, [u8; 12])> {
    let mut context = account.as_slice().to_vec();
    context.extend_from_slice(&version.to_be_bytes());
    context.extend_from_slice(binding);
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
    Ok((key, nonce))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn envelope_binds_predecessor_account_and_state_and_checks_overflow() {
        let key = [9; 32];
        let account = Address::repeat_byte(3);
        let seal = |previous_blob, padded_state| {
            write_blob(
                &key,
                CohortTransition {
                    account,
                    previous_version: 0,
                    previous_blob,
                    padded_state,
                },
            )
            .unwrap()
        };
        let first = seal(&[], b"first padded state");
        assert_eq!(first, seal(&[], b"first padded state"));
        let divergent = seal(&[], b"other padded state");
        assert_ne!(&first[12..HEADER_LEN], &divergent[12..HEADER_LEN]);
        let (first_key, first_nonce) =
            opening_material(&key, account, 1, &first[12..HEADER_LEN]).unwrap();
        let (other_key, other_nonce) =
            opening_material(&key, account, 1, &divergent[12..HEADER_LEN]).unwrap();
        assert_ne!(*first_key, *other_key);
        assert_ne!(first_nonce, other_nonce);
        let next = |predecessor: &[u8]| {
            write_blob(
                &key,
                CohortTransition {
                    account,
                    previous_version: 1,
                    previous_blob: predecessor,
                    padded_state: b"identical final state",
                },
            )
            .unwrap()
        };
        assert_ne!(
            &next(&first)[12..HEADER_LEN],
            &next(&divergent)[12..HEADER_LEN]
        );
        assert_eq!(
            read_blob(&key, account, &first).unwrap(),
            (1, b"first padded state".to_vec())
        );
        assert!(read_blob(&[8; 32], account, &first).is_err());
        assert!(read_blob(&key, Address::repeat_byte(4), &first).is_err());
        for offset in [0, 8, 12, HEADER_LEN] {
            let mut altered = first.clone();
            altered[offset] ^= 1;
            assert!(read_blob(&key, account, &altered).is_err());
        }
        assert!(read_blob(&key, account, &[0; 56]).is_err());
        assert!(write_blob(
            &key,
            CohortTransition {
                account,
                previous_version: u64::MAX,
                previous_blob: &first,
                padded_state: b"state"
            }
        )
        .is_err());
    }
}
