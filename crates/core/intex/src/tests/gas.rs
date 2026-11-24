//! The destination gas of the messages of Outbe.

use crate::gas::*;

#[test]
fn the_clearing_round_gas_stays_between_its_floor_and_its_cap() {
    assert_eq!(
        intex_auction_clearing_start(0),
        INTEX_AUCTION_CLEARING_START
    );
    assert_eq!(intex_auction_clearing_start(5_000_000), 5_000_000);
    assert_eq!(
        intex_auction_clearing_start(u64::MAX),
        INTEX_AUCTION_CLEARING_START_MAX
    );
}

#[test]
fn a_winners_page_pays_the_settlement_and_the_proceeds_only_with_winners() {
    assert_eq!(intex_auction_winners(0), RETURN_BASE);
    assert_eq!(
        intex_auction_winners(2),
        RETURN_BASE + RETURN_SETTLE_BASE + 2 * RETURN_PER_ITEM + RETURN_PROCEEDS_ROUTE
    );
}

#[test]
fn the_ica_limit_covers_the_forwarded_share_and_the_account() {
    let message = 6_300_000;
    let limit = ica_gas_limit(message);
    assert_eq!(limit, message + 100_000 + ICA_OVERHEAD);
    assert!((limit - ICA_OVERHEAD) * 63 / 64 >= message);
}
