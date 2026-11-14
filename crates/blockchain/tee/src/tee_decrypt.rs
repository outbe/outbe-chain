//! Domain-neutral owner-local opening of context-bound ChaCha20Poly1305 envelopes.
//!
//! Domain crates supply the HKDF domain strings, context bindings, and ledger
//! error mapping. This module owns only the generic HKDF + ChaCha20Poly1305
//! opening recipes and zeroizing plaintext buffer.

use alloy_primitives::Address;
use ring::aead;
use zeroize::Zeroizing;

use crate::{errors::TeeCryptoError, tee_encrypt::hkdf_sha256};

/// Zeroizing plaintext buffer returned by [`tee_decrypt_raw`],
/// [`tee_decrypt_counter_slot`], and [`tee_decrypt_v2_envelope`].
pub struct OpenedPlaintext {
    bytes: Zeroizing<Vec<u8>>,
    plaintext_len: usize,
}

impl std::fmt::Debug for OpenedPlaintext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenedPlaintext")
            .field("plaintext_len", &self.plaintext_len)
            .finish_non_exhaustive()
    }
}

impl OpenedPlaintext {
    /// Borrow the opened plaintext bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.plaintext_len]
    }

    /// Copy the opened plaintext into a fixed-size byte array or return
    /// [`TeeCryptoError::InvalidPlaintextLength`].
    pub fn try_into_array<const N: usize>(&self) -> Result<[u8; N], TeeCryptoError> {
        let slice = self.as_slice();
        slice
            .try_into()
            .map_err(|_| TeeCryptoError::InvalidPlaintextLength {
                expected: N,
                actual: slice.len(),
            })
    }
}

/// Open a context-bound ChaCha20Poly1305 ciphertext whose 12-byte nonce is
/// derived via `HKDF-SHA256(salt = key, ikm = nonce_context, info = nonce_info)[..12]`.
pub fn tee_decrypt_raw(
    key: &[u8; 32],
    nonce_context: &[u8],
    nonce_info: &[u8],
    ciphertext: &[u8],
) -> Result<OpenedPlaintext, TeeCryptoError> {
    let nonce_material = Zeroizing::new(hkdf_sha256(key, nonce_context, nonce_info)?);
    let mut nonce = [0_u8; 12];
    nonce.copy_from_slice(&nonce_material[..12]);
    let opening = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(TeeCryptoError::aead_key)?,
    );
    let mut bytes = Zeroizing::new(ciphertext.to_vec());
    let plaintext_len = opening
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::empty(),
            &mut bytes,
        )
        .map_err(|_| TeeCryptoError::DecryptionFailed)?
        .len();
    Ok(OpenedPlaintext {
        bytes,
        plaintext_len,
    })
}

/// Build the canonical counter-slot nonce input `crypto_slot || field_tag || counter_be8`.
#[must_use]
pub fn counter_slot_nonce_context(crypto_slot: &[u8], field_tag: u8, counter: u64) -> Vec<u8> {
    let mut input = Vec::with_capacity(crypto_slot.len() + 1 + 8);
    input.extend_from_slice(crypto_slot);
    input.push(field_tag);
    input.extend_from_slice(&counter.to_be_bytes());
    input
}

/// HKDF and slot context for [`tee_decrypt_counter_slot`].
#[derive(Clone, Copy, Debug)]
pub struct CounterSlotContext<'a> {
    /// Digest binding the record context as HKDF salt.
    pub context_digest: &'a [u8],
    /// HKDF info string for deriving the record key.
    pub key_info: &'a [u8],
    /// Slot identifier for deterministic counter-slot nonce derivation.
    pub crypto_slot: &'a [u8],
    /// HKDF info string for deriving the AEAD nonce.
    pub nonce_info: &'a [u8],
}

/// Derive an immutable record key from `view_key` and open its first-counter
/// (`version == 1`, `field_tag == 0`) envelope.
pub fn tee_decrypt_counter_slot(
    view_key: &[u8; 32],
    context: CounterSlotContext<'_>,
    versioned_blob: &[u8],
) -> Result<OpenedPlaintext, TeeCryptoError> {
    if versioned_blob.len() < 8 + 16 || versioned_blob[..8] != 1u64.to_be_bytes() {
        return Err(TeeCryptoError::InvalidEnvelope);
    }
    let key = Zeroizing::new(hkdf_sha256(
        context.context_digest,
        view_key,
        context.key_info,
    )?);
    let nonce_context = counter_slot_nonce_context(context.crypto_slot, 0, 1);
    tee_decrypt_raw(
        &key,
        &nonce_context,
        context.nonce_info,
        &versioned_blob[8..],
    )
}

