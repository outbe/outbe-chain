//! Storage schema for the Credis contract.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::CREDIS_ADDRESS;

use crate::errors::CredisError;

/// Position lifecycle state.
///
/// A position is settleable from the moment it opens. `Open -> Called` is the
/// sustained-breach trigger. Both `Settled` (fully repaid) and `Void` (call
/// window lapsed with a remainder) are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CredisState {
    Open = 0,
    Called = 1,
    Settled = 2,
    Void = 3,
}

impl CredisState {
    pub fn from_u8(value: u8) -> Result<Self, CredisError> {
        match value {
            0 => Ok(Self::Open),
            1 => Ok(Self::Called),
            2 => Ok(Self::Settled),
            3 => Ok(Self::Void),
            other => Err(CredisError::InvalidStateValue(other)),
        }
    }

    /// True once the position can no longer change: fully repaid or voided.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Settled | Self::Void)
    }
}

/// Position record. Keyed by `keccak256(cca || smart_account || asset || block_number)`.
///
/// Every term, both currency codes included, is sealed at opening and never
/// changes afterwards. Only `outstanding_principal_minor`, `outstanding_gratis_minor`,
/// `interest_paid_minor`, `last_settled_at`, `called_at` and `state` move over the
/// position's life.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = smart_account)]
pub struct Position {
    #[key]
    pub position_id: U256,

    /// The card bundle the loan was disbursed to.
    #[attribute(order = 0)]
    pub smart_account: Address,

    /// The agent that originated the position. Carries the accountability for
    /// how it resolves.
    #[attribute(order = 1)]
    pub cca: Address,

    /// The stablecoin the position is denominated and disbursed in.
    #[attribute(order = 2)]
    pub asset: Address,

    /// ISO 4217 numeric code of `asset` (e.g. 840 = USD), read at opening.
    /// Denominates the position and keys its policy rate. It is NOT the call
    /// threshold anchor. See [`Self::reference_currency`].
    #[attribute(order = 3)]
    pub issuance_currency: u16,

    /// Main account whose pledged Gratis backs the position. Repayments release
    /// collateral to it and a default burns it from it.
    #[attribute(order = 4)]
    pub source: Address,

    /// `P` - stablecoin minor units disbursed. Fixed.
    #[attribute(order = 5)]
    pub principal_minor: U256,

    /// `P_out` - outstanding principal. Reaching zero closes the position.
    #[attribute(order = 6)]
    pub outstanding_principal_minor: U256,

    /// `G` - pledged Gratis, valued 1:1 against principal at the pledge quote
    /// rate (COEN/`issuance_currency`, sealed into the ticket). Fixed.
    #[attribute(order = 7)]
    pub gratis_minor: U256,

    /// The share of `G` still locked. Released principal-proportionally.
    #[attribute(order = 8)]
    pub outstanding_gratis_minor: U256,

    /// `r` - the currency's annual official policy rate (scale `1e6`) times the
    /// policy-rate factor, pinned at opening for the position's life.
    #[attribute(order = 9)]
    pub policy_rate: U256,

    /// Principal / Gratis, in the issuance currency (scale `1e6`). Sealed on the
    /// pledge and copied here. Not an oracle quote and not the call anchor.
    #[attribute(order = 10)]
    pub entry_price_minor: U256,

    /// `call_anchor_price_minor * 164 / 100`, in the reference currency (scale `1e6`).
    /// The daily scan calls the position when 21 of the last 28 finalized
    /// COEN/`reference_currency` VWAPs are strictly above this price. Immutable.
    #[attribute(order = 11)]
    pub call_price_minor: U256,

    /// Issuance timestamp. The interest anchor starts here, and the call scan
    /// ignores daily VWAPs from before this instant's UTC day.
    #[attribute(order = 12)]
    pub issued_at: u64,

    /// Start of the current accrual period. Equals `issued_at` until the
    /// first settlement. Then it advances by the whole days that each settlement
    /// charges, not to the settlement timestamp. Thus a sub-day remainder
    /// carries forward and is not discarded.
    #[attribute(order = 13)]
    pub last_settled_at: u64,

