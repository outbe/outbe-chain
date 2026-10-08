use super::*;

#[cucumber::when("a fresh snapshot recipient provisions its own identity without syncing")]
pub(super) fn provision_snapshot_recipient(world: &mut crate::world::World) {
    let index = world.validators.joiner_index();
    let data = world.validators.data_dir(index);
    for relative in [
        "db",
        "static_files",
        "compressed_entities",
        "offchain",
        "consensus",
    ] {
        assert!(
            !data.join(relative).exists(),
            "recipient already has native chain history"
        );
    }
    let node = data.parent().expect("recipient node root");
    assert!(
        !node.join("ocomp/domain-v1/node-v1").exists(),
        "recipient already has OCOMP history"
    );
    world
        .localnet
        .prepare_snapshot_full_node(index)
        .expect("provision own node-host and normal configuration");
    world
        .ocomp
        .stage_keyless_full_node_domain(index.try_into().unwrap())
        .expect("stage public FullNode deployment files");
    for key in ["evm-key.hex", "signing-key.hex", "reth-p2p-secret.hex"] {
        let donor = world.validators.data_dir(3).parent().unwrap().join(key);
        assert_ne!(
            snapshot_file_sha256(&node.join(key)).unwrap(),
            snapshot_file_sha256(&donor).unwrap(),
            "recipient must own a distinct {key}"
        );
    }
}

pub(super) fn recipient_identity(
    world: &crate::world::World,
) -> eyre::Result<BTreeMap<std::path::PathBuf, crate::world::state::SnapshotFingerprint>> {
    use std::os::unix::fs::PermissionsExt;
    let data = world.validators.data_dir(world.validators.joiner_index());
    let node = data.parent().unwrap();
    let mut values = BTreeMap::new();
    for relative in [
        "evm-key.hex",
        "signing-key.hex",
        "reth-p2p-secret.hex",
        "offchain-storage.toml",
        "data/tee-node-host-v1/noise-initiator.key",
        "data/tee-node-host-v1/initialization-manifest.bin",
    ] {
        let path = node.join(relative);
        let metadata = std::fs::metadata(&path)?;
        values.insert(
            path.clone(),
            crate::world::state::SnapshotFingerprint {
                sha256: snapshot_file_sha256(&path)?,
                mode: metadata.permissions().mode() & 0o7777,
            },
        );
    }
    for relative in [
        "ocomp-key-v1.hex",
        "ocomp-evm-key.hex",
        "supervisor-v1/sign-once",
        "supervisor-v1/vote-submissions",
        "supervisor-v1/materialization-submissions",
        "supervisor-v1/payout-submissions",
    ] {
        ensure!(
            !node.join("ocomp/domain-v1").join(relative).exists(),
            "FullNode imported donor signing authority: {relative}"
        );
    }
    Ok(values)
}

pub(crate) fn place_snapshot_payload(
    world: &crate::world::World,
    archive: &std::path::Path,
    evidence_dir: &std::path::Path,
) -> eyre::Result<(Vec<u8>, Vec<u8>)> {
    use std::process::Command;
    let scratch = tempfile::tempdir()?;
    let mut command = Command::new("tar");
    command
        .args(["--no-same-owner", "-xpf"])
        .arg(archive)
        .arg("-C")
        .arg(scratch.path());
    let extracted = run_snapshot_command(
        command,
        evidence_dir,
        "extract",
        std::time::Duration::from_secs(600),
    )?;
    successful_command(&extracted)?;
    let manifest = std::fs::read(scratch.path().join("manifest.json"))?;
    let signature = std::fs::read(scratch.path().join("signature.json"))?;
    let value: serde_json::Value = serde_json::from_slice(&manifest)?;
    let data = world.validators.data_dir(world.validators.joiner_index());
    let node = data.parent().unwrap();
    for domain in value["domains"]
        .as_array()
        .ok_or_else(|| eyre!("missing domain inventory"))?
    {
        let id = domain["id"]
            .as_str()
            .ok_or_else(|| eyre!("missing domain id"))?;
        let entries = domain["entries"]
            .as_array()
            .ok_or_else(|| eyre!("missing entries"))?;
        if entries.is_empty() {
            continue;
        }
        let target = match domain["native_root"]
            .as_str()
            .ok_or_else(|| eyre!("missing target root"))?
        {
            "chain" => data.clone(),
            "consensus" => data.join("consensus"),
            "ocomp" => node.join("ocomp/domain-v1"),
            "offchain" => data.join("offchain"),
            "static-files" => data.join("static_files"),
            "execution-rocks-db" => data.join("rocksdb"),
            root => return Err(eyre!("unknown native root {root}")),
        };
        std::fs::create_dir_all(&target)?;
        let mut command = Command::new("cp");
        command
            .args(["-a", "--no-preserve=ownership", "--"])
            .arg(scratch.path().join("payload").join(id).join("."))
            .arg(&target);
        successful_command(&run_snapshot_command(
            command,
            evidence_dir,
            &format!("place-{id}"),
            std::time::Duration::from_secs(600),
        )?)?;
    }
    scratch.close()?;
    Ok((manifest, signature))
}

