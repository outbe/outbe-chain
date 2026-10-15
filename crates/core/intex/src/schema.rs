//! Storage schema for the Intex runtime module: the canonical per-series
//! identity + lifecycle ledger. One record per `seriesId`.

use alloy_primitives::{keccak256, Address, FixedBytes, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::INTEX_ADDRESS;
use outbe_primitives::stablecoin::iso_4217_alpha;
use outbe_primitives::storage::types::{Storable, StorableType, StorageKey};
use outbe_primitives::time::WorldwideDay;
use std::fmt;

use crate::errors::IntexError;

/// Series lifecycle state: `Issued -> Called -> Expired`.
/// A Called series becomes Expired after its notice-period deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IntexState {
    Issued = 0,
    Called = 2,
    Expired = 3,
}

impl IntexState {
    pub fn from_u8(value: u8) -> Result<Self, IntexError> {
        match value {
            0 => Ok(Self::Issued),
            2 => Ok(Self::Called),
            3 => Ok(Self::Expired),
            other => Err(IntexError::InvalidStateValue(other)),
        }
    }
}

/// Forced-call trigger parameters for a series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IntexCallTrigger {
    pub call_window_seconds: u32,
    pub call_threshold_seconds: u32,
    /// Seconds between `called_at` and the settlement deadline.
    pub call_notice_period_seconds: u32,
}

/// Series identifier: the 14 ASCII bytes of `20260212-TRY-U`. A currency with no
/// alpha code in ISO 4217 falls back to its zero-padded numeric code (`949`).
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeriesId([u8; SERIES_ID_LEN]);

pub const SERIES_ID_LEN: usize = 14;

const DAY_DIGITS: usize = 8;
const ISSUANCE_AT: usize = 9;
const REFERENCE_AT: usize = 13;

impl SeriesId {
    /// Rejects a day outside eight digits and code bytes outside `A-Z` / `0-9`.
    pub fn pack(
        worldwide_day: WorldwideDay,
        issuance: [u8; 3],
        reference: u8,
    ) -> Result<Self, IntexError> {
        let mut day = worldwide_day.value();
        if day == 0 || day > 99_999_999 {
            return Err(IntexError::InvalidSeriesId);
        }
        if !issuance.iter().all(|byte| is_code_byte(*byte)) || !is_code_byte(reference) {
            return Err(IntexError::InvalidSeriesId);
        }
        let mut bytes = [b'-'; SERIES_ID_LEN];
        for slot in bytes[..DAY_DIGITS].iter_mut().rev() {
            *slot = b'0' + (day % 10) as u8;
            day /= 10;
        }
        bytes[ISSUANCE_AT..ISSUANCE_AT + 3].copy_from_slice(&issuance);
        bytes[REFERENCE_AT] = reference;
        Ok(Self(bytes))
    }

    pub const fn from_bytes(bytes: [u8; SERIES_ID_LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; SERIES_ID_LEN] {
        &self.0
    }

    pub fn worldwide_day(&self) -> WorldwideDay {
        WorldwideDay::new(self.0[..DAY_DIGITS].iter().fold(0u32, |acc, byte| {
            acc * 10 + u32::from(byte.wrapping_sub(b'0'))
        }))
    }

    /// `949 -> b"949"`, `32 -> b"032"`. ISO 4217 numbers are three digits, and a
    /// wider one has no spelling here: folding it would give two currencies one id.
    pub fn numeric_code(iso: u16) -> Result<[u8; 3], IntexError> {
        if iso > 999 {
            return Err(IntexError::InvalidSeriesId);
        }
        Ok([
            b'0' + (iso / 100) as u8,
            b'0' + ((iso / 10) % 10) as u8,
            b'0' + (iso % 10) as u8,
        ])
    }

    /// How a currency is spelled inside an id: its alpha-3 code, or its numeric
    /// code when ISO assigns none.
    pub fn currency_code(iso: u16) -> Result<[u8; 3], IntexError> {
        // todo validate iso and refactor this code
        match iso_4217_alpha(iso) {
            Some(alpha) => Ok(alpha),
            None => Self::numeric_code(iso),
        }
    }

    /// The id of the series a day issues for one `(issuance, reference)` pair.
    pub fn for_pair(
        worldwide_day: WorldwideDay,
        issuance: u16,
        reference: u16,
    ) -> Result<Self, IntexError> {
        Self::pack(
            worldwide_day,
            Self::currency_code(issuance)?,
            Self::currency_code(reference)?[0],
        )
    }
}

const fn is_code_byte(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte.is_ascii_digit()
}

impl fmt::Display for SeriesId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.0))
    }
}

