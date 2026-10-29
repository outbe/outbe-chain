use outbe_cycle::handler::{protocol_day_action, ProtocolDayAction};

#[test]
fn same_day_has_no_daily_economic_action() {
    assert_eq!(
        protocol_day_action(20_260_817, 20_260_817).unwrap(),
        ProtocolDayAction::SameDay
    );
}

#[test]
fn contiguous_day_transition_settles_the_previous_day() {
    assert_eq!(
        protocol_day_action(20_260_817, 20_260_818).unwrap(),
        ProtocolDayAction::SettlePrevious { day: 20_260_817 }
    );
}

#[test]
fn contiguous_transition_crosses_month_and_year_boundaries() {
    assert_eq!(
        protocol_day_action(20_251_231, 20_260_101).unwrap(),
        ProtocolDayAction::SettlePrevious { day: 20_251_231 }
    );
}

#[test]
fn three_day_halt_settles_oldest_unallocated_day_first() {
    assert_eq!(
        protocol_day_action(20_260_817, 20_260_820).unwrap(),
        ProtocolDayAction::SettlePrevious { day: 20_260_817 }
    );
}

#[test]
fn active_day_after_block_day_is_rejected() {
    let error = protocol_day_action(20_260_820, 20_260_819).unwrap_err();
    match error {
        outbe_primitives::error::PrecompileError::RevertBytes(bytes) => {
            let selector = alloy_primitives::keccak256(b"ActiveUtcDayAfterBlock(uint32,uint32)");
            assert_eq!(&bytes[..4], &selector[..4]);
        }
        other => panic!("expected typed revert bytes, got {other:?}"),
    }
}

#[test]
fn invalid_calendar_day_is_rejected() {
    let error = protocol_day_action(20_260_231, 20_260_301).unwrap_err();
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::Fatal(ref msg) if msg.contains("invalid UTC day")
    ));

    let block_error = protocol_day_action(20_260_228, 20_260_231).unwrap_err();
    match block_error {
        outbe_primitives::error::PrecompileError::RevertBytes(bytes) => {
            let selector = alloy_primitives::keccak256(b"InvalidBlockUtcDay(uint32,uint32)");
            assert_eq!(&bytes[..4], &selector[..4]);
        }
        other => panic!("expected typed revert bytes, got {other:?}"),
    }
}
