#[cfg(feature = "tee-attestation-v1")]
use alloy_primitives::keccak256;
use alloy_primitives::{Address, B256};
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::TeeRegistry;

/// Domain label binding the per-validator `keys_hash` (mirrors the bootstrap
/// bundle's `outbe/tee/keys/v1`), so a V1 `registerEnclave` produces the
/// same `keysHash(addr)` shape as the block-1 bootstrap registration.
#[cfg(feature = "tee-attestation-v1")]
const TEE_KEYS_HASH_DOMAIN: &[u8] = b"outbe/tee/keys/v1";

/// `keccak256(domain ‖ validator ‖ recipient ‖ attestation ‖ noise ‖ mrenclave ‖
/// mrsigner ‖ isv_svn_be)` — binds all of a validator's enclave key material.
#[cfg(feature = "tee-attestation-v1")]
pub(crate) fn compute_keys_hash(
    validator: Address,
    recipient_x25519: B256,
    attestation_pub: B256,
    noise_static_pub: B256,
    mrenclave: B256,
    mrsigner: B256,
    isv_svn: u16,
) -> B256 {
    let mut buf = Vec::with_capacity(TEE_KEYS_HASH_DOMAIN.len() + 20 + 32 * 5 + 2);
    buf.extend_from_slice(TEE_KEYS_HASH_DOMAIN);
    buf.extend_from_slice(validator.as_slice());
    buf.extend_from_slice(recipient_x25519.as_slice());
    buf.extend_from_slice(attestation_pub.as_slice());
    buf.extend_from_slice(noise_static_pub.as_slice());
    buf.extend_from_slice(mrenclave.as_slice());
    buf.extend_from_slice(mrsigner.as_slice());
    buf.extend_from_slice(&isv_svn.to_be_bytes());
    keccak256(&buf)
}

/// Per-validator TEE registration bundle written at bootstrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeeRegistration {
    pub validator: Address,
    pub recipient_x25519: B256,
    pub attestation_pub: B256,
    pub noise_static_pub: B256,
    pub mrenclave: B256,
    pub mrsigner: B256,
    pub isv_svn: u64,
    pub keys_hash: B256,
}

/// The one-time bootstrap payload written into the registry by the
/// `TeeBootstrap` system transaction (Phase 3b). The system-tx native
/// handler validates the payload (signatures, policy, committee match) before
/// calling [`TeeRegistry::write_bootstrap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeeBootstrapData {
    pub tribute_offer_public_key: B256,
    pub policy_hash: B256,
    pub key_epoch: u64,
    pub tribute_offer_epoch: u64,
    pub dkg_transcript_hash: B256,
    pub committee_snapshot_block: u64,
    pub committee_snapshot_hash: B256,
    /// Encoded DKG group public key (constant term) of the bootstrapping committee.
    /// The verification key for reshare endorsements; stored chunked on-chain.
    pub tribute_offer_group_public_key: alloy_primitives::Bytes,
    pub registrations: Vec<TeeRegistration>,
}

