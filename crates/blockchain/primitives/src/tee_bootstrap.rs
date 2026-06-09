//! `TeeBootstrap` system-transaction payload (Phase 3b).
//!
//! Wire format (deterministic, manual big-endian codec, mirroring
//! [`crate::consensus_metadata`]): `magic(4) || fixed scalars || registrations ||
//! validator_signatures`. The payload is carried in the begin-zone system tx
//! body. The native handler verifies it (committee match, policy, N×ECDSA
//! signatures, registry-empty) before writing the registry; this module is the
//! codec only.

use alloy_primitives::{keccak256, Address, Bytes, B256};
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};

use crate::error::{PrecompileError, Result};

/// Magic prefix for the bootstrap payload wire format.
const MAGIC: &[u8; 4] = b"TTB1";

/// Hard cap on registrations / signatures in one payload. Bounds allocation; the
/// handler additionally checks counts against the committee size.
pub const MAX_TEE_REGISTRATIONS: usize = 256;

/// Domain label the validator ECDSA signatures cover (with the payload hash).
pub const TEE_BOOTSTRAP_SIGNING_DOMAIN: &[u8] = b"outbe/tee/bootstrap/v1";

/// Domain label binding `TeeRegistrationBundle::computed_keys_hash`.
const TEE_KEYS_HASH_DOMAIN: &[u8] = b"outbe/tee/keys/v1";

/// Domain label binding [`TeePolicy::compute_hash`].
pub const TEE_POLICY_HASH_DOMAIN: &[u8] = b"outbe/tee/policy/v1";

/// Hard cap on allowlist entries per policy field. Bounds allocation/iteration.
pub const MAX_TEE_POLICY_ENTRIES: usize = 64;

/// Genesis TEE attestation policy: the allowlist a `TeeBootstrap` payload's
/// enclave registrations are checked against (Phase 3b). Carried in the signed
/// payload and bound to the genesis-seeded `TeeRegistry.policy_hash` (slot 2);
/// the handler enforces it deterministically.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeePolicy {
    pub allowed_mrsigner: Vec<B256>,
    pub allowed_mrenclave: Vec<B256>,
    pub min_isv_svn: u16,
}

impl TeePolicy {
    /// Canonical, order-independent policy hash: `keccak256(domain ||
    /// len(mrsigner) || sorted(mrsigner) || len(mrenclave) || sorted(mrenclave)
    /// || min_isv_svn)`. Sorting makes the hash independent of allowlist ordering
    /// so the genesis seed and the payload agree regardless of insertion order.
    pub fn compute_hash(&self) -> B256 {
        let mut signers = self.allowed_mrsigner.clone();
        signers.sort_unstable();
        let mut enclaves = self.allowed_mrenclave.clone();
        enclaves.sort_unstable();
        let mut buf = Vec::with_capacity(
            TEE_POLICY_HASH_DOMAIN.len() + 4 + 32 * (signers.len() + enclaves.len()) + 2,
        );
        buf.extend_from_slice(TEE_POLICY_HASH_DOMAIN);
        buf.extend_from_slice(&(signers.len() as u16).to_be_bytes());
        for s in &signers {
            buf.extend_from_slice(s.as_slice());
        }
        buf.extend_from_slice(&(enclaves.len() as u16).to_be_bytes());
        for e in &enclaves {
            buf.extend_from_slice(e.as_slice());
        }
        buf.extend_from_slice(&self.min_isv_svn.to_be_bytes());
        keccak256(buf)
    }

    /// True when no policy is configured (empty allowlists + zero floor). The
    /// handler skips measurement enforcement only when the genesis policy_hash is
    /// `ZERO`, not on this — kept for producer/host convenience.
    pub fn is_empty(&self) -> bool {
        self.allowed_mrsigner.is_empty()
            && self.allowed_mrenclave.is_empty()
            && self.min_isv_svn == 0
    }

    /// AND-policy: a registration's measurements are admitted iff its MRSIGNER is
    /// allowlisted AND its MRENCLAVE is allowlisted AND its SVN meets the floor.
    pub fn admits(&self, mrsigner: B256, mrenclave: B256, isv_svn: u16) -> bool {
        self.allowed_mrsigner.contains(&mrsigner)
            && self.allowed_mrenclave.contains(&mrenclave)
            && isv_svn >= self.min_isv_svn
    }
}