pub(crate) fn assert_local_committee_anchor(text: &str, finalized_height: u64) -> eyre::Result<()> {
    let record = text
        .lines()
        .find(|line| line.contains("follower restored committee from local finalized history"))
        .ok_or_else(|| eyre!("snapshot fullnode rebuilt committee history from genesis"))?;
    let field = |key: &str| -> eyre::Result<u64> {
        let prefix = format!("{key}=");
        record
            .split_whitespace()
            .find_map(|word| word.strip_prefix(&prefix))
            .ok_or_else(|| eyre!("missing {key} in local committee anchor"))?
            .parse()
            .map_err(Into::into)
    };
    let epoch = field("anchor_epoch")?;
    let height = field("anchor_height")?;
    ensure!(
        epoch > 0 && height > 1 && height <= finalized_height,
        "snapshot fullnode did not resume from a non-genesis finalized committee anchor"
    );
    Ok(())
}

pub(super) fn observe_snapshot_launch(
    world: &mut crate::world::World,
    launch: crate::world::localnet::NodeLaunchObservation,
    before_launch: crate::world::state::SnapshotNativeObservation,
) -> eyre::Result<crate::world::state::SnapshotLaunchObservation> {
    use crate::world::state::*;
    use std::os::unix::fs::MetadataExt;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let (text, fields) = loop {
        let text = world.localnet.node_launch_log(launch.index, launch.pid)?;
        if text.contains("certified follower startup recovery barrier completed") {
            let fields = parse_recovery_record(&text)?;
            break (text, fields);
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "recipient did not finish native startup; see {}",
            launch.log_path.display()
        );
        world
            .localnet
            .follower_launch_observation("snapshot-recipient", launch.index)?;
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let (marshal_processed, anchor, ce_marker_height, last_execution_height) = fields;
    assert_local_committee_anchor(&text, anchor.number)?;
    let canonical = canonical_snapshot_block(world, anchor.number)?;
    let metadata = std::fs::metadata(&launch.log_path)?;
    let log = SnapshotLogSlice {
        path: launch.log_path,
        device: metadata.dev(),
        inode: metadata.ino(),
        start: launch.log_start,
        end: launch.log_start + text.len() as u64,
        bytes: text.into_bytes(),
    };
    let mut argv = vec![launch.program.display().to_string()];
    argv.extend(launch.argv);
    let value = SnapshotLaunchObservation {
        slot: launch.index.try_into()?,
        started: launch.started_at_millis,
        pid: launch.pid,
        argv,
        before_launch,
        recovery: SnapshotRecoveryObservation {
            pid: launch.pid,
            incarnation_started: launch.started_at_millis,
            observed: snapshot_now_millis()?,
            log,
            marshal_processed,
            anchor,
            ce_marker_height,
            last_execution_height,
            canonical,
        },
    };
    ordinary_launch(&value)?;
    Ok(value)
}

pub(super) fn start_snapshot_recipient(
    world: &mut crate::world::World,
    chain_id: u64,
    genesis: alloy_primitives::B256,
) -> eyre::Result<crate::world::state::SnapshotLaunchObservation> {
    let slot = world.validators.joiner_index();
    let data = world.validators.data_dir(slot);
    let node = data.parent().unwrap();
    let before =
        observe_stopped_native(node, &node.join("offchain-storage.toml"), chain_id, genesis)?;
    world
        .localnet
        .launch_dcap_full_node("snapshot-recipient", slot, 0)?;
    let launched = world
        .localnet
        .follower_launch_observation("snapshot-recipient", slot)?;
    let observation = observe_snapshot_launch(world, launched, before)?;
    let target = observation.recovery.anchor.number + 1;
    ensure!(
        world
            .rpc
            .wait_finalized_at_least(world.validators.http_port(slot), target, 180),
        "recipient did not advance beyond startup anchor"
    );
    world
        .ocomp
        .start_keyless_full_node_roles(slot.try_into()?)?;
    Ok(observation)
}

#[cucumber::when(
    "the signed files start fresh FullNode placements with and without offline validation"
)]
pub(super) fn place_and_start_snapshot_recipient(world: &mut crate::world::World) {
    place_and_start_snapshot_recipient_result(world)
        .expect("ordinary signed-file placement and startup");
}

