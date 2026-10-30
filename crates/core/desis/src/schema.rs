//! Storage schema for the Desis module.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::DESIS_ADDRESS;
use outbe_primitives::time::EmissionDate;

/// Auction lifecycle stage, in order: a day is `Briefed`, `Started` for the
/// commit window, `Revealing` for the reveal window, `Clearing` while the
/// fan-in aggregates bids, then terminal `Cleared` / `Cancelled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum AuctionStage {
    #[default]
    None = 0,
    Briefed = 1,
    Started = 2,
    Revealing = 3,
    Clearing = 4,
    Cleared = 5,
    Cancelled = 6,
}

impl AuctionStage {
    pub fn from_u8(value: u8) -> Result<Self, crate::DesisError> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Briefed),
            2 => Ok(Self::Started),
            3 => Ok(Self::Revealing),
            4 => Ok(Self::Clearing),
            5 => Ok(Self::Cleared),
            6 => Ok(Self::Cancelled),
            _ => Err(crate::DesisError::InvalidStageTransition),
        }
    }
}

pub use outbe_intex::IntexCallTrigger;

/// Entry and call price of one reference currency, fixed at auction start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceCurrencyPrice {
    pub iso_code: u16,
    /// Per-unit entry price in ISO stable-units (1e6).
    pub entry_price_minor: U256,
    /// Per-unit call price in ISO stable-units (1e6). IntexFactory `call_terms` derives it
    /// from the entry price at auction start.
    pub call_price_minor: U256,
}

/// Auction configuration (demand side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuctionConfig {
    /// PROMIS-units per Intex unit (1e6). Bounded by uint128.
    pub promis_load_minor: u128,
    /// Call-trigger parameters fixed at auction start by IntexFactory `call_terms`.
    pub call_trigger: IntexCallTrigger,
    /// Minimum acceptable bid rate (1e6 fixed-point, % of the escrow basis). 0 -> no floor.
    pub min_intex_bid_rate: u32,
    /// Minimum bid quantity in Intex units. The floor rescales 4% of the prior issued count
    /// by the prior PROMIS load divided by today's load.
    pub min_intex_bid_quantity: u16,
    /// Commit-entry bond in protocol units. A value of 0 disables the bond.
    pub commit_bond_minor: u128,
    /// One row per reference currency the oracle could price for this day.
    pub reference_prices: Vec<ReferenceCurrencyPrice>,
}

impl AuctionConfig {
    /// Build the demand-side config from the day's per-reference entry prices and
    /// the load the ladder stepped to. `min_intex_bid_rate = 0` means no bid floor.
    /// This function leaves `call_trigger`, `min_intex_bid_quantity` and
    /// `commit_bond_minor` at their defaults. `start_auction` fixes them.
    pub fn from_reference_prices(
        reference_prices: Vec<ReferenceCurrencyPrice>,
        promis_load_minor: u128,
    ) -> Self {
        Self {
            promis_load_minor,
            call_trigger: IntexCallTrigger::default(),
            min_intex_bid_rate: 0,
            min_intex_bid_quantity: 0,
            commit_bond_minor: 0,
            reference_prices,
        }
    }

    /// The day's price row for one reference currency.
    pub fn price_for(&self, iso_code: u16) -> Option<&ReferenceCurrencyPrice> {
        self.reference_prices
            .iter()
            .find(|row| row.iso_code == iso_code)
    }

    /// Per-Intex escrow basis = `promis_load` COEN, which the day is briefed at. The escrow
    /// pays wCOEN, so the bid rate applies against this. entry_price feeds only the call price.
    pub fn escrow_basis_minor(&self) -> u128 {
        self.promis_load_minor
    }
}

/// One bid relayed from a target chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidData {
    pub bidder_address: Address,
    /// Bid rate (1e6 fixed-point, % of the escrow basis).
    pub intex_bid_rate: u32,
    /// Bid timestamp (ordering only).
    pub timestamp: u32,
    /// Requested quantity (Intex units).
    pub intex_quantity: u16,
    /// Declared issuance currency (ISO numeric).
    pub issuance_currency: u16,
    /// Reference currency the bid prices in (ISO numeric).
    pub reference_currency: u16,
}

/// Auction clearing result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClearingResult {
    pub issued_units: u32,
    pub clearing_rate: u32,
    pub winners: Vec<Address>,
    pub winner_quantities: Vec<U256>,
    /// Source chain of each winning bid (parallel to `winners`).
    pub winner_chains: Vec<u32>,
    /// `(issuance, reference)` ISO pair of each winning bid (parallel to `winners`).
    /// The day issues one series per distinct pair.
    pub winner_currencies: Vec<(u16, u16)>,
    /// Index into `winners` of the one bid filled in part, where the Desis Limit ran out.
    pub partial_winner: Option<usize>,
    pub all_bidders: Vec<Address>,
    pub refunded_amounts: Vec<u128>,
    pub paid_amounts: Vec<u128>,
    /// Source chain of each bid (parallel to `all_bidders`).
    pub bidder_chains: Vec<u32>,
}