/// Domain marker, scope, and HKDF labels for [`tee_decrypt_v2_envelope`].
#[derive(Clone, Copy, Debug)]
pub struct V2EnvelopeDomain<'a> {
    /// Additional context scope bytes appended after the 20-byte account address.
    pub scope: &'a [u8],
    /// 4-byte envelope format marker at `blob[8..12]`.
    pub marker: &'a [u8; 4],
    /// HKDF info string for deriving the envelope key.
    pub key_info: &'a [u8],
    /// HKDF info string for deriving the envelope nonce.
    pub nonce_info: &'a [u8],
}

/// Open a V2 context-bound envelope (`version(8) || marker(4) || binding(32) || ciphertext_with_tag`).
pub fn tee_decrypt_v2_envelope(
    view_key: &[u8; 32],
    account: Address,
    domain: V2EnvelopeDomain<'_>,
    blob: &[u8],
) -> Result<OpenedPlaintext, TeeCryptoError> {
    if blob.len() < 60 || &blob[8..12] != domain.marker || blob[..8] == [0; 8] {
        return Err(TeeCryptoError::InvalidEnvelope);
    }
    let mut context = Vec::with_capacity(20 + domain.scope.len() + 8 + 32);
    context.extend_from_slice(account.as_slice());
    context.extend_from_slice(domain.scope);
    context.extend_from_slice(&blob[..8]);
    context.extend_from_slice(&blob[12..44]);
    let key = Zeroizing::new(hkdf_sha256(view_key, &context, domain.key_info)?);
    tee_decrypt_raw(&key, &context, domain.nonce_info, &blob[44..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seal_v2_for_test(
        view_key: &[u8; 32],
        account: Address,
        marker: &[u8; 4],
        plaintext: &[u8],
    ) -> Vec<u8> {
        let version = 1u64.to_be_bytes();
        let binding = [0x77u8; 32];
        let mut context = Vec::new();
        context.extend_from_slice(account.as_slice());
        context.push(0);
        context.extend_from_slice(&version);
        context.extend_from_slice(&binding);

        let key = hkdf_sha256(view_key, &context, b"outbe/test/v2-key").unwrap();
        let nonce_material = hkdf_sha256(&key, &context, b"outbe/test/v2-nonce").unwrap();
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&nonce_material[..12]);
        let sealing =
            aead::LessSafeKey::new(aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &key).unwrap());
        let mut ct = plaintext.to_vec();
        sealing
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::empty(),
                &mut ct,
            )
            .unwrap();

        let mut blob = Vec::new();
        blob.extend_from_slice(&version);
        blob.extend_from_slice(marker);
        blob.extend_from_slice(&binding);
        blob.extend_from_slice(&ct);
        blob
    }

    #[test]
    fn v2_envelope_opens_plaintext_and_checks_marker_account_and_array_len() {
        let view_key = [0x19; 32];
        let account = Address::repeat_byte(0x42);
        let blob = seal_v2_for_test(&view_key, account, b"TST2", &[0xabu8; 32]);

        let test_domain = V2EnvelopeDomain {
            scope: &[0],
            marker: b"TST2",
            key_info: b"outbe/test/v2-key",
            nonce_info: b"outbe/test/v2-nonce",
        };
        let opened = tee_decrypt_v2_envelope(&view_key, account, test_domain, &blob)
            .expect("open valid V2 envelope");
        assert_eq!(opened.try_into_array::<32>().unwrap(), [0xab; 32]);
        assert_eq!(
            opened.try_into_array::<16>().unwrap_err(),
            TeeCryptoError::InvalidPlaintextLength {
                expected: 16,
                actual: 32,
            }
        );

        assert_eq!(
            tee_decrypt_v2_envelope(
                &view_key,
                account,
                V2EnvelopeDomain {
                    marker: b"BAD2",
                    ..test_domain
                },
                &blob,
            )
            .unwrap_err(),
            TeeCryptoError::InvalidEnvelope
        );

        assert_eq!(
            tee_decrypt_v2_envelope(&view_key, Address::repeat_byte(0x43), test_domain, &blob)
                .unwrap_err(),
            TeeCryptoError::DecryptionFailed
        );
    }
}
