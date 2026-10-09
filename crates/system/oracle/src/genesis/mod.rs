//! Genesis import/export for the Oracle contract.
//!
//! Owns the `OracleGenesisConfig` shape plus the `init_from_genesis` /
//! `export_genesis` round-trip used for chain bootstrap and state migration.

mod export;
mod import;
mod validate;

use crate::constants::{DAY_TYPE_ISO, DEFAULT_USD_CURRENCY_RATE};
use crate::types::AssetType;
use alloy_primitives::{Address, U256};
use outbe_primitives::asset_type::COEN_ASSET;

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
    /// UTC midnight timestamp of the peak day.
    pub peak_day: u64,
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

/// An independently registered annual policy rate for one ISO 4217 currency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyRate {
    /// ISO 4217 numeric code (e.g., 840 = USD).
    pub iso_code: u16,
    /// Annualized rate at scale `1e6` (e.g., 0.043 -> 43_000).
    pub annual_rate_1e6: U256,
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
    /// Sorted unique ISO codes valid as pricing reference currencies.
    pub reference_currencies: Vec<u16>,
    /// Sorted unique annual policy rates, independent from references and pairs.
    pub policy_rates: Vec<PolicyRate>,
    /// Penalty counters as `(validator, success, abstain, miss)`.
    pub penalty_counters: Vec<(Address, u64, u64, u64)>,
    /// Pending aggregate votes that have not yet been tallied.
    pub aggregate_votes: Vec<GenesisAggregateVote>,
    /// Price snapshots for the circular buffer.
    pub snapshots: Vec<GenesisSnapshot>,
    /// Active S-curve entries.
    pub scurve_entries: Vec<GenesisScurveEntry>,
    /// Validators protected from slashing.
    pub protected_validators: Vec<Address>,
}

impl OracleGenesisConfig {
    /// Returns the config that matches the current hard-coded genesis values.
    pub fn default_config() -> Self {
        Self {
            vote_period: 2,
            reward_band: U256::from(20_000_000_000_000_000u128), // 0.02
            slash_window: 96,
            min_valid_per_window: U256::from(50_000_000_000_000_000u128), // 0.05
            slash_fraction: U256::ZERO,
            lookback_duration: 86400,
            pairs: vec![(COEN_ASSET, AssetType::IsoCurrency(DAY_TYPE_ISO).into())],
            initial_rates: vec![],
            feeder_delegations: vec![],
            reference_currencies: vec![156, 344, 392, 826, 840, 978],
            policy_rates: vec![PolicyRate {
                iso_code: 840,
                annual_rate_1e6: DEFAULT_USD_CURRENCY_RATE,
            }],
            penalty_counters: vec![],
            aggregate_votes: vec![],
            snapshots: vec![],
            scurve_entries: vec![],
            protected_validators: vec![],
        }
    }
}