/// Per-validator TEE registration bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeeRegistrationBundle {
    pub validator: Address,
    pub recipient_x25519: B256,
    pub attestation_pub: B256,
    pub noise_static_pub: B256,
    pub mrenclave: B256,
    pub mrsigner: B256,
    pub isv_svn: u16,
    pub keys_hash: B256,
}

impl TeeRegistrationBundle {
    /// Canonical preimage binding the validator identity to its enclave key
    /// material. Producer (enclave-side proposer) and verifier (`run_tee_bootstrap`)
    /// must agree byte-for-byte; `keys_hash` is the keccak of this preimage.
    fn keys_hash_preimage(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            TEE_KEYS_HASH_DOMAIN.len() + 20 + 32 * 5 + 2, // domain + addr + 5×B256 + isv_svn
        );
        buf.extend_from_slice(TEE_KEYS_HASH_DOMAIN);
        buf.extend_from_slice(self.validator.as_slice());
        buf.extend_from_slice(self.recipient_x25519.as_slice());
        buf.extend_from_slice(self.attestation_pub.as_slice());
        buf.extend_from_slice(self.noise_static_pub.as_slice());
        buf.extend_from_slice(self.mrenclave.as_slice());
        buf.extend_from_slice(self.mrsigner.as_slice());
        buf.extend_from_slice(&self.isv_svn.to_be_bytes());
        buf
    }

    /// Recompute `keys_hash` from this bundle's key material. The handler
    /// rejects any registration whose stored `keys_hash` disagrees, so the
    /// 32-byte digest cannot be decoupled from the keys it commits to.
    pub fn computed_keys_hash(&self) -> B256 {
        keccak256(self.keys_hash_preimage())
    }
}

/// A validator's recoverable secp256k1 ECDSA signature over the payload hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeeValidatorSignature {
    pub validator: Address,
    pub signature: [u8; 65],
}

/// The one-time bootstrap payload that initializes `TeeRegistry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeeBootstrapPayload {
    pub policy_hash: B256,
    pub committee_snapshot_hash: B256,
    pub committee_snapshot_block: u64,
    pub key_epoch: u64,
    pub tribute_offer_epoch: u64,
    pub dkg_transcript_hash: B256,
    pub tribute_offer_public_key: B256,
    pub registrations: Vec<TeeRegistrationBundle>,
    /// Genesis attestation allowlist (signed). `policy_hash` must equal
    /// `policy.compute_hash()`; the handler binds it to the genesis-seeded
    /// `TeeRegistry.policy_hash` and enforces it against each registration.
    pub policy: TeePolicy,
    pub validator_signatures: Vec<TeeValidatorSignature>,
}

impl TeeBootstrapPayload {
    /// Encode to the deterministic wire format.
    pub fn encode(&self) -> Result<Bytes> {
        if self.registrations.len() > MAX_TEE_REGISTRATIONS {
            return Err(revert(format!(
                "too many TEE registrations: {}",
                self.registrations.len()
            )));
        }
        if self.validator_signatures.len() > MAX_TEE_REGISTRATIONS {
            return Err(revert(format!(
                "too many TEE signatures: {}",
                self.validator_signatures.len()
            )));
        }

        let mut buf = Vec::new();
        self.encode_body_into(&mut buf);

        buf.extend_from_slice(&(self.validator_signatures.len() as u16).to_be_bytes());
        for sig in &self.validator_signatures {
            buf.extend_from_slice(sig.validator.as_slice());
            buf.extend_from_slice(&sig.signature);
        }

        Ok(Bytes::from(buf))
    }

