//! Bounded waits on finalized time and on live node checkpoints.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FinalizedClockWaitDecisionV1 {
    Reached,
    Progressed,
    Waiting,
    Stalled,
    Regressed,
}

pub(super) fn classify_finalized_clock_wait(
    current: (u64, u64),
    target_timestamp: u64,
    previous: (u64, u64),
    now: Instant,
    progress_deadline: Instant,
) -> FinalizedClockWaitDecisionV1 {
    if current.1 >= target_timestamp {
        FinalizedClockWaitDecisionV1::Reached
    } else if current.0 < previous.0 || current.1 < previous.1 {
        FinalizedClockWaitDecisionV1::Regressed
    } else if current.0 > previous.0 || current.1 > previous.1 {
        FinalizedClockWaitDecisionV1::Progressed
    } else if now >= progress_deadline {
        FinalizedClockWaitDecisionV1::Stalled
    } else {
        FinalizedClockWaitDecisionV1::Waiting
    }
}

pub(super) fn finalized_clock_observation(world: &World, port: u16) -> Option<(u64, u64)> {
    let height = world.rpc.finalized(port)?;
    let timestamp = world.rpc.block_timestamp(port, height)?;
    Some((height, timestamp))
}

pub(in crate::features) fn wait_for_finalized_timestamp(
    world: &World,
    target_timestamp: u64,
    label: &str,
) -> (u64, u64) {
    let port = world.validators.primary_port();
    let initial_deadline = Instant::now() + FINALIZED_CLOCK_STALL_TIMEOUT;
    let mut previous = loop {
        if let Some(observation) = finalized_clock_observation(world, port) {
            break observation;
        }
        assert!(
            Instant::now() < initial_deadline,
            "timed out reading the initial finalized clock while waiting for {label}"
        );
        sleep(Duration::from_secs(1));
    };
    let started = previous;
    let max_step_seconds = outbe_primitives::consensus::MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS / 1_000;
    let minimum_remaining_blocks = target_timestamp
        .saturating_sub(previous.1)
        .div_ceil(max_step_seconds);
    eprintln!(
        "[tee-lease] waiting for {label}: start_height={}, start_timestamp={}, \
         target_timestamp={target_timestamp}, minimum_remaining_blocks={minimum_remaining_blocks}",
        started.0, started.1
    );

    let mut progress_deadline = Instant::now() + FINALIZED_CLOCK_STALL_TIMEOUT;
    loop {
        let now = Instant::now();
        let Some(current) = finalized_clock_observation(world, port) else {
            assert!(
                now < progress_deadline,
                "finalized RPC stalled while waiting for {label}: \
                 start={started:?}, last={previous:?}, target_timestamp={target_timestamp}"
            );
            sleep(Duration::from_secs(1));
            continue;
        };
        match classify_finalized_clock_wait(
            current,
            target_timestamp,
            previous,
            now,
            progress_deadline,
        ) {
            FinalizedClockWaitDecisionV1::Reached => {
                eprintln!(
                    "[tee-lease] reached {label}: height={}, timestamp={}, target_timestamp={target_timestamp}",
                    current.0, current.1
                );
                return current;
            }
            FinalizedClockWaitDecisionV1::Progressed => {
                previous = current;
                progress_deadline = now + FINALIZED_CLOCK_STALL_TIMEOUT;
            }
            FinalizedClockWaitDecisionV1::Waiting => {}
            FinalizedClockWaitDecisionV1::Stalled => {
                panic!(
                    "finalized clock stalled while waiting for {label}: \
                     start={started:?}, last={current:?}, target_timestamp={target_timestamp}"
                );
            }
            FinalizedClockWaitDecisionV1::Regressed => {
                panic!(
                    "finalized clock regressed while waiting for {label}: \
                     previous={previous:?}, current={current:?}, target_timestamp={target_timestamp}"
                );
            }
        }
        sleep(Duration::from_secs(1));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FullNodeSyncProbeV1 {
    NotTracked,
    Pending,
    Reached(u64),
    Exited,
}

pub(super) fn classify_full_node_sync_probe(
    head: Option<u64>,
    checkpoint: u64,
    exited: Option<bool>,
) -> FullNodeSyncProbeV1 {
    if exited.is_none() {
        FullNodeSyncProbeV1::NotTracked
    } else if exited == Some(true) {
        FullNodeSyncProbeV1::Exited
    } else if let Some(height) = head {
        if height >= checkpoint {
            return FullNodeSyncProbeV1::Reached(height);
        }
        FullNodeSyncProbeV1::Pending
    } else {
        FullNodeSyncProbeV1::Pending
    }
}

pub(super) fn wait_for_live_full_node_checkpoint(
    world: &mut World,
    full_node: usize,
    checkpoint: u64,
    timeout: Duration,
) -> Result<u64, String> {
    let port = world.validators.http_port(full_node);
    let node_log = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{full_node}/node.log"));
    let started = Instant::now();
    let expected = world
        .rpc
        .checkpoint_at(world.validators.primary_port(), checkpoint)
        .map_err(|error| error.to_string())?;
    loop {
        let head = world.rpc.finalized(port);
        let exited = world.localnet.joiner_full_node_exit_status(full_node);
        match classify_full_node_sync_probe(head, checkpoint, exited) {
            FullNodeSyncProbeV1::Reached(height) => {
                let actual = world
                    .rpc
                    .checkpoint_at(port, checkpoint)
                    .map_err(|error| error.to_string())?;
                if actual != expected
                    || world.localnet.joiner_full_node_exit_status(full_node) != Some(false)
                {
                    return Err(
                        "FullNode finalized checkpoint differs or owned process exited".to_owned(),
                    );
                }
                world
                    .state
                    .tee_lease
                    .observations
                    .push(json!({"phase": "full_node_checkpoint",
                    "slot": full_node, "height": checkpoint, "block_hash": actual.block_hash,
                    "state_root": actual.state_root, "observed_finalized_height": height}));
                return Ok(height);
            }
            FullNodeSyncProbeV1::NotTracked => {
                return Err(format!(
                    "FullNode {full_node} has no owned process under canonical key {}; inspect {}",
                    crate::world::localnet::Localnet::joiner_full_node_name(full_node),
                    node_log.display()
                ));
            }
            FullNodeSyncProbeV1::Exited => {
                return Err(format!(
                    "FullNode {full_node} exited at head {head:?} before checkpoint {checkpoint}; inspect {}",
                    node_log.display()
                ));
            }
            FullNodeSyncProbeV1::Pending => {}
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "FullNode {full_node} remained alive but did not reach checkpoint {checkpoint} within {}s (last head {head:?}); inspect {}",
                timeout.as_secs(),
                node_log.display()
            ));
        }
        sleep(Duration::from_secs(2));
    }
}

