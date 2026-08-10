//! Storage schema for the Intex runtime module: the canonical per-series
//! identity + lifecycle ledger. One record per `seriesId`.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::INTEX_ADDRESS;

use crate::errors::IntexError;
use crate::series_id::SeriesId;

/// Series lifecycle state. `Issued -> Qualified -> Called`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IntexState {
    Issued = 0,
    Qualified = 1,
    Called = 2,
}

impl IntexState {
    pub fn from_u8(value: u8) -> Result<Self, IntexError> {
        match value {
            0 => Ok(Self::Issued),
            1 => Ok(Self::Qualified),
            2 => Ok(Self::Called),
            other => Err(IntexError::InvalidStateValue(other)),
        }
    }
}

/// Forced-call trigger parameters for a series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IntexCallTrigger {
    pub call_window: u32,
    pub call_threshold: u32,
    /// Seconds between `called_at` and the settlement deadline.
    pub call_notice_period: u32,
}

/// Identity parameters captured once at series creation.
///
/// `promis_load_minor` is `u128` to mirror the Origin `uint128` ABI; storage
/// widens it to `U256` (the storage DSL has no `u128` codec), always lossless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSeriesParams {
    pub series_id: SeriesId,
    pub worldwide_day: u32,
    pub issued_intex_count: u32,
    /// Promis tokens per Intex unit (18 decimals); bounded by source `uint128`.
    pub promis_load_minor: u128,
    /// Entry price (per-unit, reference currency, 1e18 oracle scale). Primary
    /// anchor; cost/floor/call derive from it.
    pub entry_price_minor: U256,
    /// Price floor (1e18, oracle scale).
    pub floor_price_minor: U256,
    /// Call price level that arms the forced call (1e18, oracle scale).
    pub call_price_minor: U256,
    pub call_trigger: IntexCallTrigger,
    /// Creation timestamp (UNIX seconds); non-zero, doubles as existence sentinel.
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
    pub issued_intex_count: u32,

    #[attribute(order = 3)]
    pub promis_load_minor: U256,

    #[attribute(order = 4)]
    pub entry_price_minor: U256,

    #[attribute(order = 5)]
    pub floor_price_minor: U256,

    #[attribute(order = 6)]
    pub call_price_minor: U256,

    // call_trigger group — stored flat (the storage DSL has no nested-struct codec),
    // exposed nested via `call_trigger()`.
    #[attribute(order = 7)]
    pub call_window: u32,

    #[attribute(order = 8)]
    pub call_threshold: u32,

    #[attribute(order = 9)]
    pub call_notice_period: u32,

    #[attribute(order = 10)]
    pub issued_at: u32,

    #[attribute(order = 11, default = 0)]
    pub called_at: u32,

    /// Lifecycle state as `u8`; decode via [`IntexState::from_u8`].
    #[attribute(order = 12)]
    pub state: u8,

    /// Worldwide day whose tributes fed this series (== series_id until multi-currency).
    #[attribute(order = 13, default = 0)]
    pub worldwide_day: u32,
}

impl SeriesRecord {
    pub fn lifecycle_state(&self) -> Result<IntexState, IntexError> {
        IntexState::from_u8(self.state)
    }

    pub fn cost_amount_minor(&self) -> Result<U256, IntexError> {
        cost_amount_minor(self.entry_price_minor, self.promis_load_minor)
    }

    pub fn call_trigger(&self) -> IntexCallTrigger {
        IntexCallTrigger {
            call_window: self.call_window,
            call_threshold: self.call_threshold,
            call_notice_period: self.call_notice_period,
        }
    }
}

/// Cost of one Intex in the reference currency, on the same 1e18 oracle scale as
/// entry/floor/call. `promis_load_minor` carries its own 1e18, hence the divisor.
/// Settling converts this into the chosen token's minor units — see
/// `intexfactory::runtime::quote_cost_amount`.
pub fn cost_amount_minor(
    entry_price_minor: U256,
    promis_load_minor: U256,
) -> Result<U256, IntexError> {
    entry_price_minor
        .checked_mul(promis_load_minor)
        .map(|v| v / U256::from(10u64).pow(U256::from(18u64)))
        .ok_or(IntexError::CostAmountOverflow)
}

