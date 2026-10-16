use super::*;

#[cucumber::when("a stopped post-Lysis donor creates the signed native snapshot")]
pub(super) fn create_stopped_snapshot(world: &mut crate::world::World) {
    create_stopped_snapshot_result(world)
        .expect("create signed snapshot from stopped native files");
}

pub(super) fn create_snapshot_archive(
    world: &crate::world::World,
    launch: &crate::world::localnet::NodeLaunchObservation,
    evidence_dir: &std::path::Path,
    native: &crate::world::state::SnapshotNativeObservation,
) -> eyre::Result<(
    SnapshotCommandObservation,
    SnapshotCommandObservation,
    String,
)> {
    use std::process::Command;
    let index = 3;
    let archive = evidence_dir.join("created.tar");
    // Native MDBX files reserve large sparse ranges. Snapshot tar stores those
    // ranges as bytes, so creating it on the validators' disk can stall their
    // native persistence while consensus is still live. Stage in memory, then copy
    // the finished tar back with holes in its all-zero ranges.
    let scratch_root = std::path::Path::new("/dev/shm");
    let scratch_free = rustix::fs::statvfs(scratch_root)
        .map(|stat| u128::from(stat.f_bavail) * u128::from(stat.f_frsize))?;
    let memory_available_kib = std::fs::read_to_string("/proc/meminfo")?
        .lines()
        .find_map(|line| {
            line.strip_prefix("MemAvailable:")?
                .split_whitespace()
                .next()
        })
        .ok_or_else(|| eyre!("cannot read MemAvailable for snapshot staging"))?
        .parse::<u128>()?;
    ensure!(
        scratch_free >= 16 * 1024_u128.pow(3)
            && memory_available_kib * 1024 >= 24 * 1024_u128.pow(3),
        "insufficient memory for isolated native snapshot staging: shm_free={scratch_free}, mem_available_kib={memory_available_kib}"
    );
    let scratch = tempfile::Builder::new()
        .prefix("outbe-snapshot-create-")
        .tempdir_in(scratch_root)?;
    let staged_archive = scratch.path().join("created.tar");
    let mut command = Command::new(&launch.program);
    command
        .args(["snapshot", "create", "--output"])
        .arg(&staged_archive)
        .arg("--signing-key")
        .arg(world.validators.get(index).evm_key_path())
        .args([
            "--creator",
            "E2E donor validator 3",
            "--source",
            "stopped localnet donor",
            "--",
        ])
        .args(&launch.argv[1..]);
    let create = run_snapshot_command(
        command,
        evidence_dir,
        "create",
        std::time::Duration::from_secs(600),
    )?;
    successful_command(&create)?;
    let mut command = Command::new("cp");
    command
        .args(["--sparse=always", "--"])
        .arg(&staged_archive)
        .arg(&archive);
    successful_command(&run_snapshot_command(
        command,
        evidence_dir,
        "save-sparse-archive",
        std::time::Duration::from_secs(600),
    )?)?;
    ensure!(
        std::fs::metadata(&archive)?.len() == std::fs::metadata(&staged_archive)?.len(),
        "sparse archive copy changed native tar length"
    );
    scratch.close()?;
    let archive_sha256 = snapshot_file_sha256(&archive)?;
    let mut command = Command::new("tar");
    command.arg("-xOf").arg(&archive).arg("manifest.json");
    let manifest = run_snapshot_command(
        command,
        evidence_dir,
        "read-manifest",
        std::time::Duration::from_secs(60),
    )?;
    successful_command(&manifest)?;
    let recorded: SnapshotManifestObservation = serde_json::from_slice(&manifest.stdout)?;
    ensure!(
        recorded.progress == native.progress,
        "creation changed or misreported stopped native progress"
    );
    Ok((create, manifest, archive_sha256))
}

