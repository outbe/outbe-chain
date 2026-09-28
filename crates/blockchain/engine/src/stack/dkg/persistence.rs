use super::super::*;

/// DKG state file names within keys_dir.
pub(in crate::stack) const DKG_SHARE_FILE: &str = "dkg_share.hex";

pub(in crate::stack) const DKG_POLYNOMIAL_FILE: &str = "dkg_polynomial.hex";

pub(in crate::stack) const DKG_OUTPUT_FILE: &str = "dkg_output.hex";

pub(in crate::stack) const DKG_PENDING_SHARE_FILE: &str = "dkg_pending_share.hex";

pub(in crate::stack) const DKG_PENDING_POLYNOMIAL_FILE: &str = "dkg_pending_polynomial.hex";

pub(in crate::stack) const DKG_PENDING_OUTPUT_FILE: &str = "dkg_pending_output.hex";

pub(in crate::stack) const DKG_PENDING_BOUNDARY_FILE: &str = "dkg_pending_boundary.bin";

pub(in crate::stack) const DKG_PENDING_BOUNDARY_TMP_FILE: &str = "dkg_pending_boundary.bin.tmp";

pub(in crate::stack) const DKG_DEALER_RETRY_FILE: &str = "dkg_dealer_retry.hex";

pub(in crate::stack) const DKG_PLAYER_RETRY_FILE: &str = "dkg_player_retry.hex";

pub(in crate::stack) fn dkg_retry_store(
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
) -> Result<dkg_actor::DkgRetryStore> {
    args.keys_dir
        .as_ref()
        .map(|keys_dir| dkg_actor::DkgRetryStore::in_keys_dir(keys_dir, key_backend.clone()))
        .ok_or_else(|| eyre::eyre!("DKG participant recovery requires --consensus.keys-dir"))
}

pub(in crate::stack) fn retire_activated_dkg_retry_state(
    keys_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
) -> Result<()> {
    remove_pending_dkg_state(keys_dir);
    clear_pending_dkg_boundary(keys_dir);
    dkg_actor::DkgRetryStore::in_keys_dir(keys_dir, key_backend.clone())
        .clear()
        .wrap_err("failed to retire activated DKG retry state")
}

const DKG_ALL_FILES: &[&str] = &[
    DKG_SHARE_FILE,
    DKG_POLYNOMIAL_FILE,
    DKG_OUTPUT_FILE,
    DKG_PENDING_SHARE_FILE,
    DKG_PENDING_POLYNOMIAL_FILE,
    DKG_PENDING_OUTPUT_FILE,
    DKG_PENDING_BOUNDARY_FILE,
    DKG_DEALER_RETRY_FILE,
    DKG_PLAYER_RETRY_FILE,
];

/// Move DKG key files from legacy location (`consensus/`) to dedicated `keys/` dir.
pub fn migrate_dkg_keys_if_needed(
    consensus_dir: &std::path::Path,
    keys_dir: &std::path::Path,
) -> eyre::Result<()> {
    let old_share = consensus_dir.join(DKG_SHARE_FILE);
    if !old_share.exists() || keys_dir.join(DKG_SHARE_FILE).exists() {
        return Ok(());
    }
    std::fs::create_dir_all(keys_dir)
        .wrap_err_with(|| format!("failed to create keys dir: {}", keys_dir.display()))?;
    for file in DKG_ALL_FILES {
        let src = consensus_dir.join(file);
        if src.exists() {
            let dst = keys_dir.join(file);
            std::fs::rename(&src, &dst).wrap_err_with(|| {
                format!("failed to migrate {} -> {}", src.display(), dst.display())
            })?;
            info!(from = %src.display(), to = %dst.display(), "migrated DKG key file");
        }
    }
    Ok(())
}

const DKG_PENDING_BOUNDARY_MAGIC: &[u8; 8] = b"ODKGPB02";

const DKG_PENDING_BOUNDARY_LEGACY_MAGIC: &[u8; 8] = b"ODKGPB01";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::stack) struct PendingDkgBoundarySnapshot {
    pub(in crate::stack) artifact: DkgBoundaryArtifact,
    pub(in crate::stack) completed_at_height: u64,
}

pub(in crate::stack) fn decode_boundary_output(
    artifact: &DkgBoundaryArtifact,
) -> Result<Output<MinSig, bls12381::PublicKey>> {
    let decoded = dkg_manager::OdkoOutcome::decode(artifact.outcome.as_ref())?;
    ensure!(
        decoded.epoch.get() == artifact.epoch,
        "DKG boundary outcome epoch {} does not match artifact epoch {}",
        decoded.epoch.get(),
        artifact.epoch
    );
    Ok(decoded.output)
}

