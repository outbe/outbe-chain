use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::first_full_day;

use crate::fixture_kernel::{FixtureKernelExt, TEST_WWD};
use crate::schema::MetadosisContract;

/// 2024-01-01 00:00:00 UTC.
const SCHEDULED_AT: u64 = 1_704_067_200;
const REQUEST_AT: u64 = SCHEDULED_AT + 2 * 86_400 + 3_600;
const EARLY_ACTIVATION: u64 = REQUEST_AT + 3_600;
const LATE_ACTIVATION: u64 = REQUEST_AT + 3 * 86_400 + 60;

fn certified_issued_at(activation_height: u64, activation_time: u64) -> u64 {
    let mut fixture = crate::fixture_kernel::ActivationScenario::new_with_request_clock(
        activation_height,
        activation_time,
        REQUEST_AT,
    );
    StorageHandle::enter(&mut fixture.provider, |storage| {
        MetadosisContract::new(storage)
            .fixture_set_scheduled_process_time(TEST_WWD, SCHEDULED_AT)
            .unwrap();
    });
    fixture
        .apply()
        .expect("activation applies with a request instant distinct from the block");
    fixture.semantic_snapshot().nod.issued_at
}

/// The request instant also fixes the entry price, so a late request cannot count call days
/// that closed before its price.
#[test]
fn certified_nod_issued_at_is_the_request_instant() {
    let early = certified_issued_at(20, EARLY_ACTIVATION);
    let later = certified_issued_at(40, LATE_ACTIVATION);
    const {
        assert!(SCHEDULED_AT < REQUEST_AT);
        assert!(REQUEST_AT < EARLY_ACTIVATION);
        assert!(EARLY_ACTIVATION < LATE_ACTIVATION);
    };
    assert_eq!(early, REQUEST_AT);
    assert_eq!(later, REQUEST_AT);
    assert_eq!(first_full_day(early), 20_240_104);
    assert!(
        first_full_day(early) > outbe_nod::openings::entry_price_day(REQUEST_AT),
        "the call window starts after the priced day"
    );
    assert_ne!(first_full_day(early), first_full_day(SCHEDULED_AT));
    assert_ne!(first_full_day(early), first_full_day(LATE_ACTIVATION));
}