/// Paginated creator-reward distribution progress for a worldwide day. Exists
/// while a distribution is in flight; `active != 0` is the existence sentinel.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = active)]
pub struct DistProgress {
    #[key]
    pub worldwide_day: u32,

    /// Total native COEN received for that day's distribution.
    #[attribute(order = 0)]
    pub amount: U256,

    /// Σ of contributor nominals (the proportionality denominator).
    #[attribute(order = 1)]
    pub total_nominal: U256,

    /// Native COEN already paid out across chunks so far.
    #[attribute(order = 2)]
    pub paid_so_far: U256,

    /// Index of the next contributor to pay.
    #[attribute(order = 3)]
    pub cursor: u32,

    /// Existence sentinel: 1 while in flight.
    #[attribute(order = 4)]
    pub active: u8,
}

/// Constant-size certified contributor authority for one Intex series.
///
/// Contributor bodies remain in authenticated result chunks; activation stores
/// only the proof root and exact aggregate scalars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertifiedContributorGenerationProjection {
    /// Worldwide day; named for the protocol field it mirrors.
    pub series_id: u32,
    pub series_version: u64,
    pub contributor_root: B256,
    pub contributor_count: u32,
    pub eligible_nominal_total: U256,
}

impl CertifiedContributorGenerationProjection {
    pub(crate) fn metadata_word(&self) -> U256 {
        U256::from(self.series_version) | (U256::from(self.contributor_count) << 64)
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
    pub series_id_at_index: outbe_primitives::storage::dsl::Map<u64, u64>,

    // --- Creator-reward: per-day contributors (owner → nominal share) ---
    // Orders 3-23 are keyed by worldwide day, not by series id.
    /// worldwide_day -> number of contributors.
    #[attribute(order = 3)]
    pub contributor_count: outbe_primitives::storage::dsl::Map<u32, u32>,

    /// keccak256(worldwide_day_be32 ++ index_be32) -> contributor owner.
    #[attribute(order = 4)]
    pub contributor_owner_at: outbe_primitives::storage::dsl::Map<B256, Address>,

    /// keccak256(worldwide_day_be32 ++ index_be32) -> contributor nominal share.
    #[attribute(order = 5)]
    pub contributor_nominal_at: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// worldwide_day -> Σ nominal across all contributors.
    #[attribute(order = 6)]
    pub contributor_total: outbe_primitives::storage::dsl::Map<u32, U256>,

    // --- Creator-reward: paginated distribution progress + active set ---
    /// worldwide_day -> in-flight distribution progress.
    #[attribute(order = 7)]
    pub dist_progress: outbe_primitives::storage::dsl::Map<u32, DistProgress>,

    /// Number of in-flight distributions (dense active set for the begin-block drain).
    #[attribute(order = 8)]
    pub active_dist_count: outbe_primitives::storage::dsl::Value<u32>,

    /// dense index -> worldwide_day.
    #[attribute(order = 9)]
    pub active_dist_at: outbe_primitives::storage::dsl::Map<u32, u32>,

    /// worldwide_day -> (active index + 1); 0 = not active.
    #[attribute(order = 10)]
    pub active_dist_slot: outbe_primitives::storage::dsl::Map<u32, u32>,

    // --- Creator-reward: multi-chain proceeds fan-in aggregation ---
    /// worldwide_day -> proceeds accumulated but not yet handed to a distribution round.
    #[attribute(order = 11)]
    pub proceeds_pot: outbe_primitives::storage::dsl::Map<u32, U256>,

    /// worldwide_day -> deadline after which the pot distributes without the missing chains.
    #[attribute(order = 12)]
    pub proceeds_deadline: outbe_primitives::storage::dsl::Map<u32, u64>,

    /// worldwide_day -> number of winning chains expected to route proceeds.
    #[attribute(order = 13)]
    pub proceeds_expected_count: outbe_primitives::storage::dsl::Map<u32, u32>,

    /// worldwide_day -> number of expected chains whose proceeds have arrived.
    #[attribute(order = 14)]
    pub proceeds_arrived_count: outbe_primitives::storage::dsl::Map<u32, u32>,

    /// keccak256(worldwide_day_be32 ++ chain_be32) -> 1 if a winning (expected) chain.
    #[attribute(order = 15)]
    pub proceeds_expected: outbe_primitives::storage::dsl::Map<B256, u8>,

    /// keccak256(worldwide_day_be32 ++ chain_be32) -> 1 once that chain's proceeds arrived.
    #[attribute(order = 16)]
    pub proceeds_arrived: outbe_primitives::storage::dsl::Map<B256, u8>,

    /// worldwide_day -> 1 if the in-flight distribution round should finalize (clear the
    /// contributor map + aggregation state) on completion; 0 = retain for a late top-up.
    #[attribute(order = 17)]
    pub proceeds_finalize_on_done: outbe_primitives::storage::dsl::Map<u32, u8>,

    // Awaiting-proceeds set (dense) for the begin-block deadline sweep.
    #[attribute(order = 18)]
    pub awaiting_proceeds_count: outbe_primitives::storage::dsl::Value<u32>,
    /// dense index -> worldwide_day.
    #[attribute(order = 19)]
    pub awaiting_proceeds_at: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// worldwide_day -> (awaiting index + 1); 0 = not awaiting.
    #[attribute(order = 20)]
    pub awaiting_proceeds_slot: outbe_primitives::storage::dsl::Map<u32, u32>,

    /// Certified contributor proof root for one worldwide day.
    #[attribute(order = 21)]
    pub ocomp_contributor_root: outbe_primitives::storage::dsl::Map<u32, B256>,

    /// Packed active selector: series version (low u64), contributor count
    /// (next u32), with all remaining bits reserved as zero.
    #[attribute(order = 22)]
    pub ocomp_contributor_metadata: outbe_primitives::storage::dsl::Map<u32, U256>,

    /// Exact eligible nominal total committed by the certified root.
    #[attribute(order = 23)]
    pub ocomp_eligible_nominal_total: outbe_primitives::storage::dsl::Map<u32, U256>,

    /// worldwide_day -> number of series created for that day.
    #[attribute(order = 24)]
    pub day_series_count: outbe_primitives::storage::dsl::Map<u32, u32>,
}

impl IntexContract<'_> {
    /// Composite key for per-day contributor index lists:
    /// `keccak256(worldwide_day_be32 ++ index_be32)`.
    pub fn contributor_index_key(worldwide_day: u32, index: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&worldwide_day.to_be_bytes());
        buf[4..8].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    /// Composite key for per-(day, chain) proceeds flags:
    /// `keccak256(worldwide_day_be32 ++ chain_be32)`.
    pub fn proceeds_chain_key(worldwide_day: u32, chain_id: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&worldwide_day.to_be_bytes());
        buf[4..8].copy_from_slice(&chain_id.to_be_bytes());
        keccak256(buf)
    }

    /// Reads the active constant-size contributor proof authority.
    pub fn ocomp_certified_contributor_generation(
        &self,
        worldwide_day: u32,
    ) -> outbe_primitives::error::Result<Option<CertifiedContributorGenerationProjection>> {
        let contributor_root = self.ocomp_contributor_root.read(&worldwide_day)?;
        let metadata = self.ocomp_contributor_metadata.read(&worldwide_day)?;
        let eligible_nominal_total = self.ocomp_eligible_nominal_total.read(&worldwide_day)?;

        if metadata.is_zero() {
            if !contributor_root.is_zero() || !eligible_nominal_total.is_zero() {
                return Err(outbe_primitives::error::PrecompileError::Fatal(
                    "absent Intex certified contributor generation has residual state".into(),
                ));
            }
            return Ok(None);
        }
        if !(metadata >> 96usize).is_zero() || contributor_root.is_zero() {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "installed Intex certified contributor generation is malformed".into(),
            ));
        }

        let series_version = (metadata & U256::from(u64::MAX)).to::<u64>();
        let contributor_count = ((metadata >> 64usize) & U256::from(u32::MAX)).to::<u32>();
        // Version 1 is the valid absent -> certified history and remains valid
        // if the independent auction creates the series later. Version 2 is
        // valid only when the series already existed before certification.
        let valid_series_version =
            series_version == 1 || (series_version == 2 && self.day_has_series(worldwide_day)?);
        if !valid_series_version || (contributor_count == 0) != eligible_nominal_total.is_zero() {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "installed Intex certified contributor metadata is malformed".into(),
            ));
        }

        Ok(Some(CertifiedContributorGenerationProjection {
            series_id: worldwide_day,
            series_version,
            contributor_root,
            contributor_count,
            eligible_nominal_total,
        }))
    }
}