impl StorableType for SeriesId {
    const SLOTS: usize = 1;
}

/// Left-aligned in the word, matching the Solidity `bytes14` layout.
impl Storable for SeriesId {
    fn from_word(word: U256) -> Self {
        let mut bytes = [0u8; SERIES_ID_LEN];
        bytes.copy_from_slice(&word.to_be_bytes::<32>()[..SERIES_ID_LEN]);
        Self(bytes)
    }

    fn to_word(&self) -> U256 {
        let mut word = [0u8; 32];
        word[..SERIES_ID_LEN].copy_from_slice(&self.0);
        U256::from_be_bytes(word)
    }
}

impl StorageKey for SeriesId {
    fn key_bytes(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

impl From<SeriesId> for FixedBytes<SERIES_ID_LEN> {
    fn from(id: SeriesId) -> Self {
        Self(id.0)
    }
}

impl From<FixedBytes<SERIES_ID_LEN>> for SeriesId {
    fn from(value: FixedBytes<SERIES_ID_LEN>) -> Self {
        Self(value.0)
    }
}

/// Identity parameters captured once at series creation.
///
/// `promis_load_minor` is `u128` to mirror the Origin `uint128` ABI. Storage
/// widens it to `U256` (the storage DSL has no `u128` codec), always lossless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSeriesParams {
    pub series_id: SeriesId,
    pub worldwide_day: WorldwideDay,
    pub issued_units: u32,
    /// PROMIS-units per Intex unit (1e6). Bounded by source `uint128`.
    pub promis_load_minor: u128,
    /// Entry price (per-unit, reference ISO stable-units, 1e6). Primary
    /// anchor; cost/floor/call derive from it.
    pub entry_price_minor: U256,
    /// Price floor in reference ISO stable-units (1e6).
    pub floor_price_minor: U256,
    /// Call price level that arms the forced call, in reference ISO stable-units (1e6).
    pub call_price_minor: U256,
    pub call_trigger: IntexCallTrigger,
    /// Creation timestamp (UNIX seconds). Non-zero. It is also the existence sentinel.
    pub issued_at: u32,
    pub issuance_currency: u16,
    pub reference_currency: u16,
}

/// Per-series identity + lifecycle record. Keyed by `series_id`.
/// `issued_at == 0` means "no series".
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = issued_at)]
pub struct SeriesRecord {
    #[key]
    pub series_id: SeriesId,

    #[attribute(order = 0)]
    pub issuance_currency: u16,

    #[attribute(order = 1)]
    pub reference_currency: u16,

    #[attribute(order = 2)]
    pub issued_units: u32,

    #[attribute(order = 3)]
    pub promis_load_minor: U256,

    #[attribute(order = 4)]
    pub entry_price_minor: U256,

    #[attribute(order = 5)]
    pub floor_price_minor: U256,

    #[attribute(order = 6)]
    pub call_price_minor: U256,

    // call_trigger group. Storage keeps it flat (the storage DSL has no nested-struct
    // codec). `call_trigger()` exposes it nested.
    #[attribute(order = 7)]
    pub call_window_seconds: u32,

    #[attribute(order = 8)]
    pub call_threshold_seconds: u32,

    #[attribute(order = 9)]
    pub call_notice_period_seconds: u32,

    #[attribute(order = 10)]
    pub issued_at: u32,

    #[attribute(order = 11, default = 0)]
    pub called_at: u32,

    /// Lifecycle state as `u8`; decode via [`IntexState::from_u8`].
    #[attribute(order = 12)]
    pub state: u8,

