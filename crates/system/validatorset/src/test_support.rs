//! Test-only typed fixture operations for ValidatorSet.
//!
//! This module is deliberately feature-gated. Production callers must use the
//! named lifecycle commands in [`crate::runtime`], while cross-crate tests use
//! semantic fixture seams without receiving raw storage-slot handles,
//! caller-selected numeric status bytes, or lifecycle payload constructors.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::ValidatorSet;
use crate::state_machine::{self, StakeProjection, ValidatorHistory, ValidatorLifecycle};
use crate::EpochSnapshot;

impl ValidatorSet<'_> {
    /// Registers a fixture through the explicit bootstrap-only no-PoP seam.
    ///
    /// The underlying registration path is unavailable in production builds;
    /// ordinary callers must submit a valid proof of possession.
    pub fn test_register_validator_without_pop(
        &mut self,
        validator: Address,
        consensus_pubkey: &[u8; 48],
    ) -> Result<()> {
        let owner = self.config_owner.read()?;
        self.register_validator(owner, validator, consensus_pubkey)
    }

    /// Moves a registered fixture through the canonical typed join path.
    ///
    /// This intentionally does not expose constructors for lifecycle payloads:
    /// identity, P2P data and history are carried forward from the current
    /// registered state by the real transition functions.
    pub fn test_activate_validator_canonically(
        &mut self,
        address: Address,
        stake: StakeProjection,
        minimum: U256,
    ) -> Result<()> {
        if stake.bonded() < minimum {
            return Err(PrecompileError::Revert(format!(
                "fixture activation stake {} is below minimum {minimum}",
                stake.bonded()
            )));
        }

        let before = self.validator_state(address)?;
        let with_stake = state_machine::with_stake(before.lifecycle().clone(), stake)?;
        let active = match with_stake {
            ValidatorLifecycle::WaitingForStake(waiting) => {
                let waiting = state_machine::reach_minimum(waiting, stake, minimum)?;
                state_machine::activate_at_boundary(state_machine::confirm_ready(waiting))
            }
            ValidatorLifecycle::WaitingForReadiness(waiting) => {
                state_machine::activate_at_boundary(state_machine::confirm_ready(waiting))
            }
            ValidatorLifecycle::Joining(joining) => state_machine::activate_at_boundary(joining),
            ValidatorLifecycle::Active(active) => active,
            lifecycle => {
                return Err(PrecompileError::Revert(format!(
                    "fixture activation requires a join-path lifecycle, got status {:?}",
                    lifecycle.stored_status()
                )));
            }
        };
        let after = before
            .clone()
            .with_lifecycle(ValidatorLifecycle::Active(active))?;
        self.persist_validator_state_delta(&before, &after)
    }

    /// Calls the consensus-validated boundary seam from a test without
    /// exposing it in production or falling back to the legacy owner path.
    pub fn test_activate_validated_boundary_set(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
        freeze_height: u64,
    ) -> Result<()> {
        self.activate_validated_boundary_set(new_active_set, active_set_hash, freeze_height)
    }

    /// Replaces the ValidatorSet stake mirror; Staking remains authoritative.
    pub fn test_set_stake_projection(
        &mut self,
        address: Address,
        stake: StakeProjection,
    ) -> Result<()> {
        let before = self.validator_state(address)?;
        let lifecycle = state_machine::with_stake(before.lifecycle().clone(), stake)?;
        let after = before.clone().with_lifecycle(lifecycle)?;
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
        let lifecycle = state_machine::with_history(before.lifecycle().clone(), history)?;
        let after = before.clone().with_lifecycle(lifecycle)?;
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
