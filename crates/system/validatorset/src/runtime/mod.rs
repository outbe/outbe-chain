use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};

use crate::schema::ValidatorSet;
use crate::state_machine::ValidatorLifecycle;

mod boundary;
mod cleanup;
mod lifecycle;
mod ocomp_recovery;
mod participation;
mod readiness;
mod records;
mod registration;
mod staking;

#[cfg(any(test, feature = "test-utils"))]
mod test_support;

pub use lifecycle::DeferredValidatorPunishment;
pub use ocomp_recovery::{OcompMissRecord, OcompRecoveryOutcome, OcompRecoveryWindow};

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

/// maximum number of validators that may be in the `REGISTERED`
/// (self-registered, not-yet-staked) state at once.
///
/// `REGISTERED` self-registration is permissionless and free on the ZeroFee
/// chain, and a `REGISTERED` node is intentionally admitted to the consensus
/// P2P secondary tier so a TEE verifier full-node can sync and execute offer
/// blocks before staking (see
/// [`ValidatorSet::get_admitted_non_consensus_validators`]). That admission is
/// by design, but without a bound an attacker can self-register up to
/// `config_max_validators` free Sybil identities - consuming registration slots
/// (griefing legitimate staked joins with "max validators reached") and
/// consensus-P2P connection / handshake / decode slots. This caps the unstaked
/// self-registration surface well below `config_max_validators` (default 128),
/// so legitimate verifiers (few) still register while Sybils cannot fill the
/// validator set. The owner (`config_owner`) is NOT subject to this cap and may
/// register validators directly beyond it.
pub const MAX_SELF_REGISTERED_UNSTAKED: u32 = 32;

/// Canonical committee/codec bound shared by every validator-registry scan.
/// This is not a configurable product capacity.
pub const CONSENSUS_VALIDATOR_BOUND: u32 = outbe_consensus::bls::MAX_VALIDATORS;

/// One day at the protocol's two-second block target. This is a recovery
/// deadline, not a polling interval: the recovery sweep runs every block.
pub const OCOMP_RECOVERY_WINDOW_BLOCKS: u64 = 43_200;

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
    /// consensus codec/committee bound. OCOMP reads this consensus limit and
    /// does not define a second participant ceiling.
    pub fn set_config_max_validators(&mut self, max_validators: u32) -> Result<()> {
        if max_validators > CONSENSUS_VALIDATOR_BOUND {
            return Err(PrecompileError::Revert(format!(
                "max validators exceeds consensus bound: {max_validators} > {}",
                CONSENSUS_VALIDATOR_BOUND
            )));
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
    lifecycle.stored_status().ok_or_else(|| {
        PrecompileError::Fatal("Unregistered lifecycle has no persisted status".into())
    })
}
