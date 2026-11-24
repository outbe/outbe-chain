//! The messages Outbe sends to the target chains, byte for byte as `BridgeMsgCodec` encodes them.

use alloy_primitives::{Address, Bytes, FixedBytes};
use alloy_sol_types::{sol, SolValue};
use outbe_primitives::error::PrecompileError;

pub const BODY_VERSION_V1: u8 = 1;

pub const MSG_INTEX_AUCTION_START: u8 = 3;
pub const MSG_INTEX_AUCTION_CLEARING_START: u8 = 4;
pub const MSG_INTEX_AUCTION_RESULT: u8 = 5;
pub const MSG_INTEX_SERIES: u8 = 6;
pub const MSG_INTEX_AUCTION_WINNERS: u8 = 7;
pub const MSG_INTEX_SERIES_CALL_NOTICE: u8 = 8;
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

fn invalid(reason: &str) -> PrecompileError {
    PrecompileError::Revert(format!("intex wire: {reason}"))
}

fn header(msg_type: u8) -> Vec<u8> {
    vec![BODY_VERSION_V1, msg_type]
}

fn with_body(msg_type: u8, body: Vec<u8>) -> Bytes {
    let mut out = header(msg_type);
    out.extend_from_slice(&body);
    out.into()
}

fn check_page(page_index: u16, total_pages: u16) -> Result<(), PrecompileError> {
    if total_pages == 0 || total_pages > MAX_PAGES || page_index >= total_pages {
        return Err(invalid("page out of range"));
    }
    Ok(())
}

pub fn encode_intex_auction_start(start: &IntexAuctionStart) -> Result<Bytes, PrecompileError> {
    let rows = u8::try_from(start.prices.len()).map_err(|_| invalid("too many price rows"))?;
    if rows == 0 {
        return Err(invalid("no reference prices"));
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
    check_page(page_index, total_pages)?;
    if series.is_empty() || series.len() > MAX_SERIES_PER_ISSUANCE {
        return Err(invalid("series page size"));
    }
    if series.iter().any(|s| s.emissionDay != emission_day) {
        return Err(invalid("series of another day"));
    }
    let body = (emission_day, page_index, total_pages, series.to_vec()).abi_encode_params();
    Ok(with_body(MSG_INTEX_SERIES, body))
}

pub fn encode_intex_auction_winners(
    winners: &IntexAuctionWinners,
) -> Result<Bytes, PrecompileError> {
    check_page(winners.page_index, winners.total_pages)?;
    if winners.winners.len() > MAX_PAYLOAD_ARRAY_LEN {
        return Err(invalid("too many winners"));
    }
    if winners.partial_won != 0 && usize::from(winners.partial_index) >= winners.winners.len() {
        return Err(invalid("partial fill outside the page"));
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
        return Err(invalid("call notice page size"));
    }
    let body = (emission_day, called_at, series_ids.to_vec()).abi_encode_params();
    Ok(with_body(MSG_INTEX_SERIES_CALL_NOTICE, body))
}