    /// Serialize the signed body: everything except the `validator_signatures`
    /// section (`MAGIC || scalars || registrations`). The validator ECDSA
    /// signatures cover exactly these bytes (under `signing_hash`), so the
    /// signed message is a structural prefix of the wire format.
    fn encode_body_into(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(self.policy_hash.as_slice());
        buf.extend_from_slice(self.committee_snapshot_hash.as_slice());
        buf.extend_from_slice(&self.committee_snapshot_block.to_be_bytes());
        buf.extend_from_slice(&self.key_epoch.to_be_bytes());
        buf.extend_from_slice(&self.tribute_offer_epoch.to_be_bytes());
        buf.extend_from_slice(self.dkg_transcript_hash.as_slice());
        buf.extend_from_slice(self.tribute_offer_public_key.as_slice());

        buf.extend_from_slice(&(self.registrations.len() as u16).to_be_bytes());
        for reg in &self.registrations {
            buf.extend_from_slice(reg.validator.as_slice());
            buf.extend_from_slice(reg.recipient_x25519.as_slice());
            buf.extend_from_slice(reg.attestation_pub.as_slice());
            buf.extend_from_slice(reg.noise_static_pub.as_slice());
            buf.extend_from_slice(reg.mrenclave.as_slice());
            buf.extend_from_slice(reg.mrsigner.as_slice());
            buf.extend_from_slice(&reg.isv_svn.to_be_bytes());
            buf.extend_from_slice(reg.keys_hash.as_slice());
        }

        // Policy allowlist (signed): mrsigner set, mrenclave set, min SVN. Order
        // is preserved on the wire; `compute_hash` is order-independent.
        buf.extend_from_slice(&(self.policy.allowed_mrsigner.len() as u16).to_be_bytes());
        for s in &self.policy.allowed_mrsigner {
            buf.extend_from_slice(s.as_slice());
        }
        buf.extend_from_slice(&(self.policy.allowed_mrenclave.len() as u16).to_be_bytes());
        for e in &self.policy.allowed_mrenclave {
            buf.extend_from_slice(e.as_slice());
        }
        buf.extend_from_slice(&self.policy.min_isv_svn.to_be_bytes());
    }

    /// The 32-byte digest each validator ECDSA-signs to authorize this bootstrap.
    ///
    /// `keccak256(TEE_BOOTSTRAP_SIGNING_DOMAIN || encode_body())`. Domain
    /// separation prevents a bootstrap signature from being replayed as any
    /// other secp256k1 message. The signed body excludes `validator_signatures`
    /// so signers commit to the payload, not to each other's signatures.
    pub fn signing_hash(&self) -> B256 {
        let mut buf = Vec::new();
        buf.extend_from_slice(TEE_BOOTSTRAP_SIGNING_DOMAIN);
        self.encode_body_into(&mut buf);
        keccak256(buf)
    }

    /// Decode from the deterministic wire format; rejects malformed or
    /// trailing-byte input.
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut reader = Reader::new(data);
        if reader.take(4)? != MAGIC {
            return Err(revert("bad TeeBootstrap magic".to_string()));
        }
        let policy_hash = reader.b256()?;
        let committee_snapshot_hash = reader.b256()?;
        let committee_snapshot_block = reader.u64()?;
        let key_epoch = reader.u64()?;
        let tribute_offer_epoch = reader.u64()?;
        let dkg_transcript_hash = reader.b256()?;
        let tribute_offer_public_key = reader.b256()?;

        let reg_count = usize::from(reader.u16()?);
        if reg_count > MAX_TEE_REGISTRATIONS {
            return Err(revert(format!("too many TEE registrations: {reg_count}")));
        }
        let mut registrations = Vec::with_capacity(reg_count);
        for _ in 0..reg_count {
            registrations.push(TeeRegistrationBundle {
                validator: reader.address()?,
                recipient_x25519: reader.b256()?,
                attestation_pub: reader.b256()?,
                noise_static_pub: reader.b256()?,
                mrenclave: reader.b256()?,
                mrsigner: reader.b256()?,
                isv_svn: reader.u16()?,
                keys_hash: reader.b256()?,
            });
        }

        // Policy allowlist (signed): mirrors `encode_body_into`.
        let ms_count = usize::from(reader.u16()?);
        if ms_count > MAX_TEE_POLICY_ENTRIES {
            return Err(revert(format!(
                "too many policy mrsigner entries: {ms_count}"
            )));
        }
        let mut allowed_mrsigner = Vec::with_capacity(ms_count);
        for _ in 0..ms_count {
            allowed_mrsigner.push(reader.b256()?);
        }
        let me_count = usize::from(reader.u16()?);
        if me_count > MAX_TEE_POLICY_ENTRIES {
            return Err(revert(format!(
                "too many policy mrenclave entries: {me_count}"
            )));
        }
        let mut allowed_mrenclave = Vec::with_capacity(me_count);
        for _ in 0..me_count {
            allowed_mrenclave.push(reader.b256()?);
        }
        let policy = TeePolicy {
            allowed_mrsigner,
            allowed_mrenclave,
            min_isv_svn: reader.u16()?,
        };