pub(in crate::stack) fn encode_pending_dkg_boundary_snapshot(
    snapshot: &PendingDkgBoundarySnapshot,
) -> Result<Vec<u8>> {
    let boundary = encode_boundary_artifact(&snapshot.artifact)
        .map_err(|error| eyre::eyre!("failed to encode pending DKG boundary artifact: {error}"))?;
    let len: u32 = boundary.len().try_into().map_err(|_| {
        eyre::eyre!(
            "pending DKG boundary artifact too large: {} bytes",
            boundary.len()
        )
    })?;
    let mut out = Vec::with_capacity(DKG_PENDING_BOUNDARY_MAGIC.len() + 8 + 4 + boundary.len());
    out.extend_from_slice(DKG_PENDING_BOUNDARY_MAGIC);
    out.extend_from_slice(&snapshot.completed_at_height.to_be_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(boundary.as_ref());
    Ok(out)
}

pub(in crate::stack) fn decode_pending_dkg_boundary_snapshot(
    bytes: &[u8],
) -> Result<PendingDkgBoundarySnapshot> {
    let header_len = DKG_PENDING_BOUNDARY_MAGIC.len() + 8 + 4;
    ensure!(
        bytes.len() >= header_len,
        "pending DKG boundary snapshot too short: {} < {header_len}",
        bytes.len()
    );
    let magic = &bytes[..DKG_PENDING_BOUNDARY_MAGIC.len()];
    ensure!(
        magic != DKG_PENDING_BOUNDARY_LEGACY_MAGIC,
        "unsupported pending DKG boundary snapshot version ODKGPB01"
    );
    ensure!(
        magic == DKG_PENDING_BOUNDARY_MAGIC,
        "invalid pending DKG boundary snapshot magic"
    );
    let height_offset = DKG_PENDING_BOUNDARY_MAGIC.len();
    let completed_at_height = u64::from_be_bytes(
        bytes[height_offset..height_offset + 8]
            .try_into()
            .map_err(|_| eyre::eyre!("invalid pending DKG boundary height field"))?,
    );
    let len_offset = height_offset + 8;
    let artifact_len = u32::from_be_bytes(
        bytes[len_offset..len_offset + 4]
            .try_into()
            .map_err(|_| eyre::eyre!("invalid pending DKG boundary length field"))?,
    ) as usize;
    let artifact_start = len_offset + 4;
    let artifact_end = artifact_start
        .checked_add(artifact_len)
        .ok_or_else(|| eyre::eyre!("pending DKG boundary length overflow"))?;
    ensure!(
        bytes.len() == artifact_end,
        "pending DKG boundary snapshot length mismatch: expected {artifact_end}, got {}",
        bytes.len()
    );
    let artifact = decode_boundary_artifact(&bytes[artifact_start..artifact_end])
        .map_err(|error| eyre::eyre!("failed to decode pending DKG boundary artifact: {error}"))?
        .ok_or_else(|| {
            eyre::eyre!("pending DKG boundary snapshot does not contain a BoundaryOutcome")
        })?;
    Ok(PendingDkgBoundarySnapshot {
        artifact,
        completed_at_height,
    })
}

fn pending_dkg_boundary_path(storage_dir: &std::path::Path) -> std::path::PathBuf {
    storage_dir.join(DKG_PENDING_BOUNDARY_FILE)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::stack) fn build_completed_dkg_boundary(
    current_epoch: Epoch,
    vrf_material_version: u64,
    current_participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    target: &FrozenDkgTarget,
    output: &Output<MinSig, bls12381::PublicKey>,
    completed_participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
) -> Result<DkgBoundaryArtifact> {
    let activated_validator_set =
        validator_set_for_dkg_output_players(output, &target.validator_set)?;
    let activated_participants = participants_from_validator_set(&activated_validator_set)?;
    ensure!(
        completed_participants == &activated_participants,
        "completed DKG participant set does not match reconstructed output players"
    );
    let next_epoch = next_consensus_epoch_after_dkg_activation(current_epoch);
    let next_vrf_material_version =
        outbe_validatorset::next_vrf_material_version(vrf_material_version)?;
    dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: next_epoch,
        validator_set: &activated_validator_set,
        output,
        is_full_dkg: false,
        dkg_cycle: target.dkg_cycle,
        freeze_height: target.freeze_height,
        planned_activation_height: target.planned_activation_height,
        vrf_material_version: next_vrf_material_version,
        is_validator_set_change: activated_participants != *current_participants,
        tee_expired_target_exclusions: target.tee_expired_target_exclusions.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::stack) fn persist_completed_dkg_before_activation(
    keys_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    current_epoch: Epoch,
    vrf_material_version: u64,
    current_participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    target: &FrozenDkgTarget,
    complete: &dkg_actor::DkgComplete,
    completed_at_height: u64,
) -> Result<DkgBoundaryArtifact> {
    let boundary_artifact = build_completed_dkg_boundary(
        current_epoch,
        vrf_material_version,
        current_participants,
        target,
        &complete.output,
        &complete.participants,
    )?;
    let next_epoch = next_consensus_epoch_after_dkg_activation(current_epoch);

    save_pending_dkg_state(
        keys_dir,
        &complete.share,
        complete.output.public(),
        &complete.output,
        key_backend,
    )
    .wrap_err("failed to durably save completed DKG state before activation")?;
    save_pending_dkg_boundary(
        keys_dir,
        &PendingDkgBoundarySnapshot {
            artifact: boundary_artifact.clone(),
            completed_at_height,
        },
    )
    .wrap_err("failed to durably save completed DKG boundary before activation")?;
    info!(
        keys_dir = %keys_dir.display(),
        dkg_cycle = target.dkg_cycle,
        epoch = %next_epoch,
        completed_at_height,
        dkg_output_hash = %dkg_manager::dkg_output_hash(&complete.output),
        "persisted completed DKG state before activation"
    );
    Ok(boundary_artifact)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::stack) fn persist_observed_dkg_boundary_before_activation(
    keys_dir: &std::path::Path,
    current_epoch: Epoch,
    vrf_material_version: u64,
    current_participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    target: &FrozenDkgTarget,
    output: &Output<MinSig, bls12381::PublicKey>,
    completed_at_height: u64,
) -> Result<DkgBoundaryArtifact> {
    let boundary_artifact = build_completed_dkg_boundary(
        current_epoch,
        vrf_material_version,
        current_participants,
        target,
        output,
        &target.participants,
    )?;
    save_pending_dkg_boundary(
        keys_dir,
        &PendingDkgBoundarySnapshot {
            artifact: boundary_artifact.clone(),
            completed_at_height,
        },
    )
    .wrap_err("failed to durably save observed DKG boundary before activation")?;
    Ok(boundary_artifact)
}

pub(in crate::stack) fn save_pending_dkg_boundary(
    storage_dir: &std::path::Path,
    snapshot: &PendingDkgBoundarySnapshot,
) -> Result<()> {
    std::fs::create_dir_all(storage_dir)
        .wrap_err_with(|| format!("failed to create storage dir: {}", storage_dir.display()))?;
    let bytes = encode_pending_dkg_boundary_snapshot(snapshot)?;
    let tmp_path = storage_dir.join(DKG_PENDING_BOUNDARY_TMP_FILE);
    let final_path = pending_dkg_boundary_path(storage_dir);
    std::fs::write(&tmp_path, bytes)
        .wrap_err_with(|| format!("failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &final_path).wrap_err_with(|| {
        format!(
            "failed to atomically install pending DKG boundary snapshot {}",
            final_path.display()
        )
    })?;
    Ok(())
}

pub(in crate::stack) fn load_pending_dkg_boundary(
    storage_dir: &std::path::Path,
) -> Result<Option<PendingDkgBoundarySnapshot>> {
    let path = pending_dkg_boundary_path(storage_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        std::fs::read(&path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    decode_pending_dkg_boundary_snapshot(&bytes).map(Some)
}

pub(in crate::stack) fn clear_pending_dkg_boundary(storage_dir: &std::path::Path) {
    let _ = std::fs::remove_file(pending_dkg_boundary_path(storage_dir));
    let _ = std::fs::remove_file(storage_dir.join(DKG_PENDING_BOUNDARY_TMP_FILE));
}

pub(in crate::stack) fn pending_boundary_is_finalized(
    pending: &PendingDkgBoundarySnapshot,
    recovered_boundary: Option<&(u64, DkgBoundaryArtifact)>,
) -> bool {
    recovered_boundary.is_some_and(|(_height, artifact)| {
        artifact == &pending.artifact || artifact.dkg_cycle > pending.artifact.dkg_cycle
    })
}

fn validate_pending_boundary_snapshot(
    snapshot: &PendingDkgBoundarySnapshot,
    local_output: &Output<MinSig, bls12381::PublicKey>,
    node: &OutbeFullNode,
) -> Result<()> {
    let boundary_output = decode_boundary_output(&snapshot.artifact)
        .wrap_err("failed to decode pending DKG boundary output")?;
    dkg_manager::assert_canonical_output(
        local_output,
        &boundary_output,
        "pending boundary snapshot",
    )?;
    let (frozen, tee_expired_target_exclusions) = match refresh_validator_set_at_height(
        node,
        snapshot.artifact.freeze_height,
    )? {
        FrozenValidatorSetRefresh::Ready {
            validator_set,
            tee_expired_target_exclusions,
            ..
        } => (validator_set, tee_expired_target_exclusions),
        FrozenValidatorSetRefresh::PendingBlockHash => {
            return Err(eyre::eyre!(
                "pending DKG boundary freeze-height state unavailable at height {}; refusing unsafe recovery",
                snapshot.artifact.freeze_height
            ));
        }
    };
    let activated_validator_set = validator_set_for_dkg_output_players(local_output, &frozen)?;
    let rebuilt = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(snapshot.artifact.epoch),
        validator_set: &activated_validator_set,
        output: local_output,
        is_full_dkg: false,
        dkg_cycle: snapshot.artifact.dkg_cycle,
        freeze_height: snapshot.artifact.freeze_height,
        planned_activation_height: snapshot.artifact.planned_activation_height,
        vrf_material_version: snapshot.artifact.vrf_material_version,
        is_validator_set_change: snapshot.artifact.is_validator_set_change,
        tee_expired_target_exclusions,
    })?;
    ensure!(
        rebuilt == snapshot.artifact,
        "pending DKG boundary snapshot does not match freeze-height validator set and DKG output"
    );
    Ok(())
}

pub(in crate::stack) fn recover_pending_dkg_boundary_snapshot(
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    local_consensus_key: &bls12381::PublicKey,
    node: &OutbeFullNode,
    recovered_boundary: Option<&(u64, DkgBoundaryArtifact)>,
) -> Result<Option<PendingDkgBoundarySnapshot>> {
    let Some(snapshot) = load_pending_dkg_boundary(storage_dir)? else {
        return Ok(None);
    };
    if let Some((_height, finalized)) = recovered_boundary {
        ensure!(
            finalized.dkg_cycle != snapshot.artifact.dkg_cycle || finalized == &snapshot.artifact,
            "finalized DKG boundary conflicts with pending snapshot for cycle {}",
            snapshot.artifact.dkg_cycle
        );
    }
    if pending_boundary_is_finalized(&snapshot, recovered_boundary) {
        let exact_boundary_finalized =
            recovered_boundary.is_some_and(|(_height, finalized)| finalized == &snapshot.artifact);
        if exact_boundary_finalized {
            let boundary_output = decode_boundary_output(&snapshot.artifact)
                .wrap_err("failed to decode finalized pending DKG boundary output")?;
            if boundary_output
                .players()
                .position(local_consensus_key)
                .is_some()
            {
                let (share, polynomial, pending_output) =
                    load_pending_dkg_state(storage_dir, key_backend)?.ok_or_else(|| {
                        eyre::eyre!(
                            "finalized pending DKG boundary includes the local validator but pending private material is unavailable"
                        )
                    })?;
                dkg_manager::assert_canonical_output(
                    &pending_output,
                    &boundary_output,
                    "finalized pending DKG material",
                )?;
                save_dkg_state(
                    storage_dir,
                    &share,
                    &polynomial,
                    &pending_output,
                    key_backend,
                )
                .wrap_err("failed to promote finalized pending DKG state during restart")?;
            }
        }
        clear_pending_dkg_boundary(storage_dir);
        remove_pending_dkg_state(storage_dir);
        dkg_actor::DkgRetryStore::in_keys_dir(storage_dir, key_backend.clone())
            .clear()
            .wrap_err("failed to retire finalized pending DKG retry state during restart")?;
        info!(
            storage_dir = %storage_dir.display(),
            completed_at_height = snapshot.completed_at_height,
            dkg_cycle = snapshot.artifact.dkg_cycle,
            exact_boundary_finalized,
            "retired pending DKG snapshot after finalized boundary recovery"
        );
        return Ok(None);
    }

    let boundary_output = decode_boundary_output(&snapshot.artifact)
        .wrap_err("failed to decode recovered pending DKG boundary output")?;
    validate_pending_boundary_snapshot(&snapshot, &boundary_output, node)?;
    let local_is_incoming_participant = boundary_output
        .players()
        .position(local_consensus_key)
        .is_some();
    match load_pending_dkg_state(storage_dir, key_backend)? {
        Some((_, _, pending_output)) => {
            dkg_manager::assert_canonical_output(
                &pending_output,
                &boundary_output,
                "recovered pending DKG material",
            )?;
        }
        None if local_is_incoming_participant => {
            return Err(eyre::eyre!(
                "pending DKG boundary includes the local validator but matching pending private material is unavailable"
            ));
        }
        None => {
            info!(
                epoch = snapshot.artifact.epoch,
                dkg_cycle = snapshot.artifact.dkg_cycle,
                "recovered dealer-only pending DKG boundary without incoming private share"
            );
        }
    }
    Ok(Some(snapshot))
}

pub(in crate::stack) fn restore_pending_dkg_activation(
    snapshot: PendingDkgBoundarySnapshot,
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    local_consensus_key: &bls12381::PublicKey,
    node: &OutbeFullNode,
) -> Result<RestoredPendingDkgActivation> {
    let output = decode_boundary_output(&snapshot.artifact)
        .wrap_err("failed to decode pending DKG output during runtime restore")?;
    let (validator_set, tee_expired_target_exclusions) =
        match refresh_validator_set_at_height(node, snapshot.artifact.freeze_height)? {
            FrozenValidatorSetRefresh::Ready {
                validator_set,
                tee_expired_target_exclusions,
                ..
            } => (validator_set, tee_expired_target_exclusions),
            FrozenValidatorSetRefresh::PendingBlockHash => {
                return Err(eyre::eyre!(
                "pending DKG freeze-height state unavailable at height {} during runtime restore",
                snapshot.artifact.freeze_height
            ));
            }
        };
    let activated_validator_set = validator_set_for_dkg_output_players(&output, &validator_set)
        .wrap_err("pending DKG output does not match its frozen validator set")?;
    let participants = participants_from_validator_set(&activated_validator_set)?;
    ensure!(
        tee_expired_target_exclusions == snapshot.artifact.tee_expired_target_exclusions,
        "pending DKG TEE expiry exclusions do not match freeze-height state"
    );
    let target = FrozenDkgTarget {
        dkg_cycle: snapshot.artifact.dkg_cycle,
        freeze_height: snapshot.artifact.freeze_height,
        planned_activation_height: snapshot.artifact.planned_activation_height,
        validator_set,
        participants: participants.clone(),
        tee_expired_target_exclusions,
        is_validator_set_change: snapshot.artifact.is_validator_set_change,
    };

    if participants.position(local_consensus_key).is_some() {
        let (share, _polynomial, pending_output) =
            load_pending_dkg_state(storage_dir, key_backend)?.ok_or_else(|| {
                eyre::eyre!(
                    "pending DKG boundary includes the local validator but pending private material is unavailable"
                )
            })?;
        dkg_manager::assert_canonical_output(
            &pending_output,
            &output,
            "restored pending participant DKG state",
        )?;
        Ok(RestoredPendingDkgActivation::Participant(
            PendingDkgActivation {
                target,
                complete: dkg_actor::DkgComplete {
                    output: output.clone(),
                    share,
                    participants,
                },
                boundary_artifact: snapshot.artifact,
                recovered_output: Some(output),
            },
        ))
    } else {
        Ok(RestoredPendingDkgActivation::DealerOnly(
            DealerOnlyDkgActivation {
                target,
                boundary_artifact: Some(snapshot.artifact),
                recovered_output: Some(output),
            },
        ))
    }
}

type PersistedDkgState = (Share, Sharing<MinSig>, Output<MinSig, bls12381::PublicKey>);

#[allow(clippy::too_many_arguments)]
fn load_dkg_state_files(
    storage_dir: &std::path::Path,
    share_file: &str,
    polynomial_file: &str,
    output_file: &str,
    key_backend: &bls::KeyBackend,
    label: &str,
) -> Result<Option<PersistedDkgState>> {
    let share_path = storage_dir.join(share_file);
    let poly_path = storage_dir.join(polynomial_file);
    let output_path = storage_dir.join(output_file);

    let has_share = share_path.exists();
    let has_poly = poly_path.exists();
    let has_output = output_path.exists();

    if !has_share && !has_poly && !has_output {
        return Ok(None);
    }

    ensure!(
        has_share && has_poly && has_output,
        "{label} DKG state is incomplete in {}: expected {}, {}, and {}",
        storage_dir.display(),
        share_file,
        polynomial_file,
        output_file,
    );

    let signing_share = bls::load_signing_share(&share_path, key_backend)
        .wrap_err_with(|| format!("failed to load BLS signing share from {label} DKG state"))?;
    let polynomial = bls::load_public_polynomial(&poly_path, key_backend)
        .wrap_err_with(|| format!("failed to load BLS public polynomial from {label} DKG state"))?;
    let output = bls::load_dkg_output(&output_path, key_backend)
        .wrap_err_with(|| format!("failed to load BLS DKG output from {label} DKG state"))?;
    bls::validate_dkg_triplet(&signing_share, &polynomial, &output)
        .wrap_err_with(|| format!("{label} DKG state triplet is inconsistent"))?;

    Ok(Some((signing_share, polynomial, output)))
}

/// Load finalized DKG results from disk for crash recovery.
#[allow(clippy::type_complexity)]
pub(in crate::stack) fn load_saved_dkg_state(
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
) -> Result<Option<PersistedDkgState>> {
    load_dkg_state_files(
        storage_dir,
        DKG_SHARE_FILE,
        DKG_POLYNOMIAL_FILE,
        DKG_OUTPUT_FILE,
        key_backend,
        "saved",
    )
}

pub(in crate::stack) fn load_pending_dkg_state(
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
) -> Result<Option<PersistedDkgState>> {
    load_dkg_state_files(
        storage_dir,
        DKG_PENDING_SHARE_FILE,
        DKG_PENDING_POLYNOMIAL_FILE,
        DKG_PENDING_OUTPUT_FILE,
        key_backend,
        "pending",
    )
}

#[allow(clippy::too_many_arguments)]
fn save_dkg_state_files(
    storage_dir: &std::path::Path,
    share_file: &str,
    polynomial_file: &str,
    output_file: &str,
    share: &Share,
    polynomial: &Sharing<MinSig>,
    output: &Output<MinSig, bls12381::PublicKey>,
    key_backend: &bls::KeyBackend,
) -> Result<()> {
    std::fs::create_dir_all(storage_dir)
        .wrap_err_with(|| format!("failed to create storage dir: {}", storage_dir.display()))?;

    bls::save_signing_share(&storage_dir.join(share_file), share, key_backend)
        .wrap_err("failed to save DKG signing share")?;

    bls::save_public_polynomial(&storage_dir.join(polynomial_file), polynomial, key_backend)
        .wrap_err("failed to save DKG public polynomial")?;

    bls::save_dkg_output(&storage_dir.join(output_file), output, key_backend)
        .wrap_err("failed to save DKG output artifact")?;

    Ok(())
}

/// Save finalized DKG results to disk for crash recovery.
pub(in crate::stack) fn save_dkg_state(
    storage_dir: &std::path::Path,
    share: &Share,
    polynomial: &Sharing<MinSig>,
    output: &Output<MinSig, bls12381::PublicKey>,
    key_backend: &bls::KeyBackend,
) -> Result<()> {
    save_dkg_state_files(
        storage_dir,
        DKG_SHARE_FILE,
        DKG_POLYNOMIAL_FILE,
        DKG_OUTPUT_FILE,
        share,
        polynomial,
        output,
        key_backend,
    )
}

pub(in crate::stack) fn save_pending_dkg_state(
    storage_dir: &std::path::Path,
    share: &Share,
    polynomial: &Sharing<MinSig>,
    output: &Output<MinSig, bls12381::PublicKey>,
    key_backend: &bls::KeyBackend,
) -> Result<()> {
    save_dkg_state_files(
        storage_dir,
        DKG_PENDING_SHARE_FILE,
        DKG_PENDING_POLYNOMIAL_FILE,
        DKG_PENDING_OUTPUT_FILE,
        share,
        polynomial,
        output,
        key_backend,
    )
}

pub(in crate::stack) fn remove_pending_dkg_state(storage_dir: &std::path::Path) {
    for file in [
        DKG_PENDING_SHARE_FILE,
        DKG_PENDING_POLYNOMIAL_FILE,
        DKG_PENDING_OUTPUT_FILE,
    ] {
        let _ = std::fs::remove_file(storage_dir.join(file));
    }
}
