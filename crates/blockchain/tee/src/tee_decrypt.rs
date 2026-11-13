//! Domain-neutral owner-local opening of context-bound ChaCha20Poly1305 envelopes.
//!
//! Domain crates supply the HKDF domain strings, context bindings, and ledger
//! error mapping. This module owns only the generic HKDF + ChaCha20Poly1305
//! opening recipes and zeroizing plaintext buffer.

use alloy_primitives::{Address, U256};
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

/// Inputs for [`tee_decrypt_raw`].
#[derive(Clone, Copy, Debug)]
pub struct RawDecryptInput<'a> {
    /// Symmetric 32-byte ChaCha20Poly1305 key (also used as HKDF salt for nonce derivation).
    pub key: &'a [u8; 32],
    /// Context bytes fed as IKM into HKDF-SHA256 nonce derivation.
    pub nonce_context: &'a [u8],
    /// Domain info bytes fed into HKDF-SHA256 nonce derivation.
    pub nonce_info: &'a [u8],
    /// Ciphertext with trailing 16-byte Poly1305 tag.
    pub ciphertext: &'a [u8],
}

/// Open a context-bound ChaCha20Poly1305 ciphertext whose 12-byte nonce is
/// derived via `HKDF-SHA256(salt = key, ikm = nonce_context, info = nonce_info)[..12]`.
pub fn tee_decrypt_raw(input: RawDecryptInput<'_>) -> Result<OpenedPlaintext, TeeCryptoError> {
    let nonce_material = Zeroizing::new(hkdf_sha256(
        input.key,
        input.nonce_context,
        input.nonce_info,
    )?);
    let mut nonce = [0_u8; 12];
    nonce.copy_from_slice(&nonce_material[..12]);
    let opening = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, input.key)
            .map_err(TeeCryptoError::aead_key)?,
    );
    let mut bytes = Zeroizing::new(input.ciphertext.to_vec());
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

/// Inputs for [`tee_decrypt_counter_slot`].
#[derive(Clone, Copy, Debug)]
pub struct CounterSlotInput<'a> {
    /// Record context digest used as HKDF salt when deriving the record AEAD key.
    pub context_digest: &'a [u8],
    /// Domain info string for deriving the record AEAD key.
    pub key_info: &'a [u8],
    /// Unique record crypto slot bound into the counter nonce.
    pub crypto_slot: &'a [u8],
    /// Domain info string for deriving the 12-byte AEAD nonce.
    pub nonce_info: &'a [u8],
    /// Versioned blob (`counter_be8(1) || ciphertext_with_tag`).
    pub versioned_blob: &'a [u8],
}

/// Derive an immutable record key from `view_key` and open its first-counter
/// (`version == 1`, `field_tag == 0`) envelope.
pub fn tee_decrypt_counter_slot(
    view_key: &[u8; 32],
    input: CounterSlotInput<'_>,
) -> Result<OpenedPlaintext, TeeCryptoError> {
    if input.versioned_blob.len() < 8 + 16 || input.versioned_blob[..8] != 1u64.to_be_bytes() {
        return Err(TeeCryptoError::InvalidEnvelope);
    }
    let key = Zeroizing::new(hkdf_sha256(input.context_digest, view_key, input.key_info)?);
    let nonce_context = counter_slot_nonce_context(input.crypto_slot, 0, 1);
    tee_decrypt_raw(RawDecryptInput {
        key: &key,
        nonce_context: &nonce_context,
        nonce_info: input.nonce_info,
        ciphertext: &input.versioned_blob[8..],
    })
}

/// Inputs for [`tee_decrypt_v2_envelope`].
#[derive(Clone, Copy, Debug)]
pub struct V2EnvelopeInput<'a> {
    /// Owner account bound into the V2 envelope context.
    pub account: Address,
    /// Optional sub-scope bytes appended after `account`.
    pub scope: &'a [u8],
    /// Expected 4-byte envelope format marker at `blob[8..12]`.
    pub marker: &'a [u8; 4],
    /// Domain info string for deriving the envelope AEAD key.
    pub key_info: &'a [u8],
    /// Domain info string for deriving the envelope AEAD nonce.
    pub nonce_info: &'a [u8],
    /// Encoded V2 envelope (`version(8) || marker(4) || binding(32) || ciphertext_with_tag`).
    pub blob: &'a [u8],
}

