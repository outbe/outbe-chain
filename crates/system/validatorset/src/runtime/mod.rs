//! The in-process lifecycle and accounting hooks listed below trust their Rust caller.
//!
//! `config_owner` does not gate these hooks. Staking, evidence, the oracle,
//! the executor, and the begin-block path call them after their own checks.
//! An external account reaches ValidatorSet only through `IValidatorSet` and
//! [`crate::precompile::dispatch`]. That route exposes the views plus
//! `setDelegate`, `revokeDelegate`, `setP2pAddress`,
//! `deactivateValidator`, and `confirmValidatorReady`.
//! `confirmValidatorReady` uses the transaction sender. `activateResharedSet`
//! is not an external selector.
//!
//! Production callers of the in-process methods:
//!
//! - `crates/system/staking/src/logic.rs` calls `register_with_initial_stake`,
//!   `record_stake_increase`, `record_unstake`, and `record_stake_slash`.
//! - `complete_unbonding` and `record_unstake` are called from
//!   `crates/system/staking/src/unbonding.rs`.
//! - `unjail_after_stake_check` is called from `Staking::unjail_validator` in
//!   `crates/system/staking/src/logic.rs`. The Staking precompile enters that
//!   function.
//! - `jail_validator` is called from SlashIndicator `runtime.rs` and
//!   `misses.rs`, from `crates/system/oracle/src/tally/slash_window.rs`, and
//!   from `crates/system/hyperlanecontroller/src/liveness.rs`.
//! - `jail_validator_deferred` is called from
//!   `crates/blockchain/evm/src/executor/hooks.rs`.
//! - `jail_validator_for_tee_expiry` is called from
//!   `enforce_tee_lease_deadlines` in
//!   `crates/blockchain/evm/src/begin_block_precompile/cycle.rs`. The call in
//!   `crates/blockchain/engine/src/validators.rs` is inside `#[cfg(test)]`
//!   tests.
//! - `cleanup_inactive_validators` is called from
//!   `crates/blockchain/evm/src/executor/boundary.rs` with a cap of 16.
//! - `activate_boundary_atomic`, `advance_epoch`, and `reset_epoch_counters`
//!   are called from `crates/blockchain/evm/src/executor/boundary.rs`.
//! - `record_proposer` is called from
//!   `crates/blockchain/evm/src/begin_block_precompile/cycle.rs` and
//!   `crates/blockchain/evm/src/begin_block_precompile/boundary.rs`.
//! - `record_finalized_participation` is called from
//!   `crates/blockchain/evm/src/begin_block_precompile/late_credits.rs`.
//!
//! `force_exit_validator`, `record_missed_block`, and `hooks::record_participation`
//! have no production caller.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};

use crate::errors::ValidatorSetError;
use crate::schema::ValidatorSet;
use crate::state_machine::ValidatorLifecycle;

mod boundary;
mod cleanup;
mod lifecycle;
mod participation;
mod queries;
mod readiness;
mod records;
mod registration;
mod staking;

#[cfg(any(test, feature = "test-utils"))]
mod test_support;

pub use lifecycle::DeferredValidatorPunishment;
pub use registration::decode_registration_key_and_proof;

/// Stable ABI status constants. The effective Rust states are richer: PENDING
/// distinguishes readiness from joining, and JAILED distinguishes retained from
/// boundary-excluded. See [`ValidatorLifecycle`].
pub mod status {
    pub const REGISTERED: u8 = 0;
    pub const PENDING: u8 = 1;
    pub const ACTIVE: u8 = 2;
    pub const EXITING: u8 = 3;
    pub const UNBONDING: u8 = 4;
    pub const INACTIVE: u8 = 5;
    pub const JAILED: u8 = 6;
}

/// Canonical committee/codec bound shared by every validator-registry scan.
/// This is not a configurable product capacity.
pub const CONSENSUS_VALIDATOR_BOUND: u32 = outbe_consensus::bls::MAX_VALIDATORS;

/// Legacy flat read/ABI projection.
///
/// Lifecycle decisions must use [`crate::state_machine::ValidatorState`] or [`ValidatorLifecycle`].
/// This shape remains public for compatibility with existing Rust consumers
/// and the Solidity `validatorByAddress` / `validatorByIndex` tuples.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatorRecord {
    pub validator_address: Address,
    /// 48-byte BLS MinPk consensus public key.
    pub consensus_pubkey: [u8; 48],
    pub stake: U256,
    pub status: u8,
    pub slash_count: u64,
    pub missed_blocks: u64,
    pub missed_votes: u64,
    pub blocks_proposed: u64,
    pub joined_at_height: u64,
    pub deactivated_at_height: u64,
    pub unbonding_end: u64,
    pub has_bls_share: bool,
}

impl ValidatorSet<'_> {
    /// Store the configured validator cap only when it fits the canonical
    /// consensus codec/committee bound.
    pub fn set_config_max_validators(&mut self, max_validators: u32) -> Result<()> {
        if max_validators > CONSENSUS_VALIDATOR_BOUND {
            return Err(ValidatorSetError::MaxValidatorsExceedsConsensusBound {
                max_validators,
                bound: CONSENSUS_VALIDATOR_BOUND,
            }
            .into());
        }
        self.config_max_validators.write(max_validators)
    }
}

/// Read-only epoch metadata exposed without leaking raw storage slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochSnapshot {
    pub number: U256,
    pub start_timestamp: u64,
    pub start_block: u64,
    pub length_blocks: u32,
}

/// Read-only participation counters exposed independently of lifecycle writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidatorParticipation {
    pub blocks_proposed: u64,
    pub missed_blocks: u64,
    pub missed_votes: u64,
}

fn registered_status(lifecycle: &ValidatorLifecycle) -> Result<u8> {
    lifecycle
        .stored_status()
        .ok_or_else(|| PrecompileError::from(ValidatorSetError::UnregisteredLifecycleNoStatus))
}