        let sig_count = usize::from(reader.u16()?);
        if sig_count > MAX_TEE_REGISTRATIONS {
            return Err(revert(format!("too many TEE signatures: {sig_count}")));
        }
        let mut validator_signatures = Vec::with_capacity(sig_count);
        for _ in 0..sig_count {
            validator_signatures.push(TeeValidatorSignature {
                validator: reader.address()?,
                signature: reader.array65()?,
            });
        }

        reader.finish()?;
        Ok(Self {
            policy_hash,
            committee_snapshot_hash,
            committee_snapshot_block,
            key_epoch,
            tribute_offer_epoch,
            dkg_transcript_hash,
            tribute_offer_public_key,
            registrations,
            policy,
            validator_signatures,
        })
    }
}

fn revert(message: String) -> PrecompileError {
    PrecompileError::Revert(message)
}

/// Recover the EVM address that produced a recoverable secp256k1 ECDSA
/// `signature` (65 bytes: `r(32) || s(32) || v(1)`) over `prehash`.
///
/// `v` is accepted as either the raw recovery id (`0`/`1`) or the EIP-155-free
/// legacy form (`27`/`28`). Mirrors the construction in
/// [`crate::signer::OutbeEvmSigner`] so a signature produced there round-trips.
pub fn recover_signer(prehash: &B256, signature: &[u8; 65]) -> Result<Address> {
    let recid_byte = signature[64];
    let normalized = match recid_byte {
        27 | 28 => recid_byte - 27,
        other => other,
    };
    let recovery_id = RecoveryId::from_byte(normalized)
        .ok_or_else(|| revert(format!("TeeBootstrap: invalid recovery id {recid_byte}")))?;
    let ecdsa_sig = EcdsaSignature::from_slice(&signature[..64])
        .map_err(|error| revert(format!("TeeBootstrap: malformed signature: {error}")))?;
    let verifying_key =
        VerifyingKey::recover_from_prehash(prehash.as_slice(), &ecdsa_sig, recovery_id)
            .map_err(|error| revert(format!("TeeBootstrap: signature recovery failed: {error}")))?;
    // Uncompressed SEC1 point: 0x04 || X(32) || Y(32). The EVM address is the
    // low 20 bytes of keccak256(X || Y).
    let encoded = verifying_key.to_encoded_point(false);
    let pubkey = encoded.as_bytes();
    let digest = keccak256(&pubkey[1..]);
    Ok(Address::from_slice(&digest[12..]))
}

