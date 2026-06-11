use alloy_primitives::{keccak256, Address, B256};
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::TeeRegistry;

/// Domain label binding the per-validator `keys_hash` (mirrors the bootstrap
/// bundle's `outbe/tee/keys/v1`), so a mid-chain `registerEnclave` produces the
/// same `keysHash(addr)` shape as the block-1 bootstrap registration.
const TEE_KEYS_HASH_DOMAIN: &[u8] = b"outbe/tee/keys/v1";

/// `keccak256(domain ‖ validator ‖ recipient ‖ attestation ‖ noise ‖ mrenclave ‖
/// mrsigner ‖ isv_svn_be)` — binds all of a validator's enclave key material.
fn compute_keys_hash(
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

    /// The current tribute-offer epoch (slot 4). The enclave derives the resident
    /// offer key for this epoch from `group_sig`; `0` until an offer-key rotation
    /// advances it. Read by the key-handoff so a newcomer derives the right epoch.
    pub fn tribute_offer_epoch(&self) -> Result<u64> {
        self.tribute_offer_epoch.read()
    }

    /// The genesis-seeded TEE policy hash (`TeePolicy::compute_hash`), read from
    /// slot 2. `B256::ZERO` means no policy was seeded at genesis, so Phase 3b
    /// skips measurement enforcement (backward-compatible). After bootstrap this
    /// slot holds the verified policy hash the bootstrap committed to.
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

    /// On-chain attestation verification for a mid-chain `registerEnclave` call.
    ///
    /// A node submits its enclave keys + attestation quote and the chain verifies
    /// the RA proof on-chain. The
    /// real verification — DCAP signature + `REPORT_DATA` key binding + measurement
    /// allowlist (genesis `teePolicy`) + caller ∈ active validator set — is **NOT
    /// YET wired** (same posture as the dev `dev_accept_any` policy + the `dcap`
    /// feature gate). For now it ACCEPTS any registration. The whole registration
    /// mechanism is real; only this gate is a stub to fill in later.
    ///
    // TODO(tee): replace the stub body with real on-chain attestation verification.
    #[allow(clippy::too_many_arguments)]
    fn verify_enclave_registration(
        &self,
        _caller: Address,
        _recipient_x25519: B256,
        _attestation_pub: B256,
        _noise_static_pub: B256,
        _mrenclave: B256,
        _mrsigner: B256,
        _isv_svn: u16,
    ) -> Result<bool> {
        Ok(true)
    }

    /// Mid-chain enclave registration: a validator records its enclave keys
    /// on-chain (canonical committee record + handoff binding). Verifies the
    /// attestation via [`Self::verify_enclave_registration`] (currently a stub that
    /// accepts), then writes the per-validator slots and counts a first-time
    /// registrant. A re-registration overwrites (key rotation) and does not
    /// double-count. Returns `true` on success.
    #[allow(clippy::too_many_arguments)]
    pub fn register_enclave(
        &mut self,
        caller: Address,
        recipient_x25519: B256,
        attestation_pub: B256,
        noise_static_pub: B256,
        mrenclave: B256,
        mrsigner: B256,
        isv_svn: u16,
    ) -> Result<bool> {
        if !self.verify_enclave_registration(
            caller,
            recipient_x25519,
            attestation_pub,
            noise_static_pub,
            mrenclave,
            mrsigner,
            isv_svn,
        )? {
            return Err(PrecompileError::Revert(
                "enclave registration attestation verification failed".to_string(),
            ));
        }
        let first_time = self.recipient_x25519.read(&caller)?.is_zero();
        let keys_hash = compute_keys_hash(
            caller,
            recipient_x25519,
            attestation_pub,
            noise_static_pub,
            mrenclave,
            mrsigner,
            isv_svn,
        );
        self.recipient_x25519.write(&caller, recipient_x25519)?;
        self.attestation_pub.write(&caller, attestation_pub)?;
        self.noise_static_pub.write(&caller, noise_static_pub)?;
        self.mrenclave.write(&caller, mrenclave)?;
        self.mrsigner.write(&caller, mrsigner)?;
        self.isv_svn.write(&caller, u64::from(isv_svn))?;
        self.keys_hash.write(&caller, keys_hash)?;
        if first_time {
            let count = self.registered_count.read()?.saturating_add(1);
            self.registered_count.write(count)?;
        }

        // On-chain offer-key delivery: on a TEE-bootstrapped
        // chain, deterministically seal the resident tribute offer key to the
        // registrant's recipient X25519 key (inside the enclave) and EMIT it as an
        // `OfferKeySealed` event, so the joining validator reads the blob from THIS
        // tx's receipt and installs the offer key in its enclave before its node
        // starts executing offer blocks. Skipped when the chain has no offer key yet
        // (non-TEE chain) or this node has no enclave configured (unit tests). The
        // seal is deterministic, so every committee enclave emits the same log and
        // the receipts root agrees; a TEE node without a working enclave cannot
        // execute offer blocks anyway, so its divergence here is its own fault.
        if !self.tribute_offer_public_key.read()?.is_zero() {
            match outbe_tee::seal_offer_key_for_registry(recipient_x25519.0) {
                Ok(Some(sealed)) => {
                    self.emit(crate::precompile::OfferKeySealed {
                        validator: caller,
                        sealedOfferKey: sealed.into(),
                    })?;
                }
                Ok(None) => {
                    // No enclave on this node (unit test / non-TEE) — nothing to seal.
                }
                Err(e) => {
                    return Err(PrecompileError::Revert(format!(
                        "offer-key seal for registration failed: {e}"
                    )))
                }
            }
        }
        Ok(true)
    }
}
