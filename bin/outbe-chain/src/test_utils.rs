//! Isolation for tests that bind the process-wide consensus chain identity.

use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

/// Run the exact test in a fresh process; return true only inside that child.
pub(crate) fn in_isolated_process(case: &str) -> bool {
    const CASE: &str = "OUTBE_TEST_ISOLATED_CHAIN_CASE";
    const STARTED: &str = "OUTBE_TEST_ISOLATED_CHAIN_STARTED";
    if std::env::var(CASE).ok().as_deref() == Some(case) {
        fs::write(std::env::var_os(STARTED).expect("child witness path"), case).unwrap();
        return true;
    }
    let witness = tempfile::tempdir().unwrap();
    let started = witness.path().join("started");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", case, "--nocapture", "--test-threads=1"])
        .env(CASE, case)
        .env(STARTED, &started)
        .env("RAYON_NUM_THREADS", "2")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "isolated case {case} failed: {status}");
                assert_eq!(
                    fs::read_to_string(&started).unwrap(),
                    case,
                    "child filter ran no case"
                );
                return false;
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => {
                let _ = child.kill();
                let reaped = child.wait();
                panic!("isolated case {case} did not finish: {result:?}; reap={reaped:?}");
            }
        }
    }
}