    /// Worldwide day whose tributes fed this series. It is also the id's leading digits.
    #[attribute(order = 13, default = WorldwideDay::new(0))]
    pub worldwide_day: WorldwideDay,
}

impl SeriesRecord {
    /// The stored state. This node never writes `Expired`: ask `effective_state` whether one expired.
    pub fn lifecycle_state(&self) -> Result<IntexState, IntexError> {
        IntexState::from_u8(self.state)
    }

    /// The state as of `now`: derived, because no transaction arrives at the
    /// deadline to write it.
    pub fn effective_state(&self, now: u64) -> Result<IntexState, IntexError> {
        let stored = self.lifecycle_state()?;
        let deadline = u64::from(self.called_at) + u64::from(self.call_notice_period_seconds);
        match stored == IntexState::Called && now > deadline {
            true => Ok(IntexState::Expired),
            false => Ok(stored),
        }
    }

    pub fn call_trigger(&self) -> IntexCallTrigger {
        IntexCallTrigger {
            call_window_seconds: self.call_window_seconds,
            call_threshold_seconds: self.call_threshold_seconds,
            call_notice_period_seconds: self.call_notice_period_seconds,
        }
    }
}

/// EVM storage layout for the Intex module.
#[storage_schema]
#[contract(addr = INTEX_ADDRESS)]
pub struct IntexContract {
    #[attribute(order = 0)]
    pub series: outbe_primitives::storage::dsl::Map<SeriesId, SeriesRecord>,

