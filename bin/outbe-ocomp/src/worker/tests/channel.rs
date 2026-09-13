use super::*;

#[test]
fn zero_mq_cancel_command_stops_work_at_the_next_execution_checkpoint() {
    let cancelled = AtomicBool::new(false);
    cancelled.store(true, Ordering::Release);
    assert!(matches!(
        require_lease_active(Some(&cancelled)),
        Err(WorkerError::LeaseCancelled)
    ));
}

#[test]
fn accepted_lease_pre_execution_error_becomes_terminal_failure() {
    let unit_id = B256::repeat_byte(0xA5);
    let finished =
        terminal_completion(unit_id, "test-lease", Err(WorkerError::UnitBindingMismatch));
    assert_eq!(finished.unit_id, unit_id);
    assert_eq!(
        finished.status,
        outbe_ocomp_protocol::UnitFinishedStatus::Failed
    );
    assert_eq!(finished.exact_staged_bytes, 0);
    assert_eq!(finished.transport_digest, B256::ZERO);
}
