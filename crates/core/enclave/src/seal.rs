//! Pure AES-256-GCM sealed-blob format (`TSEAL`) and seal/unseal operations.
//!
//! ```text
//! magic   "TSEAL" (5B)
//! header  format_version u8 | key_policy u8 | isv_svn u16 LE
//!         | key_epoch u64 LE | tribute_offer_epoch u64 LE | nonce 12B   (= 32B)
//! binding = canonical `NetworkBindingV1` (66B)
//! ciphertext = AES-256-GCM( payload ),  AAD = header || binding
//!   payload = tribute_offer_secret(32) || group_sig_len u16 LE || group_sig_bytes
//! ```

use ring::aead;
use zeroize::Zeroizing;

use outbe_primitives::{crypto::OneNonce, tee_attestation_v1::NetworkBindingV1};

use crate::errors::{Result, TeeError};

pub use outbe_tee::{
    SEALED_STATE_SCHEMA_V1 as SEAL_FORMAT, SEAL_HEADER_LEN as HEADER_LEN, SEAL_MAGIC,
};
pub const NETWORK_BINDING_LEN: usize = NetworkBindingV1::CANONICAL_LEN;

/// EGETKEY key policy used to derive the sealing key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyPolicy {
    /// dev/CI stub key (no real SGX).
    Mock = 0,
    /// EGETKEY bound to MRSIGNER - survives enclave update by the same signer.
    MrSigner = 1,
    /// EGETKEY bound to MRENCLAVE - strict, per-build (no update survival).
    MrEnclaveStrict = 2,
    /// Direct EGETKEY bound to both measurements. Parameters live in TSGX1.
    MrEnclaveAndSigner = 3,
}

impl KeyPolicy {
    fn from_u8(b: u8) -> Result<Self> {
        match b {
            0 => Ok(Self::Mock),
            1 => Ok(Self::MrSigner),
            2 => Ok(Self::MrEnclaveStrict),
            3 => Ok(Self::MrEnclaveAndSigner),
            other => Err(TeeError::SealedBlobBadKeyPolicy(other)),
        }
    }
}

/// Parsed sealed-blob header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SealHeader {
    pub format_version: u8,
    pub key_policy: KeyPolicy,
    pub isv_svn: u16,
    pub key_epoch: u64,
    pub tribute_offer_epoch: u64,
    pub nonce: [u8; 12],
}

impl SealHeader {
    fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0] = self.format_version;
        out[1] = self.key_policy as u8;
        out[2..4].copy_from_slice(&self.isv_svn.to_le_bytes());
        out[4..12].copy_from_slice(&self.key_epoch.to_le_bytes());
        out[12..20].copy_from_slice(&self.tribute_offer_epoch.to_le_bytes());
        out[20..32].copy_from_slice(&self.nonce);
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != HEADER_LEN {
            return Err(TeeError::SealedBlobTooShort);
        }
        let mut isv = [0u8; 2];
        isv.copy_from_slice(&bytes[2..4]);
        let mut ke = [0u8; 8];
        ke.copy_from_slice(&bytes[4..12]);
        let mut rse = [0u8; 8];
        rse.copy_from_slice(&bytes[12..20]);
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&bytes[20..32]);
        Ok(Self {
            format_version: bytes[0],
            key_policy: KeyPolicy::from_u8(bytes[1])?,
            isv_svn: u16::from_le_bytes(isv),
            key_epoch: u64::from_le_bytes(ke),
            tribute_offer_epoch: u64::from_le_bytes(rse),
            nonce,
        })
    }
}

fn aes256gcm_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    use aead::BoundKey;
    let unbound =
        aead::UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| TeeError::EncryptFailed)?;
    let mut sealing = aead::SealingKey::new(unbound, OneNonce::new(*nonce));
    let mut in_out = plaintext.to_vec();
    sealing
        .seal_in_place_append_tag(aead::Aad::from(aad), &mut in_out)
        .map_err(|_| TeeError::EncryptFailed)?;
    Ok(in_out)
}

