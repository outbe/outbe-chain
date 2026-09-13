use crate::features::ocomp::*;

pub(in crate::features::ocomp) const OCOMP_PROGRESS_STALL_TIMEOUT_SECS: u64 = 120;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::features::ocomp) enum ProgressWaitDecision {
    Reached,
    Progressed,
    Waiting,
    Stalled,
}

pub(in crate::features::ocomp) fn monotonic_progress_decision(
    current: u64,
    target: u64,
    previous: u64,
    now: Instant,
    progress_deadline: Instant,
) -> ProgressWaitDecision {
    if current >= target {
        ProgressWaitDecision::Reached
    } else if current > previous {
        ProgressWaitDecision::Progressed
    } else if now >= progress_deadline {
        ProgressWaitDecision::Stalled
    } else {
        ProgressWaitDecision::Waiting
    }
}

pub(in crate::features::ocomp) fn wait_for_common_finalized_checkpoint(
    world: &mut World,
    target: u64,
    fault_label: &str,
) -> crate::world::rpc::FinalizedCheckpoint {
    let ports = world.validators.committee_ports();
    let mut previous_common_height = 0_u64;
    let mut progress_deadline =
        Instant::now() + Duration::from_secs(OCOMP_PROGRESS_STALL_TIMEOUT_SECS);
    loop {
        let observations = ports
            .iter()
            .map(|&port| (port, world.rpc.finalized_result(port)))
            .collect::<Vec<_>>();
        let common_height = observations
            .iter()
            .map(|(_, height)| height.as_ref().ok().copied())
            .collect::<Option<Vec<_>>>()
            .and_then(|heights| heights.into_iter().min());
        let now = Instant::now();
        if let Some(current) = common_height {
            match monotonic_progress_decision(
                current,
                target,
                previous_common_height,
                now,
                progress_deadline,
            ) {
                ProgressWaitDecision::Reached => {
                    world
                        .rpc
                        .wait_finalized_checkpoint(&ports, target, 1)
                        .unwrap_or_else(|error| {
                            panic!(
                                "{fault_label} nodes reached h{target} but disagree on the common finalized checkpoint: {error:#}"
                            )
                        });
                    let expected =
                        world
                            .rpc
                            .checkpoint_at(ports[0], target)
                            .unwrap_or_else(|error| {
                                panic!("read {fault_label} checkpoint h{target}: {error:#}")
                            });
                    for &port in &ports[1..] {
                        let observed = world.rpc.checkpoint_at(port, target).unwrap_or_else(
                            |error| {
                                panic!(
                                    "read {fault_label} checkpoint h{target} on port {port}: {error:#}"
                                )
                            },
                        );
                        assert_eq!(
                            observed, expected,
                            "{fault_label} nodes disagree at exact finalized h{target}"
                        );
                    }
                    return expected;
                }
                ProgressWaitDecision::Progressed => {
                    previous_common_height = current;
                    progress_deadline =
                        now + Duration::from_secs(OCOMP_PROGRESS_STALL_TIMEOUT_SECS);
                }
                ProgressWaitDecision::Waiting => {}
                ProgressWaitDecision::Stalled => {
                    panic!("{fault_label} finality stalled below h{target}: {observations:?}");
                }
            }
        } else if now >= progress_deadline {
            panic!(
                "{fault_label} finality became unobservable for {}s below h{target}: {observations:?}",
                OCOMP_PROGRESS_STALL_TIMEOUT_SECS
            );
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .unwrap_or_else(|error| panic!("{fault_label} OCOMP role exited: {error:#}"));
        sleep(Duration::from_millis(250));
    }
}

pub(in crate::features::ocomp) fn finalized_points_at_common_height(
    world: &World,
    minimum_height: u64,
) -> Vec<MetadosisFinalizedPointV1> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let ports = world.validators.committee_ports();
        let finalized = ports
            .iter()
            .map(|port| world.rpc.finalized(*port))
            .collect::<Vec<_>>();
        if finalized.iter().all(Option::is_some) {
            let common_height = finalized
                .iter()
                .flatten()
                .copied()
                .min()
                .expect("four finalized heights");
            if common_height >= minimum_height {
                let points = finalized_points_at_height(world, common_height);
                if points
                    .iter()
                    .all(|point| point.block_hash == points[0].block_hash)
                    && points
                        .iter()
                        .all(|point| point.block_timestamp == points[0].block_timestamp)
                {
                    return points;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "four validators did not converge on one finalized block at or above {minimum_height}"
        );
        sleep(Duration::from_millis(250));
    }
}

pub(in crate::features::ocomp) fn finalized_points_at_height(
    world: &World,
    height: u64,
) -> Vec<MetadosisFinalizedPointV1> {
    world
        .validators
        .committee_ports()
        .iter()
        .enumerate()
        .map(|(validator_index, port)| MetadosisFinalizedPointV1 {
            validator_index: u8::try_from(validator_index).expect("validator index fits u8"),
            block_number: height,
            block_hash: world
                .rpc
                .block_hash(*port, height)
                .and_then(|hash| B256::from_str(&hash).ok())
                .expect("canonical finalized block hash"),
            block_timestamp: world
                .rpc
                .block_timestamp(*port, height)
                .expect("canonical finalized block timestamp"),
        })
        .collect()
}

pub(in crate::features::ocomp) fn common_block_hash(world: &World, height: u64) -> B256 {
    let hashes = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| {
            world
                .rpc
                .block_hash(port, height)
                .and_then(|hash| B256::from_str(&hash).ok())
                .expect("canonical block hash")
        })
        .collect::<Vec<_>>();
    assert!(
        hashes.iter().all(|hash| *hash == hashes[0]),
        "validators expose different canonical block {height} hashes"
    );
    hashes[0]
}

pub(in crate::features::ocomp) fn capture_ocomp_finality_before_fault(
    world: &mut World,
    action: &str,
) {
    let ports = world.validators.committee_ports();
    world.state.ocomp_finality_before_fault = Some(
        world
            .rpc
            .wait_finalized_checkpoint(&ports, 1, 60)
            .unwrap_or_else(|error| {
                panic!("capture common finalized checkpoint before {action}: {error:#}")
            })
            .height,
    );
}
