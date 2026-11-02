//! Storage schema for the Credis contract.

use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::CREDIS_ADDRESS;
use outbe_primitives::series_id::SeriesId;
#[cfg(any(test, feature = "e2e-test", feature = "test-utils"))]
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

use crate::errors::CredisError;

/// Credis lifecycle state.
///
/// A Credis is settleable from the moment it is issued. `Issued -> Called` is its
/// group's sustained-breach trigger. Both `Settled` (fully repaid) and `Forfeited`
/// (settlement deadline passed with a remainder) are terminal. A forfeited Credis is
/// deleted, so storage holds only `Issued` and `Settled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CredisState {
    Issued = 0,
    Called = 1,
    Settled = 2,
    Forfeited = 3,
}

impl CredisState {
    pub fn from_u8(value: u8) -> Result<Self, CredisError> {
        match value {
            0 => Ok(Self::Issued),
            1 => Ok(Self::Called),
            2 => Ok(Self::Settled),
            3 => Ok(Self::Forfeited),
            other => Err(CredisError::InvalidStateValue(other)),
        }
    }

    /// True once the Credis can no longer change: fully repaid or forfeited.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Settled | Self::Forfeited)
    }
}

/// Credis record. Keyed by `keccak256(series_id || owner || block_number)`.
///
/// Every term, both currency codes included, is sealed at issuance and never
/// changes afterwards. Only `outstanding_principal_minor`, `outstanding_gratis_minor`,
/// `interest_settled_minor`, `last_settled_at` and `called_at` move over the
/// Credis's life. A live Credis is called with its series: loading it reads the call and
/// its terms from the series, and settling it copies them onto the record.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(packed, exists_field = owner)]
pub struct Credis {
    #[key]
    pub credis_id: U256,

    /// `P` - stablecoin minor units disbursed. Fixed.
    #[attribute(order = 0)]
    pub principal_minor: U256,

    /// `P_out` - outstanding principal. Reaching zero settles the Credis. Past its
    /// deadline it is the principal written off; readers go through `outcome`.
    #[attribute(order = 1)]
    pub outstanding_principal_minor: U256,

    /// `G` - pledged Gratis: principal priced at the reservation's eight-hour
    /// COEN/`issuance_currency` VWAP. Fixed.
    #[attribute(order = 2)]
    pub gratis_minor: U256,

    /// The share of `G` still locked. Released principal-proportionally. Past its
    /// deadline it is the Gratis burned.
    #[attribute(order = 3)]
    pub outstanding_gratis_minor: U256,

    /// `r` - the currency's annual official policy rate (scale `1e6`), fixed by the
    /// reservation for the Credis's life.
    #[attribute(order = 4)]
    pub policy_rate: U256,

    /// Principal / Gratis, in the issuance currency (scale `1e6`). Sealed on the
    /// pledge and copied here. Not an oracle quote and not the call anchor.
    #[attribute(order = 5)]
    pub entry_price_minor: U256,

    /// `call_anchor_price_minor` marked up by `call_rate`, in the reference currency (scale `1e6`).
    /// The daily scan calls the Credis when `call_threshold_seconds` of its last
    /// `call_window_seconds` of finalized COEN/`reference_currency` VWAPs are strictly
    /// above this price. Immutable.
    #[attribute(order = 6)]
    pub call_price_minor: U256,

    /// COEN price in `reference_currency` (scale `1e6`) fixed by the reservation:
    /// the previous closed UTC-day VWAP, independent of spot.
    /// Immutable. `call_price_minor` is this value marked up by `call_rate`.
    #[attribute(order = 7)]
    pub call_anchor_price_minor: U256,

    /// Lifetime interest collected, in the asset's minor units. The sum of
    /// successful settlement interest deltas. Unpaid interest is left out,
    /// including when the remainder is forfeited.
    #[attribute(order = 8)]
    pub interest_settled_minor: U256,

    /// The address the Credis was issued to.
    #[attribute(order = 9)]
    pub owner: Address,

