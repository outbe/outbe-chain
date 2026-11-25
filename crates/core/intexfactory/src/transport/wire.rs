//! The messages Outbe sends to the target chains, byte for byte as `BridgeMsgCodec` encodes them.

use alloy_primitives::{Address, Bytes, FixedBytes};
use alloy_sol_types::{sol, SolValue};
use outbe_primitives::error::PrecompileError;

pub const BODY_VERSION_V1: u8 = 1;

pub const MSG_BIDS_PAGE: u8 = 1;
pub const MSG_BIDS_TOTALS: u8 = 2;
pub const MSG_INTEX_AUCTION_START: u8 = 3;
pub const MSG_INTEX_AUCTION_CLEARING_START: u8 = 4;
pub const MSG_INTEX_AUCTION_RESULT: u8 = 5;
pub const MSG_INTEX_SERIES: u8 = 6;
pub const MSG_INTEX_AUCTION_WINNERS: u8 = 7;
pub const MSG_INTEX_SERIES_CALL_NOTICE: u8 = 8;
pub const MSG_BIDS_PAUSE: u8 = 9;
pub const MSG_INTEX_AUCTION_CANCEL: u8 = 10;

pub const MAX_PAYLOAD_ARRAY_LEN: usize = 64;
pub const MAX_SERIES_PER_ISSUANCE: usize = 16;
pub const MAX_SERIES_PER_CALL_NOTICE: usize = 8;
pub const MAX_PAGES: u16 = 256;

