use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::first_full_day;

use crate::fixture_kernel::{FixtureKernelExt, TEST_WWD};
use crate::schema::MetadosisContract;

/// 2024-01-01 00:00:00 UTC.
const FREEZE_AT: u64 = 1_704_067_200;
const REQUEST_AT: u64 = FREEZE_AT + 3_600;
const EARLY_ACTIVATION: u64 = FREEZE_AT + 7_200;
const LATE_ACTIVATION: u64 = FREEZE_AT + 3 * 86_400 + 60;

fn certified_issued_at(activation_height: u64, activation_time: u64) -> u64 {
    let mut fixture = crate::fixture_kernel::ActivationScenario::new_with_request_clock(
        activation_height,
        activation_time,
        REQUEST_AT,
    );
    StorageHandle::enter(&mut fixture.provider, |storage| {
        MetadosisContract::new(storage)
            .fixture_set_scheduled_process_time(TEST_WWD, FREEZE_AT)
            .unwrap();
    });
    fixture
        .apply()
        .expect("activation applies with a freeze instant distinct from the block");
    fixture.semantic_snapshot().nod.issued_at
}

#[test]
fn certified_nod_issued_at_is_the_initial_lysis_freeze() {
    let early = certified_issued_at(20, EARLY_ACTIVATION);
    let later = certified_issued_at(40, LATE_ACTIVATION);
    const {
        assert!(FREEZE_AT < REQUEST_AT);
        assert!(REQUEST_AT < EARLY_ACTIVATION);
        assert!(EARLY_ACTIVATION < LATE_ACTIVATION);
    };
    assert_eq!(early, FREEZE_AT);
    assert_eq!(later, FREEZE_AT);
    assert_ne!(early, REQUEST_AT);
    assert_eq!(first_full_day(early), 20_240_101);
    assert_ne!(first_full_day(early), first_full_day(REQUEST_AT));
    assert_ne!(first_full_day(early), first_full_day(LATE_ACTIVATION));
    assert_ne!(
        first_full_day(EARLY_ACTIVATION),
        first_full_day(LATE_ACTIVATION)
    );
}
