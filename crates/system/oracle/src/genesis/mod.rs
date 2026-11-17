//! Genesis import/export for the Oracle contract.
//!
//! Owns the `OracleGenesisConfig` shape plus the `init_from_genesis` /
//! `export_genesis` round-trip used for chain bootstrap and state migration.

mod export;
mod import;
mod validate;

use crate::constants::{DAY_TYPE_ISO, REFERENCE_CURRENCIES};
use crate::types::AssetType;
use alloy_primitives::{Address, U256};
use outbe_system_common::asset_type::COEN_ASSET;
use outbe_system_common::time::SECONDS_PER_DAY;

pub use export::export_genesis;
pub use import::init_from_genesis;

/// A price snapshot entry for genesis import/export.
#[derive(Clone, Debug)]
pub struct GenesisSnapshot {
    /// Unix timestamp of the snapshot.
    pub timestamp: u64,
    /// Entries as `(base, quote, rate, volume)` in each pair's registered scale.
    pub entries: Vec<(Address, Address, U256, U256)>,
}

/// An S-curve entry for genesis import/export.
#[derive(Clone, Debug)]
pub struct GenesisScurveEntry {
    /// The pair this peak belongs to, quoted as registered.
    pub base: Address,
    /// Quote asset of the pair.
    pub quote: Address,
    /// The whole UTC hour the curve starts at, in Unix seconds.
    pub start_hour: u64,
    /// Peak price in the COEN/840 S-Curve's six-decimal scale.
    pub peak_price: U256,
}

/// A pending aggregate vote for genesis import/export.
#[derive(Clone, Debug)]
pub struct GenesisAggregateVote {
    /// Validator address that owns this pending vote.
    pub validator: Address,
    /// Entries as `(base, quote, rate, volume)` in each pair's registered scale.
    pub entries: Vec<(Address, Address, U256, U256)>,
}

/// Configurable genesis parameters for the Oracle contract.
///
/// Dimensionless policy fields retain FP18. COEN/ISO pair rates, volumes and
/// snapshots use six decimals. The COEN/840 S-Curve uses the same rate scale 1e6.
/// Generic non-ISO pair data retains its existing contract.
pub struct OracleGenesisConfig {
    /// Vote period in blocks (default: 2).
    pub vote_period: u64,
    /// Reward band width (1e18 scaled, default: 0.02 * 1e18 = 2e16).
    pub reward_band: U256,
    /// Reward band width of the gas price votes (1e18 scaled, default: 0.2).
    pub gas_reward_band: U256,
    /// Slash window in blocks (default: 96).
    pub slash_window: u64,
    /// Minimum valid-vote ratio per window (1e18 scaled, default: 0.05 * 1e18 = 5e16).
    pub min_valid_per_window: U256,
    /// Slash fraction (1e18 scaled, default: 0).
    pub slash_fraction: U256,
    /// Lookback duration in seconds for VWAP (default: 86400).
    pub lookback_duration: u64,
    /// Trading pairs to register at genesis as `(base, quote)` asset addresses.
    /// The direction given here is the direction reads must be quoted in.
    pub pairs: Vec<(Address, Address)>,
    /// Initial exchange rates as `(base, quote, rate)` in each pair's scale.
    pub initial_rates: Vec<(Address, Address, U256)>,
    /// Feeder delegations as `(validator, feeder)`.
    pub feeder_delegations: Vec<(Address, Address)>,
    /// Currencies that are policy rate vote targets, as ISO 4217 numeric
    /// codes. Each reference currency must be in the list. Genesis gives no
    /// rate: the first rate of a currency comes from the first tally.
    pub policy_rate_currencies: Vec<u16>,
    /// Penalty counters as `(validator, success, abstain, miss)`.
    pub penalty_counters: Vec<(Address, u64, u64, u64)>,
    /// Pending aggregate votes that have not yet been tallied.
    pub aggregate_votes: Vec<GenesisAggregateVote>,
    /// Price snapshots for the snapshot log.
    pub snapshots: Vec<GenesisSnapshot>,
    /// Active S-curve entries.
    pub scurve_entries: Vec<GenesisScurveEntry>,
}

impl OracleGenesisConfig {
    /// Returns the config that matches the current hard-coded genesis values.
    pub fn default_config() -> Self {
        Self {
            vote_period: 2,
            reward_band: U256::from(20_000_000_000_000_000u128), // 0.02
            gas_reward_band: U256::from(200_000_000_000_000_000u128), // 0.2
            slash_window: 96,
            min_valid_per_window: U256::from(50_000_000_000_000_000u128), // 0.05
            slash_fraction: U256::ZERO,
            lookback_duration: SECONDS_PER_DAY,
            pairs: vec![(COEN_ASSET, AssetType::IsoCurrency(DAY_TYPE_ISO).into())],
            initial_rates: vec![],
            feeder_delegations: vec![],
            policy_rate_currencies: REFERENCE_CURRENCIES.to_vec(),
            penalty_counters: vec![],
            aggregate_votes: vec![],
            snapshots: vec![],
            scurve_entries: vec![],
        }
    }
}
