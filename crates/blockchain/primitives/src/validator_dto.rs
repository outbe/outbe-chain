//! Neutral validator-set records shared by consensus and execution crates.

use alloy_primitives::{Address, U256};

/// Legacy flat read/ABI projection of one registered validator.
///
/// Lifecycle decisions use the validator state machine. This shape stays public
/// for existing Rust consumers and the Solidity validator tuples.
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

/// Read-only epoch metadata exposed without raw storage slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochSnapshot {
    pub number: U256,
    pub start_timestamp: u64,
    pub start_block: u64,
    pub length_blocks: u32,
}

/// Read-only participation counters, independent of lifecycle writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidatorParticipation {
    pub blocks_proposed: u64,
    pub missed_blocks: u64,
    pub missed_votes: u64,
}

/// `vrf_material_version` reached `u64::MAX` and cannot increment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("vrf material version overflow")]
pub struct VrfVersionOverflow;

/// Returns the next `vrf_material_version` after a successful reshare activation.
///
/// The version increments by exactly 1 and never saturates. Overflow is a typed
/// error so two DKG outputs cannot share a version.
pub const fn next_vrf_material_version(previous: u64) -> Result<u64, VrfVersionOverflow> {
    match previous.checked_add(1) {
        Some(next) => Ok(next),
        None => Err(VrfVersionOverflow),
    }
}
