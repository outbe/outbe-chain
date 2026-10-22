use alloy_primitives::B256;
use outbe_primitives::error::Result;

use crate::{errors::TeeRegistryError, schema::TeeRegistry};

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
    /// Encoded DKG group public key (constant term) of the bootstrapping committee,
    /// stored chunked on-chain as part of the founding bootstrap record.
    pub tribute_offer_group_public_key: alloy_primitives::Bytes,
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
    /// offer key for this epoch from `group_sig`. The value is `0` until an
    /// offer-key rotation advances it. The one-time ingestion of registry
    /// onboarding binds this epoch.
    pub fn tribute_offer_epoch(&self) -> Result<u64> {
        self.tribute_offer_epoch.read()
    }

    /// The policy hash committed by the mandatory block-1 OST3 payload. The active
    /// canonical V1 policy is installed before bootstrap and zero is never an
    /// accepted production authority.
    pub fn policy_hash(&self) -> Result<B256> {
        self.policy_hash.read()
    }

    /// Store the founding committee's DKG group public key from the canonical
    /// bootstrap payload, chunked into 32-byte words plus a byte length.
    pub fn set_group_public_key(&mut self, bytes: &[u8]) -> Result<()> {
        let len =
            u32::try_from(bytes.len()).map_err(|_| TeeRegistryError::GroupPublicKeyTooLarge)?;
        self.group_public_key_len.write(len)?;
        for (i, chunk) in bytes.chunks(32).enumerate() {
            let mut word = [0u8; 32];
            word[..chunk.len()].copy_from_slice(chunk);
            let idx = u32::try_from(i).map_err(|_| TeeRegistryError::GroupPublicKeyTooLarge)?;
            self.group_public_key.write(&idx, B256::from(word))?;
        }
        Ok(())
    }

    /// Write the one-time bootstrap result.
    ///
    /// Native-only: the `TeeBootstrap` system-tx handler calls this
    /// through `StorageHandle::contract` after full validation. This function
    /// enforces idempotency as a defense in depth. It rejects a second bootstrap
    /// even if the system-tx ordering guard is bypassed.
    pub fn write_bootstrap(&mut self, data: &TeeBootstrapData) -> Result<()> {
        if self.bootstrapped.read()? {
            return Err(TeeRegistryError::AlreadyBootstrapped.into());
        }

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

        self.bootstrapped.write(true)?;
        Ok(())
    }
}