pub(super) struct SnapshotPlacement {
    pub(super) slot: usize,
    pub(super) node: std::path::PathBuf,
    pub(super) data: std::path::PathBuf,
    pub(super) root: std::path::PathBuf,
    pub(super) archive: std::path::PathBuf,
    pub(super) retained_archive: std::path::PathBuf,
    pub(super) received: std::path::PathBuf,
    pub(super) manifest_path: std::path::PathBuf,
    pub(super) signature_path: std::path::PathBuf,
    pub(super) donor_launch: crate::world::localnet::NodeLaunchObservation,
    pub(super) chain: String,
    pub(super) chain_id: u64,
    pub(super) genesis: alloy_primitives::B256,
    pub(super) identity: BTreeMap<std::path::PathBuf, crate::world::state::SnapshotFingerprint>,
    pub(super) initial_data_members: std::collections::BTreeSet<std::ffi::OsString>,
    pub(super) tee_root: std::path::PathBuf,
    pub(super) tee_before: BTreeMap<std::path::PathBuf, crate::world::state::SnapshotFingerprint>,
    pub(super) hidden: [(std::path::PathBuf, std::path::PathBuf); 2],
}

impl SnapshotPlacement {
    fn remove_artifact_sources(&self, validate: bool) -> eyre::Result<()> {
        let Self {
            received,
            manifest_path,
            signature_path,
            archive,
            retained_archive,
            hidden,
            ..
        } = self;
        std::fs::remove_file(received)?;
        std::fs::remove_file(manifest_path)?;
        std::fs::remove_file(signature_path)?;
        if validate {
            std::fs::rename(archive, retained_archive)?;
        } else {
            std::fs::remove_file(retained_archive)?;
        }
        ensure!(
            !archive.exists()
                && !received.exists()
                && !manifest_path.exists()
                && !signature_path.exists(),
            "startup still has original artifact paths"
        );
        ensure!(
            hidden.iter().all(|(original, _)| !original.exists()),
            "donor source paths remain available"
        );
        Ok(())
    }

    fn run_phase(&self, world: &mut crate::world::World, validate: bool) -> eyre::Result<()> {
        use crate::world::state::*;
        use std::{process::Command, time::Duration};
        let Self {
            slot,
            node,
            root,
            archive,
            retained_archive,
            received,
            manifest_path,
            signature_path,
            chain_id,
            genesis,
            identity,
            tee_root,
            tee_before,
            ..
        } = self;
        let (slot, chain_id, genesis) = (*slot, *chain_id, *genesis);

        let phase_dir = root.join(if validate {
            "validated-placement"
        } else {
            "unvalidated-placement"
        });
        std::fs::create_dir_all(&phase_dir)?;
        let source = if validate { archive } else { retained_archive };
        let mut command = Command::new("cp");
        command.arg("--").arg(source).arg(received);
        let transfer =
            run_snapshot_command(command, &phase_dir, "transfer", Duration::from_secs(600))?;
        successful_command(&transfer)?;
        let transferred_archive_sha256 = snapshot_file_sha256(received)?;
        let (manifest_bytes, signature_bytes) =
            place_snapshot_payload(world, received, &phase_dir)?;
        let original = world
            .state
            .offline_snapshot
            .as_ref()
            .ok_or_else(|| eyre!("missing created artifact"))?;
        ensure!(
            manifest_bytes == original.manifest_bytes
                && transferred_archive_sha256 == original.archive_sha256,
            "placement used a different artifact"
        );
        ensure!(
            recipient_identity(world)? == *identity,
            "placement changed own identity"
        );
        if validate {
            ensure!(
                fingerprint_snapshot_tree(tee_root)? == *tee_before,
                "placement changed recipient NodeHost records"
            );
        }
        std::fs::write(manifest_path, &manifest_bytes)?;
        std::fs::write(signature_path, signature_bytes)?;
        let placed =
            observe_stopped_native(node, &node.join("offchain-storage.toml"), chain_id, genesis)?;
        let placement = SnapshotPlacementObservation {
            completed: snapshot_now_millis()?,
            native: placed,
        };
        let validation = if validate {
            self.validate(original, &phase_dir)?
        } else {
            SnapshotValidationObservation::NotRun
        };
        self.remove_artifact_sources(validate)?;
        if !validate {
            let bundle = world
                .ocomp
                .canonical_fork_install()?
                .request_profile
                .protocol_bundle_hash;
            let port = world.ocomp.snapshot_worker_port(slot);
            world.state.offline_snapshot_worker_inventory = Some(snapshot_worker_inventory(
                &world.ocomp,
                &node.join("ocomp/domain-v1"),
                port,
                bundle,
            )?);
        }
        let started = start_snapshot_recipient(world, chain_id, genesis)?;
        ensure!(
            started.before_launch.progress == placement.native.progress,
            "startup did not open placed native state"
        );
        if validate {
            self.finish_validated(world, &phase_dir, started, placement, validation)?;
        } else {
            let e = world.state.offline_snapshot.as_mut().unwrap();
            e.transfer = Some(transfer);
            e.transferred_archive_sha256 = transferred_archive_sha256;
            e.placement = Some(placement);
            e.validation = validation;
            e.identity_before = identity.clone();
            e.identity_placed = identity.clone();
            e.first_start = Some(started);
        }

        Ok(())
    }

