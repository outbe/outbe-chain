//! Waiting, finalized reads and event checks the lifecycle scenarios share.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::Address;
use alloy_sol_types::SolEvent;

use crate::internal::eth;
use crate::world::rpc::FinalizedCheckpoint;
use crate::world::World;

/// Poll `ready` once a second until it holds, failing with `what` when `timeout` runs out.
pub(crate) fn poll_until(
    timeout: Duration,
    what: impl Fn() -> String,
    mut ready: impl FnMut() -> bool,
) {
    let deadline = Instant::now() + timeout;
    while !ready() {
        assert!(Instant::now() < deadline, "{}", what());
        sleep(Duration::from_secs(1));
    }
}

/// Retry a failed settlement read twice while the body projection applies updates.
pub(crate) fn settlement_read<T>(mut read: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut result = read();
    for attempt in 1..=2 {
        let Err(error) = &result else { return result };
        eprintln!("settlement preflight attempt {attempt} failed: {error}; retrying");
        sleep(Duration::from_secs(2));
        result = read();
    }
    result.map_err(|error| format!("settlement preflight failed after 3 attempts: {error}"))
}

/// The committee head's timestamp, which the lifecycle measures time against.
pub(crate) fn head_time(world: &World) -> u64 {
    world
        .rpc
        .latest_block_timestamp(world.validators.primary_port())
        .expect("committee head timestamp")
}

/// The primary head, finalized on every validator before its state is read.
pub(crate) fn finalized_checkpoint(world: &World) -> FinalizedCheckpoint {
    let head = world
        .rpc
        .head(world.validators.primary_port())
        .expect("lifecycle primary head");
    world
        .rpc
        .wait_finalized_checkpoint(&world.validators.committee_ports(), head, 120)
        .expect("finalized lifecycle checkpoint on every validator")
}

/// Every validator still serves the same block at the checkpoint's height.
pub(crate) fn verify_checkpoint(world: &World, checkpoint: FinalizedCheckpoint) {
    for port in world.validators.committee_ports() {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, checkpoint.height)
                .expect("lifecycle checkpoint"),
            checkpoint
        );
    }
}

/// Exactly one `expected` event from `address` in `from..=to`, matched on its first
/// indexed topic and compared whole: sweeps leave no receipt but their logs.
pub(crate) fn assert_single_event<E: SolEvent>(
    url: &str,
    address: Address,
    from: u64,
    to: u64,
    expected: E,
) {
    let encoded = expected.encode_log_data();
    let logs = eth::raw_json_result(
        url,
        "eth_getLogs",
        serde_json::json!([{
            "address": address, "fromBlock": format!("0x{from:x}"), "toBlock": format!("0x{to:x}"),
            "topics": [encoded.topics()[0], encoded.topics()[1]],
        }]),
    )
    .expect("finalized lifecycle events");
    let logs = logs.as_array().expect("lifecycle log array");
    assert_eq!(logs.len(), 1, "expected exactly one {} here", E::SIGNATURE);
    assert_eq!(
        logs[0]["topics"],
        serde_json::json!(encoded.topics()),
        "{} identity mismatch",
        E::SIGNATURE
    );
    assert_eq!(
        logs[0]["data"],
        serde_json::json!(encoded.data),
        "{} amount mismatch",
        E::SIGNATURE
    );
}

#[cfg(test)]
mod tests {
    use super::settlement_read;

    #[test]
    fn settlement_preflight_recovers_without_repeating_a_successful_read() {
        let mut attempts = 0;
        let value = settlement_read(|| {
            attempts += 1;
            if attempts < 3 {
                Err("body commitment mismatch".into())
            } else {
                Ok(42)
            }
        })
        .unwrap();
        assert_eq!(value, 42);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn settlement_preflight_stops_after_two_retries_and_keeps_the_error() {
        let mut attempts = 0;
        let result: Result<(), String> = settlement_read(|| {
            attempts += 1;
            Err(format!("read failed on attempt {attempts}"))
        });
        assert_eq!(attempts, 3);
        assert_eq!(
            result.unwrap_err(),
            "settlement preflight failed after 3 attempts: read failed on attempt 3"
        );
    }

    #[test]
    fn settlement_preflight_returns_the_first_success_immediately() {
        let mut attempts = 0;
        assert_eq!(
            settlement_read(|| {
                attempts += 1;
                Ok(42)
            })
            .unwrap(),
            42
        );
        assert_eq!(attempts, 1);
    }
}