/// EVM storage layout for the Desis module.
///
/// Per-series: auction config, stage, bid-batch metadata, pending clearing
/// inputs. Bids and clearing results are stored in separate vec-maps.
///
/// Attribute orders are append-only once the chain is live: renumbering or
/// reusing an order changes the slot derivation and needs a chain wipe.
#[storage_schema]
#[contract(addr = DESIS_ADDRESS)]
pub struct DesisContract {
    // --- Auction config (per series) ---
    /// emission_day -> promis_load_minor.
    #[attribute(order = 0)]
    pub config_promis_load_minor: outbe_primitives::storage::dsl::Map<EmissionDate, U256>,
    #[attribute(order = 1)]
    pub config_min_bid_rate: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,
    #[attribute(order = 2)]
    pub config_min_bid_quantity: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,

    // --- Auction stage ---
    /// emission_day -> AuctionStage (u8).
    #[attribute(order = 4)]
    pub auction_stage: outbe_primitives::storage::dsl::Map<EmissionDate, u8>,

    // --- Bid storage (per chain) ---
    /// keccak256(emission_day_be32 ++ chain_be32 ++ index_be32) -> bidder address.
    #[attribute(order = 5)]
    pub bid_bidder: outbe_primitives::storage::dsl::Map<B256, Address>,
    /// Packed bid fields: limbs[0]=rate(u32); limbs[1]=issuance(u16)<<48|
    /// quantity(u16)<<32|timestamp(u32); limbs[2]=reference(u16).
    #[attribute(order = 6)]
    pub bid_packed: outbe_primitives::storage::dsl::Map<B256, U256>,

    // --- Pending clearing ---
    /// emission_day -> Desis Limit in whole Intex units, pending at clearing stage.
    #[attribute(order = 7)]
    pub pending_desis_limit_units: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,

    // --- Global clearing state ---
    /// Most recently cleared emission_day (for minBidQty 4% derivation).
    #[attribute(order = 8)]
    pub last_cleared_emission_day: outbe_primitives::storage::dsl::Value<EmissionDate>,
    /// issuedUnits from the most recent clearing (for minBidQty 4% derivation).
    #[attribute(order = 9)]
    pub last_clearing_issued_count: outbe_primitives::storage::dsl::Value<u32>,

    /// emission_day -> 1 once `arm_clearing` has run. This lets `force_clear` tell a
    /// genuine zero Desis Limit from a clearing that was never initiated.
    #[attribute(order = 10)]
    pub clearing_initiated: outbe_primitives::storage::dsl::Map<EmissionDate, u8>,

    // --- Extended auction config ---
    /// emission_day -> call-trigger window (seconds).
    #[attribute(order = 13)]
    pub config_call_window_seconds: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,
    /// emission_day -> call-trigger threshold (seconds).
    #[attribute(order = 14)]
    pub config_call_threshold_seconds: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,
    /// emission_day -> call cooldown (seconds).
    #[attribute(order = 15)]
    pub config_call_notice_period_seconds: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,
    /// emission_day -> transit margin past the settlement deadline (seconds).
    #[attribute(order = 42)]
    pub config_transit_margin_seconds: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,

    /// emission_day -> commit-entry bond (protocol units).
    #[attribute(order = 16)]
    pub config_commit_bond_minor: outbe_primitives::storage::dsl::Map<EmissionDate, U256>,

    /// series/day -> scheduled auction timestamp, recorded at start (bounded until 2106).
    #[attribute(order = 17)]
    pub auction_at: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,

    // --- Per-chain bid intake (keyed keccak256(emission_day_be32 ++ chain_be32)) ---
    /// totalBatches the chain's first batch declared; every later batch must carry the same.
    #[attribute(order = 19)]
    pub chain_total_batches: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// Bitmap of arrived batchIndices (bit i = batchIndex i seen).
    #[attribute(order = 20)]
    pub chain_arrived_mask: outbe_primitives::storage::dsl::Map<B256, U256>,
    /// Bids accepted from the chain.
    #[attribute(order = 21)]
    pub chain_bid_count: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// 1 once the chain's intake is complete (marker + all batches + totals match).
    #[attribute(order = 22)]
    pub chain_done: outbe_primitives::storage::dsl::Map<B256, u8>,
    /// totalBatches claimed by the chain's BIDS_DONE marker (0 = no marker yet).
    #[attribute(order = 23)]
    pub chain_done_batches: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// totalBids claimed by the chain's BIDS_DONE marker.
    #[attribute(order = 24)]
    pub chain_done_bids: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// emission_day -> bids accepted across all chains (getBidsCount view).
    #[attribute(order = 25)]
    pub day_bid_count: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,