impl TeeRegistry<'_> {
    /// True once the registry has been bootstrapped.
    pub fn is_bootstrapped(&self) -> Result<bool> {
        self.bootstrapped.read()
    }

    /// The tribute offer public key clients encrypt to (zero until bootstrap).
    pub fn offer_public_key(&self) -> Result<B256> {
        self.tribute_offer_public_key.read()
    }

    /// Current TEE key epoch committed by OST3 and later canonical lifecycle
    /// artifacts.
    pub fn key_epoch(&self) -> Result<u64> {
        self.key_epoch.read()
    }

    /// The current tribute-offer epoch (slot 4). The enclave derives the resident
    /// offer key for this epoch from `group_sig`; `0` until an offer-key rotation
    /// advances it. Bound into one-time registry onboarding ingestion.
    pub fn tribute_offer_epoch(&self) -> Result<u64> {
        self.tribute_offer_epoch.read()
    }

    /// The policy hash committed by the mandatory block-1 OST3 payload. The active
    /// canonical V1 policy is installed before bootstrap and zero is never an
    /// accepted production authority.
    pub fn policy_hash(&self) -> Result<B256> {
        self.policy_hash.read()
    }

    /// Read a validator's full registration bundle.
    pub fn registration(&self, validator: Address) -> Result<TeeRegistration> {
        Ok(TeeRegistration {
            validator,
            recipient_x25519: self.recipient_x25519.read(&validator)?,
            attestation_pub: self.attestation_pub.read(&validator)?,
            noise_static_pub: self.noise_static_pub.read(&validator)?,
            mrenclave: self.mrenclave.read(&validator)?,
            mrsigner: self.mrsigner.read(&validator)?,
            isv_svn: self.isv_svn.read(&validator)?,
            keys_hash: self.keys_hash.read(&validator)?,
        })
    }

    /// Record the recipient X25519 pubkeys announced by a `BoundaryOutcome`
    /// (`DkgBoundaryArtifact::tee_recipient_pubkeys`). Latest announcement wins
    /// (key rotation). Called from the boundary system-tx handler; the keys ride
    /// in the hash-committed block artifact, so every validator records the same
    /// ordered set deterministically. A `B256::ZERO` key clears the announcement.
    pub fn record_boundary_recipient_keys(&mut self, keys: &[(Address, B256)]) -> Result<()> {
        for (validator, recipient_x25519) in keys {
            self.announced_recipient_x25519
                .write(validator, *recipient_x25519)?;
        }
        Ok(())
    }

    /// Read a validator's boundary-announced recipient X25519 pubkey
    /// (`B256::ZERO` if none has been announced).
    pub fn announced_recipient_key(&self, validator: Address) -> Result<B256> {
        self.announced_recipient_x25519.read(&validator)
    }

    /// Store the active committee's DKG group public key (constant term), chunked
    /// into 32-byte words plus a byte length. The reshare endorsement verify reads
    /// it back via [`Self::prior_group_public_key`]. Written at bootstrap and updated
    /// on each reshare activation so the NEXT reshare verifies against this set.
    pub fn set_group_public_key(&mut self, bytes: &[u8]) -> Result<()> {
        let len = u32::try_from(bytes.len())
            .map_err(|_| PrecompileError::Revert("group public key too large".to_string()))?;
        self.group_public_key_len.write(len)?;
        for (i, chunk) in bytes.chunks(32).enumerate() {
            let mut word = [0u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            let idx = u32::try_from(i)
                .map_err(|_| PrecompileError::Revert("group public key too large".to_string()))?;
            self.group_public_key.write(&idx, B256::from(word))?;
        }
        Ok(())
    }

    /// The active committee's stored group public key bytes (empty until set). The
    /// verification key for a prior-committee reshare endorsement.
    pub fn prior_group_public_key(&self) -> Result<Vec<u8>> {
        let len = self.group_public_key_len.read()? as usize;
        if len == 0 {
            return Ok(Vec::new());
        }
        let words = len.div_ceil(32);
        let mut out = Vec::with_capacity(words * 32);
        for i in 0..words {
            let idx = u32::try_from(i).map_err(|_| {
                PrecompileError::Revert("group public key index overflow".to_string())
            })?;
            out.extend_from_slice(self.group_public_key.read(&idx)?.as_slice());
        }
        out.truncate(len);
        Ok(out)
    }

    /// Write the one-time bootstrap result.
    ///
    /// Native-only: the `TeeBootstrap` system-tx handler calls this
    /// through `StorageHandle::contract` after full validation. Idempotency is
    /// enforced here as a defense in depth — a second bootstrap is rejected even
    /// if the system-tx ordering guard is bypassed.
    pub fn write_bootstrap(&mut self, data: &TeeBootstrapData) -> Result<()> {
        if self.bootstrapped.read()? {
            return Err(PrecompileError::Revert(
                "TEE registry already bootstrapped".to_string(),
            ));
        }

        let count = u32::try_from(data.registrations.len())
            .map_err(|_| PrecompileError::Revert("too many TEE registrations".to_string()))?;

        self.tribute_offer_public_key
            .write(data.tribute_offer_public_key)?;
        self.policy_hash.write(data.policy_hash)?;
        self.key_epoch.write(data.key_epoch)?;
        self.tribute_offer_epoch.write(data.tribute_offer_epoch)?;
        self.dkg_transcript_hash.write(data.dkg_transcript_hash)?;
        self.committee_snapshot_block
            .write(data.committee_snapshot_block)?;
        self.committee_snapshot_hash
            .write(data.committee_snapshot_hash)?;
        self.set_group_public_key(&data.tribute_offer_group_public_key)?;

        for reg in &data.registrations {
            self.recipient_x25519
                .write(&reg.validator, reg.recipient_x25519)?;
            self.attestation_pub
                .write(&reg.validator, reg.attestation_pub)?;
            self.noise_static_pub
                .write(&reg.validator, reg.noise_static_pub)?;
            self.mrenclave.write(&reg.validator, reg.mrenclave)?;
            self.mrsigner.write(&reg.validator, reg.mrsigner)?;
            self.isv_svn.write(&reg.validator, reg.isv_svn)?;
            self.keys_hash.write(&reg.validator, reg.keys_hash)?;
        }

        self.registered_count.write(count)?;
        self.bootstrapped.write(true)?;
        Ok(())
    }

    /// Re-register the new committee's per-validator enclave keys after a
    /// tribute-offer reshare (R5). Each entry is
    /// `(validator, recipient_x25519, attestation_pub, noise_static_pub)`. The
    /// offer key is PRESERVED across a reshare, so the offer-key / bootstrapped /
    /// policy / snapshot slots are NOT touched — only the rotating per-validator
    /// enclave keys. Native-only: called from the begin-zone `BoundaryOutcome`
    /// handler after the artifact is validated.
    pub fn record_reshare_registrations(
        &mut self,
        registrations: &[(Address, B256, B256, B256)],
    ) -> Result<()> {
        for (validator, recipient_x25519, attestation_pub, noise_static_pub) in registrations {
            self.recipient_x25519.write(validator, *recipient_x25519)?;
            self.attestation_pub.write(validator, *attestation_pub)?;
            self.noise_static_pub.write(validator, *noise_static_pub)?;
        }
        Ok(())
    }
}
