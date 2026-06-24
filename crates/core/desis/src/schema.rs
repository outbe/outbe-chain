//! Storage schema for the Desis module.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::DESIS_ADDRESS;
use outbe_primitives::units::SCALE_1E18_U128;

use crate::constants::PROMIS_LOAD;

/// Auction lifecycle stage.
///
/// `Revealing` is the bid-collecting window (entered on a green-day reveal);
/// `BidsReceived` follows once bids arrive. `Cleared` / `Cancelled` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum AuctionStage {
    #[default]
    None = 0,
    Started = 1,
    Revealing = 2,
    BidsReceived = 3,
    Cleared = 4,
    Cancelled = 5,
}

impl AuctionStage {
    pub fn from_u8(value: u8) -> Result<Self, crate::DesisError> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Started),
            2 => Ok(Self::Revealing),
            3 => Ok(Self::BidsReceived),
            4 => Ok(Self::Cleared),
            5 => Ok(Self::Cancelled),
            _ => Err(crate::DesisError::InvalidStageTransition),
        }
    }
}

/// Auction configuration (demand side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuctionConfig {
    /// Promis tokens per Intex unit (18 decimals); bounded by uint128.
    pub promis_load_minor: u128,
    /// Minimum acceptable bid price (payment-token decimals). 0 → no floor.
    pub min_intex_bid_price: u64,
    /// Cost amount (payment-token decimals).
    pub cost_amount_minor: u64,
    /// Entry price (per-unit, reference currency, 1e18) captured at auction start;
    /// carried to IntexFactory.issue to derive floor_price_minor and call_price_minor.
    pub entry_price: U256,
}

impl AuctionConfig {
    /// Build the demand-side config from the per-unit entry price (1e18-scaled).
    ///
    /// `cost_amount_minor` is `entry_price * PROMIS_LOAD / 1e12` (payment-token
    /// decimals), rounded up to the next multiple of 100. `promis_load_minor`
    /// scales `PROMIS_LOAD` to 18-dec minor units; `min_intex_bid_price = 0`
    /// means no bid floor.
    pub fn from_entry_price(entry_price: U256) -> Self {
        let cost_amount_u256 =
            entry_price.saturating_mul(U256::from(PROMIS_LOAD)) / U256::from(10u128.pow(12));
        let raw_cost_amount: u64 = cost_amount_u256.try_into().unwrap_or(u64::MAX);
        let cost_amount_minor = raw_cost_amount.div_ceil(100).saturating_mul(100);
        Self {
            promis_load_minor: PROMIS_LOAD.saturating_mul(SCALE_1E18_U128),
            min_intex_bid_price: 0,
            cost_amount_minor,
            entry_price,
        }
    }
}

/// One bid relayed from BNB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidData {
    pub bidder_address: Address,
    /// Bid price (payment-token decimals).
    pub intex_bid_price: u64,
    /// Bid timestamp (ordering only).
    pub timestamp: u32,
    /// Requested quantity (Intex units).
    pub intex_quantity: u16,
}

/// Auction clearing result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClearingResult {
    pub issued_intex_count: u32,
    pub clearing_price: u64,
    pub winners: Vec<Address>,
    pub winner_quantities: Vec<U256>,
    pub all_bidders: Vec<Address>,
    pub refunded_amounts: Vec<u64>,
    pub paid_amounts: Vec<u64>,
}

/// EVM storage layout for the Desis module.
///
/// Per-series: auction config, stage, bid-batch metadata, pending clearing
/// inputs. Bids and clearing results are stored in separate vec-maps.
#[storage_schema]
#[contract(addr = DESIS_ADDRESS)]
pub struct DesisContract {
    // --- Auction config (per series) ---
    /// series_id -> promis_load_minor.
    #[attribute(order = 0)]
    pub config_promis_load_minor: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 1)]
    pub config_min_bid_price: outbe_primitives::storage::dsl::Map<u32, u64>,
    #[attribute(order = 2)]
    pub config_cost_amount_minor: outbe_primitives::storage::dsl::Map<u32, u64>,
    #[attribute(order = 3)]
    pub config_min_bid_quantity: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// Entry price (1e18) captured at auction start; carried to IntexFactory.
    #[attribute(order = 4)]
    pub config_entry_price: outbe_primitives::storage::dsl::Map<u32, U256>,

    // --- Auction stage ---
    /// series_id -> AuctionStage (u8).
    #[attribute(order = 5)]
    pub auction_stage: outbe_primitives::storage::dsl::Map<u32, u8>,

    // --- Bid-batch metadata ---
    /// series_id -> source-chain endpoint id of the last accepted bid batch.
    #[attribute(order = 6)]
    pub bid_source_eid: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// series_id -> highest accepted bid-batch generation.
    #[attribute(order = 7)]
    pub last_bids_generation: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// series_id -> bid count.
    #[attribute(order = 8)]
    pub bid_count: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// keccak256(series_id_be32 ++ index_be32) -> bidder address.
    #[attribute(order = 9)]
    pub bid_bidder: outbe_primitives::storage::dsl::Map<B256, Address>,
    /// Packed bid fields: limbs[0]=price(u64), limbs[1]=quantity(u16)<<32|timestamp(u32).
    #[attribute(order = 10)]
    pub bid_packed: outbe_primitives::storage::dsl::Map<B256, U256>,

    // --- Pending clearing ---
    /// series_id -> supply (Intex units) pending at clearing stage.
    #[attribute(order = 11)]
    pub pending_supply_intex: outbe_primitives::storage::dsl::Map<u32, u32>,

    // --- Global clearing state ---
    /// Most recently cleared series_id (for minBidQty 4% derivation).
    #[attribute(order = 12)]
    pub last_cleared_series_id: outbe_primitives::storage::dsl::Value<u32>,
    /// issuedIntexCount from the most recent clearing (for minBidQty 4% derivation).
    #[attribute(order = 13)]
    pub last_clearing_issued_count: outbe_primitives::storage::dsl::Value<u32>,
}

impl DesisContract<'_> {
    /// Composite key for per-bid fields: `keccak256(series_id_be32 ++ index_be32)`.
    pub fn bid_key(series_id: u32, index: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&series_id.to_be_bytes());
        buf[4..8].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }
}