    // --- Clearing fan-in gate ---
    /// emission_day -> deadline after which clearing proceeds without the missing chains.
    #[attribute(order = 26)]
    pub clearing_deadline: outbe_primitives::storage::dsl::Map<EmissionDate, u64>,
    /// Days awaiting the clearing gate (dense active set for the begin-block tick).
    #[attribute(order = 27)]
    pub gate_active_count: outbe_primitives::storage::dsl::Value<u32>,
    /// dense index -> emission_day. Raw `u32` on the value side: the contract
    /// macro treats a non-scalar map value as a `StorageRecord` and would size the
    /// slot span from it, so the day is converted at the read/write sites.
    #[attribute(order = 28)]
    pub gate_active_at: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// emission_day -> (active index + 1); 0 = not active.
    #[attribute(order = 29)]
    pub gate_active_slot: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,

    // --- Auction brief ---
    /// emission_day -> Desis Limit awaiting the auction, in raw PROMIS minor units.
    #[attribute(order = 30)]
    pub pending_desis_limit_minor: outbe_primitives::storage::dsl::Map<EmissionDate, U256>,
    /// emission_day -> 1 when the day was briefed with a limit to sell.
    #[attribute(order = 31)]
    pub brief_has_limit: outbe_primitives::storage::dsl::Map<EmissionDate, u8>,
    /// Days with a scheduled auction (dense active set for the begin-block tick).
    #[attribute(order = 32)]
    pub sched_active_count: outbe_primitives::storage::dsl::Value<u32>,
    /// dense index -> emission_day. Raw `u32` on the value side: the contract
    /// macro treats a non-scalar map value as a `StorageRecord` and would size the
    /// slot span from it, so the day is converted at the read/write sites.
    #[attribute(order = 33)]
    pub sched_active_at: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// emission_day -> (active index + 1); 0 = not active.
    #[attribute(order = 34)]
    pub sched_active_slot: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,

    // --- Entry and call price per reference currency, fixed at auction start ---
    /// emission_day -> number of priced reference currencies.
    #[attribute(order = 35)]
    pub reference_price_count: outbe_primitives::storage::dsl::Map<EmissionDate, u32>,
    /// keccak256(emission_day_be32 ++ index_be32) -> reference currency ISO code.
    #[attribute(order = 36)]
    pub reference_price_iso: outbe_primitives::storage::dsl::Map<B256, u32>,
    /// keccak256(emission_day_be32 ++ index_be32) -> entry price in ISO stable-units (1e6).
    /// The call price derived from it at auction start is stored in `reference_call_price_minor`.
    #[attribute(order = 37)]
    pub reference_entry_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,

    // --- PROMIS load ladder ---
    /// Exponent `k` of the current load, `promis_load_minor = 10^k`. Zero until the
    /// chain has briefed a day it could price.
    #[attribute(order = 38)]
    pub promis_load_exponent: outbe_primitives::storage::dsl::Value<u32>,
    /// iso_code -> price the currency was captured at. Zero until captured.
    #[attribute(order = 39)]
    pub promis_load_reference_minor: outbe_primitives::storage::dsl::Map<u16, U256>,
    /// iso_code -> exponent the price was captured at. A capture under another
    /// exponent is stale.
    #[attribute(order = 40)]
    pub promis_load_reference_exponent: outbe_primitives::storage::dsl::Map<u16, u32>,

    /// keccak256(emission_day_be32 ++ index_be32) -> call price in ISO stable-units (1e6),
    /// fixed at auction start. Same key as `reference_entry_price_minor`.
    #[attribute(order = 41)]
    pub reference_call_price_minor: outbe_primitives::storage::dsl::Map<B256, U256>,
}

impl DesisContract<'_> {
    /// Composite key for per-(day, chain) fields: `keccak256(emission_day_be32 ++ chain_be32)`.
    pub fn chain_key(emission_day: EmissionDate, chain_id: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&emission_day.value().to_be_bytes());
        buf[4..8].copy_from_slice(&chain_id.to_be_bytes());
        keccak256(buf)
    }

    /// Composite key for per-(day, row) price fields.
    pub fn reference_price_key(emission_day: EmissionDate, index: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&emission_day.value().to_be_bytes());
        buf[4..8].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    /// Composite key for per-bid fields: `keccak256(emission_day_be32 ++ chain_be32 ++ index_be32)`.
    pub fn bid_key(emission_day: EmissionDate, chain_id: u32, index: u32) -> B256 {
        let mut buf = [0u8; 12];
        buf[0..4].copy_from_slice(&emission_day.value().to_be_bytes());
        buf[4..8].copy_from_slice(&chain_id.to_be_bytes());
        buf[8..12].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }
}
