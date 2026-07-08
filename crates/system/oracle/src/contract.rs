use alloy_primitives::{Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_macros::contract;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::storage::types::{Mapping, Slot, StorageBytes, StorageVec};
pub use outbe_primitives::units::SCALE_1E18;

/// EVM storage layout for the Oracle contract.
///
/// Manages exchange rates, validator voting, price snapshots, and VWAP
/// calculation for whitelisted trading pairs.
///
/// All prices and volumes use U256 with 1e18 scale factor.
/// Pair identification uses `pair_hash = keccak256("BASE/QUOTE")`.
#[contract(addr = ORACLE_ADDRESS)]
pub struct OracleContract {
    // === Config (slots 0-7) ===
    // slot 0: vote period in blocks (default: 2)
    pub config_vote_period: Slot<u64>,
    // slot 1: reward band (1e18 scaled, default: 0.02 * 1e18 = 2e16)
    pub config_reward_band: Slot<U256>,
    // slot 2: slash window in blocks (default: 96)
    pub config_slash_window: Slot<u64>,
    // slot 3: min valid per window (1e18 scaled, default: 0.05 * 1e18 = 5e16)
    pub config_min_valid_per_window: Slot<U256>,
    // slot 4: slash fraction (1e18 scaled, default: 0)
    pub config_slash_fraction: Slot<U256>,
    // slot 5: lookback duration in seconds (default: 86400)
    pub config_lookback_duration: Slot<u64>,
    // slot 6: oracle enabled flag
    pub config_enabled: Slot<bool>,
    // slot 7: genesis guard
    pub config_is_initialized: Slot<bool>,

    // === Pair Registry (slots 8-11) ===
    // slot 8: number of registered pairs (1-indexed: pair_id starts at 1)
    pub pair_count: Slot<u32>,
    // slot 9: mapping(pair_id => pair_hash) where pair_hash = keccak256("BASE/QUOTE")
    pub pair_id_to_hash: Mapping<u32, B256>,
    // slot 10: mapping(pair_hash => pair_id) (0 = not registered)
    pub pair_hash_to_id: Mapping<B256, u32>,
    // slot 11: mapping(pair_hash => is_vote_target)
    pub vote_target: Mapping<B256, bool>,

    // === Exchange Rates (slots 12-14) ===
    // slot 12: mapping(pair_hash => exchange_rate) 1e18 scaled
    pub exchange_rate: Mapping<B256, U256>,
    // slot 13: mapping(pair_hash => last_update_block)
    pub exchange_rate_block: Mapping<B256, u64>,
    // slot 14: mapping(pair_hash => last_update_timestamp)
    pub exchange_rate_timestamp: Mapping<B256, u64>,

    // === Feeder Delegation (slot 15) ===
    // slot 15: mapping(validator_address => feeder_address)
    // Address::ZERO means self-delegation (validator is its own feeder)
    pub feeder_delegation: Mapping<Address, Address>,

    // === Vote Penalty Counters (slots 16-18) ===
    // slot 16: mapping(validator => success_count)
    pub penalty_success_count: Mapping<Address, u64>,
    // slot 17: mapping(validator => abstain_count)
    pub penalty_abstain_count: Mapping<Address, u64>,
    // slot 18: mapping(validator => miss_count)
    pub penalty_miss_count: Mapping<Address, u64>,

    // === Aggregate Votes (slots 19-24) ===
    // slot 19: mapping(validator => has_voted_this_period)
    pub vote_exists: Mapping<Address, bool>,
    // slot 20: mapping(validator => number_of_tuples)
    pub vote_tuple_count: Mapping<Address, u32>,
    // slot 21: mapping(validator => mapping(tuple_idx => pair_id))
    pub vote_pair_id: Mapping<Address, Mapping<u32, u32>>,
    // slot 22: mapping(validator => mapping(tuple_idx => rate))
    pub vote_rate: Mapping<Address, Mapping<u32, U256>>,
    // slot 23: mapping(validator => mapping(tuple_idx => volume))
    pub vote_volume: Mapping<Address, Mapping<u32, U256>>,

    // === Voter Tracking (slot 24) ===
    // slot 24: dynamic array of voter addresses (length at slot 24, data at keccak256(24))
    pub voter_list: StorageVec<Address>,

    // === Price Snapshots — circular buffer (slots 26-31) ===
    // slot 26: monotonic write index (tail pointer, u64 to avoid wrapping)
    pub snapshot_write_idx: Slot<u64>,
    // slot 27: oldest valid index (head pointer)
    pub snapshot_oldest_idx: Slot<u64>,
    // slot 28: mapping(snapshot_idx => timestamp)
    pub snapshot_timestamp: Mapping<u64, u64>,
    // slot 29: mapping(snapshot_idx => pair_count in this snapshot)
    pub snapshot_pair_count: Mapping<u64, u32>,
    // slot 30: mapping(snapshot_idx => mapping(pair_index => pair_id))
    pub snapshot_pair_id: Mapping<u64, Mapping<u32, u32>>,
    // slot 31: mapping(snapshot_idx => mapping(pair_index => rate))
    pub snapshot_rate: Mapping<u64, Mapping<u32, U256>>,
    // slot 32: mapping(snapshot_idx => mapping(pair_index => volume))
    pub snapshot_volume: Mapping<u64, Mapping<u32, U256>>,

    // === Protected Validators (slots 33-34) ===
    // slot 33: mapping(validator => is_protected)
    pub protected_validator: Mapping<Address, bool>,
    // slot 34: allow protected validators flag
    pub config_allow_protected: Slot<bool>,

    // === S-Curve Active Entries (slots 35-39) ===
    // slot 35: number of active S-curve entries
    pub scurve_count: Slot<u32>,
    // slot 36: mapping(entry_idx => pair_id) for which pair
    pub scurve_pair_id: Mapping<u32, u32>,
    // slot 37: mapping(entry_idx => peak_day_timestamp) UTC midnight
    pub scurve_peak_day: Mapping<u32, u64>,
    // slot 38: mapping(entry_idx => peak_price) 1e18 scaled
    pub scurve_peak_price: Mapping<u32, U256>,
    // slot 39: oldest non-evicted entry index (head pointer for cleanup)
    pub scurve_oldest_idx: Slot<u32>,
    // slot 40: last UTC day (truncated timestamp) when S-curve processing ran
    pub scurve_last_processed_day: Slot<u64>,

    // === Settlement Currencies (slots 41-43) ===
    // slot 41: number of registered settlement currencies
    pub settlement_count: Slot<u32>,
    // slot 42: mapping(iso_code => denom_hash) where denom_hash = keccak256(denom_string)
    pub settlement_iso_to_denom: Mapping<u16, B256>,
    // slot 43: mapping(iso_code => pair_hash) linking settlement currency to its trading pair
    pub settlement_iso_to_pair: Mapping<u16, B256>,

    // === Reversible Genesis Export Metadata ===
    // Pair and settlement runtime lookups remain hash-based, but export needs
    // the original strings to produce an importable OracleGenesisConfig.
    pub pair_id_to_base: Mapping<u32, StorageBytes>,
    pub pair_id_to_quote: Mapping<u32, StorageBytes>,
    pub settlement_index_to_iso: Mapping<u32, u16>,
    pub settlement_iso_to_denom_string: Mapping<u16, StorageBytes>,

    // === WorldwideDay VWAP Snapshots (slots 46-51) ===
    // slot 46: mapping(worldwide_day => exists)
    pub worldwide_day_vwap_exists: Mapping<WorldwideDay, bool>,
    // slot 47: mapping(worldwide_day => start_time)
    pub worldwide_day_vwap_start: Mapping<WorldwideDay, u64>,
    // slot 48: mapping(worldwide_day => end_time)
    pub worldwide_day_vwap_end: Mapping<WorldwideDay, u64>,
    // slot 49: mapping(worldwide_day => pair_count)
    pub worldwide_day_vwap_pair_count: Mapping<WorldwideDay, u32>,
    // slot 50: mapping(worldwide_day => mapping(index => pair_id))
    pub worldwide_day_vwap_pair_id: Mapping<WorldwideDay, Mapping<u32, u32>>,
    // slot 51: mapping(worldwide_day => mapping(index => vwap))
    pub worldwide_day_vwap_value: Mapping<WorldwideDay, Mapping<u32, U256>>,

    // === Daily Rolling VWAP Aggregates (slots 52-53) ===
    // Updated on every write_snapshot. Keyed by (pair_id, utc_day_timestamp).
    // utc_day_timestamp = timestamp - (timestamp % 86400).
    // slot 52: mapping(pair_id => mapping(utc_day_ts => cumulative price*volume sum))
    pub daily_pv_sum: Mapping<u32, Mapping<u64, U256>>,
    // slot 53: mapping(pair_id => mapping(utc_day_ts => cumulative volume sum))
    pub daily_vol_sum: Mapping<u32, Mapping<u64, U256>>,

    // === Reference Currencies ===
    // Dynamic list of ISO 4217 numeric codes considered "reference" currencies
    // for off-chain pricing. Length at the base slot, data at keccak256(slot)
    // + index. Pre-filled at genesis with [840] (USD).
    pub reference_currencies: StorageVec<u16>,

    // === Per-UTC-Day VWAP Snapshots ===
    // Finalized VWAP for a full UTC calendar day, keyed by a yyyymmdd UTC date
    // key (e.g. 20260625) — NOT a WorldwideDay (which is UTC+14). Written once
    // per closed day by the begin-block lifecycle from the canonical
    // `[date_key_to_utc_timestamp(utc_day), +SECONDS_PER_DAY)` window. Stored
    // forever (no pruning). A day with no oracle data is never written, so
    // `pair_count == 0` means "not finalized OR finalized-empty"; the three
    // states are disambiguated against `utc_day_vwap_last_finalized`.
    //
    // mapping(utc_day => number of (pair_id, vwap) entries finalized for the day)
    pub utc_day_vwap_pair_count: Mapping<u32, u32>,
    // mapping(utc_day => mapping(entry_idx => pair_id))
    pub utc_day_vwap_pair_id: Mapping<u32, Mapping<u32, u32>>,
    // mapping(utc_day => mapping(entry_idx => vwap)) 1e18 scaled
    pub utc_day_vwap_value: Mapping<u32, Mapping<u32, U256>>,
    // Monotonic watermark: most recent fully-closed UTC day that has been
    // finalized (yyyymmdd). 0 = nothing finalized yet. Backfill is contiguous,
    // so every day <= this watermark is considered finalized.
    pub utc_day_vwap_last_finalized: Slot<u32>,

    // Reference-currency refinancing rates
    pub reference_refinancing_rate: Mapping<u16, U256>,
}
