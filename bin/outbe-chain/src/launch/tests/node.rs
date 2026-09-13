#[test]
fn ocomp_openings_remain_available_for_completed_full_node_replay() {
    use outbe_ocomp_protocol::state::OcompJobStatus;

    assert!(super::ocomp_job_available_for_calculation(
        OcompJobStatus::VotingOpen
    ));
    assert!(super::ocomp_job_available_for_calculation(
        OcompJobStatus::Completed
    ));
    for unavailable in [
        OcompJobStatus::AwaitingFinality,
        OcompJobStatus::Expired,
        OcompJobStatus::Failed,
    ] {
        assert!(!super::ocomp_job_available_for_calculation(unavailable));
    }
}
