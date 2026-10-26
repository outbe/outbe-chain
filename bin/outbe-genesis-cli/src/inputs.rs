//! Inputs shared by the genesis calculation and its operator stages.

use alloy_primitives::U256;
use serde_json::Value;

/// Options controlling how `apply_seed` and `seed_genesis_from_config` populate genesis state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedOptions {
    /// Directory containing external contract `.code.hex` and `.state.json` files.
    pub contracts_dir: Option<String>,
    /// Directory holding `metacanon.md` and `canon.md` for Governance slot seeding.
    pub canon_dir: Option<String>,
    /// Optional override for the active worldwide day (`YYYYMMDD`).
    pub worldwide_day: Option<u32>,
    /// When true, clears pre-seeded `metadosis.worldwide_days` so block 1 creates the first day.
    pub fresh_metadosis: bool,
}

/// Inputs to the genesis seeding stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedInputs {
    /// Base `genesis.json` object (`config`, `alloc`, `timestamp`, etc.).
    pub base_genesis: Value,
    /// Declarative protocol seed sections (`staking`, `oracle`, `balance`, etc.).
    pub seed: Value,
    /// Founding validator descriptors (`address`, `public_key`, `radicle_node_id`, `p2p_address`).
    pub validators: Vec<Value>,
}

/// Terms passed to `seed_validator_set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatorSeedTerms {
    /// Epoch length in blocks (`config.epochLengthBlocks`).
    pub epoch_length_blocks: u64,
    /// Genesis epoch start unix timestamp (`config.genesisTime`).
    pub epoch_start_timestamp: u64,
    /// Minimum stake in wei (`U256`).
    pub min_stake: U256,
    /// Per-founder initial bonded stake in wei (`U256`).
    pub validator_stake: U256,
}

/// Terms passed to `seed_staking`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StakeSeedTerms {
    /// Minimum stake in wei (`U256`).
    pub min_stake: U256,
    /// Per-founder initial bonded stake in wei (`U256`).
    pub validator_stake: U256,
}
