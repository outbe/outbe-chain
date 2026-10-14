use super::super::*;

/// Active authority and the exact frozen target used to construct the next boundary.
pub(in crate::stack) struct DkgBoundaryContext<'a> {
    pub(in crate::stack) current_epoch: Epoch,
    pub(in crate::stack) vrf_material_version: u64,
    pub(in crate::stack) current_participants:
        &'a commonware_utils::ordered::Set<bls12381::PublicKey>,
    pub(in crate::stack) target: &'a FrozenDkgTarget,
}

/// File custody shared by material reads and writes.
pub(in crate::stack) struct DkgStateStore<'a> {
    pub(in crate::stack) directory: &'a std::path::Path,
    pub(in crate::stack) key_backend: &'a bls::KeyBackend,
}

impl<'a> DkgStateStore<'a> {
    pub(in crate::stack) fn new(
        directory: &'a std::path::Path,
        key_backend: &'a bls::KeyBackend,
    ) -> Self {
        Self {
            directory,
            key_backend,
        }
    }
}

struct DkgStateFiles {
    share: &'static str,
    polynomial: &'static str,
    output: &'static str,
    label: &'static str,
}

pub(in crate::stack) struct DkgStateMaterial<'a> {
    pub(in crate::stack) share: &'a Share,
    pub(in crate::stack) polynomial: &'a Sharing<MinSig>,
    pub(in crate::stack) output: &'a Output<MinSig, bls12381::PublicKey>,
}

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

const SAVED_DKG_FILES: DkgStateFiles = DkgStateFiles {
    share: DKG_SHARE_FILE,
    polynomial: DKG_POLYNOMIAL_FILE,
    output: DKG_OUTPUT_FILE,
    label: "saved",
};

const PENDING_DKG_FILES: DkgStateFiles = DkgStateFiles {
    share: DKG_PENDING_SHARE_FILE,
    polynomial: DKG_PENDING_POLYNOMIAL_FILE,
    output: DKG_PENDING_OUTPUT_FILE,
    label: "pending",
};

pub(in crate::stack) fn dkg_retry_store(
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
) -> Result<dkg_actor::DkgRetryStore> {
    args.keys_dir
        .as_ref()
        .map(|keys_dir| dkg_actor::DkgRetryStore::in_keys_dir(keys_dir, key_backend.clone()))
        .ok_or_else(|| crate::errors::StackDkgError::DkgParticipantRecoveryRequiresKeysDir.into())
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
    if decoded.epoch.get() != artifact.epoch {
        return Err(crate::errors::StackDkgError::BoundaryOutcomeEpochMismatch {
            decoded_epoch: decoded.epoch.get(),
            artifact_epoch: artifact.epoch,
        }
        .into());
    }
    Ok(decoded.output)
}

