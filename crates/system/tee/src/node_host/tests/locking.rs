use super::*;

#[test]
fn replacement_state_lock_serializes_first_submission_writer() {
    use std::{sync::mpsc, time::Duration};

    use rustix::fs::FlockOperation;

    let fixture = replacement_fixture();
    let submission = read_replacement_submission(&fixture.paths.replacement_submission).unwrap();
    let evidence = AttestationEvidenceV1::decode_canonical(submission.evidence()).unwrap();
    let node_signature = *submission.node_signature();
    let enclave_signature = *submission.enclave_signature();
    std::fs::remove_file(&fixture.paths.replacement_submission).unwrap();
    File::open(&fixture.paths.root).unwrap().sync_all().unwrap();

    let lock_path = fixture.paths.root.join("state.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)
        .unwrap();
    rustix::fs::flock(&lock, FlockOperation::LockExclusive).unwrap();

    let node_data_dir = fixture.node_data_dir.clone();
    let (result_sender, result_receiver) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let result = persist_replacement_candidate_submission(
            &node_data_dir,
            &evidence,
            &node_signature,
            &enclave_signature,
        );
        result_sender.send(result).unwrap();
    });
    assert!(
        result_receiver
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "submission writer bypassed the NodeHost state lock"
    );
    rustix::fs::flock(&lock, FlockOperation::Unlock).unwrap();
    drop(lock);
    assert!(result_receiver
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .is_ok());
    writer.join().unwrap();
}