    /// 0 until the position is called.
    #[attribute(order = 14, default = 0)]
    pub called_at: u64,

    /// Lifecycle state as `u8`. Decode it via [`CredisState::from_u8`].
    #[attribute(order = 15)]
    pub state: u8,

    /// ISO 4217 numeric code of the reference currency elected at issuance
    /// and fixed for the position's life. `call_anchor_price_minor` and `call_price_minor`
    /// are quoted here, and the daily breach scan reads the
    /// COEN/`reference_currency` series. It does not denominate `entry_price_minor`.
    #[attribute(order = 16)]
    pub reference_currency: u16,

    /// Call Notice Period in seconds: a called position whose remainder is
    /// still outstanding at `called_at + call_notice_period_seconds` is voided.
    /// Snapshot of the protocol constant at opening.
    #[attribute(order = 17, default = 0)]
    pub call_notice_period_seconds: u32,

    /// Call-price markup percent (snapshot of `CALL_RATE_PCT` at issuance).
    /// Applied to `call_anchor_price_minor`, not to `entry_price_minor` (64 => 1.64x).
    #[attribute(order = 18, default = 0)]
    pub call_rate: u16,

    /// Call-trigger evaluation window in seconds (snapshot of the protocol
    /// constant at opening). This is the trailing span the daily scan reads for
    /// Call Price breaches. Divided by 86400 to get the day count.
    #[attribute(order = 19, default = 0)]
    pub call_window_seconds: u32,

    /// Breach threshold in seconds (snapshot of the protocol constant at
    /// opening). Divided by 86400 to get the required breach-day count.
    #[attribute(order = 20, default = 0)]
    pub call_threshold_seconds: u32,

    /// COEN price in `reference_currency` (scale `1e6`) sealed at issuance:
    /// the previous closed UTC-day VWAP, independent of spot.
    /// Immutable. `call_price_minor` is this value times 1.64.
    #[attribute(order = 21)]
    pub call_anchor_price_minor: U256,

    /// Lifetime interest collected, in the asset's minor units. The sum of
    /// successful settlement interest deltas. Unpaid interest is left out,
    /// including when the remainder is voided.
    #[attribute(order = 22)]
    pub interest_paid_minor: U256,
}

impl Position {
    pub fn lifecycle_state(&self) -> Result<CredisState, CredisError> {
        CredisState::from_u8(self.state)
    }
}

/// EVM storage layout for the Credis position contract.
///
/// `address_position_*` and `total_positions` / `position_id_at_index` provide
/// dense, no-`Vec` enumeration in the same shape as `outbe-nod`'s owner index
/// (`crates/core/nod/src/schema.rs`). Slot assignment is macro-generated: a
/// `Map<K, V>` over a record reserves `V::SLOTS` top-level slots, so the
/// positions map alone spans as many slots as `Position` has attributes.
#[storage_schema]
#[contract(addr = CREDIS_ADDRESS)]
pub struct CredisContract {
    /// Position record keyed by position_id.
    #[attribute(order = 0)]
    pub positions: outbe_primitives::storage::dsl::Map<U256, Position>,

    /// Per-account count of positions ever created.
    #[attribute(order = 1)]
    pub address_position_counts: outbe_primitives::storage::dsl::Map<Address, u32>,

    /// Per-account index - keccak(addr ++ idx_be32) -> position_id.
    #[attribute(order = 2)]
    pub address_position_ids: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// Total positions ever created (backs `totalSupply` / `positionByIndex`).
    #[attribute(order = 3)]
    pub total_positions: outbe_primitives::storage::dsl::Value<u64>,

    /// Dense index - index -> position_id.
    #[attribute(order = 4)]
    pub position_id_at_index: outbe_primitives::storage::dsl::Map<u64, U256>,

