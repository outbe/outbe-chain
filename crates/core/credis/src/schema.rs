//! Storage schema for the Credis contract.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_core_common::series_id::SeriesId;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_system_common::addresses::CREDIS_ADDRESS;
use outbe_system_common::time::timestamp_to_date_key;

use crate::constants::SECONDS_PER_HOUR;

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
/// changes afterwards. Only `outstanding_amount_minor`, `outstanding_gratis_minor`,
/// `settled_fee_minor` and `last_settled_at` move over the Credis's life. The call
/// terms and the call time are the group's: see [`CredisView`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(packed, exists_field = owner)]
pub struct Credis {
    #[key]
    pub credis_id: U256,

    /// `P` - stablecoin minor units disbursed. Fixed.
    #[attribute(order = 0)]
    pub amount_minor: U256,

    /// `P_out` - outstanding amount. Reaching zero settles the Credis. Past its
    /// deadline it is the amount written off; readers go through `outcome`.
    #[attribute(order = 1)]
    pub outstanding_amount_minor: U256,

    /// `G` - pledged Gratis: amount priced at the reservation hour's 24-hour
    /// COEN/`issuance_currency` VWAP. Fixed.
    #[attribute(order = 2)]
    pub gratis_minor: U256,

    /// The share of `G` still locked. Released amount-proportionally. Past its
    /// deadline it is the Gratis burned.
    #[attribute(order = 3)]
    pub outstanding_gratis_minor: U256,

    /// `r` - the currency's annual official policy rate (scale `1e6`), fixed by the
    /// reservation for the Credis's life.
    #[attribute(order = 4)]
    pub policy_rate: U256,

    /// Entry price of the sub-series: the 24-hour COEN/`reference_currency` VWAP at the
    /// reservation hour (scale `1e6`).
    #[attribute(order = 5)]
    pub entry_price_minor: U256,

    /// `entry_price_minor` marked up by `call_rate`, in the reference currency (scale `1e6`).
    /// The daily scan calls the Credis's group when `call_threshold_seconds` of its last
    /// `call_window_seconds` of finalized COEN/`reference_currency` VWAPs are strictly
    /// above this price. Immutable.
    #[attribute(order = 6)]
    pub call_price_minor: U256,

    /// Lifetime fee collected, in the asset's minor units. The sum of
    /// successful settlement fee deltas. Unpaid fee is left out,
    /// including when the remainder is forfeited.
    #[attribute(order = 7)]
    pub settled_fee_minor: U256,

    /// The address the Credis was issued to.
    #[attribute(order = 8)]
    pub owner: Address,

    /// Issuance timestamp. The fee anchor starts here. The series and sub-series are the
    /// ones the reservation fixed, not derived from this timestamp.
    #[attribute(order = 9)]
    pub issued_at: u64,

    /// The agent that originated the Credis. Carries the accountability for
    /// how it resolves.
    #[attribute(order = 10)]
    pub cca: Address,

    /// Start of the current accrual period. Equals `issued_at` until the
    /// first settlement. Then it advances by the whole days that each settlement
    /// charges, not to the settlement timestamp. Thus a sub-day remainder
    /// carries forward and is not discarded.
    #[attribute(order = 11)]
    pub last_settled_at: u64,

    /// The stablecoin the Credis is denominated and disbursed in.
    #[attribute(order = 12)]
    pub asset: Address,

    /// Main account whose pledged Gratis backs the Credis. Settlements free its Gratis and a
    /// forfeit burns it. No read, event or `tokenURI` shows it.
    #[attribute(order = 13)]
    pub pledge_account: Address,

    /// ISO 4217 numeric code of `asset` (e.g. 840 = USD), read at issuance.
    /// Denominates the Credis and keys its policy rate.
    #[attribute(order = 14)]
    pub issuance_currency: u16,

    /// ISO 4217 numeric code of the reference currency elected at the reservation
    /// and fixed for the Credis's life. `entry_price_minor` and `call_price_minor`
    /// are quoted here, and the daily breach scan reads the COEN/`reference_currency` series.
    #[attribute(order = 15)]
    pub reference_currency: u16,

    /// Series id copied from the reservation
    /// (`SeriesId::for_pair(reservation day, issuance_currency, reference_currency)`).
    #[attribute(order = 16)]
    pub series_id: SeriesId,