    fn validate(
        &self,
        original: &OfflineSnapshotEvidence,
        phase_dir: &std::path::Path,
    ) -> eyre::Result<SnapshotValidationObservation> {
        use std::{process::Command, time::Duration};
        let Self {
            donor_launch,
            manifest_path,
            signature_path,
            chain,
            data,
            node,
            received,
            ..
        } = self;

        let creator = std::str::from_utf8(&original.create.as_ref().unwrap().stdout)?
            .split_whitespace()
            .find_map(|field| field.strip_prefix("creator_public_key="))
            .ok_or_else(|| eyre!("create omitted signer public key"))?;
        let mut command = Command::new(&donor_launch.program);
        command
            .args(["snapshot", "validate", "--checks", "all", "--manifest"])
            .arg(manifest_path)
            .arg("--signature")
            .arg(signature_path)
            .arg("--expected-signer")
            .arg(creator)
            .arg("--report")
            .arg(phase_dir.join("validation.json"))
            .args(["--", "--chain"])
            .arg(chain)
            .arg("--datadir")
            .arg(data)
            .arg("--consensus.storage-dir")
            .arg(data.join("consensus"))
            .arg("--projection.storage-config")
            .arg(node.join("offchain-storage.toml"));
        let observation =
            run_snapshot_command(command, phase_dir, "validate", Duration::from_secs(900))?;
        let report = parse_snapshot_validation_report(&observation.stdout)?;
        ensure!(
            report.checks["files"].status == SnapshotCheckStatus::Passed
                && report.checks["provenance"].status == SnapshotCheckStatus::Passed,
            "original signed file checks did not pass"
        );
        ensure!(
            !report
                .checks
                .values()
                .any(|check| check.status == SnapshotCheckStatus::Failed),
            "semantic validation failed; inspect retained actual report"
        );
        // Incomplete remains a nonzero audit, never relabeled complete.
        let complete = report
            .checks
            .values()
            .all(|check| check.status == SnapshotCheckStatus::Passed);
        ensure!(
            (complete && observation.exit_code == Some(0))
                || (!complete && observation.exit_code.is_some_and(|code| code != 0)),
            "validation report/exit disagree"
        );
        snapshot_rejects_damaged_artifact(SnapshotArtifactCheck {
            program: &donor_launch.program,
            archive: received,
            manifest: manifest_path,
            signature: signature_path,
            evidence_dir: phase_dir,
            creator,
            chain,
            node,
        })?;
        Ok(SnapshotValidationObservation::Run(observation))
    }

