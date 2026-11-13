use outbe_cycle::handler::day_to_allocate;

#[test]
fn same_day_has_no_day_to_allocate() {
    assert_eq!(day_to_allocate(20_260_817, 20_260_817).unwrap(), None);
}

#[test]
fn contiguous_day_transition_allocates_the_previous_day() {
    assert_eq!(
        day_to_allocate(20_260_817, 20_260_818).unwrap(),
        Some(20_260_817)
    );
}

#[test]
fn contiguous_transition_crosses_month_and_year_boundaries() {
    assert_eq!(
        day_to_allocate(20_251_231, 20_260_101).unwrap(),
        Some(20_251_231)
    );
}

#[test]
fn three_day_halt_allocates_oldest_unallocated_day_first() {
    assert_eq!(
        day_to_allocate(20_260_817, 20_260_820).unwrap(),
        Some(20_260_817)
    );
}

#[test]
fn active_day_after_block_day_is_rejected() {
    let error = day_to_allocate(20_260_820, 20_260_819).unwrap_err();
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
    let error = day_to_allocate(20_260_231, 20_260_301).unwrap_err();
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::Fatal(ref msg) if msg.contains("invalid UTC day")
    ));

    let block_error = day_to_allocate(20_260_228, 20_260_231).unwrap_err();
    match block_error {
        outbe_primitives::error::PrecompileError::RevertBytes(bytes) => {
            let selector = alloy_primitives::keccak256(b"InvalidBlockUtcDay(uint32,uint32)");
            assert_eq!(&bytes[..4], &selector[..4]);
        }
        other => panic!("expected typed revert bytes, got {other:?}"),
    }
}