pub(super) fn wait_for_live_validator_checkpoint(
    world: &mut World,
    validator: usize,
    checkpoint: u64,
    expected_hash: &str,
    timeout: Duration,
) -> Result<u64, String> {
    let port = world.validators.http_port(validator);
    let node_log = world
        .localnet
        .scenario_dir()
        .join(format!("validator-{validator}/node.log"));
    let started = Instant::now();
    loop {
        if !world.localnet.validator_running(validator) {
            return Err(format!(
                "validator-{validator} exited before reaching finalized recovery checkpoint \
                 {checkpoint}; inspect {}",
                node_log.display()
            ));
        }
        if let Some(height) = world.rpc.finalized(port) {
            if height >= checkpoint {
                let observed_hash = world.rpc.block_hash(port, checkpoint).ok_or_else(|| {
                    format!(
                        "validator-{validator} finalized height {height} but cannot read block \
                         {checkpoint}; inspect {}",
                        node_log.display()
                    )
                })?;
                if observed_hash != expected_hash {
                    return Err(format!(
                        "validator-{validator} recovery checkpoint hash mismatch at height \
                         {checkpoint}: committee {expected_hash}, validator {observed_hash}; \
                         inspect {}",
                        node_log.display()
                    ));
                }
                return Ok(height);
            }
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "validator-{validator} remained alive but did not reach finalized recovery \
                 checkpoint {checkpoint} within {}s; inspect {}",
                timeout.as_secs(),
                node_log.display()
            ));
        }
        sleep(Duration::from_secs(2));
    }
}