pub(super) fn read_copied_snapshot_result(
    world: &crate::world::World,
    index: usize,
) -> eyre::Result<SnapshotResultObservation> {
    let copied_job = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .ok_or_else(|| eyre!("missing actual Lysis generation"))?
        .job_id;
    let copied_bytes = std::fs::read(super::super::local_result_path(world, index, copied_job))?;
    let copied_result = outbe_ocomp_protocol::result::LysisResultV1::decode_canonical(
        &copied_bytes,
        &outbe_ocomp_protocol::profile::poc_schema_limits(),
    )?;
    ensure!(
        copied_result.job_id == copied_job,
        "copied result belongs to another job"
    );
    let copied_result = SnapshotResultObservation {
        job_id: hex::encode(copied_job),
        digest: hex::encode(
            copied_result.result_digest(&outbe_ocomp_protocol::profile::poc_schema_limits())?,
        ),
    };
    Ok(copied_result)
}

pub(super) fn initial_snapshot_evidence(
    create: SnapshotCommandObservation,
    manifest_bytes: Vec<u8>,
    archive_sha256: String,
    cut: SnapshotBlock,
    copied_result: SnapshotResultObservation,
) -> OfflineSnapshotEvidence {
    OfflineSnapshotEvidence {
        create: Some(create),
        manifest_bytes,
        archive_sha256,
        transferred_archive_sha256: String::new(),
        transfer: None,
        placement: None,
        validation: SnapshotValidationObservation::NotRun,
        cut_canonical: cut,
        identity_before: BTreeMap::new(),
        identity_placed: BTreeMap::new(),
        identity_at_k: BTreeMap::new(),
        identity_restarted: BTreeMap::new(),
        first_start: None,
        copied_result,
        new_job: None,
        before_restart: None,
        k_canonical: None,
        first_exit: None,
        second_start: None,
    }
}

pub(super) fn create_stopped_snapshot_result(world: &mut crate::world::World) -> eyre::Result<()> {
    capture_certified_generation_for_snapshot(world)?;
    let SnapshotDonorContext {
        index,
        node,
        evidence_dir,
        launch,
        genesis,
    } = snapshot_donor_context(world)?;
    let price_publication = crate::features::price_oracle::stop_before_clock_restart(world);
    let clients = world
        .ocomp
        .stop_node_facing_roles_for_snapshot(index as u8)?;
    ensure!(clients.index() == index as u8, "wrong selected clients");
    let client_exits: Vec<_> = clients.stops().iter().map(|stop| serde_json::json!({
        "index":stop.index, "role":stop.role, "worker_ordinal":stop.worker_ordinal,
        "pid":stop.pid, "requested":stop.stop_requested_at_millis, "reaped":stop.reaped_at_millis,
        "code":stop.code,"signal":stop.signal,
    })).collect();
    let stopped = world
        .localnet
        .stop_validator_for_snapshot(index, launch.pid)?;
    let native = observe_stopped_native(&node, genesis)?;
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .ok_or_else(|| eyre!("missing actual certified generation before stopped cut"))?;
    let pending = snapshot_pending_cut(&native.progress, |at| {
        snapshot_pending_materialization_at(world, at, generation)
    })?;
    std::fs::write(
        evidence_dir.join("pending-native-cut.json"),
        serde_json::to_vec_pretty(&pending)?,
    )?;
    let cut = canonical_snapshot_block(world, native.progress.finalized.number)?;
    ensure!(
        native.progress.finalized == cut,
        "stopped donor has noncanonical H"
    );
    let (create, manifest, archive_sha256) =
        create_snapshot_archive(world, &launch, &evidence_dir, &native)?;
    let copied_result = read_copied_snapshot_result(world, index)?;
    std::fs::write(
        evidence_dir.join("donor-stop.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "node_pid": stopped.observation.launch.pid,
            "node_stop_requested": stopped.observation.stop_requested_at_millis,
            "node_reaped": stopped.observation.reaped_at_millis,
            "node_code": stopped.observation.code,"node_signal": stopped.observation.signal,
            "clients": client_exits,"native": native.progress,
        }))?,
    )?;
    world.state.offline_snapshot = Some(initial_snapshot_evidence(
        create,
        manifest.stdout,
        archive_sha256,
        cut,
        copied_result,
    ));
    world.localnet.resume_snapshot_node(stopped)?;
    ensure!(
        world.rpc.wait_finalized_at_least(
            world.validators.http_port(index),
            native.progress.finalized.number + 1,
            180
        ),
        "resumed donor did not become ready"
    );
    world.ocomp.resume_snapshot_node_facing_roles(clients)?;
    resume_snapshot_prices(world, price_publication)?;
    Ok(())
}

