//! Canonical genesis fixtures and process isolation for node tests.

use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

/// Run the exact test in a fresh process. Return true only inside that child.
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

/// Write a native genesis without protocol fixtures.
pub(crate) fn write_base_genesis(path: &Path, chain_id: u64) -> eyre::Result<()> {
    let genesis = serde_json::json!({
        "config": {
            "chainId": chain_id,
            "epochLengthBlocks": 300,
            "homesteadBlock": 0,
            "eip150Block": 0,
            "eip155Block": 0,
            "eip158Block": 0,
            "byzantiumBlock": 0,
            "constantinopleBlock": 0,
            "petersburgBlock": 0,
            "istanbulBlock": 0,
            "berlinBlock": 0,
            "londonBlock": 0,
            "mergeNetsplitBlock": 0,
            "terminalTotalDifficulty": 0,
            "terminalTotalDifficultyPassed": true,
            "shanghaiTime": 0,
            "cancunTime": 0,
            "pragueTime": 0
        },
        "nonce": "0x0",
        "timestamp": "0x1",
        "extraData": "0x",
        "gasLimit": "0x1dcd6500",
        "difficulty": "0x0",
        "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "coinbase": "0x0000000000000000000000000000000000000000",
        "alloc": {}
    });
    fs::write(path, serde_json::to_vec_pretty(&genesis)?)?;
    Ok(())
}
