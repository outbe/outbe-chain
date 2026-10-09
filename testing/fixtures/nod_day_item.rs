//! Nod item fixture for the day-store tests.

use alloy_primitives::{Address, U256};
use outbe_nod::NodItemState;
use outbe_primitives::time::WorldwideDay;

/// The Nod of `owner` on day `day`, with entry 13, gratis load 11, league 4
/// and ISO 840. The caller supplies the entry price of the terms.
pub(crate) fn nod_day_item(owner: Address, day: u32, entry_price_minor: U256) -> NodItemState {
    let worldwide_day = WorldwideDay::new(day);
    let entry = U256::from(13u64);
    outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id: outbe_nod::identity::generate_nod_id(owner, worldwide_day).unwrap(),
            owner,
            gratis_load_minor: U256::from(11u64),
            worldwide_day,
            league_id: 4,
            bucket_key: outbe_nod::identity::bucket_key(worldwide_day, entry, 840),
            issuance_currency: 840,
            reference_currency: 840,
            issued_at: 1_752_534_000,
        },
        entry_price_minor,
    )
}