    /// Per-account count of positions currently `Called`, backing the
    /// `hasCalledPosition` view.
    #[attribute(order = 7)]
    pub called_position_counts: outbe_primitives::storage::dsl::Map<Address, u32>,

    /// Widest `call_window_seconds` ever opened in a reference currency, in seconds. It
    /// only grows, so the trailing span the daily scan collects always covers a
    /// position whose sealed window outruns the current constant.
    #[attribute(order = 8)]
    pub max_call_window_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    // Called positions, queued by the hour their settlement deadline falls in.
    #[attribute(order = 9)]
    pub expiry_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 10)]
    pub expiry_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 11)]
    pub expiry_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 12)]
    pub expiry_bucket_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 13)]
    pub expiry_bucket_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 14)]
    pub expiry_bucket_at: outbe_primitives::storage::dsl::Map<B256, U256>,
    #[attribute(order = 15)]
    pub called_position_slot: outbe_primitives::storage::dsl::Map<U256, u64>,
    #[attribute(order = 16)]
    pub called_deadline: outbe_primitives::storage::dsl::Map<U256, u64>,
    #[attribute(order = 17)]
    pub expiry_sweep_hour: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 18)]
    pub expiry_cursor: outbe_primitives::storage::dsl::Value<u64>,

    /// Lowest `call_threshold_seconds` of at least a day ever opened in a reference
    /// currency. With the widest window it bounds the call prices the scan visits.
    #[attribute(order = 19)]
    pub min_call_threshold_seconds: outbe_primitives::storage::dsl::Map<u16, u32>,

    // Open positions by call price, one trie per reference currency. A call takes
    // the position out, so the daily scan visits only what can still be called.
    #[attribute(order = 20)]
    pub call_bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,
    #[attribute(order = 21)]
    pub call_bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 22)]
    pub call_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 23)]
    pub call_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,
    #[attribute(order = 24)]
    pub call_bin_positions: outbe_primitives::storage::dsl::Map<B256, U256>,
    #[attribute(order = 25)]
    pub call_position_slot: outbe_primitives::storage::dsl::Map<U256, u64>,
    /// `(bin << 32) | positions of that bin still to visit`. 0 = start from the lowest bin.
    #[attribute(order = 26)]
    pub call_bin_cursor: outbe_primitives::storage::dsl::Map<u16, u64>,
    /// UTC day the call sweep could not price a reference currency on. The rest of that
    /// day's pass passes it by.
    #[attribute(order = 27)]
    pub call_scan_failed_day: outbe_primitives::storage::dsl::Map<u16, u32>,

    /// ISO 4217 code of the currency the call sweep resumes at.
    #[attribute(order = 28)]
    pub call_currency_cursor: outbe_primitives::storage::dsl::Value<u32>,
    /// UTC day the unfinished call sweep is pinned to. 0 = none in flight.
    #[attribute(order = 29)]
    pub call_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    /// UTC day waiting behind it. 0 = none.
    #[attribute(order = 30)]
    pub call_pending_day: outbe_primitives::storage::dsl::Value<u32>,
}

impl CredisContract<'_> {
    /// `keccak256(cca || smart_account || asset || block_number)` with packed
    /// 20-byte addresses and the execution block number as a big-endian u64.
    pub fn position_id(
        cca: Address,
        smart_account: Address,
        asset: Address,
        block_number: u64,
    ) -> U256 {
        let mut buf = [0u8; 68];
        buf[..20].copy_from_slice(cca.as_slice());
        buf[20..40].copy_from_slice(smart_account.as_slice());
        buf[40..60].copy_from_slice(asset.as_slice());
        buf[60..].copy_from_slice(&block_number.to_be_bytes());
        U256::from_be_bytes(keccak256(buf).0)
    }

    /// Composite key for per-address position index: `keccak256(addr ++ idx_be32)`.
    pub fn address_index_key(account: Address, index: u32) -> B256 {
        let mut buf = [0u8; 24];
        buf[0..20].copy_from_slice(account.as_slice());
        buf[20..24].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }
}