    #[attribute(order = 1)]
    pub total_series: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 2)]
    pub series_id_at_index: outbe_primitives::storage::dsl::Map<u64, U256>,

    // Disjoint classes: a series forfeits `issued_units - settled - exercised -
    // gem_factory` at its deadline. Not on `SeriesRecord` because a record write
    // rewrites every field.
    /// series_id -> units paid for and not yet exercised.
    #[attribute(order = 27)]
    pub settled_units: outbe_primitives::storage::dsl::Map<SeriesId, u32>,

    /// series_id -> units sent to the Gem Factory.
    #[attribute(order = 28)]
    pub gem_factory_units: outbe_primitives::storage::dsl::Map<SeriesId, u32>,

    /// series_id -> settled units burned into Promis so far.
    #[attribute(order = 29)]
    pub exercised_units: outbe_primitives::storage::dsl::Map<SeriesId, u32>,

    // Per-owner history. The two current classes are the owner's Issued- and
    // Settled-class token balances, so the ledger keeps only what the token no longer holds.
    /// `owner_units_key` -> units this owner exercised in the series.
    #[attribute(order = 30)]
    pub owner_exercised_units: outbe_primitives::storage::dsl::Map<B256, u32>,

    /// `owner_units_key` -> units this owner sent to the Gem Factory.
    #[attribute(order = 31)]
    pub owner_gem_factory_units: outbe_primitives::storage::dsl::Map<B256, u32>,

    // --- Call lifecycle: the price bins the daily scan walks and the called groups ---
    // Call-price bin index the daily Called scan walks. A series enters it at issuance.
    #[attribute(order = 32)]
    pub call_bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,
    #[attribute(order = 33)]
    pub call_bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 34)]
    pub call_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,
    /// `scoped(iso, bin_id)` -> count of groups in the bin.
    #[attribute(order = 35)]
    pub call_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,

    // Genesis parameter-profile selector (0 = auto, 1 = dev, 2 = prod). See crate::config.
    #[attribute(order = 36)]
    pub config_profile: outbe_primitives::storage::dsl::Value<u8>,

    // ISO 4217 code where the call scan resumes. This cursor prevents a currency
    // that exhausts the shared budget from starving later currencies.
    #[attribute(order = 37)]
    pub call_currency_cursor: outbe_primitives::storage::dsl::Value<u32>,

    // `(bin << 32) | groups of that bin still to visit` where each currency's call scan
    // resumes. 0 = fresh sweep.
    #[attribute(order = 38)]
    pub call_bin_cursor: outbe_primitives::storage::dsl::Map<u16, u64>,

    // Group members, keyed by `scoped(iso, day)`: a decision reads only fields the
    // whole (reference currency, worldwide day) pair shares. A called group keeps them
    // until each member expires.
    #[attribute(order = 39)]
    pub call_group_count: outbe_primitives::storage::dsl::Map<u64, u32>,
    /// `keccak256(iso_be16 ++ worldwide_day_be32 ++ index_be32)` -> series_id word.
    #[attribute(order = 40)]
    pub call_group_members: outbe_primitives::storage::dsl::Map<B256, U256>,
    /// `scoped(iso, worldwide_day)` -> `(bin << 32) | (index + 1)` of the group in its bin.
    #[attribute(order = 41)]
    pub call_group_slot: outbe_primitives::storage::dsl::Map<u64, u64>,

    // UTC day an unfinished call sweep is pinned to, so its later slices decide
    // against the prices it opened with. 0 = none in flight. A date key is never 0.
    #[attribute(order = 42)]
    pub call_sweep_day: outbe_primitives::storage::dsl::Value<u32>,

    /// `keccak256(iso_be16 ++ bin_id_be32 ++ index_be32)` -> `scoped(iso, worldwide_day)`.
    #[attribute(order = 43)]
    pub call_bin_groups: outbe_primitives::storage::dsl::Map<B256, u64>,

    // Called groups awaiting their settlement window, bucketed by the hour it closes in.
    #[attribute(order = 44)]
    pub expiry_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 45)]
    pub expiry_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 46)]
    pub expiry_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    /// `scoped(iso, day)` -> when the group's settlement window closes. Stored so
    /// the head check costs no record load.
    #[attribute(order = 47)]
    pub called_deadline: outbe_primitives::storage::dsl::Map<u64, u64>,
    // Widest terms ever issued in a currency. Both only move outwards, so the range
    // they define covers series the live profile no longer names.
    #[attribute(order = 48)]
    pub max_call_window_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,
    #[attribute(order = 49)]
    pub min_call_threshold_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    /// Slots ever used in a bucket. Retired slots are zeroed in place, not compacted.
    #[attribute(order = 50)]
    pub expiry_bucket_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 51)]
    pub expiry_bucket_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// `keccak256(bucket_be32 ++ slot_be32)` -> `scoped(iso, worldwide_day)`.
    #[attribute(order = 52)]
    pub expiry_bucket_at: outbe_primitives::storage::dsl::Map<B256, u64>,
    /// `scoped(iso, day)` -> `(bucket << 32) | slot`. 0 = not queued.
    #[attribute(order = 53)]
    pub called_group_slot: outbe_primitives::storage::dsl::Map<u64, u64>,
    #[attribute(order = 54)]
    pub expiry_sweep_hour: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 55)]
    pub expiry_cursor: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 56)]
    pub call_pending_day: outbe_primitives::storage::dsl::Value<u32>,

    /// UTC day the call sweep could not price a reference currency on. The rest of that
    /// day's pass passes it by.
    #[attribute(order = 57)]
    pub call_scan_failed_day: outbe_primitives::storage::dsl::Map<u16, u32>,
}

impl IntexContract<'_> {
    /// Namespace a bin-index column by the reference currency its prices are in.
    pub const fn scoped(reference_currency: u16, key: u32) -> u64 {
        outbe_primitives::call_bins::scoped(reference_currency, key)
    }

    /// Inverse of [`Self::scoped`] for a key that is a worldwide day.
    pub const fn unscoped(scoped: u64) -> (u16, WorldwideDay) {
        (
            (scoped >> 32) as u16,
            WorldwideDay::new((scoped & 0xffff_ffff) as u32),
        )
    }

    /// Composite key for the per-owner ledgers: `keccak256(series_id ++ owner)`.
    pub fn owner_units_key(series_id: SeriesId, owner: Address) -> B256 {
        let mut buf = [0u8; SERIES_ID_LEN + 20];
        buf[..SERIES_ID_LEN].copy_from_slice(series_id.as_bytes());
        buf[SERIES_ID_LEN..].copy_from_slice(owner.as_slice());
        keccak256(buf)
    }
}