/// Minimal offset reader with bounds checks.
struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(n)
            .ok_or_else(|| revert("TeeBootstrap length overflow".to_string()))?;
        let slice = self
            .data
            .get(self.offset..end)
            .ok_or_else(|| revert("TeeBootstrap truncated".to_string()))?;
        self.offset = end;
        Ok(slice)
    }

    fn b256(&mut self) -> Result<B256> {
        Ok(B256::from_slice(self.take(32)?))
    }

    fn address(&mut self) -> Result<Address> {
        Ok(Address::from_slice(self.take(20)?))
    }

    fn u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let bytes = self.take(8)?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(bytes);
        Ok(u64::from_be_bytes(arr))
    }

    fn array65(&mut self) -> Result<[u8; 65]> {
        let bytes = self.take(65)?;
        let mut arr = [0u8; 65];
        arr.copy_from_slice(bytes);
        Ok(arr)
    }

    fn finish(&self) -> Result<()> {
        if self.offset != self.data.len() {
            return Err(revert(format!(
                "TeeBootstrap trailing bytes: consumed {} of {}",
                self.offset,
                self.data.len()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TeeBootstrapPayload {
        TeeBootstrapPayload {
            policy_hash: B256::repeat_byte(0xA1),
            committee_snapshot_hash: B256::repeat_byte(0xA2),
            committee_snapshot_block: 1,
            key_epoch: 0,
            tribute_offer_epoch: 0,
            dkg_transcript_hash: B256::repeat_byte(0xA3),
            tribute_offer_public_key: B256::repeat_byte(0xA4),
            registrations: vec![
                TeeRegistrationBundle {
                    validator: Address::repeat_byte(0x11),
                    recipient_x25519: B256::repeat_byte(0x21),
                    attestation_pub: B256::repeat_byte(0x22),
                    noise_static_pub: B256::repeat_byte(0x23),
                    mrenclave: B256::repeat_byte(0x24),
                    mrsigner: B256::repeat_byte(0x25),
                    isv_svn: 3,
                    keys_hash: B256::repeat_byte(0x26),
                },
                TeeRegistrationBundle {
                    validator: Address::repeat_byte(0x12),
                    recipient_x25519: B256::repeat_byte(0x31),
                    attestation_pub: B256::repeat_byte(0x32),
                    noise_static_pub: B256::repeat_byte(0x33),
                    mrenclave: B256::repeat_byte(0x34),
                    mrsigner: B256::repeat_byte(0x35),
                    isv_svn: 4,
                    keys_hash: B256::repeat_byte(0x36),
                },
            ],
            policy: TeePolicy {
                allowed_mrsigner: vec![B256::repeat_byte(0x25), B256::repeat_byte(0x35)],
                allowed_mrenclave: vec![B256::repeat_byte(0x24), B256::repeat_byte(0x34)],
                min_isv_svn: 2,
            },
            validator_signatures: vec![
                TeeValidatorSignature {
                    validator: Address::repeat_byte(0x11),
                    signature: [0x41; 65],
                },
                TeeValidatorSignature {
                    validator: Address::repeat_byte(0x12),
                    signature: [0x42; 65],
                },
            ],
        }
    }

    #[test]
    fn roundtrip() {
        let payload = sample();
        let encoded = payload.encode().unwrap();
        let decoded = TeeBootstrapPayload::decode(&encoded).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn roundtrip_empty_lists() {
        let mut payload = sample();
        payload.registrations.clear();
        payload.validator_signatures.clear();
        let encoded = payload.encode().unwrap();
        assert_eq!(TeeBootstrapPayload::decode(&encoded).unwrap(), payload);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut encoded = sample().encode().unwrap().to_vec();
        encoded[0] ^= 0xFF;
        assert!(TeeBootstrapPayload::decode(&encoded).is_err());
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut encoded = sample().encode().unwrap().to_vec();
        encoded.push(0);
        assert!(TeeBootstrapPayload::decode(&encoded).is_err());
    }

    #[test]
    fn rejects_truncated() {
        let encoded = sample().encode().unwrap();
        assert!(TeeBootstrapPayload::decode(&encoded[..encoded.len() - 1]).is_err());
    }

    #[test]
    fn policy_hash_is_order_independent_and_binds_fields() {
        let a = TeePolicy {
            allowed_mrsigner: vec![B256::repeat_byte(0x01), B256::repeat_byte(0x02)],
            allowed_mrenclave: vec![B256::repeat_byte(0x03)],
            min_isv_svn: 5,
        };
        // Same set, reversed order -> identical hash (sorted canonicalization).
        let b = TeePolicy {
            allowed_mrsigner: vec![B256::repeat_byte(0x02), B256::repeat_byte(0x01)],
            allowed_mrenclave: vec![B256::repeat_byte(0x03)],
            min_isv_svn: 5,
        };
        assert_eq!(a.compute_hash(), b.compute_hash());
        // A different floor changes the hash.
        let mut c = a.clone();
        c.min_isv_svn = 6;
        assert_ne!(a.compute_hash(), c.compute_hash());
        // A different member changes the hash.
        let mut d = a.clone();
        d.allowed_mrenclave = vec![B256::repeat_byte(0x99)];
        assert_ne!(a.compute_hash(), d.compute_hash());
        // Empty policy hash is non-zero (so ZERO stays a clean "unconfigured" sentinel).
        assert_ne!(TeePolicy::default().compute_hash(), B256::ZERO);
    }

    #[test]
    fn policy_hash_matches_genesis_seed_golden() {
        // Cross-language golden: these MUST equal `scripts/seed_genesis.py`
        // `compute_tee_policy_hash` for the same inputs, so a genesis-seeded
        // `TeeRegistry.policy_hash` (slot 2) matches the producer's
        // `payload.policy_hash` and the Phase 3b binding check passes. If this
        // test fails, the Rust and Python policy-hash encodings have diverged.
        let p = TeePolicy {
            allowed_mrsigner: vec![B256::repeat_byte(0x60)],
            allowed_mrenclave: vec![B256::repeat_byte(0x50)],
            min_isv_svn: 3,
        };
        assert_eq!(
            p.compute_hash(),
            alloy_primitives::b256!(
                "a0db4f302ad67ef58cf921c227363fc1f97066724264a83d4a38386c99ced6b7"
            ),
        );
        assert_eq!(
            TeePolicy::default().compute_hash(),
            alloy_primitives::b256!(
                "22db5b503c5b75519fb8c98344de14b1896c74da9e67bbd9013291242eeeefd0"
            ),
        );
    }

    #[test]
    fn policy_admits_is_strict_and() {
        let p = TeePolicy {
            allowed_mrsigner: vec![B256::repeat_byte(0x25)],
            allowed_mrenclave: vec![B256::repeat_byte(0x24)],
            min_isv_svn: 3,
        };
        assert!(p.admits(B256::repeat_byte(0x25), B256::repeat_byte(0x24), 3));
        assert!(p.admits(B256::repeat_byte(0x25), B256::repeat_byte(0x24), 9));
        // Wrong mrsigner / mrenclave / below-floor SVN each fail.
        assert!(!p.admits(B256::repeat_byte(0x99), B256::repeat_byte(0x24), 3));
        assert!(!p.admits(B256::repeat_byte(0x25), B256::repeat_byte(0x99), 3));
        assert!(!p.admits(B256::repeat_byte(0x25), B256::repeat_byte(0x24), 2));
    }

    #[test]
    fn signing_hash_binds_policy() {
        let base = sample();
        let hash = base.signing_hash();
        // Changing the policy allowlist changes the signed digest.
        let mut changed = base.clone();
        changed.policy.min_isv_svn += 1;
        assert_ne!(
            changed.signing_hash(),
            hash,
            "policy is part of the signed body"
        );
    }

    #[test]
    fn signing_hash_ignores_signatures_but_binds_body() {
        let base = sample();
        let hash = base.signing_hash();

        // Mutating only the signatures must not change the signed digest:
        // signers commit to the body, not to one another's signatures.
        let mut sig_changed = base.clone();
        sig_changed.validator_signatures[0].signature = [0x99; 65];
        assert_eq!(sig_changed.signing_hash(), hash);

        // Mutating any body field must change the digest.
        let mut block_changed = base.clone();
        block_changed.committee_snapshot_block += 1;
        assert_ne!(block_changed.signing_hash(), hash);

        let mut key_changed = base.clone();
        key_changed.tribute_offer_public_key = B256::repeat_byte(0xFE);
        assert_ne!(key_changed.signing_hash(), hash);

        let mut reg_changed = base;
        reg_changed.registrations[0].recipient_x25519 = B256::repeat_byte(0xFD);
        assert_ne!(reg_changed.signing_hash(), hash);
    }

    #[test]
    fn signing_hash_is_domain_separated() {
        // The signed digest must not collide with the raw keccak of the body
        // (domain-separation guard against cross-protocol signature replay).
        let payload = sample();
        let mut body = Vec::new();
        payload.encode_body_into(&mut body);
        assert_ne!(payload.signing_hash(), keccak256(&body));
    }

    #[test]
    fn computed_keys_hash_binds_key_material() {
        let reg = sample().registrations[0].clone();
        let base = reg.computed_keys_hash();

        let mut changed = reg.clone();
        changed.recipient_x25519 = B256::repeat_byte(0xEE);
        assert_ne!(changed.computed_keys_hash(), base);

        let mut svn_changed = reg;
        svn_changed.isv_svn += 1;
        assert_ne!(svn_changed.computed_keys_hash(), base);
    }

    #[test]
    fn recover_signer_roundtrips_with_evm_signer_construction() {
        use alloy_primitives::keccak256;
        use k256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey};

        // Deterministic non-zero scalar as the secret key.
        let signing_key = SigningKey::from_slice(&[0x42u8; 32]).unwrap();

        // Expected address = keccak(uncompressed pubkey[1..])[12..].
        let vk = signing_key.verifying_key();
        let point = vk.to_encoded_point(false);
        let expected = Address::from_slice(&keccak256(&point.as_bytes()[1..])[12..]);

        let prehash = B256::repeat_byte(0x7c);
        let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
            signing_key.sign_prehash(prehash.as_slice()).unwrap();
        let mut sig65 = [0u8; 65];
        sig65[..64].copy_from_slice(sig.to_bytes().as_slice());
        sig65[64] = recid.to_byte();

        assert_eq!(recover_signer(&prehash, &sig65).unwrap(), expected);

        // Legacy 27/28 parity encoding recovers the same address.
        let mut legacy = sig65;
        legacy[64] = recid.to_byte() + 27;
        assert_eq!(recover_signer(&prehash, &legacy).unwrap(), expected);

        // A different prehash must not recover the same signer.
        let other = B256::repeat_byte(0x01);
        assert_ne!(recover_signer(&other, &sig65).unwrap(), expected);
    }
}
