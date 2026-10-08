use super::*;

#[cucumber::when(
    "the snapshot FullNode executes a fresh next-day OCOMP job and restarts at current progress"
)]
pub(super) fn new_snapshot_work_and_restart(world: &mut crate::world::World) {
    new_snapshot_work_and_restart_result(world)
        .expect("new real OCOMP work and ordinary current-K restart");
}

pub(super) fn audit_current_snapshot(
    world: &crate::world::World,
    launched: &crate::world::localnet::NodeLaunchObservation,
) -> eyre::Result<()> {
    use std::{process::Command, time::Duration};
    let slot = world.validators.joiner_index();
    let data = world.validators.data_dir(slot);
    let node = data.parent().unwrap();
    let root = world.localnet.scenario_dir().join("offline-snapshot");
    let chain = snapshot_option(&launched.argv, "--chain")?;
    let mut command = Command::new(&launched.program);
    command
        .args([
            "snapshot",
            "validate",
            "--checks",
            "headers,evm,ce,bodies,ocomp",
            "--report",
        ])
        .arg(root.join("current-k-validation.json"))
        .args(["--", "--chain"])
        .arg(chain)
        .arg("--datadir")
        .arg(&data)
        .arg("--consensus.storage-dir")
        .arg(data.join("consensus"))
        .arg("--projection.storage-config")
        .arg(node.join("offchain-storage.toml"));
    let audit = run_snapshot_command(
        command,
        &root,
        "validate-current-k",
        Duration::from_secs(900),
    )?;
    let report = parse_snapshot_validation_report(&audit.stdout)?;
    ensure!(
        report.checks["files"].status == SnapshotCheckStatus::NotRequested
            && report.checks["provenance"].status == SnapshotCheckStatus::NotRequested,
        "current K audit unexpectedly requires old sidecars"
    );
    ensure!(
        !report
            .checks
            .values()
            .any(|check| check.status == SnapshotCheckStatus::Failed),
        "current K native validation failed"
    );
    let complete = report
        .checks
        .values()
        .filter(|check| check.selected)
        .all(|check| check.status == SnapshotCheckStatus::Passed);
    ensure!(
        (complete && audit.exit_code == Some(0))
            || (!complete && audit.exit_code.is_some_and(|code| code != 0)),
        "current K validation report/exit disagree"
    );
    Ok(())
}

pub(super) struct SnapshotFollowupJob {
    pub(super) requested: u64,
    pub(super) request_block: SnapshotBlock,
    pub(super) canonical_result: SnapshotResultObservation,
    pub(super) canonical_result_at: SnapshotBlock,
    pub(super) live: SnapshotWorkerLiveResult,
    pub(super) job_id: alloy_primitives::B256,
    pub(super) result_digest: alloy_primitives::B256,
}

