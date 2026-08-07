use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_macros::contract;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::storage::types::{Mapping, Slot, StorageVec};
pub use outbe_primitives::units::SCALE_1E18;

/// EVM storage layout for the Oracle contract.
///
/// Manages exchange rates, validator voting, price snapshots, and VWAP
/// calculation for whitelisted trading pairs.
///
/// All prices and volumes use U256 with 1e18 scale factor.
/// A pair is identified by its two asset addresses packed ascending into an
/// [`AddressPair`]; `pair_ordinal` maps that key onto a dense 1-based index that
/// exists only so the registry can be enumerated.
///
/// Slot number == field declaration index; `StorageVec` and
/// `Mapping<_, StorageBytes>` each occupy exactly one slot. `openings.rs` and
/// `scripts/seed_genesis.py` hardcode these numbers — see the slot-parity tests
/// in `tests/state.rs` before reordering anything.
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
    // slot 8: number of registered pairs. Ordinals are 1-based and never
    // reused: nothing unregisters, so `1..=pair_count` is dense and every
    // enumeration relies on that.
    pub pair_count: Slot<u32>,
    // Slot 9 (`pair_id_to_hash`) is a retired hole. The reverse lookup it served
    // is now `pair_ordinal_base`/`pair_ordinal_quote` at slots 43-44, which
    // rebuild the key itself instead of a hash of it. Do not reuse.
    // slot 10: mapping(pair => ordinal) (0 = not registered)
    #[slot(10)]
    pub pair_ordinal: Mapping<AddressPair, u32>,
    // slot 11: mapping(pair => is_vote_target)
    pub vote_target: Mapping<AddressPair, bool>,

    // === Exchange Rates (slots 12-14) ===
    // slot 12: mapping(pair => exchange_rate) 1e18 scaled
    pub exchange_rate: Mapping<AddressPair, U256>,
    // slot 13: mapping(pair => last_update_block)
    pub exchange_rate_block: Mapping<AddressPair, u64>,
    // slot 14: mapping(pair => last_update_timestamp)
    pub exchange_rate_timestamp: Mapping<AddressPair, u64>,

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
    // slot 21: mapping(validator => mapping(tuple_idx => pair base))
    // Paired with `vote_pair_quote` at slot 65 — see the quote-column block.
    pub vote_pair_base: Mapping<Address, Mapping<u32, Address>>,
    // slot 22: mapping(validator => mapping(tuple_idx => rate))
    pub vote_rate: Mapping<Address, Mapping<u32, U256>>,
    // slot 23: mapping(validator => mapping(tuple_idx => volume))
    pub vote_volume: Mapping<Address, Mapping<u32, U256>>,

    // === Voter Tracking (slot 24) ===
    // slot 24: dynamic array of voter addresses (length at slot 24, data at keccak256(24))
    pub voter_list: StorageVec<Address>,

    // === Price Snapshots — circular buffer (slots 25-31) ===
    // slot 25: monotonic write index (tail pointer, u64 to avoid wrapping)
    pub snapshot_write_idx: Slot<u64>,
    // slot 26: oldest valid index (head pointer)
    pub snapshot_oldest_idx: Slot<u64>,
    // slot 27: mapping(snapshot_idx => timestamp)
    pub snapshot_timestamp: Mapping<u64, u64>,
    // slot 28: mapping(snapshot_idx => pair_count in this snapshot)
    pub snapshot_pair_count: Mapping<u64, u32>,
    // slot 29: mapping(snapshot_idx => mapping(pair_index => pair base))
    // Paired with `snapshot_pair_quote` at slot 66.
    pub snapshot_pair_base: Mapping<u64, Mapping<u32, Address>>,
    // slot 30: mapping(snapshot_idx => mapping(pair_index => rate))
    pub snapshot_rate: Mapping<u64, Mapping<u32, U256>>,
    // slot 31: mapping(snapshot_idx => mapping(pair_index => volume))
    pub snapshot_volume: Mapping<u64, Mapping<u32, U256>>,

    // === Protected Validators (slots 32-33) ===
    // slot 32: mapping(validator => is_protected)
    pub protected_validator: Mapping<Address, bool>,
    // slot 33: allow protected validators flag
    pub config_allow_protected: Slot<bool>,

    // === S-Curve Active Entries (slots 34-39) ===
    // slot 34: number of active S-curve entries
    pub scurve_count: Slot<u32>,
    // slot 35: mapping(entry_idx => pair base) for which pair
    // Paired with `scurve_pair_quote` at slot 67.
    pub scurve_pair_base: Mapping<u32, Address>,
    // slot 36: mapping(entry_idx => peak_day_timestamp) UTC midnight
    pub scurve_peak_day: Mapping<u32, u64>,
    // slot 37: mapping(entry_idx => peak_price) 1e18 scaled
    pub scurve_peak_price: Mapping<u32, U256>,
    // slot 38: oldest non-evicted entry index (head pointer for cleanup)
    pub scurve_oldest_idx: Slot<u32>,
    // slot 39: last UTC day (truncated timestamp) when S-curve processing ran
    pub scurve_last_processed_day: Slot<u64>,

    // === Settlement Currencies — retired (slots 40-42) ===
    // The settlement pair is derived, not stored: `coen_iso_pair(iso)` packs the
    // zero address with the marked ISO address, and an ISO is usable exactly
    // when that pair is present in `pair_ordinal`. Slots 40
    // (`settlement_count`), 41 (`settlement_iso_to_denom`) and 42
    // (`settlement_iso_to_pair`) are retired holes with no live writer. Do not
    // reuse them.

    // === Ordinal -> Pair Reverse Lookup (slots 43-44) ===
    // The only way to enumerate the registry: mappings are not iterable, so
    // `1..=pair_count` walks these two columns to rebuild each `AddressPair`.
    // Both are written together by `register_pair` and read together by
    // `pair_at`; a half-written ordinal would decode as a different pair.
    // slot 43 — pinned: without this anchor the running slot counter would
    // slide these two down into the retired 40-42 hole.
    #[slot(43)]
    pub pair_ordinal_base: Mapping<u32, Address>,
    // slot 44
    pub pair_ordinal_quote: Mapping<u32, Address>,
    // Slots 45 (`settlement_index_to_iso`) and 46
    // (`settlement_iso_to_denom_string`) are retired holes.

    // === WorldwideDay VWAP Snapshots (slots 47-52) ===
    // slot 47: mapping(worldwide_day => exists)
    #[slot(47)]
    pub worldwide_day_vwap_exists: Mapping<WorldwideDay, bool>,
    // slot 48: mapping(worldwide_day => start_time)
    pub worldwide_day_vwap_start: Mapping<WorldwideDay, u64>,
    // slot 49: mapping(worldwide_day => end_time)
    pub worldwide_day_vwap_end: Mapping<WorldwideDay, u64>,
    // slot 50: mapping(worldwide_day => pair_count)
    pub worldwide_day_vwap_pair_count: Mapping<WorldwideDay, u32>,
    // slot 51: mapping(worldwide_day => mapping(index => pair base))
    // Paired with `worldwide_day_vwap_pair_quote` at slot 68.
    pub worldwide_day_vwap_pair_base: Mapping<WorldwideDay, Mapping<u32, Address>>,
    // slot 52: mapping(worldwide_day => mapping(index => vwap))
    pub worldwide_day_vwap_value: Mapping<WorldwideDay, Mapping<u32, U256>>,

    // === Daily Rolling VWAP Aggregates (slots 53-54) ===
    // Updated on every write_snapshot. Keyed by (pair, utc_day_timestamp).
    // utc_day_timestamp = timestamp - (timestamp % 86400).
    // slot 53: mapping(pair => mapping(utc_day_ts => cumulative price*volume sum))
    pub daily_pv_sum: Mapping<AddressPair, Mapping<u64, U256>>,
    // slot 54: mapping(pair => mapping(utc_day_ts => cumulative volume sum))
    pub daily_vol_sum: Mapping<AddressPair, Mapping<u64, U256>>,

    // === Reference Currencies (slot 55) ===
    // Dynamic list of ISO 4217 numeric codes considered "reference" currencies
    // for off-chain pricing. Length at the base slot, data at keccak256(slot)
    // + index. Pre-filled at genesis with [840] (USD).
    pub reference_currencies: StorageVec<u16>,

    // === Per-UTC-Day VWAP Snapshots (slots 56-59) ===
    // Finalized VWAP for a full UTC calendar day, keyed by a yyyymmdd UTC date
    // key (e.g. 20260625) — NOT a WorldwideDay (which is UTC+14). Written once
    // per closed day by the begin-block lifecycle from the canonical
    // `[date_key_to_utc_timestamp(utc_day), +SECONDS_PER_DAY)` window. Stored
    // forever (no pruning). A day with no oracle data is never written, so
    // `pair_count == 0` means "not finalized OR finalized-empty"; the three
    // states are disambiguated against `utc_day_vwap_last_finalized`.
    //
    // mapping(utc_day => number of (pair, vwap) entries finalized for the day)
    pub utc_day_vwap_pair_count: Mapping<u32, u32>,
    // mapping(utc_day => mapping(entry_idx => pair base))
    // Paired with `utc_day_vwap_pair_quote` at slot 69.
    pub utc_day_vwap_pair_base: Mapping<u32, Mapping<u32, Address>>,
    // mapping(utc_day => mapping(entry_idx => vwap)) 1e18 scaled
    pub utc_day_vwap_value: Mapping<u32, Mapping<u32, U256>>,
    // Monotonic watermark: most recent fully-closed UTC day that has been
    // finalized (yyyymmdd). 0 = nothing finalized yet. Backfill is contiguous,
    // so every day <= this watermark is considered finalized.
    pub utc_day_vwap_last_finalized: Slot<u32>,

    // slot 60: reference-currency currency rates
    pub reference_currency_rate: Mapping<u16, U256>,

    // === OCOMP PoC pre-admission projection (slots 61, 63-64) ===
    // These trailing fields are inert until the fresh-devnet fork handler sets
    // `ocomp_profile_ready`. Existing pre-fork Oracle writes therefore keep
    // their exact historical state footprint.
    pub ocomp_profile_ready: Slot<bool>,
    // Slot 62 (`ocomp_day_type_pair_id`) is a retired hole. The day-type pair is
    // the compile-time `DAY_TYPE_PAIR_KEY`, so pinning its ordinal in storage
    // bought nothing once pairs stopped being identified by ordinal.
    // Do not reuse.
    // slot 63: direct O(1) lookup for the single fork-fixed auction-entry pair.
    // The canonical UTC-day finalizer is its only mutation owner.
    #[slot(63)]
    pub ocomp_day_type_vwap_by_utc_day: Mapping<u32, U256>,
    // Advances for every Oracle mutation visible to OCOMP pre-admission.
    pub ocomp_state_version: Slot<u64>,

    // === Pair Quote Columns (slots 65-69) ===
    // A pair does not fit in one word, so each identity column above stores the
    // registered `base` and these store the matching `quote`. Never write one
    // without the other — everything goes through `write_pair_columns` /
    // `read_pair_columns`, which also keeps both SSTOREs inside the same
    // checkpoint so a partial write cannot survive.
    // slot 65: companion of `vote_pair_base` (21)
    pub vote_pair_quote: Mapping<Address, Mapping<u32, Address>>,
    // slot 66: companion of `snapshot_pair_base` (29)
    pub snapshot_pair_quote: Mapping<u64, Mapping<u32, Address>>,
    // slot 67: companion of `scurve_pair_base` (35)
    pub scurve_pair_quote: Mapping<u32, Address>,
    // slot 68: companion of `worldwide_day_vwap_pair_base` (51)
    pub worldwide_day_vwap_pair_quote: Mapping<WorldwideDay, Mapping<u32, Address>>,
    // slot 69: companion of `utc_day_vwap_pair_base` (57)
    pub utc_day_vwap_pair_quote: Mapping<u32, Mapping<u32, Address>>,
}
