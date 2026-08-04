//! Test-only typed fixture operations for ValidatorSet.
//!
//! This module is deliberately feature-gated. Production callers must use the
//! named lifecycle commands in [`crate::runtime`], while cross-crate tests can
//! build otherwise unreachable system states without receiving raw storage-slot
//! handles or caller-selected numeric status bytes.

use alloy_primitives::{Address, B256};
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::ValidatorSet;
use crate::state_machine::{StakeProjection, ValidatorHistory, ValidatorLifecycle};
use crate::EpochSnapshot;

impl ValidatorSet<'_> {
    /// Replaces only the complete typed lifecycle dimension of a registered fixture.
    pub fn test_set_lifecycle(
        &mut self,
        address: Address,
        lifecycle: ValidatorLifecycle,
    ) -> Result<()> {
        if matches!(lifecycle.phase(), ValidatorLifecycle::Unregistered) {
            return Err(PrecompileError::Fatal(
                "test fixture cannot unregister without updating registry identity".into(),
            ));
        }
        let before = self.validator_state(address)?;
        if !before.is_registered() {
            return Err(PrecompileError::Fatal(
                "test fixture lifecycle requires a registered validator".into(),
            ));
        }
        let after = before.clone().with_lifecycle(lifecycle);
        self.persist_validator_state_delta(&before, &after)
    }

    /// Replaces the ValidatorSet stake mirror; Staking remains authoritative.
    pub fn test_set_stake_projection(
        &mut self,
        address: Address,
        stake: StakeProjection,
    ) -> Result<()> {
        let before = self.validator_state(address)?;
        let after = before.clone().with_stake_projection(stake);
        self.persist_validator_state_delta(&before, &after)
    }

    /// Replaces retained historical counters/heights for a registered fixture.
    pub fn test_set_history(&mut self, address: Address, history: ValidatorHistory) -> Result<()> {
        let before = self.validator_state(address)?;
        if !before.is_registered() {
            return Err(PrecompileError::Fatal(
                "test fixture history requires a registered validator".into(),
            ));
        }
        let after = before.clone().with_history(history);
        self.persist_validator_state_delta(&before, &after)
    }

    /// Replaces epoch metadata as one test fixture bundle.
    pub fn test_set_epoch_snapshot(&mut self, epoch: EpochSnapshot) -> Result<()> {
        self.epoch_number.write(epoch.number)?;
        self.epoch_start_timestamp.write(epoch.start_timestamp)?;
        self.epoch_start_block.write(epoch.start_block)?;
        self.config_epoch_length_blocks.write(epoch.length_blocks)
    }

    pub fn test_set_pending_set_change(&mut self, pending: bool) -> Result<()> {
        self.pending_set_change.write(pending)
    }

    pub fn test_set_active_consensus_set_hash(&mut self, hash: B256) -> Result<()> {
        self.active_consensus_set_hash.write(hash)
    }

    /// Deliberately installs malformed P2P storage for a fail-closed corruption
    /// test. Normal fixtures must use `set_p2p_address`.
    pub fn test_corrupt_p2p_storage(
        &mut self,
        address: Address,
        version: u8,
        payload: &[u8],
    ) -> Result<()> {
        self.val_p2p_address_version.write(&address, version)?;
        self.val_p2p_address_payload
            .get_bytes(&address)
            .write(payload)
    }
}