    /// UTC hour of the reservation, 0-23.
    #[attribute(order = 17)]
    pub sub_series: u8,

    /// Decimals of `asset` at the reservation. The amounts of the Credis are in these units.
    #[attribute(order = 18)]
    pub asset_decimals: u8,
}

impl Credis {
    pub fn is_settled(&self) -> bool {
        self.outstanding_amount_minor.is_zero()
    }
}

/// The call terms a group seals with its first Credis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CallTerms {
    /// Trailing span the daily scan reads for Call Price breaches, in seconds.
    pub call_window_seconds: u32,
    /// Breach seconds within that span that call the group.
    pub call_threshold_seconds: u32,
    /// Seconds after the call in which a called Credis must be settled.
    pub call_notice_period_seconds: u32,
}

/// A Credis as its group sees it: the record, the group's call terms and its call time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredisView {
    pub record: Credis,
    pub terms: CallTerms,
    /// When the group was called, 0 while it is not.
    pub called_at: u64,
}

impl std::ops::Deref for CredisView {
    type Target = Credis;

    fn deref(&self) -> &Credis {
        &self.record
    }
}

impl std::ops::DerefMut for CredisView {
    fn deref_mut(&mut self) -> &mut Credis {
        &mut self.record
    }
}

/// EVM storage layout for the Credis contract.
///
/// `owner_ids` lists each owner's live Credis, as `outbe-nod` lists its Nods.
/// Slot assignment is macro-generated: a `Map<K, V>` over a record reserves
/// `V::SLOTS` top-level slots, so the Credis map alone spans as many slots as `Credis`
/// has attributes.
///
/// A series is a reservation day, a sub-series its hour, and each sub-series is in one call group.
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

    /// Widest `call_window_seconds` ever sealed in a reference currency, in seconds. It
    /// only grows, so the trailing span the daily scan collects always covers a
    /// group whose sealed window outruns the live profile.
    #[attribute(order = 8)]
    pub max_call_window_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    // The queue orders called groups by the UTC day that contains their settlement deadline.
    #[attribute(order = 9)]
    pub close_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 10)]
    pub close_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 11)]
    pub close_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 12)]
    pub close_day_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 13)]
    pub close_day_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 14)]
    pub close_day_at: outbe_primitives::storage::dsl::Map<B256, B256>,
    #[attribute(order = 15)]
    pub called_group_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 16)]
    pub called_deadline: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 17)]
    pub close_pass_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 18)]
    pub close_cursor: outbe_primitives::storage::dsl::Value<u64>,

    /// Lowest `call_threshold_seconds` of at least a day ever sealed in a reference
    /// currency. With the widest window it bounds the call prices the scan visits.
    #[attribute(order = 19)]
    pub min_call_threshold_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    // Uncalled groups by call price, one trie per reference currency. A call takes
    // the group out, so the daily scan visits only what can still be called.
    #[attribute(order = 20)]
    pub call_bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,
    #[attribute(order = 21)]
    pub call_bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 22)]
    pub call_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 23)]
    pub call_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,
    #[attribute(order = 24)]
    pub call_bin_groups: outbe_primitives::storage::dsl::Map<B256, B256>,
    #[attribute(order = 25)]
    pub call_group_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    /// `(bin << 32) | groups of that bin still to visit`. 0 = start from the lowest bin.
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

    /// Each group's unsettled Credis.
    #[attribute(order = 32)]
    pub group_members:
        outbe_primitives::storage::dsl::Map<B256, outbe_primitives::storage::dsl::List<U256>>,
    #[attribute(order = 33)]
    pub group_member_position: outbe_primitives::storage::dsl::Map<U256, u32>,

    // --- Group terms, sealed by its first member.
    #[attribute(order = 34)]
    pub group_currency: outbe_primitives::storage::dsl::Map<B256, u16>,
    /// The first full day every member counts call days from.
    #[attribute(order = 35)]
    pub group_start_day: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 36)]
    pub group_call_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,
    #[attribute(order = 37)]
    pub group_call_window_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 38)]
    pub group_call_threshold_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    #[attribute(order = 39)]
    pub group_call_notice_period_seconds: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// Block timestamp the group was called. `0` until then.
    #[attribute(order = 40)]
    pub group_called_at: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 41)]
    pub group_settlement_deadline: outbe_primitives::storage::dsl::Map<B256, u64>,
    /// `sub_series_key`s of the group's sub-series.
    #[attribute(order = 42)]
    pub group_sub_series:
        outbe_primitives::storage::dsl::Map<B256, outbe_primitives::storage::dsl::List<B256>>,

    // The queue orders forfeited groups by deadline day while their Credis await deletion.
    #[attribute(order = 43)]
    pub forfeit_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 44)]
    pub forfeit_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 45)]
    pub forfeit_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 46)]
    pub forfeit_day_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 47)]
    pub forfeit_day_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 48)]
    pub forfeit_day_at: outbe_primitives::storage::dsl::Map<B256, B256>,
    #[attribute(order = 49)]
    pub forfeit_group_slot: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 50)]
    pub forfeit_deadline: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 51)]
    pub forfeit_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 52)]
    pub forfeit_cursor: outbe_primitives::storage::dsl::Value<u64>,

    /// Each account's amount and outstanding amount over its live Credis.
    #[attribute(order = 53)]
    pub owner_amount_minor: outbe_primitives::storage::dsl::Map<Address, U256>,
    #[attribute(order = 54)]
    pub owner_outstanding_minor: outbe_primitives::storage::dsl::Map<Address, U256>,
    #[attribute(order = 55)]
    pub daily_phase: outbe_primitives::storage::dsl::Value<u8>,
    #[attribute(order = 56)]
    pub daily_budget: outbe_primitives::storage::dsl::Value<U256>,

    // --- Sub-series, keyed by `sub_series_key(series, sub_series)`, sealed by its first member.
    /// The group the sub-series is a member of. Zero until it opens.
    #[attribute(order = 57)]
    pub sub_series_group: outbe_primitives::storage::dsl::Map<B256, B256>,
    #[attribute(order = 58)]
    pub sub_series_series_id: outbe_primitives::storage::dsl::Map<B256, SeriesId>,
    #[attribute(order = 59)]
    pub sub_series_number: outbe_primitives::storage::dsl::Map<B256, u8>,
    #[attribute(order = 60)]
    pub sub_series_issuance_currency: outbe_primitives::storage::dsl::Map<B256, u16>,
    #[attribute(order = 61)]
    pub sub_series_entry_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,
    #[attribute(order = 62)]
    pub sub_series_call_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,
    #[attribute(order = 63)]
    pub sub_series_unsettled_count: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// Outstanding amount of the sub-series's unsettled Credis.
    #[attribute(order = 64)]
    pub sub_series_outstanding_amount_minor: outbe_primitives::storage::dsl::Map<B256, U256>,
}