    /// Issuance timestamp. The interest anchor starts here. The series id is the one
    /// the reservation fixed, not a day derived from this timestamp.
    #[attribute(order = 10)]
    pub issued_at: u64,

    /// Call Notice Period in seconds: a called Credis whose remainder is
    /// still outstanding at `called_at + call_notice_period_seconds` is forfeited.
    /// Snapshotted from the profile at issuance; a live Credis reads its series's.
    #[attribute(order = 11, default = 0)]
    pub call_notice_period_seconds: u32,

    /// The agent that originated the Credis. Carries the accountability for
    /// how it resolves.
    #[attribute(order = 12)]
    pub cca: Address,

    /// Start of the current accrual period. Equals `issued_at` until the
    /// first settlement. Then it advances by the whole days that each settlement
    /// charges, not to the settlement timestamp. Thus a sub-day remainder
    /// carries forward and is not discarded.
    #[attribute(order = 13)]
    pub last_settled_at: u64,

    /// Call-trigger evaluation window in seconds, snapshotted from the profile at
    /// issuance; a live Credis reads its series's. This is the trailing span the daily
    /// scan reads for Call Price breaches. Divided by 86400 to get the day count.
    #[attribute(order = 14, default = 0)]
    pub call_window_seconds: u32,

    /// The stablecoin the Credis is denominated and disbursed in.
    #[attribute(order = 15)]
    pub asset: Address,

    /// When the Credis's series was called, 0 while it is not.
    #[attribute(order = 16, default = 0)]
    pub called_at: u64,

    /// Breach threshold in seconds, snapshotted from the profile at issuance; a live
    /// Credis reads its series's. Divided by 86400 to get the required breach-day count.
    #[attribute(order = 17, default = 0)]
    pub call_threshold_seconds: u32,

    /// Main account whose pledged Gratis backs the Credis. Repayments release
    /// collateral to it and a default burns it from it.
    #[attribute(order = 18)]
    pub source: Address,

    /// ISO 4217 numeric code of `asset` (e.g. 840 = USD), read at issuance.
    /// Denominates the Credis and keys its policy rate. It is NOT the call
    /// threshold anchor. See [`Self::reference_currency`].
    #[attribute(order = 19)]
    pub issuance_currency: u16,

    /// ISO 4217 numeric code of the reference currency elected at the reservation
    /// and fixed for the Credis's life. `call_anchor_price_minor` and `call_price_minor`
    /// are quoted here, and the daily breach scan reads the
    /// COEN/`reference_currency` series. It does not denominate `entry_price_minor`.
    #[attribute(order = 20)]
    pub reference_currency: u16,

    /// Call-price markup percent (snapshot of the profile call rate at issuance).
    /// Applied to `call_anchor_price_minor`, not to `entry_price_minor` (64 => 1.64x, 2 => 1.02x).
    #[attribute(order = 21, default = 0)]
    pub call_rate: u16,

    /// Series id copied from the reservation
    /// (`SeriesId::for_pair(reservation day, issuance_currency, reference_currency)`).
    #[attribute(order = 22)]
    pub series_id: SeriesId,
}

impl Credis {
    pub fn is_settled(&self) -> bool {
        self.outstanding_principal_minor.is_zero()
    }
}

/// EVM storage layout for the Credis contract.
///
/// `owner_ids` lists each owner's live Credis, as `outbe-nod` lists its Nods.
/// Slot assignment is macro-generated: a `Map<K, V>` over a record reserves
/// `V::SLOTS` top-level slots, so the Credis map alone spans as many slots as `Credis`
/// has attributes.
///
/// Credis are called and forfeited in series keyed by
/// `SeriesId::for_pair(last_closed_day, issuance_currency, reference_currency)`. A series
/// seals its first member's call terms and lists its unsettled members.
#[storage_schema]
#[contract(addr = CREDIS_ADDRESS)]
pub struct CredisContract {
    /// Credis record keyed by credis_id.
    #[attribute(order = 0)]
    pub records: outbe_primitives::storage::dsl::Map<U256, Credis>,