pub(in crate::stack) fn encode_pending_dkg_boundary_snapshot(
    snapshot: &PendingDkgBoundarySnapshot,
) -> Result<Vec<u8>> {
    let boundary = encode_boundary_artifact(&snapshot.artifact).map_err(|error| {
        crate::errors::StackDkgError::EncodePendingBoundaryArtifact {
            reason: format!("{error}"),
        }
    })?;
    let len: u32 = boundary.len().try_into().map_err(|_| {
        crate::errors::StackDkgError::PendingBoundaryArtifactTooLarge {
            len: boundary.len(),
        }
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
    if bytes.len() < header_len {
        return Err(
            crate::errors::StackDkgError::PendingBoundarySnapshotTooShort {
                actual: bytes.len(),
                expected: header_len,
            }
            .into(),
        );
    }
    let magic = &bytes[..DKG_PENDING_BOUNDARY_MAGIC.len()];
    if magic == DKG_PENDING_BOUNDARY_LEGACY_MAGIC {
        return Err(crate::errors::StackDkgError::UnsupportedPendingBoundarySnapshotVersion.into());
    }
    if magic != DKG_PENDING_BOUNDARY_MAGIC {
        return Err(crate::errors::StackDkgError::InvalidPendingBoundarySnapshotMagic.into());
    }
    let height_offset = DKG_PENDING_BOUNDARY_MAGIC.len();
    let completed_at_height = u64::from_be_bytes(
        bytes[height_offset..height_offset + 8]
            .try_into()
            .map_err(|_| crate::errors::StackDkgError::InvalidPendingBoundaryHeightField)?,
    );
    let len_offset = height_offset + 8;
    let artifact_len = u32::from_be_bytes(
        bytes[len_offset..len_offset + 4]
            .try_into()
            .map_err(|_| crate::errors::StackDkgError::InvalidPendingBoundaryLengthField)?,
    ) as usize;
    let artifact_start = len_offset + 4;
    let artifact_end = artifact_start
        .checked_add(artifact_len)
        .ok_or(crate::errors::StackDkgError::PendingBoundaryLengthOverflow)?;
    if bytes.len() != artifact_end {
        return Err(
            crate::errors::StackDkgError::PendingBoundarySnapshotLengthMismatch {
                expected: artifact_end,
                actual: bytes.len(),
            }
            .into(),
        );
    }
    let artifact = decode_boundary_artifact(&bytes[artifact_start..artifact_end])
        .map_err(
            |error| crate::errors::StackDkgError::DecodePendingBoundaryArtifact {
                reason: format!("{error}"),
            },
        )?
        .ok_or(crate::errors::StackDkgError::PendingBoundarySnapshotMissingBoundaryOutcome)?;
    Ok(PendingDkgBoundarySnapshot {
        artifact,
        completed_at_height,
    })
}

fn pending_dkg_boundary_path(storage_dir: &std::path::Path) -> std::path::PathBuf {
    storage_dir.join(DKG_PENDING_BOUNDARY_FILE)
}

pub(in crate::stack) fn build_completed_dkg_boundary(
    context: DkgBoundaryContext<'_>,
    output: &Output<MinSig, bls12381::PublicKey>,
    completed_participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
) -> Result<DkgBoundaryArtifact> {
    let DkgBoundaryContext {
        current_epoch,
        vrf_material_version,
        current_participants,
        target,
    } = context;
    let activated_validator_set =
        validator_set_for_dkg_output_players(output, &target.validator_set)?;
    let activated_participants = participants_from_validator_set(&activated_validator_set)?;
    if completed_participants != &activated_participants {
        return Err(crate::errors::StackDkgError::CompletedParticipantSetMismatch.into());
    }
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

pub(in crate::stack) fn persist_completed_dkg_before_activation(
    store: DkgStateStore<'_>,
    context: DkgBoundaryContext<'_>,
    complete: &dkg_actor::DkgComplete,
    completed_at_height: u64,
) -> Result<DkgBoundaryArtifact> {
    let DkgStateStore {
        directory: keys_dir,
        key_backend,
    } = store;
    let current_epoch = context.current_epoch;
    let target = context.target;
    let boundary_artifact =
        build_completed_dkg_boundary(context, &complete.output, &complete.participants)?;
    let next_epoch = next_consensus_epoch_after_dkg_activation(current_epoch);

    save_pending_dkg_state(
        DkgStateStore::new(keys_dir, key_backend),
        DkgStateMaterial {
            share: &complete.share,
            polynomial: complete.output.public(),
            output: &complete.output,
        },
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

pub(in crate::stack) fn persist_observed_dkg_boundary_before_activation(
    keys_dir: &std::path::Path,
    context: DkgBoundaryContext<'_>,
    output: &Output<MinSig, bls12381::PublicKey>,
    completed_at_height: u64,
) -> Result<DkgBoundaryArtifact> {
    let target = context.target;
    let boundary_artifact = build_completed_dkg_boundary(context, output, &target.participants)?;
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

fn require_frozen_validator_set(
    node: &OutbeFullNode,
    freeze_height: u64,
    unavailable: impl FnOnce() -> eyre::Report,
) -> Result<(validators::ValidatorSet, Vec<EthAddress>)> {
    match refresh_validator_set_at_height(node, freeze_height)? {
        FrozenValidatorSetRefresh::Ready {
            validator_set,
            tee_expired_target_exclusions,
            ..
        } => Ok((validator_set, tee_expired_target_exclusions)),
        FrozenValidatorSetRefresh::PendingBlockHash => Err(unavailable()),
    }
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
    let (frozen, tee_expired_target_exclusions) =
        require_frozen_validator_set(node, snapshot.artifact.freeze_height, || {
            crate::errors::StackDkgError::PendingBoundaryFreezeHeightStateUnavailable {
                height: snapshot.artifact.freeze_height,
            }
            .into()
        })?;
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
    if rebuilt != snapshot.artifact {
        return Err(crate::errors::StackDkgError::PendingBoundarySnapshotFreezeMismatch.into());
    }
    Ok(())
}

fn quarantine_pending_dkg_boundary_snapshot(
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    reason: &str,
) {
    warn!(
        storage_dir = %storage_dir.display(),
        reason,
        "quarantining unfinalized or stale local pending DKG boundary snapshot"
    );
    clear_pending_dkg_boundary(storage_dir);
    remove_pending_dkg_state(storage_dir);
    let _ = dkg_actor::DkgRetryStore::in_keys_dir(storage_dir, key_backend.clone()).clear();
}

fn load_or_quarantine_pending_boundary(
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
) -> Option<PendingDkgBoundarySnapshot> {
    match load_pending_dkg_boundary(storage_dir) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            quarantine_pending_dkg_boundary_snapshot(
                storage_dir,
                key_backend,
                &format!("failed to decode pending DKG boundary snapshot: {error:#}"),
            );
            None
        }
    }
}

fn check_finalized_pending_boundary_conflict(
    snapshot: &PendingDkgBoundarySnapshot,
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    local_consensus_key: &bls12381::PublicKey,
    recovered_boundary: Option<&(u64, DkgBoundaryArtifact)>,
) -> Result<bool> {
    let Some((_height, finalized)) = recovered_boundary.filter(|(_height, finalized)| {
        finalized.dkg_cycle == snapshot.artifact.dkg_cycle && finalized != &snapshot.artifact
    }) else {
        return Ok(false);
    };
    let finalized_output = decode_boundary_output(finalized)
        .wrap_err("failed to decode finalized DKG boundary output")?;
    let local_in_finalized = finalized_output
        .players()
        .position(local_consensus_key)
        .is_some();
    let saved_matches_finalized = load_saved_dkg_state(storage_dir, key_backend)
        .ok()
        .flatten()
        .is_some_and(|(_, _, saved_output)| saved_output == finalized_output);
    if !local_in_finalized || saved_matches_finalized {
        quarantine_pending_dkg_boundary_snapshot(
            storage_dir,
            key_backend,
            &format!(
                "superseded local pending DKG boundary for cycle {} after finalized boundary commit",
                snapshot.artifact.dkg_cycle
            ),
        );
        return Ok(true);
    }
    Err(
        crate::errors::StackDkgError::FinalizedBoundaryConflictsWithPendingSnapshot {
            cycle: snapshot.artifact.dkg_cycle,
        }
        .into(),
    )
}

fn retire_finalized_pending_boundary(
    snapshot: &PendingDkgBoundarySnapshot,
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    local_consensus_key: &bls12381::PublicKey,
    recovered_boundary: Option<&(u64, DkgBoundaryArtifact)>,
) -> Result<()> {
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
                load_pending_dkg_state(storage_dir, key_backend)?.ok_or(
                    crate::errors::StackDkgError::FinalizedPendingBoundaryMissingPrivateMaterial,
                )?;
            dkg_manager::assert_canonical_output(
                &pending_output,
                &boundary_output,
                "finalized pending DKG material",
            )?;
            save_dkg_state(
                DkgStateStore::new(storage_dir, key_backend),
                DkgStateMaterial {
                    share: &share,
                    polynomial: &polynomial,
                    output: &pending_output,
                },
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
    Ok(())
}

fn validate_unfinalized_pending_boundary(
    snapshot: PendingDkgBoundarySnapshot,
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    local_consensus_key: &bls12381::PublicKey,
    node: &OutbeFullNode,
) -> Result<Option<PendingDkgBoundarySnapshot>> {
    let Ok(boundary_output) = decode_boundary_output(&snapshot.artifact).inspect_err(|error| {
        quarantine_pending_dkg_boundary_snapshot(
            storage_dir,
            key_backend,
            &format!("failed to decode unfinalized pending DKG boundary output: {error:#}"),
        );
    }) else {
        return Ok(None);
    };
    if let Err(error) = validate_pending_boundary_snapshot(&snapshot, &boundary_output, node) {
        if matches!(
            error.downcast_ref::<crate::errors::StackDkgError>(),
            Some(crate::errors::StackDkgError::PendingBoundarySnapshotFreezeMismatch)
        ) {
            quarantine_pending_dkg_boundary_snapshot(
                storage_dir,
                key_backend,
                &format!("unfinalized pending DKG boundary does not match freeze state: {error:#}"),
            );
            return Ok(None);
        }
        return Err(error);
    }
    let local_is_incoming_participant = boundary_output
        .players()
        .position(local_consensus_key)
        .is_some();
    if let Some(reason) = check_unfinalized_pending_private_state(
        &snapshot,
        &boundary_output,
        storage_dir,
        key_backend,
        local_is_incoming_participant,
    ) {
        quarantine_pending_dkg_boundary_snapshot(storage_dir, key_backend, &reason);
        return Ok(None);
    }
    Ok(Some(snapshot))
}

fn check_unfinalized_pending_private_state(
    snapshot: &PendingDkgBoundarySnapshot,
    boundary_output: &Output<MinSig, bls12381::PublicKey>,
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    local_is_incoming_participant: bool,
) -> Option<String> {
    match load_pending_dkg_state(storage_dir, key_backend) {
        Ok(Some((_, _, pending_output))) => dkg_manager::assert_canonical_output(
            &pending_output,
            boundary_output,
            "recovered pending DKG material",
        )
        .err()
        .map(|error| {
            format!("unfinalized pending DKG material does not match pending boundary: {error:#}")
        }),
        Ok(None) if local_is_incoming_participant => {
            Some("unfinalized pending DKG boundary missing local private material".to_string())
        }
        Ok(None) => {
            info!(
                epoch = snapshot.artifact.epoch,
                dkg_cycle = snapshot.artifact.dkg_cycle,
                "recovered dealer-only pending DKG boundary without incoming private share"
            );
            None
        }
        Err(error) => Some(format!(
            "unfinalized pending DKG private state is incomplete or corrupt: {error:#}"
        )),
    }
}

pub(in crate::stack) fn recover_pending_dkg_boundary_snapshot(
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    local_consensus_key: &bls12381::PublicKey,
    node: &OutbeFullNode,
    recovered_boundary: Option<&(u64, DkgBoundaryArtifact)>,
) -> Result<Option<PendingDkgBoundarySnapshot>> {
    let Some(snapshot) = load_or_quarantine_pending_boundary(storage_dir, key_backend) else {
        return Ok(None);
    };
    if check_finalized_pending_boundary_conflict(
        &snapshot,
        storage_dir,
        key_backend,
        local_consensus_key,
        recovered_boundary,
    )? {
        return Ok(None);
    }
    if pending_boundary_is_finalized(&snapshot, recovered_boundary) {
        retire_finalized_pending_boundary(
            &snapshot,
            storage_dir,
            key_backend,
            local_consensus_key,
            recovered_boundary,
        )?;
        return Ok(None);
    }
    validate_unfinalized_pending_boundary(
        snapshot,
        storage_dir,
        key_backend,
        local_consensus_key,
        node,
    )
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
        require_frozen_validator_set(node, snapshot.artifact.freeze_height, || {
            crate::errors::StackDkgError::PendingFreezeHeightStateUnavailableDuringRestore {
                height: snapshot.artifact.freeze_height,
            }
            .into()
        })?;
    let activated_validator_set = validator_set_for_dkg_output_players(&output, &validator_set)
        .wrap_err("pending DKG output does not match its frozen validator set")?;
    let participants = participants_from_validator_set(&activated_validator_set)?;
    if tee_expired_target_exclusions != snapshot.artifact.tee_expired_target_exclusions {
        return Err(crate::errors::StackDkgError::PendingTeeExpiryExclusionsMismatch.into());
    }
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
            load_pending_dkg_state(storage_dir, key_backend)?
                .ok_or(crate::errors::StackDkgError::PendingBoundaryMissingPrivateMaterial)?;
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

fn load_dkg_state_files(
    store: DkgStateStore<'_>,
    files: DkgStateFiles,
) -> Result<Option<PersistedDkgState>> {
    let DkgStateStore {
        directory: storage_dir,
        key_backend,
    } = store;
    let DkgStateFiles {
        share: share_file,
        polynomial: polynomial_file,
        output: output_file,
        label,
    } = files;
    let share_path = storage_dir.join(share_file);
    let poly_path = storage_dir.join(polynomial_file);
    let output_path = storage_dir.join(output_file);

    let has_share = share_path.exists();
    let has_poly = poly_path.exists();
    let has_output = output_path.exists();

    if !has_share && !has_poly && !has_output {
        return Ok(None);
    }

    if !(has_share && has_poly && has_output) {
        return Err(crate::errors::StackDkgError::IncompleteDkgState {
            label,
            dir: format!("{}", storage_dir.display()),
            share_file,
            polynomial_file,
            output_file,
        }
        .into());
    }

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
        DkgStateStore {
            directory: storage_dir,
            key_backend,
        },
        SAVED_DKG_FILES,
    )
}

pub(in crate::stack) fn load_pending_dkg_state(
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
) -> Result<Option<PersistedDkgState>> {
    load_dkg_state_files(
        DkgStateStore {
            directory: storage_dir,
            key_backend,
        },
        PENDING_DKG_FILES,
    )
}

fn save_dkg_state_files(
    store: DkgStateStore<'_>,
    files: DkgStateFiles,
    material: DkgStateMaterial<'_>,
) -> Result<()> {
    let DkgStateStore {
        directory: storage_dir,
        key_backend,
    } = store;
    let DkgStateFiles {
        share: share_file,
        polynomial: polynomial_file,
        output: output_file,
        ..
    } = files;
    let DkgStateMaterial {
        share,
        polynomial,
        output,
    } = material;
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
    store: DkgStateStore<'_>,
    material: DkgStateMaterial<'_>,
) -> Result<()> {
    save_dkg_state_files(store, SAVED_DKG_FILES, material)
}

pub(in crate::stack) fn save_pending_dkg_state(
    store: DkgStateStore<'_>,
    material: DkgStateMaterial<'_>,
) -> Result<()> {
    save_dkg_state_files(store, PENDING_DKG_FILES, material)
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