fn capture_certified_generation_for_snapshot(world: &mut crate::world::World) -> eyre::Result<()> {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .ok_or_else(|| eyre!("missing finalized JobIntent before snapshot"))?;
    let ports = world.validators.committee_ports();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        let activations = ports
            .iter()
            .copied()
            .map(|port| {
                world.rpc.finalized_ocomp_activation_on(
                    port,
                    request.request_height,
                    request.intent_id,
                )
            })
            .collect::<Vec<_>>();
        if activations.iter().all(Option::is_some) {
            let activation = activations[0].clone().expect("all activations present");
            ensure!(
                activations
                    .iter()
                    .all(|value| value.as_ref() == Some(&activation)),
                "validators disagree on finalized Lysis activation before snapshot"
            );
            let generations = ports
                .iter()
                .copied()
                .map(|port| {
                    world
                        .rpc
                        .finalized_ocomp_certified_generation_on(port, &activation)
                })
                .collect::<Vec<_>>();
            if generations.iter().all(Option::is_some) {
                let generation = generations[0].clone().expect("all generations present");
                ensure!(
                    generations
                        .iter()
                        .all(|value| value.as_ref() == Some(&generation)),
                    "validators disagree on finalized Lysis generation before snapshot"
                );
                ensure!(
                    generation.job_id == request.job_id
                        && generation.block_number == activation.block_number
                        && generation.block_hash == activation.block_hash,
                    "snapshot generation does not match finalized JobIntent activation"
                );
                world.state.ocomp_activation = Some(activation);
                world.state.ocomp_certified_generation = Some(generation);
                return Ok(());
            }
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "finalized Lysis generation did not become available before snapshot"
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

struct SnapshotDonorContext {
    index: usize,
    node: std::path::PathBuf,
    evidence_dir: std::path::PathBuf,
    launch: crate::world::localnet::NodeLaunchObservation,
    genesis: alloy_primitives::B256,
}

fn snapshot_donor_context(world: &mut crate::world::World) -> eyre::Result<SnapshotDonorContext> {
    // Exercise restart after committee rotation, so genesis reconstruction
    // cannot satisfy the snapshot acceptance check below.
    let rotated_height = super::super::OCOMP_TEST_EPOCH_LENGTH_BLOCKS + 1;
    ensure!(
        world
            .rpc
            .wait_finalized_at_least(world.validators.primary_port(), rotated_height, 600),
        "snapshot donor did not finalize beyond committee rotation"
    );
    let index = 3;
    let node = world
        .validators
        .data_dir(index)
        .parent()
        .unwrap()
        .to_path_buf();
    let evidence_dir = world.localnet.scenario_dir().join("offline-snapshot");
    std::fs::create_dir_all(&evidence_dir)?;
    let launch = world.localnet.validator_launch_observation(index)?;
    ensure!(
        launch.argv.first().map(String::as_str) == Some("node"),
        "unexpected ordinary command"
    );
    let genesis = canonical_snapshot_block(world, 0)?
        .hash
        .parse::<alloy_primitives::B256>()?;
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .ok_or_else(|| eyre!("chain id unavailable"))?;
    ensure!(
        chain_id == 54322345,
        "snapshot E2E requires the existing SGX testnet profile"
    );
    Ok(SnapshotDonorContext {
        index,
        node,
        evidence_dir,
        launch,
        genesis,
    })
}