    /// Each account's live Credis.
    #[attribute(order = 1)]
    pub owner_ids:
        outbe_primitives::storage::dsl::Map<Address, outbe_primitives::storage::dsl::List<U256>>,
    #[attribute(order = 2)]
    pub owner_id_position: outbe_primitives::storage::dsl::Map<U256, u32>,

    /// Widest `call_window_seconds` ever opened in a reference currency, in seconds. It
    /// only grows, so the trailing span the daily scan collects always covers a
    /// Credis whose sealed window outruns the live profile.
    #[attribute(order = 8)]
    pub max_call_window_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    // The queue orders called series by the UTC day that contains their settlement deadline.
    #[attribute(order = 9)]
    pub forfeit_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 10)]
    pub forfeit_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 11)]
    pub forfeit_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 12)]
    pub forfeit_day_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 13)]
    pub forfeit_day_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 14)]
    pub forfeit_day_at: outbe_primitives::storage::dsl::Map<B256, SeriesId>,
    #[attribute(order = 15)]
    pub called_series_slot: outbe_primitives::storage::dsl::Map<SeriesId, u64>,
    #[attribute(order = 16)]
    pub called_deadline: outbe_primitives::storage::dsl::Map<SeriesId, u64>,
    #[attribute(order = 17)]
    pub forfeit_pass_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 18)]
    pub forfeit_cursor: outbe_primitives::storage::dsl::Value<u64>,

    /// Lowest `call_threshold_seconds` of at least a day ever opened in a reference
    /// currency. With the widest window it bounds the call prices the scan visits.
    #[attribute(order = 19)]
    pub min_call_threshold_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    // Uncalled series by call price, one trie per reference currency. A call takes
    // the series out, so the daily scan visits only what can still be called.
    #[attribute(order = 20)]
    pub call_bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,
    #[attribute(order = 21)]
    pub call_bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 22)]
    pub call_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 23)]
    pub call_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,
    #[attribute(order = 24)]
    pub call_bin_series: outbe_primitives::storage::dsl::Map<B256, SeriesId>,
    #[attribute(order = 25)]
    pub call_series_slot: outbe_primitives::storage::dsl::Map<SeriesId, u64>,
    /// `(bin << 32) | series of that bin still to visit`. 0 = start from the lowest bin.
    #[attribute(order = 26)]
    pub call_bin_cursor: outbe_primitives::storage::dsl::Map<u16, u64>,
    /// UTC day the call pass could not price a reference currency on. The rest of that
    /// day's pass passes it by.
    #[attribute(order = 27)]
    pub call_scan_failed_day: outbe_primitives::storage::dsl::Map<u16, u32>,

    /// ISO 4217 code of the currency the call pass resumes at.
    #[attribute(order = 28)]
    pub call_currency_cursor: outbe_primitives::storage::dsl::Value<u32>,
    /// UTC day the unfinished call pass is pinned to. 0 = none in flight.
    #[attribute(order = 29)]
    pub call_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    /// Newest closed day scheduled. Every day up to it is walked in order.
    #[attribute(order = 30)]
    pub call_latest_day: outbe_primitives::storage::dsl::Value<u32>,

    /// Profile selector (0 = by network, 1 = dev, 2 = prod). See `crate::config`.
    #[attribute(order = 31)]
    pub config_profile: outbe_primitives::storage::dsl::Value<u8>,

    /// Each series's unsettled Credis.
    #[attribute(order = 32)]
    pub series_members:
        outbe_primitives::storage::dsl::Map<SeriesId, outbe_primitives::storage::dsl::List<U256>>,
    #[attribute(order = 33)]
    pub series_member_position: outbe_primitives::storage::dsl::Map<U256, u32>,
    #[attribute(order = 34)]
    pub series_currency: outbe_primitives::storage::dsl::Map<SeriesId, u16>,
    /// The first full day every member counts call days from.
    #[attribute(order = 35)]
    pub series_full_day: outbe_primitives::storage::dsl::Map<SeriesId, u32>,
    #[attribute(order = 36)]
    pub series_call_price_minor: outbe_primitives::storage::dsl::Map<SeriesId, U256>,
    #[attribute(order = 37)]
    pub series_call_window_seconds: outbe_primitives::storage::dsl::Map<SeriesId, u32>,
    #[attribute(order = 38)]
    pub series_call_threshold_seconds: outbe_primitives::storage::dsl::Map<SeriesId, u32>,
    #[attribute(order = 39)]
    pub series_call_notice_period_seconds: outbe_primitives::storage::dsl::Map<SeriesId, u32>,
    #[attribute(order = 40)]
    pub series_called_at: outbe_primitives::storage::dsl::Map<SeriesId, u64>,
    // The queue orders forfeited series by deadline day while their Credis await deletion.
    #[attribute(order = 42)]
    pub cleanup_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 43)]
    pub cleanup_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 44)]
    pub cleanup_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 45)]
    pub cleanup_day_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 46)]
    pub cleanup_day_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 47)]
    pub cleanup_day_at: outbe_primitives::storage::dsl::Map<B256, SeriesId>,
    #[attribute(order = 48)]
    pub cleanup_series_slot: outbe_primitives::storage::dsl::Map<SeriesId, u64>,
    #[attribute(order = 49)]
    pub cleanup_deadline: outbe_primitives::storage::dsl::Map<SeriesId, u64>,
    #[attribute(order = 50)]
    pub cleanup_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 51)]
    pub cleanup_cursor: outbe_primitives::storage::dsl::Value<u64>,

    /// Each account's principal and outstanding principal over its live Credis.
    #[attribute(order = 52)]
    pub owner_principal_minor: outbe_primitives::storage::dsl::Map<Address, U256>,
    #[attribute(order = 53)]
    pub owner_outstanding_minor: outbe_primitives::storage::dsl::Map<Address, U256>,
    #[attribute(order = 54)]
    pub daily_phase: outbe_primitives::storage::dsl::Value<u8>,
    #[attribute(order = 55)]
    pub daily_budget: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 56)]
    pub daily_genesis_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 57)]
    pub series_opened: outbe_primitives::storage::dsl::Map<SeriesId, bool>,
    #[attribute(order = 58)]
    pub series_issuance_currency: outbe_primitives::storage::dsl::Map<SeriesId, u16>,
    #[attribute(order = 59)]
    pub series_call_anchor_price_minor: outbe_primitives::storage::dsl::Map<SeriesId, U256>,
    #[attribute(order = 60)]
    pub series_policy_rate: outbe_primitives::storage::dsl::Map<SeriesId, U256>,
    #[attribute(order = 61)]
    pub series_unpaid_gratis_minor: outbe_primitives::storage::dsl::Map<SeriesId, U256>,
    #[attribute(order = 62)]
    pub series_settlement_deadline: outbe_primitives::storage::dsl::Map<SeriesId, u64>,
}

impl CredisContract<'_> {
    /// `keccak256(series_id || owner || block_number)` with the 14-byte series id,
    /// the 20-byte owner address and the execution block number as a big-endian u64.
    pub fn generate_credis_id(series_id: SeriesId, owner: Address, block_number: u64) -> U256 {
        series_id.right_id(&[owner.as_slice(), &block_number.to_be_bytes()])
    }

    /// The series a Credis issued at `issued_at` for `(issuance_currency, reference_currency)`
    /// belongs to: `SeriesId::for_pair(last_closed_day(issued_at), issuance_currency, reference_currency)`.
    ///
    /// Test and e2e arming only. Production copies `series_id` from the reservation.
    #[cfg(any(test, feature = "e2e-test", feature = "test-utils"))]
    pub fn series_of(
        issued_at: u64,
        issuance_currency: u16,
        reference_currency: u16,
    ) -> outbe_primitives::error::Result<SeriesId> {
        let last_closed_day = previous_date_key(timestamp_to_date_key(issued_at))
            .map_err(crate::errors::date_step_error)?;
        SeriesId::for_pair(last_closed_day, issuance_currency, reference_currency)
            .map_err(|err| outbe_primitives::error::PrecompileError::Fatal(err.to_string()))
    }
}