    fn finish_validated(
        &self,
        world: &mut crate::world::World,
        phase_dir: &std::path::Path,
        started: SnapshotLaunchObservation,
        placement: crate::world::state::SnapshotPlacementObservation,
        validation: SnapshotValidationObservation,
    ) -> eyre::Result<()> {
        let Self {
            slot,
            data,
            node,
            initial_data_members,
            ..
        } = self;
        let slot = *slot;
        let follower_clients = world
            .ocomp
            .stop_node_facing_roles_for_snapshot(slot.try_into()?)?;
        let exited =
            world
                .localnet
                .stop_follower_for_snapshot("snapshot-recipient", slot, started.pid)?;
        std::fs::write(
            phase_dir.join("ordinary-start.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "placement":placement, "validation":validation, "launch":started,
                "stop_pid":exited.observation.launch.pid,"stop_code":exited.observation.code,
                "stop_signal":exited.observation.signal,"stop_at":exited.observation.reaped_at_millis,
                "protected_identity":recipient_identity(world)?,
            }))?,
        )?;
        drop(follower_clients);
        // A separate fresh placement, with the same recipient-owned identity.
        // Only this stopped, scenario-owned temporary node's copied data is discarded.
        for entry in std::fs::read_dir(data)? {
            let entry = entry?;
            if initial_data_members.contains(&entry.file_name()) {
                continue;
            }
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        std::fs::remove_dir_all(node.join("ocomp/domain-v1"))?;
        std::fs::create_dir_all(node.join("ocomp/domain-v1"))?;

        Ok(())
    }
}

pub(super) fn place_and_start_snapshot_recipient_result(
    world: &mut crate::world::World,
) -> eyre::Result<()> {
    let slot = world.validators.joiner_index();
    let data = world.validators.data_dir(slot);
    let node = data.parent().unwrap().to_path_buf();
    let root = world.localnet.scenario_dir().join("offline-snapshot");
    let archive = root.join("created.tar");
    let retained_archive = root.join("next-placement.tar");
    let received = root.join("received.tar");
    let manifest_path = root.join("manifest.json");
    let signature_path = root.join("signature.json");
    let donor_launch = world.localnet.validator_launch_observation(3)?;
    let chain = snapshot_option(&donor_launch.argv, "--chain")?;
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .ok_or_else(|| eyre!("chain id"))?;
    let genesis = canonical_snapshot_block(world, 0)?.hash.parse()?;
    let identity = recipient_identity(world)?;
    let initial_data_members = std::fs::read_dir(&data)?
        .map(|entry| entry.map(|v| v.file_name()))
        .collect::<std::io::Result<std::collections::BTreeSet<_>>>()?;
    let tee_root = data.join("tee-node-host-v1");
    let tee_before = fingerprint_snapshot_tree(&tee_root)?;
    // The source paths will be absent during startup. This is filesystem-path
    // independence, not OS isolation between processes using the same test UID.
    let price_publication = crate::features::price_oracle::stop_before_clock_restart(world);
    let clients = world.ocomp.stop_node_facing_roles_for_snapshot(3)?;
    let stopped = world
        .localnet
        .stop_validator_for_snapshot(3, donor_launch.pid)?;
    let donor_node = world.validators.data_dir(3).parent().unwrap().to_path_buf();
    let hidden = [
        (donor_node.join("data"), donor_node.join("data.offline-e2e")),
        (
            donor_node.join("ocomp/domain-v1"),
            donor_node.join("ocomp/domain-v1.offline-e2e"),
        ),
    ];
    hide_snapshot_sources(&hidden)?;
    let placement = SnapshotPlacement {
        slot,
        node,
        data,
        root,
        archive,
        retained_archive,
        received,
        manifest_path,
        signature_path,
        donor_launch,
        chain,
        chain_id,
        genesis,
        identity,
        initial_data_members,
        tee_root,
        tee_before,
        hidden,
    };

    let result = (|| -> eyre::Result<()> {
        for validate in [true, false] {
            placement.run_phase(world, validate)?;
        }
        Ok(())
    })();
    // Restore the donor's paths regardless of the recipient assertion result.
    restore_snapshot_sources(&placement.hidden)?;
    world.localnet.resume_snapshot_node(stopped)?;
    ensure!(
        world
            .rpc
            .wait_finalized_at_least(world.validators.http_port(3), 1, 180),
        "donor restart readiness"
    );
    world.ocomp.resume_snapshot_node_facing_roles(clients)?;
    resume_snapshot_prices(world, price_publication)?;
    result
}

pub(super) fn fingerprint_snapshot_tree(
    root: &std::path::Path,
) -> eyre::Result<BTreeMap<std::path::PathBuf, crate::world::state::SnapshotFingerprint>> {
    use std::os::unix::fs::PermissionsExt;
    let mut result = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if entry.file_type().is_file() {
            result.insert(
                entry.path().strip_prefix(root)?.to_owned(),
                crate::world::state::SnapshotFingerprint {
                    sha256: snapshot_file_sha256(entry.path())?,
                    mode: entry.metadata()?.permissions().mode() & 0o7777,
                },
            );
        }
    }
    Ok(result)
}
