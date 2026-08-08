use alloy_primitives::B256;
use outbe_ocomp::embedded::{
    EmbeddedJobActionV1, EmbeddedJobStateV1, EmbeddedOcompJobsV1, EmbeddedOcompModeV1,
};

#[test]
fn a_new_job_never_supersedes_an_existing_live_job() {
    let first = B256::repeat_byte(0x11);
    let second = B256::repeat_byte(0x22);
    let mut jobs = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::FullNode);

    jobs.observe_job(first, 100).unwrap();
    jobs.observe_job(second, 200).unwrap();
    jobs.record_local_result(second, B256::repeat_byte(0x33))
        .unwrap();

    assert_eq!(jobs.state(first), Some(EmbeddedJobStateV1::Computing));
    assert_eq!(jobs.state(second), Some(EmbeddedJobStateV1::LocalReady));
    assert_eq!(jobs.live_job_count(), 2);
}

#[test]
fn only_validator_policy_requests_vote_submission() {
    let job_id = B256::repeat_byte(0x41);
    let digest = B256::repeat_byte(0x42);
    let mut validator = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::Validator);
    let mut full_node = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::FullNode);
    validator.observe_job(job_id, 100).unwrap();
    full_node.observe_job(job_id, 100).unwrap();

    assert_eq!(
        validator.record_local_result(job_id, digest).unwrap(),
        EmbeddedJobActionV1::SubmitVote {
            job_id,
            result_digest: digest,
        }
    );
    assert_eq!(
        full_node.record_local_result(job_id, digest).unwrap(),
        EmbeddedJobActionV1::AwaitCanonical { job_id }
    );
}

#[test]
fn validator_treats_an_already_canonical_job_as_protocol_owned_terminal() {
    let job_id = B256::repeat_byte(0x43);
    let digest = B256::repeat_byte(0x44);
    let mut jobs = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::Validator);
    jobs.observe_job(job_id, 100).unwrap();

    assert_eq!(
        jobs.record_canonical_result(job_id, digest).unwrap(),
        EmbeddedJobActionV1::ProtocolOwned
    );
    assert_eq!(
        jobs.record_local_result(job_id, B256::repeat_byte(0x45))
            .unwrap(),
        EmbeddedJobActionV1::ProtocolOwned
    );
    assert_eq!(jobs.state(job_id), Some(EmbeddedJobStateV1::Verified));
    assert_eq!(jobs.live_job_count(), 0);
}

#[test]
fn full_node_waits_at_deadline_and_resumes_only_after_exact_canonical_match() {
    let job_id = B256::repeat_byte(0x51);
    let digest = B256::repeat_byte(0x52);
    let mut jobs = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::FullNode);
    jobs.observe_job(job_id, 100).unwrap();

    assert_eq!(
        jobs.observe_deadline(job_id).unwrap(),
        EmbeddedJobActionV1::HoldProgress {
            job_id,
            through_height: 100,
        }
    );
    assert_eq!(jobs.state(job_id), Some(EmbeddedJobStateV1::WaitAtDeadline));

    assert_eq!(
        jobs.record_canonical_result(job_id, digest).unwrap(),
        EmbeddedJobActionV1::AwaitLocalResult { job_id }
    );
    assert_eq!(
        jobs.record_local_result(job_id, digest).unwrap(),
        EmbeddedJobActionV1::ReleaseProgress { job_id }
    );
    assert_eq!(jobs.state(job_id), Some(EmbeddedJobStateV1::Verified));
}

#[test]
fn every_unverified_full_node_job_holds_progress_after_its_deadline() {
    let computing = B256::repeat_byte(0x53);
    let local_ready = B256::repeat_byte(0x54);
    let mut jobs = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::FullNode);
    jobs.observe_job(computing, 100).unwrap();
    jobs.observe_job(local_ready, 120).unwrap();
    jobs.record_local_result(local_ready, B256::repeat_byte(0x55))
        .unwrap();

    assert_eq!(jobs.progress_limit(99), 99);
    assert_eq!(jobs.progress_limit(100), 99);
    assert_eq!(jobs.progress_limit(130), 99);
    jobs.record_no_quorum(computing).unwrap();
    assert_eq!(jobs.progress_limit(130), 119);
    jobs.record_canonical_result(local_ready, B256::repeat_byte(0x55))
        .unwrap();
    assert_eq!(jobs.progress_limit(130), 130);
}

#[test]
fn finalized_no_quorum_closes_every_nonterminal_full_node_state() {
    let computing = B256::repeat_byte(0x61);
    let waiting = B256::repeat_byte(0x62);
    let local_ready = B256::repeat_byte(0x63);
    let mut jobs = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::FullNode);
    for job_id in [computing, waiting, local_ready] {
        jobs.observe_job(job_id, 100).unwrap();
    }
    jobs.observe_deadline(waiting).unwrap();
    jobs.record_local_result(local_ready, B256::repeat_byte(0x64))
        .unwrap();

    for job_id in [computing, waiting, local_ready] {
        assert_eq!(
            jobs.record_no_quorum(job_id).unwrap(),
            EmbeddedJobActionV1::CloseNoQuorum { job_id }
        );
        assert_eq!(jobs.state(job_id), Some(EmbeddedJobStateV1::ClosedNoQuorum));
    }
    assert_eq!(jobs.live_job_count(), 0);
}

#[test]
fn canonical_mismatch_is_a_full_node_local_fatal_action() {
    let job_id = B256::repeat_byte(0x71);
    let local = B256::repeat_byte(0x72);
    let canonical = B256::repeat_byte(0x73);
    let mut jobs = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::FullNode);
    jobs.observe_job(job_id, 100).unwrap();
    jobs.record_local_result(job_id, local).unwrap();

    assert_eq!(
        jobs.record_canonical_result(job_id, canonical).unwrap(),
        EmbeddedJobActionV1::FatalMismatch {
            job_id,
            local_result_digest: local,
            canonical_result_digest: canonical,
        }
    );
    assert_eq!(jobs.state(job_id), Some(EmbeddedJobStateV1::FatalMismatch));
}