/// One sub-series of a call group, as the call and close events report it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubSeriesSummary {
    pub series_id: SeriesId,
    pub sub_series: u8,
    pub issuance_currency: u16,
    pub unsettled_count: u32,
    pub outstanding_amount_minor: U256,
}

impl CredisContract<'_> {
    /// `keccak256(series_id || owner || block_number)` with the 14-byte series id,
    /// the 20-byte owner address and the execution block number as a big-endian u64.
    pub fn generate_credis_id(series_id: SeriesId, owner: Address, block_number: u64) -> U256 {
        series_id.right_id(&[owner.as_slice(), &block_number.to_be_bytes()])
    }

    pub fn sub_series_key(series_id: SeriesId, sub_series: u8) -> B256 {
        let mut buf = [0u8; 15];
        buf[..14].copy_from_slice(series_id.as_bytes());
        buf[14] = sub_series;
        keccak256(buf)
    }

    /// The series a Credis reserved at `reserved_at` for `(issuance_currency, reference_currency)`
    /// belongs to: `SeriesId::for_pair(utc_day(reserved_at), issuance_currency, reference_currency)`.
    pub fn series_of(
        reserved_at: u64,
        issuance_currency: u16,
        reference_currency: u16,
    ) -> outbe_primitives::error::Result<SeriesId> {
        SeriesId::for_pair(
            timestamp_to_date_key(reserved_at),
            issuance_currency,
            reference_currency,
        )
        .map_err(|err| outbe_primitives::error::PrecompileError::Fatal(err.to_string()))
    }

    /// The sub-series of a Credis reserved at `reserved_at`: its UTC hour, 0-23.
    pub fn sub_series_of(reserved_at: u64) -> u8 {
        ((reserved_at % SECONDS_PER_DAY) / SECONDS_PER_HOUR) as u8
    }
}