fn aes256gcm_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    use aead::BoundKey;
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map_err(|_| TeeError::SealedBlobUnsealFailed)?;
    let mut opening = aead::OpeningKey::new(unbound, OneNonce::new(*nonce));
    let mut in_out = Zeroizing::new(ciphertext.to_vec());
    let plaintext_len = opening
        .open_in_place(aead::Aad::from(aad), &mut in_out)
        .map_err(|_| TeeError::SealedBlobUnsealFailed)?
        .len();
    in_out.truncate(plaintext_len);
    Ok(in_out)
}

/// Encode the sealed payload: `tribute_offer_secret(32) || group_sig_len u16 LE || group_sig`.
/// `group_sig` is the encoded group threshold signature (Seam F). Errors if it is
/// larger than `u16::MAX`. The returned buffer is `Zeroizing` so the plaintext is
/// wiped after AEAD.
fn encode_sealed_payload(
    tribute_offer_secret: &[u8; 32],
    group_sig: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let group_sig_len: u16 = group_sig
        .len()
        .try_into()
        .map_err(|_| TeeError::SealedBlobBadPayload(group_sig.len()))?;
    let mut out = Zeroizing::new(Vec::with_capacity(32 + 2 + group_sig.len()));
    out.extend_from_slice(tribute_offer_secret);
    out.extend_from_slice(&group_sig_len.to_le_bytes());
    out.extend_from_slice(group_sig);
    Ok(out)
}

/// Decode a sealed payload into `(tribute_offer_secret, group_sig_bytes)`, length-checked.
type DecodedSealedPayload = (Zeroizing<[u8; 32]>, Zeroizing<Vec<u8>>);

fn decode_sealed_payload(pt: &[u8]) -> Result<DecodedSealedPayload> {
    if pt.len() < 34 {
        return Err(TeeError::SealedBlobBadPayload(pt.len()));
    }
    let mut len_bytes = [0u8; 2];
    len_bytes.copy_from_slice(&pt[32..34]);
    let group_sig_len = u16::from_le_bytes(len_bytes) as usize;
    if pt.len() != 34 + group_sig_len {
        return Err(TeeError::SealedBlobBadPayload(pt.len()));
    }
    let mut tribute_offer_secret = Zeroizing::new([0u8; 32]);
    tribute_offer_secret.copy_from_slice(&pt[..32]);
    let group_sig = Zeroizing::new(pt[34..].to_vec());
    Ok((tribute_offer_secret, group_sig))
}

