//! Rollback-safe mutable balance envelopes, readable with existing Gratis view keys.
use crate::{
    crypto::{chacha20poly1305_decrypt, chacha20poly1305_encrypt, hkdf_sha256},
    errors::{Result, TeeError},
};
use alloy_primitives::{Address, B256, U256};
use ring::hmac;
use zeroize::Zeroizing;
const MARKER: &[u8; 4] = b"GRA2";
const BLOB_LEN: usize = 8 + 4 + 32 + 32 + 16;
pub fn read_amount(
    key: &[u8; 32],
    account: Address,
    field: u8,
    blob: &[u8],
) -> Result<(u64, U256)> {
    if blob.is_empty() {
        return Ok((0, U256::ZERO));
    }
    if blob.len() != BLOB_LEN || &blob[8..12] != MARKER {
        return Err(TeeError::DecryptFailed);
    }
    let version = u64::from_be_bytes(blob[..8].try_into().map_err(|_| TeeError::DecryptFailed)?);
    if version == 0 {
        return Err(TeeError::DecryptFailed);
    }
    let (opening, nonce) = opening_material(key, account, field, version, &blob[12..44])?;
    let bytes = Zeroizing::new(chacha20poly1305_decrypt(&opening, &nonce, &blob[44..])?);
    let amount: &[u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| TeeError::DecryptFailed)?;
    Ok((version, U256::from_be_bytes(*amount)))
}
pub struct BalanceTransition {
    pub account: Address,
    pub field: u8,
    pub previous_version: u64,
    pub amount: U256,
    pub input_hash: B256,
}
pub fn write_amount(key: &[u8; 32], transition: BalanceTransition) -> Result<Vec<u8>> {
    let BalanceTransition {
        account,
        field,
        previous_version,
        amount,
        input_hash,
    } = transition;
    let version = previous_version
        .checked_add(1)
        .ok_or(TeeError::EncryptFailed)?;
    let mut context = b"outbe/gratis/transition-binding/v2".to_vec();
    context.extend_from_slice(account.as_slice());
    context.push(field);
    context.extend_from_slice(&version.to_be_bytes());
    context.extend_from_slice(input_hash.as_slice());
    context.extend_from_slice(&amount.to_be_bytes::<32>());
    let context = Zeroizing::new(context);
    let binding = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), &context);
    let (sealing, nonce) = opening_material(key, account, field, version, binding.as_ref())?;
    let plaintext = Zeroizing::new(amount.to_be_bytes::<32>());
    let ciphertext = chacha20poly1305_encrypt(&sealing, &nonce, plaintext.as_ref())?;
    let mut blob = version.to_be_bytes().to_vec();
    blob.extend_from_slice(MARKER);
    blob.extend_from_slice(binding.as_ref());
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}
fn opening_material(
    key: &[u8; 32],
    account: Address,
    field: u8,
    version: u64,
    binding: &[u8],
) -> Result<(Zeroizing<[u8; 32]>, [u8; 12])> {
    let mut context = account.as_slice().to_vec();
    context.push(field);
    context.extend_from_slice(&version.to_be_bytes());
    context.extend_from_slice(binding);
    let derived = Zeroizing::new(hkdf_sha256(key, &context, b"outbe/gratis/amount-key/v2")?);
    let nonce_bytes = hkdf_sha256(&*derived, &context, b"outbe/gratis/amount-nonce/v2")?;
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_bytes[..12]);
    Ok((derived, nonce))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn divergence_and_version_exhaustion_are_safe() {
        let key = [9; 32];
        let account = Address::repeat_byte(3);
        let a = write_amount(
            &key,
            BalanceTransition {
                account,
                field: 0,
                previous_version: 1,
                amount: U256::from(10),
                input_hash: B256::repeat_byte(4),
            },
        )
        .unwrap();
        let b = write_amount(
            &key,
            BalanceTransition {
                account,
                field: 0,
                previous_version: 1,
                amount: U256::from(11),
                input_hash: B256::repeat_byte(5),
            },
        )
        .unwrap();
        assert_ne!(&a[12..44], &b[12..44]);
        assert_eq!(
            a,
            write_amount(
                &key,
                BalanceTransition {
                    account,
                    field: 0,
                    previous_version: 1,
                    amount: U256::from(10),
                    input_hash: B256::repeat_byte(4)
                }
            )
            .unwrap()
        );
        assert_eq!(
            read_amount(&key, account, 0, &a).unwrap(),
            (2, U256::from(10))
        );
        assert!(read_amount(&key, Address::repeat_byte(7), 0, &a).is_err());
        assert!(write_amount(
            &key,
            BalanceTransition {
                account,
                field: 0,
                previous_version: u64::MAX,
                amount: U256::ONE,
                input_hash: B256::ZERO
            }
        )
        .is_err());
    }
}
