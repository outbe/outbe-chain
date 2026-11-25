//! Destination gas of the messages Outbe sends, as `IntexGas` budgets them, and the gas limit of
//! their Interchain Account call.

pub const INTEX_AUCTION_START_BASE: u64 = 365_000;
pub const INTEX_AUCTION_START_PER_PRICE: u64 = 35_000;
pub const INTEX_AUCTION_CLEARING_START: u64 = 2_300_000;
pub const INTEX_AUCTION_CLEARING_START_MAX: u64 = 14_000_000;
pub const INTEX_AUCTION_RESULT: u64 = 251_000;
pub const INTEX_AUCTION_CANCEL: u64 = 225_000;
pub const INTEX_SERIES_CALL_NOTICE_BASE: u64 = 130_000;
pub const INTEX_SERIES_CALL_NOTICE_PER_SERIES: u64 = 45_000;
pub const ISSUANCE_BASE: u64 = 250_000;
pub const ISSUANCE_PER_SERIES: u64 = 230_000;
pub const RETURN_BASE: u64 = 207_000;
pub const RETURN_SETTLE_BASE: u64 = 110_000;
pub const RETURN_PER_ITEM: u64 = 9_250;
pub const RETURN_PROCEEDS_ROUTE: u64 = 400_000;

/// The ICA router, the account creation at the first message and the multicall, as the default
/// gas of `CrosschainBridge.callRemote` budgets an Interchain Account call.
pub const ICA_OVERHEAD: u64 = 200_000;

pub fn intex_auction_start(price_count: usize) -> u64 {
    INTEX_AUCTION_START_BASE + price_count as u64 * INTEX_AUCTION_START_PER_PRICE
}

pub fn intex_auction_clearing_start(asked: u64) -> u64 {
    asked.clamp(
        INTEX_AUCTION_CLEARING_START,
        INTEX_AUCTION_CLEARING_START_MAX,
    )
}

pub fn intex_series(series_count: usize) -> u64 {
    ISSUANCE_BASE + series_count as u64 * ISSUANCE_PER_SERIES
}

pub fn intex_series_call_notice(series_count: usize) -> u64 {
    INTEX_SERIES_CALL_NOTICE_BASE + series_count as u64 * INTEX_SERIES_CALL_NOTICE_PER_SERIES
}

pub fn intex_auction_winners(winners_count: usize) -> u64 {
    if winners_count == 0 {
        return RETURN_BASE;
    }
    RETURN_BASE
        + RETURN_SETTLE_BASE
        + winners_count as u64 * RETURN_PER_ITEM
        + RETURN_PROCEEDS_ROUTE
}

/// The router keeps 1/64 at its call of the account and the account 1/64 at its call of
/// TargetRouter, so the limit covers both shares.
pub fn ica_gas_limit(message_gas: u64) -> u64 {
    let to_account = message_gas + message_gas.div_ceil(63);
    to_account + to_account.div_ceil(63) + ICA_OVERHEAD
}