/// Open a V2 context-bound envelope (`version(8) || marker(4) || binding(32) || ciphertext_with_tag`).
pub fn tee_decrypt_v2_envelope(
    view_key: &[u8; 32],
    input: V2EnvelopeInput<'_>,
) -> Result<OpenedPlaintext, TeeCryptoError> {
    if input.blob.len() < 60 || &input.blob[8..12] != input.marker || input.blob[..8] == [0; 8] {
        return Err(TeeCryptoError::InvalidEnvelope);
    }
    let mut context = Vec::with_capacity(20 + input.scope.len() + 8 + 32);
    context.extend_from_slice(input.account.as_slice());
    context.extend_from_slice(input.scope);
    context.extend_from_slice(&input.blob[..8]);
    context.extend_from_slice(&input.blob[12..44]);
    let key = Zeroizing::new(hkdf_sha256(view_key, &context, input.key_info)?);
    tee_decrypt_raw(RawDecryptInput {
        key: &key,
        nonce_context: &context,
        nonce_info: input.nonce_info,
        ciphertext: &input.blob[44..],
    })
}

/// Domain descriptor for a 92-byte V2 `U256` balance envelope (`scope = &[0]`).
#[derive(Clone, Copy, Debug)]
pub struct V2BalanceDomain<'a> {
    /// Expected 4-byte envelope format marker at `blob[8..12]`.
    pub marker: &'a [u8; 4],
    /// Domain info string for deriving the balance AEAD key.
    pub key_info: &'a [u8],
    /// Domain info string for deriving the balance AEAD nonce.
    pub nonce_info: &'a [u8],
}

impl<'a> V2BalanceDomain<'a> {
    /// Construct a V2 `U256` balance domain descriptor.
    #[must_use]
    pub const fn new(marker: &'a [u8; 4], key_info: &'a [u8], nonce_info: &'a [u8]) -> Self {
        Self {
            marker,
            key_info,
            nonce_info,
        }
    }

    /// Open a 92-byte V2 balance envelope (`U256::ZERO` when `blob` is empty).
    pub fn open_u256(
        self,
        view_key: &[u8; 32],
        account: Address,
        blob: &[u8],
    ) -> Result<U256, TeeCryptoError> {
        tee_decrypt_v2_u256(
            view_key,
            V2EnvelopeInput {
                account,
                scope: &[0],
                marker: self.marker,
                key_info: self.key_info,
                nonce_info: self.nonce_info,
                blob,
            },
        )
    }
}

/// Open a 92-byte V2 balance envelope (`version(8) || marker(4) || binding(32) || ct(48)`)
/// as a big-endian [`U256`], returning [`U256::ZERO`] for an empty uninitialized slot.
pub fn tee_decrypt_v2_u256(
    view_key: &[u8; 32],
    input: V2EnvelopeInput<'_>,
) -> Result<U256, TeeCryptoError> {
    if input.blob.is_empty() {
        return Ok(U256::ZERO);
    }
    if input.blob.len() != 92 {
        return Err(TeeCryptoError::InvalidEnvelope);
    }
    let opened = tee_decrypt_v2_envelope(view_key, input)?;
    let amount = opened.try_into_array::<32>()?;
    Ok(U256::from_be_bytes(amount))
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

        let opened = tee_decrypt_v2_envelope(
            &view_key,
            V2EnvelopeInput {
                account,
                scope: &[0],
                marker: b"TST2",
                key_info: b"outbe/test/v2-key",
                nonce_info: b"outbe/test/v2-nonce",
                blob: &blob,
            },
        )
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
                V2EnvelopeInput {
                    account,
                    scope: &[0],
                    marker: b"BAD2",
                    key_info: b"outbe/test/v2-key",
                    nonce_info: b"outbe/test/v2-nonce",
                    blob: &blob,
                },
            )
            .unwrap_err(),
            TeeCryptoError::InvalidEnvelope
        );

        assert_eq!(
            tee_decrypt_v2_envelope(
                &view_key,
                V2EnvelopeInput {
                    account: Address::repeat_byte(0x43),
                    scope: &[0],
                    marker: b"TST2",
                    key_info: b"outbe/test/v2-key",
                    nonce_info: b"outbe/test/v2-nonce",
                    blob: &blob,
                },
            )
            .unwrap_err(),
            TeeCryptoError::DecryptionFailed
        );
    }
}