/// Seal the permanent DKG-derived offer secret together with the founding Seam F
/// signature. Restart restores these exact bytes. It never derives a replacement
/// key. `header.format_version` must be [`SEAL_FORMAT`].
pub fn seal_tribute_offer_and_group_sig(
    tribute_offer_secret: &[u8; 32],
    group_sig: &[u8],
    sealing_key: &[u8; 32],
    network_binding: NetworkBindingV1,
    header: &SealHeader,
) -> Result<Vec<u8>> {
    let header_bytes = header.encode();
    let binding_bytes = network_binding
        .encode_canonical()
        .map_err(TeeError::sealed_blob_bad_network_binding)?;
    let mut aad = Vec::with_capacity(HEADER_LEN + NETWORK_BINDING_LEN);
    aad.extend_from_slice(&header_bytes);
    aad.extend_from_slice(&binding_bytes);

    let payload = encode_sealed_payload(tribute_offer_secret, group_sig)?;
    let ciphertext = aes256gcm_encrypt(sealing_key, &header.nonce, &aad, &payload)?;

    let mut out =
        Vec::with_capacity(SEAL_MAGIC.len() + HEADER_LEN + NETWORK_BINDING_LEN + ciphertext.len());
    out.extend_from_slice(SEAL_MAGIC);
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&binding_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// The unsealed payload: `(tribute_offer_secret, group_sig, header)`. Secrets are
/// `Zeroizing` so they wipe on drop.
pub struct UnsealedTributeOfferAndGroupSig {
    pub tribute_offer_secret: Zeroizing<[u8; 32]>,
    pub group_sig: Zeroizing<Vec<u8>>,
    pub header: SealHeader,
    pub network_binding: NetworkBindingV1,
}

/// Unseal a blob back to `(tribute_offer_secret, group_sig_bytes, header)`.
///
/// Any `format_version` other than [`SEAL_FORMAT`] is rejected. `running_isv_svn`
/// is the currently running enclave's SVN. A blob sealed by a strictly newer SVN
/// is rejected (anti-rollback). Magic/version/SVN are checked before AEAD so an
/// unsupported blob yields a clear error. Secrets come back `Zeroizing`.
pub fn unseal_tribute_offer_and_group_sig(
    blob: &[u8],
    sealing_key: &[u8; 32],
    expected_network_binding: NetworkBindingV1,
    running_isv_svn: u16,
) -> Result<UnsealedTributeOfferAndGroupSig> {
    let unsealed = unseal_network_bound_payload(blob, sealing_key, running_isv_svn)?;
    if unsealed.network_binding != expected_network_binding {
        return Err(TeeError::SealedBlobNetworkBindingMismatch);
    }
    Ok(unsealed)
}

/// Unseal a self-describing blob during restart. The embedded binding is covered
/// by AEAD and returned to the caller, which must compare it with the runtime
/// attestation mode before activating any secret.
pub fn unseal_network_bound_payload(
    blob: &[u8],
    sealing_key: &[u8; 32],
    running_isv_svn: u16,
) -> Result<UnsealedTributeOfferAndGroupSig> {
    let prefix = SEAL_MAGIC.len();
    if blob.len() < prefix + HEADER_LEN + NETWORK_BINDING_LEN {
        return Err(TeeError::SealedBlobTooShort);
    }
    if &blob[..prefix] != SEAL_MAGIC {
        return Err(TeeError::SealedBlobBadMagic);
    }
    let header_bytes = &blob[prefix..prefix + HEADER_LEN];
    let header = SealHeader::decode(header_bytes)?;

    if header.format_version != SEAL_FORMAT {
        return Err(TeeError::SealedBlobBadVersion(header.format_version));
    }
    if header.isv_svn > running_isv_svn {
        return Err(TeeError::SealedBlobRollback {
            blob: header.isv_svn,
            running: running_isv_svn,
        });
    }

    let binding_start = prefix + HEADER_LEN;
    let binding_end = binding_start + NETWORK_BINDING_LEN;
    let binding_bytes = &blob[binding_start..binding_end];
    let network_binding = NetworkBindingV1::decode_canonical(binding_bytes)
        .map_err(TeeError::sealed_blob_bad_network_binding)?;
    let mut aad = Vec::with_capacity(HEADER_LEN + NETWORK_BINDING_LEN);
    aad.extend_from_slice(header_bytes);
    aad.extend_from_slice(binding_bytes);

    let ciphertext = &blob[binding_end..];
    let plaintext = aes256gcm_decrypt(sealing_key, &header.nonce, &aad, ciphertext)?;

    let (tribute_offer_secret, group_sig) = decode_sealed_payload(&plaintext)?;
    Ok(UnsealedTributeOfferAndGroupSig {
        tribute_offer_secret,
        group_sig,
        header,
        network_binding,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use outbe_primitives::tee_attestation_v1::AttestationMode;

    const TEST_SEALING_KEY: [u8; 32] = [0x42; 32];
    const SAMPLE_SHARE: &[u8] = &[0x55; 36];

    fn header(isv_svn: u16) -> SealHeader {
        SealHeader {
            format_version: SEAL_FORMAT,
            key_policy: KeyPolicy::Mock,
            isv_svn,
            key_epoch: 0,
            tribute_offer_epoch: 0,
            nonce: [0xAB; 12],
        }
    }

    fn binding(chain: u8, genesis: u8, mode: AttestationMode) -> NetworkBindingV1 {
        NetworkBindingV1 {
            chain_id: [chain; 32],
            genesis_hash: B256::repeat_byte(genesis),
            attestation_mode: mode,
        }
    }

    #[test]
    fn seal_unseal_roundtrip_tribute_offer_and_share() {
        let secret = [0x11; 32];
        let network = binding(0xCD, 0xCE, AttestationMode::DcapRequired);
        let h = header(1);
        let blob =
            seal_tribute_offer_and_group_sig(&secret, SAMPLE_SHARE, &TEST_SEALING_KEY, network, &h)
                .unwrap();
        let unsealed =
            unseal_tribute_offer_and_group_sig(&blob, &TEST_SEALING_KEY, network, 1).unwrap();
        assert_eq!(*unsealed.tribute_offer_secret, secret);
        assert_eq!(unsealed.group_sig.as_slice(), SAMPLE_SHARE);
        assert_eq!(unsealed.header, h);
        assert_eq!(unsealed.network_binding, network);
    }

    #[test]
    fn payload_decode_rejects_truncated_share() {
        let mut pt = vec![0u8; 32];
        pt.extend_from_slice(&10u16.to_le_bytes());
        pt.extend_from_slice(&[1u8, 2]);
        assert!(matches!(
            decode_sealed_payload(&pt),
            Err(TeeError::SealedBlobBadPayload(_))
        ));
        assert!(matches!(
            decode_sealed_payload(&[0u8; 10]),
            Err(TeeError::SealedBlobBadPayload(_))
        ));
    }

    #[test]
    fn reject_bad_magic() {
        let network = binding(1, 2, AttestationMode::DcapRequired);
        let mut blob = seal_tribute_offer_and_group_sig(
            &[0x11; 32],
            SAMPLE_SHARE,
            &TEST_SEALING_KEY,
            network,
            &header(1),
        )
        .unwrap();
        blob[0] ^= 0xFF;
        assert!(matches!(
            unseal_tribute_offer_and_group_sig(&blob, &TEST_SEALING_KEY, network, 1),
            Err(TeeError::SealedBlobBadMagic)
        ));
    }

    #[test]
    fn reject_bad_version_before_aead() {
        let network = binding(1, 2, AttestationMode::DcapRequired);
        let mut blob = seal_tribute_offer_and_group_sig(
            &[0x11; 32],
            SAMPLE_SHARE,
            &TEST_SEALING_KEY,
            network,
            &header(1),
        )
        .unwrap();
        blob[SEAL_MAGIC.len()] = 99;
        assert!(matches!(
            unseal_tribute_offer_and_group_sig(&blob, &TEST_SEALING_KEY, network, 1),
            Err(TeeError::SealedBlobBadVersion(99))
        ));
    }

    #[test]
    fn reject_rollback() {
        let network = binding(1, 2, AttestationMode::DcapRequired);
        let blob = seal_tribute_offer_and_group_sig(
            &[0x11; 32],
            SAMPLE_SHARE,
            &TEST_SEALING_KEY,
            network,
            &header(5),
        )
        .unwrap();
        assert!(matches!(
            unseal_tribute_offer_and_group_sig(&blob, &TEST_SEALING_KEY, network, 3),
            Err(TeeError::SealedBlobRollback {
                blob: 5,
                running: 3
            })
        ));
    }

    #[test]
    fn reject_wrong_key() {
        let network = binding(1, 2, AttestationMode::DcapRequired);
        let blob = seal_tribute_offer_and_group_sig(
            &[0x11; 32],
            SAMPLE_SHARE,
            &TEST_SEALING_KEY,
            network,
            &header(1),
        )
        .unwrap();
        let wrong = [0x43; 32];
        assert!(matches!(
            unseal_tribute_offer_and_group_sig(&blob, &wrong, network, 1),
            Err(TeeError::SealedBlobUnsealFailed)
        ));
    }

    #[test]
    fn reject_any_wrong_network_binding_field() {
        let expected = binding(1, 2, AttestationMode::DcapRequired);
        let blob = seal_tribute_offer_and_group_sig(
            &[0x11; 32],
            SAMPLE_SHARE,
            &TEST_SEALING_KEY,
            expected,
            &header(1),
        )
        .unwrap();
        for foreign in [
            binding(3, 2, AttestationMode::DcapRequired),
            binding(1, 3, AttestationMode::DcapRequired),
            binding(1, 2, AttestationMode::GramineDirectDev),
        ] {
            assert!(matches!(
                unseal_tribute_offer_and_group_sig(&blob, &TEST_SEALING_KEY, foreign, 1),
                Err(TeeError::SealedBlobNetworkBindingMismatch)
            ));
        }
    }
}