sol! {
    #[derive(Debug, PartialEq, Eq)]
    struct IntexSeriesPayload {
        bytes14 seriesId;
        uint32 emissionDay;
        uint64 issuedAt;
        uint32 issuedUnits;
        uint128 promisLoadMinor;
        uint64 entryPriceMinor;
        uint32 callNoticePeriod;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint32 callWindow;
        uint32 callThreshold;
        uint64 callPriceMinor;
        uint32 transitMargin;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceCurrencyPrice {
    pub iso_code: u16,
    pub entry_price_minor: u64,
    pub call_price_minor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntexAuctionStart {
    pub emission_day: u32,
    pub commit_end: u32,
    pub reveal_end: u32,
    pub issuance_end: u32,
    pub promis_load_minor: u128,
    pub min_bid_rate: u32,
    pub prices: Vec<ReferenceCurrencyPrice>,
    pub call_notice_period: u32,
    pub call_window: u32,
    pub call_threshold: u32,
    pub min_bid_quantity: u16,
    pub bid_bond_minor: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntexAuctionResult {
    pub emission_day: u32,
    pub won_quantity: u32,
    pub won_bids_count: u32,
    pub quantity: u32,
    pub clearing_rate: u64,
    pub total_demand: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntexAuctionWinners {
    pub emission_day: u32,
    pub page_index: u16,
    pub total_pages: u16,
    pub clearing_rate: u64,
    pub basis: u128,
    pub winners: Vec<Address>,
    pub partial_index: u16,
    pub partial_won: u16,
}

fn invalid(msg_type: u8) -> PrecompileError {
    crate::errors::IntexFactoryError::InvalidWireMessage(msg_type).into()
}

fn header(msg_type: u8) -> Vec<u8> {
    vec![BODY_VERSION_V1, msg_type]
}

fn with_body(msg_type: u8, body: Vec<u8>) -> Bytes {
    let mut out = header(msg_type);
    out.extend_from_slice(&body);
    out.into()
}

fn check_page(msg_type: u8, page_index: u16, total_pages: u16) -> Result<(), PrecompileError> {
    if total_pages == 0 || total_pages > MAX_PAGES || page_index >= total_pages {
        return Err(invalid(msg_type));
    }
    Ok(())
}

pub fn encode_intex_auction_start(start: &IntexAuctionStart) -> Result<Bytes, PrecompileError> {
    let rows = u8::try_from(start.prices.len()).map_err(|_| invalid(MSG_INTEX_AUCTION_START))?;
    if rows == 0 {
        return Err(invalid(MSG_INTEX_AUCTION_START));
    }
    let mut out = header(MSG_INTEX_AUCTION_START);
    out.extend_from_slice(&start.emission_day.to_be_bytes());
    out.extend_from_slice(&start.commit_end.to_be_bytes());
    out.extend_from_slice(&start.reveal_end.to_be_bytes());
    out.extend_from_slice(&start.issuance_end.to_be_bytes());
    out.extend_from_slice(&start.promis_load_minor.to_be_bytes());
    out.extend_from_slice(&start.min_bid_rate.to_be_bytes());
    out.extend_from_slice(&start.call_notice_period.to_be_bytes());
    out.extend_from_slice(&start.call_window.to_be_bytes());
    out.extend_from_slice(&start.call_threshold.to_be_bytes());
    out.extend_from_slice(&start.min_bid_quantity.to_be_bytes());
    out.extend_from_slice(&start.bid_bond_minor.to_be_bytes());
    out.push(rows);
    for price in &start.prices {
        out.extend_from_slice(&price.iso_code.to_be_bytes());
        out.extend_from_slice(&price.entry_price_minor.to_be_bytes());
        out.extend_from_slice(&price.call_price_minor.to_be_bytes());
    }
    Ok(out.into())
}

pub fn encode_intex_auction_cancel(emission_day: u32, reason: u8) -> Bytes {
    let mut out = header(MSG_INTEX_AUCTION_CANCEL);
    out.extend_from_slice(&emission_day.to_be_bytes());
    out.push(reason);
    out.into()
}

pub fn encode_intex_auction_clearing_start(emission_day: u32) -> Bytes {
    let mut out = header(MSG_INTEX_AUCTION_CLEARING_START);
    out.extend_from_slice(&emission_day.to_be_bytes());
    out.into()
}

pub fn encode_intex_auction_result(result: &IntexAuctionResult) -> Bytes {
    let mut out = header(MSG_INTEX_AUCTION_RESULT);
    out.extend_from_slice(&result.emission_day.to_be_bytes());
    out.extend_from_slice(&result.won_quantity.to_be_bytes());
    out.extend_from_slice(&result.won_bids_count.to_be_bytes());
    out.extend_from_slice(&result.quantity.to_be_bytes());
    out.extend_from_slice(&result.clearing_rate.to_be_bytes());
    out.extend_from_slice(&result.total_demand.to_be_bytes());
    out.into()
}

pub fn encode_intex_series(
    emission_day: u32,
    page_index: u16,
    total_pages: u16,
    series: &[IntexSeriesPayload],
) -> Result<Bytes, PrecompileError> {
    check_page(MSG_INTEX_SERIES, page_index, total_pages)?;
    if series.is_empty() || series.len() > MAX_SERIES_PER_ISSUANCE {
        return Err(invalid(MSG_INTEX_SERIES));
    }
    if series.iter().any(|s| s.emissionDay != emission_day) {
        return Err(invalid(MSG_INTEX_SERIES));
    }
    let body = (emission_day, page_index, total_pages, series.to_vec()).abi_encode_params();
    Ok(with_body(MSG_INTEX_SERIES, body))
}

pub fn encode_intex_auction_winners(
    winners: &IntexAuctionWinners,
) -> Result<Bytes, PrecompileError> {
    check_page(
        MSG_INTEX_AUCTION_WINNERS,
        winners.page_index,
        winners.total_pages,
    )?;
    if winners.winners.len() > MAX_PAYLOAD_ARRAY_LEN {
        return Err(invalid(MSG_INTEX_AUCTION_WINNERS));
    }
    if winners.partial_won != 0 && usize::from(winners.partial_index) >= winners.winners.len() {
        return Err(invalid(MSG_INTEX_AUCTION_WINNERS));
    }
    let body = (
        winners.emission_day,
        winners.page_index,
        winners.total_pages,
        winners.clearing_rate,
        winners.basis,
        winners.winners.clone(),
        winners.partial_index,
        winners.partial_won,
    )
        .abi_encode_params();
    Ok(with_body(MSG_INTEX_AUCTION_WINNERS, body))
}

pub fn encode_intex_series_call_notice(
    emission_day: u32,
    called_at: u64,
    series_ids: &[FixedBytes<14>],
) -> Result<Bytes, PrecompileError> {
    if series_ids.is_empty() || series_ids.len() > MAX_SERIES_PER_CALL_NOTICE {
        return Err(invalid(MSG_INTEX_SERIES_CALL_NOTICE));
    }
    let body = (emission_day, called_at, series_ids.to_vec()).abi_encode_params();
    Ok(with_body(MSG_INTEX_SERIES_CALL_NOTICE, body))
}

/// A message of a target chain to Desis, with the day and the chain that its body names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BidsMessage {
    Page {
        emission_day: u32,
        src_chain_id: u32,
        page_index: u16,
        total_pages: u16,
        bidders: Vec<Address>,
        packed_bids: Vec<alloy_primitives::U256>,
    },
    Totals {
        emission_day: u32,
        src_chain_id: u32,
        total_pages: u16,
        total_bids: u32,
    },
    Pause {
        emission_day: u32,
        src_chain_id: u32,
    },
}

impl BidsMessage {
    pub fn emission_day(&self) -> u32 {
        match self {
            Self::Page { emission_day, .. }
            | Self::Totals { emission_day, .. }
            | Self::Pause { emission_day, .. } => *emission_day,
        }
    }

    pub fn src_chain_id(&self) -> u32 {
        match self {
            Self::Page { src_chain_id, .. }
            | Self::Totals { src_chain_id, .. }
            | Self::Pause { src_chain_id, .. } => *src_chain_id,
        }
    }
}

fn be_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn be_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

/// Decodes BIDS_PAGE, BIDS_TOTALS and BIDS_PAUSE as `BridgeMsgCodec` encodes them; `None` for any
/// other message.
pub fn decode_bids_message(payload: &[u8]) -> Option<BidsMessage> {
    if payload.len() < 2 || payload[0] != BODY_VERSION_V1 {
        return None;
    }
    match payload[1] {
        MSG_BIDS_PAGE => {
            let (emission_day, src_chain_id, page_index, total_pages, bidders, packed_bids) =
                <(
                    u32,
                    u32,
                    u16,
                    u16,
                    Vec<Address>,
                    Vec<alloy_primitives::U256>,
                )>::abi_decode_params(&payload[2..])
                .ok()?;
            (bidders.len() == packed_bids.len() && bidders.len() <= MAX_PAYLOAD_ARRAY_LEN)
                .then_some(BidsMessage::Page {
                    emission_day,
                    src_chain_id,
                    page_index,
                    total_pages,
                    bidders,
                    packed_bids,
                })
        }
        MSG_BIDS_TOTALS if payload.len() == 16 => Some(BidsMessage::Totals {
            emission_day: be_u32(payload, 2),
            src_chain_id: be_u32(payload, 6),
            total_pages: be_u16(payload, 10),
            total_bids: be_u32(payload, 12),
        }),
        MSG_BIDS_PAUSE if payload.len() == 14 => Some(BidsMessage::Pause {
            emission_day: be_u32(payload, 2),
            src_chain_id: be_u32(payload, 6),
        }),
        _ => None,
    }
}

/// The chain of an ERC-7930 EVM address when the address is `expected`, as `InteroperableAddress`
/// formats it: version 1, chain type 0, the chain id in its fewest bytes, then the 20 address bytes.
pub fn evm_chain_of(interop: &[u8], expected: Address) -> Option<u32> {
    let (head, rest) = interop.split_at_checked(5)?;
    if head[..4] != [0x00, 0x01, 0x00, 0x00] {
        return None;
    }
    let chain_len = usize::from(head[4]);
    if chain_len == 0 || chain_len > 4 || rest.len() != chain_len + 1 + 20 || rest[chain_len] != 20
    {
        return None;
    }
    if rest[chain_len + 1..] != expected[..] {
        return None;
    }
    Some(
        rest[..chain_len]
            .iter()
            .fold(0u32, |acc, byte| (acc << 8) | u32::from(*byte)),
    )
}
