use super::*;

fn renewal_epoch(number: U256, start: u64, length: u32) -> RenewalEpoch {
    RenewalEpoch {
        finalized_height: 10,
        finalized_hash: B256::repeat_byte(1),
        finalized_timestamp: 100,
        epoch: outbe_primitives::validator_dto::EpochSnapshot {
            number,
            start_timestamp: 0,
            start_block: start,
            length_blocks: length,
        },
    }
}
#[test]
fn renewal_schedule_caps_window_and_keeps_checked_arithmetic_error_order() {
    let config = TeeRenewalScheduleConfigV1 {
        dkg_prepare_window_blocks: 100,
        minimum_block_time_millis: 1000,
    };
    let schedule = renewal_epoch(U256::from(1), 5, 20)
        .schedule(config)
        .unwrap();
    assert_eq!(
        (
            schedule.next_freeze_height,
            schedule.planned_activation_height
        ),
        (5, 25)
    );
    assert_eq!(schedule.dkg_prepare_window_blocks, 20);
    for (number, start, length, message) in [
        (
            U256::from(u64::MAX) + U256::from(1),
            u64::MAX,
            0,
            "finalized epoch number exceeds u64",
        ),
        (U256::ZERO, u64::MAX, 0, "finalized epoch length is zero"),
        (
            U256::ZERO,
            u64::MAX,
            1,
            "planned activation height overflow",
        ),
    ] {
        let error = renewal_epoch(number, start, length)
            .schedule(config)
            .unwrap_err();
        assert_eq!(error.message(), message);
        assert_eq!(error.code(), jsonrpsee::types::error::INTERNAL_ERROR_CODE);
    }
}
