use super::*;

#[test]
fn dynamic_oracle_refresh_is_staged_three_hours_before_daily_cycle() {
    const SECONDS_PER_DAY: u64 = 86_400;
    const REFRESH_HEADROOM: u64 = 3 * 60 * 60;
    let next_daily_cycle = 2 * SECONDS_PER_DAY + 1;

    let refresh_timestamp = dynamic_oracle_refresh_timestamp(next_daily_cycle);

    assert_eq!(refresh_timestamp, next_daily_cycle - REFRESH_HEADROOM);
    assert_eq!(refresh_timestamp / SECONDS_PER_DAY, 1);
    assert!(next_daily_cycle - refresh_timestamp < 6 * 60 * 60);
}

#[test]
fn protocol_cycle_keeps_an_exact_hourly_processing_boundary() {
    assert_eq!(
        first_protocol_cycle_at_or_after_interval(172_800, 3_600),
        172_801
    );
}

#[test]
fn protocol_cycle_rounds_a_non_boundary_processing_time_up() {
    assert_eq!(
        first_protocol_cycle_at_or_after_interval(172_801, 3_600),
        176_401
    );
}

#[test]
fn restart_barrier_latches_the_transient_lifecycle_before_price_publication() {
    let decision =
        super::restart_barrier_decision(RestartBarrierState::default(), true, false, false);

    assert_eq!(
        decision,
        RestartBarrierDecision::Continue(RestartBarrierState {
            lifecycle_observed: true,
            publication_observed: false,
        })
    );
}

#[test]
fn restart_barrier_completes_when_price_arrives_after_the_lifecycle_was_latched() {
    let decision = super::restart_barrier_decision(
        RestartBarrierState {
            lifecycle_observed: true,
            publication_observed: false,
        },
        false,
        true,
        true,
    );

    assert_eq!(decision, RestartBarrierDecision::Complete);
}

#[test]
fn restart_barrier_requires_bounded_historical_proof_when_current_state_overshoots() {
    let decision =
        super::restart_barrier_decision(RestartBarrierState::default(), false, true, true);

    assert_eq!(
        decision,
        RestartBarrierDecision::HistoricalLifecycleRequired(RestartBarrierState {
            lifecycle_observed: false,
            publication_observed: true,
        })
    );
    assert!(super::fresh_wwd_lifecycle_overshot(
        8,
        &[Some(6), Some(6), Some(6), Some(6)]
    ));
    assert!(super::fresh_wwd_lifecycle_overshot(
        2,
        &[Some(3), Some(3), Some(3), Some(3)]
    ));
}

#[test]
fn historical_lifecycle_scan_is_descending_inclusive_and_bounded() {
    assert_eq!(
        super::historical_lifecycle_scan_heights(80, 84),
        Ok(vec![84, 83, 82, 81, 80])
    );
    assert_eq!(
        super::historical_lifecycle_scan_heights(84, 80),
        Err("current finalized height 80 precedes restart minimum 84".to_string())
    );
    assert_eq!(
        super::historical_lifecycle_scan_heights(1, 258),
        Err("historical lifecycle scan spans 258 blocks; maximum is 256".to_string())
    );
}

#[test]
fn restart_barrier_cannot_complete_from_price_publication_alone() {
    let decision =
        super::restart_barrier_decision(RestartBarrierState::default(), false, false, true);

    assert_eq!(
        decision,
        RestartBarrierDecision::Continue(RestartBarrierState {
            lifecycle_observed: false,
            publication_observed: true,
        })
    );
}

#[test]
fn restart_convergence_advances_beyond_the_most_advanced_peer() {
    assert_eq!(post_restart_convergence_target([99, 104, 107, 107]), 108);
}

#[test]
fn joiner_restart_waits_out_an_imminent_dkg_activation() {
    assert!(!joiner_restart_is_in_safe_early_epoch_window(78, 78, 20));
    assert!(!joiner_restart_is_in_safe_early_epoch_window(80, 80, 20));
    assert!(joiner_restart_is_in_safe_early_epoch_window(81, 81, 20));
    assert!(joiner_restart_is_in_safe_early_epoch_window(83, 83, 20));
    assert!(joiner_restart_is_in_safe_early_epoch_window(90, 90, 20));
    assert!(!joiner_restart_is_in_safe_early_epoch_window(91, 91, 20));
}

#[test]
fn joiner_restart_window_derives_from_the_chain_epoch() {
    assert!(!joiner_restart_is_in_safe_early_epoch_window(118, 118, 120));
    assert!(joiner_restart_is_in_safe_early_epoch_window(121, 121, 120));
    assert!(joiner_restart_is_in_safe_early_epoch_window(122, 122, 120));
}

#[test]
fn joiner_restart_accepts_an_early_pre_freeze_handover_window() {
    assert!(joiner_restart_is_in_safe_early_epoch_window(83, 83, 300));
    assert!(!joiner_restart_is_in_safe_early_epoch_window(151, 151, 300));
}

#[test]
fn joiner_restart_requires_the_full_node_to_finalize_the_boundary_block() {
    assert!(!joiner_restart_is_in_safe_early_epoch_window(101, 100, 20));
    assert!(joiner_restart_is_in_safe_early_epoch_window(101, 101, 20));
}