pub(super) fn perform_snapshot_job(
    world: &mut crate::world::World,
    running: &SnapshotWorkerRunning,
) -> eyre::Result<SnapshotFollowupJob> {
    use crate::features::ocomp::{
        quorum_applies_lysis_and_creates_nod_for_request, PublicVoteSetExpectation,
    };
    use outbe_primitives::time::WorldwideDay;
    use std::time::{Duration, Instant};
    let original = world
        .state
        .ocomp_job_request
        .clone()
        .ok_or_else(|| eyre!("original completed request"))?;
    let day = WorldwideDay::from_timestamp(
        WorldwideDay::new(original.worldwide_day)
            .start_timestamp()
            .checked_add(86_400)
            .ok_or_else(|| eyre!("day overflow"))?,
    )
    .value();
    let schedule = await_snapshot_offering(world, day)?;
    let cut = world
        .state
        .offline_snapshot
        .as_ref()
        .unwrap()
        .cut_canonical
        .number;
    let (requested, request) =
        request_snapshot_job(world, day, cut, schedule.scheduled_process_time)?;
    ensure!(
        request.intent_id != original.intent_id && request.request_height > cut,
        "new job did not follow snapshot cut"
    );
    let request_block = canonical_snapshot_block(world, request.request_height)?;
    quorum_applies_lysis_and_creates_nod_for_request(
        world,
        request,
        PublicVoteSetExpectation::Exact(&[0, 1, 2, 3]),
    );
    let activation = world
        .state
        .ocomp_activation
        .clone()
        .ok_or_else(|| eyre!("new canonical Lysis result"))?;
    let canonical_result = SnapshotResultObservation {
        job_id: hex::encode(activation.job_id),
        digest: hex::encode(activation.result_digest),
    };
    let canonical_result_at = canonical_snapshot_block(world, activation.block_number)?;
    let slot = world.validators.joiner_index();
    ensure!(
        world.rpc.wait_finalized_at_least(
            world.validators.http_port(slot),
            activation.block_number + 2,
            300
        ),
        "recipient did not finalize new result"
    );
    let deadline = Instant::now() + Duration::from_secs(180);
    let live = loop {
        match snapshot_worker_after(&mut world.ocomp, running, activation.job_id) {
            Ok(value) => break value,
            Err(error) => {
                ensure!(
                    Instant::now() < deadline,
                    "recipient new compute observation failed: {error:#}"
                );
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    };
    Ok(SnapshotFollowupJob {
        requested,
        request_block,
        canonical_result,
        canonical_result_at,
        live,
        job_id: activation.job_id,
        result_digest: activation.result_digest,
    })
}

pub(super) fn new_snapshot_work_and_restart_result(
    world: &mut crate::world::World,
) -> eyre::Result<()> {
    use crate::world::state::*;
    let inventory = world
        .state
        .offline_snapshot_worker_inventory
        .take()
        .ok_or_else(|| eyre!("missing stopped pre-request inventories"))?;
    let running = snapshot_worker_before(&mut world.ocomp, inventory)?;
    let SnapshotFollowupJob {
        requested,
        request_block,
        canonical_result,
        canonical_result_at,
        live,
        job_id,
        result_digest,
    } = perform_snapshot_job(world, &running)?;
    let primary = world.validators.primary_port();
    let slot = world.validators.joiner_index();
    let launched = world
        .localnet
        .follower_launch_observation("snapshot-recipient", slot)?;
    let clients = world
        .ocomp
        .stop_node_facing_roles_for_snapshot(slot.try_into()?)?;
    let stopped =
        world
            .localnet
            .stop_follower_for_snapshot("snapshot-recipient", slot, launched.pid)?;
    let collected = snapshot_worker_bind_admission(
        live,
        requested,
        request_block,
        canonical_result,
        canonical_result_at,
    )?;
    let root = world.localnet.scenario_dir().join("offline-snapshot");
    std::fs::write(
        root.join("worker-native-evidence.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "before_status":collected.before_status,"after_status":collected.after_status,"admission_record":collected.admission_record,
        }))?,
    )?;
    let data = world.validators.data_dir(slot);
    let node = data.parent().unwrap();
    let chain_id = world
        .rpc
        .chain_id(primary)
        .ok_or_else(|| eyre!("chain id"))?;
    let genesis = canonical_snapshot_block(world, 0)?.hash.parse()?;
    let at_k =
        observe_stopped_native(node, &node.join("offchain-storage.toml"), chain_id, genesis)?;
    let k_canonical = canonical_snapshot_block(world, at_k.progress.finalized.number)?;
    let identity_at_k = recipient_identity(world)?;
    // Native-only audit is separate from the subsequent ordinary launch.
    audit_current_snapshot(world, &launched)?;
    let before_launch =
        observe_stopped_native(node, &node.join("offchain-storage.toml"), chain_id, genesis)?;
    let first_exit = SnapshotExitObservation {
        pid: stopped.observation.launch.pid,
        reaped: stopped.observation.reaped_at_millis,
        code: stopped.observation.code,
        signal: stopped.observation.signal,
    };
    let launch = world.localnet.resume_snapshot_node(stopped)?;
    let restarted = observe_snapshot_launch(world, launch, before_launch)?;
    ensure!(
        world.rpc.wait_finalized_at_least(
            world.validators.http_port(slot),
            at_k.progress.finalized.number + 1,
            180
        ),
        "restarted current K node did not continue"
    );
    world.ocomp.resume_snapshot_node_facing_roles(clients)?;
    let identity_restarted = recipient_identity(world)?;
    let fresh = std::fs::read(super::super::local_result_path(world, slot, job_id))?;
    let result = outbe_ocomp_protocol::result::LysisResultV1::decode_canonical(
        &fresh,
        &outbe_ocomp_protocol::profile::poc_schema_limits(),
    )?;
    ensure!(
        result.result_digest(&outbe_ocomp_protocol::profile::poc_schema_limits())? == result_digest,
        "current K restart changed new result"
    );
    let evidence = world.state.offline_snapshot.as_mut().unwrap();
    evidence.new_job = Some(collected.new_job);
    evidence.before_restart = Some(at_k);
    evidence.k_canonical = Some(k_canonical);
    evidence.first_exit = Some(first_exit);
    evidence.second_start = Some(restarted);
    evidence.identity_at_k = identity_at_k;
    evidence.identity_restarted = identity_restarted;
    Ok(())
}

pub(super) fn restore_snapshot_sources(
    pairs: &[(std::path::PathBuf, std::path::PathBuf)],
) -> eyre::Result<()> {
    let mut failures = Vec::new();
    for (original, hidden) in pairs.iter().rev() {
        if let Err(error) = std::fs::rename(hidden, original) {
            failures.push(format!(
                "{} -> {}: {error}",
                hidden.display(),
                original.display()
            ));
        }
    }
    ensure!(
        failures.is_empty(),
        "restore stopped test donor paths: {}",
        failures.join("; ")
    );
    Ok(())
}

pub(super) fn hide_snapshot_sources(
    pairs: &[(std::path::PathBuf, std::path::PathBuf)],
) -> eyre::Result<()> {
    for (position, (original, hidden)) in pairs.iter().enumerate() {
        if let Err(error) = std::fs::rename(original, hidden) {
            let restored = restore_snapshot_sources(&pairs[..position]);
            return Err(eyre!(
                "move stopped test donor {} -> {}: {error}; restore: {restored:?}",
                original.display(),
                hidden.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod source_cleanup_tests {
    use super::*;
    #[test]
    fn failed_second_source_move_restores_the_first_source() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("data");
        std::fs::create_dir(&first).unwrap();
        std::fs::write(first.join("native"), b"unchanged").unwrap();
        let pairs = [
            (first.clone(), root.path().join("hidden-data")),
            (root.path().join("absent"), root.path().join("hidden-ocomp")),
        ];
        assert!(hide_snapshot_sources(&pairs).is_err());
        assert_eq!(std::fs::read(first.join("native")).unwrap(), b"unchanged");
        assert!(!pairs[0].1.exists());
    }
    #[test]
    fn source_restore_attempts_remaining_paths_after_one_failure() {
        let root = tempfile::tempdir().unwrap();
        let pairs = [
            (root.path().join("data"), root.path().join("missing-backup")),
            (root.path().join("ocomp"), root.path().join("hidden-ocomp")),
        ];
        std::fs::create_dir(&pairs[1].1).unwrap();
        std::fs::write(pairs[1].1.join("result"), b"retained").unwrap();
        assert!(restore_snapshot_sources(&pairs).is_err());
        assert_eq!(
            std::fs::read(pairs[1].0.join("result")).unwrap(),
            b"retained"
        );
    }
}
