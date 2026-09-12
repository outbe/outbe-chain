use super::super::*;

#[derive(Clone, Copy, Debug)]
pub(in crate::stack) struct DkgRotationParams {
    pub(in crate::stack) epoch_length_blocks: u64,
    pub(in crate::stack) prepare_window_blocks: u64,
    pub(in crate::stack) activation_grace_blocks: u64,
}

impl DkgRotationParams {
    pub(in crate::stack) fn from_genesis(node: &OutbeFullNode, epoch_length_blocks: u32) -> Self {
        let extra = &node.chain_spec().genesis.config.extra_fields;
        let prepare_window_blocks = extra
            .get_deserialized::<u64>("dkgPrepareWindowBlocks")
            .and_then(|r| r.ok())
            .unwrap_or(config::DEFAULT_DKG_PREPARE_WINDOW_BLOCKS);
        let activation_grace_blocks = extra
            .get_deserialized::<u64>("dkgActivationGraceBlocks")
            .and_then(|r| r.ok())
            .unwrap_or(config::DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS);

        let epoch_length_blocks = u64::from(epoch_length_blocks);
        Self {
            epoch_length_blocks,
            prepare_window_blocks: prepare_window_blocks.min(epoch_length_blocks),
            activation_grace_blocks,
        }
    }

    pub(in crate::stack) fn planned_activation_height(self, last_activation_height: u64) -> u64 {
        last_activation_height.saturating_add(self.epoch_length_blocks)
    }

    pub(in crate::stack) fn freeze_height(self, last_activation_height: u64) -> u64 {
        self.planned_activation_height(last_activation_height)
            .saturating_sub(self.prepare_window_blocks)
    }
}

pub(in crate::stack) fn publish_randomness_status(
    bridge: &ConsensusExecutionBridge,
    vrf_safety: &VrfSafetyGate,
) {
    let snapshot = vrf_safety.snapshot();
    let mut status = bridge.consensus_status();
    status.randomness_status = snapshot.randomness_status;
    status.vrf_material_version = snapshot.vrf_material_version;
    status.last_dkg_activation_height = snapshot.last_dkg_activation_height;
    status.next_planned_activation_height = snapshot.next_planned_activation_height;
    status.vrf_expiry_height = snapshot.vrf_expiry_height;
    info!(
        randomness_status = ?snapshot.randomness_status,
        vrf_material_version = snapshot.vrf_material_version,
        last_dkg_activation_height = snapshot.last_dkg_activation_height,
        next_planned_activation_height = snapshot.next_planned_activation_height,
        vrf_expiry_height = snapshot.vrf_expiry_height,
        "VRF/DKG safety status updated"
    );
    bridge.set_consensus_status(status);
}

/// Couples the active VRF provider transition with the bridge's process-local
/// authority report. Publication is ordered fail-safe: adding a
/// share becomes visible only after provider installation, while removing one
/// becomes visible before provider demotion.
pub(in crate::stack) fn activate_vrf_material_and_publish_local_share(
    bridge: &ConsensusExecutionBridge,
    vrf_materials: &VrfMaterialProvider<MinSig>,
    version: u64,
    polynomial: Sharing<MinSig>,
    signing_share: Option<Share>,
) {
    let local_share_present = signing_share.is_some();
    if !local_share_present {
        bridge.set_local_threshold_share_present(false);
    }
    vrf_materials.activate(version, polynomial, signing_share);
    if local_share_present {
        bridge.set_local_threshold_share_present(true);
    }
}

#[derive(Clone, Debug)]
pub(in crate::stack) struct FrozenDkgTarget {
    pub(in crate::stack) dkg_cycle: u64,
    pub(in crate::stack) freeze_height: u64,
    pub(in crate::stack) planned_activation_height: u64,
    pub(in crate::stack) validator_set: validators::ValidatorSet,
    pub(in crate::stack) participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    pub(in crate::stack) tee_expired_target_exclusions: Vec<EthAddress>,
    pub(in crate::stack) is_validator_set_change: bool,
}

#[derive(Clone, Debug)]
pub(in crate::stack) struct PendingDkgActivation {
    pub(in crate::stack) target: FrozenDkgTarget,
    pub(in crate::stack) complete: dkg_actor::DkgComplete,
    pub(in crate::stack) boundary_artifact: DkgBoundaryArtifact,
    /// Present only after restart. The process-local finalized dealer-log
    /// reconstruction is gone, but this output was revalidated against both
    /// the durable pending material and the exact boundary artifact.
    pub(in crate::stack) recovered_output: Option<Output<MinSig, bls12381::PublicKey>>,
}

#[derive(Clone, Debug)]
pub(in crate::stack) struct DealerOnlyDkgActivation {
    pub(in crate::stack) target: FrozenDkgTarget,
    pub(in crate::stack) boundary_artifact: Option<DkgBoundaryArtifact>,
    pub(in crate::stack) recovered_output: Option<Output<MinSig, bls12381::PublicKey>>,
}

pub(in crate::stack) enum RestoredPendingDkgActivation {
    Participant(PendingDkgActivation),
    DealerOnly(DealerOnlyDkgActivation),
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub(in crate::stack) enum DkgTaskOutcome {
    Complete(dkg_actor::DkgComplete),
    DealerOnly(dkg_actor::DkgDealerOnlyComplete),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::stack) enum LocalDkgRole {
    DealerAndPlayer,
    DealerOnly,
    PlayerOnly,
    NotParticipant,
}

pub(in crate::stack) fn classify_local_reshare_role(
    local: &bls12381::PublicKey,
    previous_output: Option<&Output<MinSig, bls12381::PublicKey>>,
    target_participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
) -> LocalDkgRole {
    let is_player = target_participants.position(local).is_some();
    let is_dealer = previous_output
        .map(|output| output.players().position(local).is_some())
        .unwrap_or(is_player);

    match (is_dealer, is_player) {
        (true, true) => LocalDkgRole::DealerAndPlayer,
        (true, false) => LocalDkgRole::DealerOnly,
        (false, true) => LocalDkgRole::PlayerOnly,
        (false, false) => LocalDkgRole::NotParticipant,
    }
}

pub(in crate::stack) enum FrozenValidatorSetRefresh {
    Ready {
        validator_set: validators::ValidatorSet,
        participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
        tee_expired_target_exclusions: Vec<EthAddress>,
    },
    PendingBlockHash,
}

pub(in crate::stack) fn should_start_dkg_rotation(
    has_frozen_target: bool,
    has_pending_activation: bool,
    current_height: u64,
    freeze_height: u64,
) -> bool {
    !has_frozen_target && !has_pending_activation && current_height >= freeze_height
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::stack) enum PendingDkgHandoffDecision {
    Wait,
    Activate { activation_anchor: u64 },
    Expired { deadline: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::stack) enum StartupPendingDkgEpochPlan {
    /// The outgoing epoch remains active. Its channels must be acquired before
    /// the recovered future-epoch channels are pre-registered.
    Defer {
        active_epoch: Epoch,
        preregister_after_current: Epoch,
    },
    /// The outgoing-finalized preannounce already authorized the handoff. The
    /// first block after restart must therefore be built by the incoming epoch.
    Activate {
        previous_epoch: Epoch,
        active_epoch: Epoch,
        activation_anchor: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::stack) enum PendingFreezeBlockHashDecision {
    Retry,
    Expired,
}

pub(in crate::stack) fn pending_freeze_block_hash_decision(
    current_height: u64,
    planned_activation_height: u64,
) -> PendingFreezeBlockHashDecision {
    if current_height >= planned_activation_height {
        PendingFreezeBlockHashDecision::Expired
    } else {
        PendingFreezeBlockHashDecision::Retry
    }
}

pub(in crate::stack) fn pending_dkg_handoff_decision(
    finalized_height: u64,
    planned_activation_height: u64,
    activation_grace_blocks: u64,
    exact_carrier_height: Option<u64>,
) -> PendingDkgHandoffDecision {
    let deadline = planned_activation_height.saturating_add(activation_grace_blocks);
    let finalized_carrier_height = exact_carrier_height.filter(|carrier_height| {
        *carrier_height <= finalized_height && *carrier_height <= deadline
    });

    if let Some(carrier_height) = finalized_carrier_height {
        if finalized_height >= planned_activation_height {
            return PendingDkgHandoffDecision::Activate {
                activation_anchor: planned_activation_height.max(carrier_height),
            };
        }
        return PendingDkgHandoffDecision::Wait;
    }

    if finalized_height >= deadline {
        PendingDkgHandoffDecision::Expired { deadline }
    } else {
        PendingDkgHandoffDecision::Wait
    }
}

pub(in crate::stack) fn startup_pending_dkg_epoch_plan(
    current_epoch: Epoch,
    pending_epoch: Epoch,
    finalized_height: u64,
    planned_activation_height: u64,
    activation_grace_blocks: u64,
    exact_carrier_height: Option<u64>,
) -> Result<StartupPendingDkgEpochPlan> {
    let expected_epoch = next_consensus_epoch_after_dkg_activation(current_epoch);
    ensure!(
        pending_epoch == expected_epoch,
        "recovered pending DKG epoch {} does not follow active epoch {}",
        pending_epoch,
        current_epoch
    );
    match pending_dkg_handoff_decision(
        finalized_height,
        planned_activation_height,
        activation_grace_blocks,
        exact_carrier_height,
    ) {
        PendingDkgHandoffDecision::Wait => Ok(StartupPendingDkgEpochPlan::Defer {
            active_epoch: current_epoch,
            preregister_after_current: pending_epoch,
        }),
        PendingDkgHandoffDecision::Activate { activation_anchor } => {
            Ok(StartupPendingDkgEpochPlan::Activate {
                previous_epoch: current_epoch,
                active_epoch: pending_epoch,
                activation_anchor,
            })
        }
        PendingDkgHandoffDecision::Expired { deadline } => Err(eyre::eyre!(
            "recovered pending DKG epoch {} missed activation deadline {} at finalized height {}",
            pending_epoch,
            deadline,
            finalized_height
        )),
    }
}

pub(in crate::stack) fn select_pending_canonical_output(
    finalized_log_output: Option<Output<MinSig, bls12381::PublicKey>>,
    recovered_output: Option<&Output<MinSig, bls12381::PublicKey>>,
) -> Option<Output<MinSig, bls12381::PublicKey>> {
    finalized_log_output.or_else(|| recovered_output.cloned())
}

pub(in crate::stack) fn preannounce_matches_pending(
    artifact: &ConsensusHeaderArtifact,
    pending: &DkgBoundaryArtifact,
) -> bool {
    matches!(
        artifact,
        ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, outcome }
            if *epoch == pending.epoch && *outcome == pending.outcome
    )
}

pub(in crate::stack) fn find_exact_finalized_preannounce_carrier(
    provider: &(impl HeaderProvider<Header = OutbeHeader> + BlockHashReader),
    pending: &DkgBoundaryArtifact,
    finalized_height: u64,
    activation_grace_blocks: u64,
) -> Result<Option<u64>> {
    let deadline = pending
        .planned_activation_height
        .saturating_add(activation_grace_blocks);
    let scan_end = finalized_height.min(deadline);
    if pending.freeze_height > scan_end {
        return Ok(None);
    }

    let mut height = pending.freeze_height;
    loop {
        let canonical_hash = provider
            .block_hash(height)
            .map_err(|error| {
                eyre::eyre!("failed to read canonical hash at height {height}: {error}")
            })?
            .ok_or_else(|| {
                eyre::eyre!(
                    "canonical finalized preannounce scan is missing block hash at height {height}"
                )
            })?;
        let header = provider
            .sealed_header(height)
            .map_err(|error| eyre::eyre!("failed to read finalized header {height}: {error}"))?
            .ok_or_else(|| {
                eyre::eyre!(
                    "canonical finalized preannounce scan is missing header at height {height}"
                )
            })?;
        ensure!(
            header.hash() == canonical_hash,
            "canonical finalized preannounce header hash mismatch at height {height}: index {canonical_hash}, header {}",
            header.hash()
        );
        ensure!(
            header.header().inner.number == height,
            "canonical finalized preannounce header number mismatch: expected {height}, got {}",
            header.header().inner.number
        );
        let artifacts = decode_outbe_block_artifacts(header.header().inner.extra_data.as_ref())
            .map_err(|error| {
                eyre::eyre!(
                    "failed to decode finalized header artifacts at height {height}: {error}"
                )
            })?;
        if artifacts
            .consensus_header_artifact
            .as_ref()
            .is_some_and(|artifact| preannounce_matches_pending(artifact, pending))
        {
            return Ok(Some(height));
        }
        if height == scan_end {
            break;
        }
        height = height.saturating_add(1);
    }
    Ok(None)
}

pub(in crate::stack) fn frozen_dkg_target_expired(
    current_height: u64,
    planned_activation_height: u64,
    activation_grace_blocks: u64,
) -> bool {
    let deadline = planned_activation_height.saturating_add(activation_grace_blocks);
    current_height >= deadline
}

pub(in crate::stack) fn next_consensus_epoch_after_dkg_activation(current_epoch: Epoch) -> Epoch {
    Epoch::new(current_epoch.get().saturating_add(1))
}

pub(in crate::stack) fn next_dkg_cycle_after_restored_target(
    current_next_cycle: u64,
    restored_target_cycle: u64,
) -> u64 {
    current_next_cycle.max(restored_target_cycle.saturating_add(1))
}

pub(in crate::stack) fn ordered_validator_addresses(
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    validator_set: &validators::ValidatorSet,
) -> Result<Vec<alloy_primitives::Address>> {
    ensure!(
        validator_set.public_keys.len() == validator_set.addresses.len(),
        "validator set has mismatched public key/address lengths: {} public keys, {} addresses",
        validator_set.public_keys.len(),
        validator_set.addresses.len(),
    );

    let mut ordered = Vec::with_capacity(participants.len());
    for pk in participants.iter() {
        let Some(idx) = validator_set.public_keys.iter().position(|p| p == pk) else {
            return Err(eyre::eyre!(
                "participant public key is missing from validator set"
            ));
        };
        ordered.push(validator_set.addresses[idx]);
    }
    Ok(ordered)
}

pub(in crate::stack) fn active_set_hash_from_addresses(addresses: &[EthAddress]) -> B256 {
    let mut bytes = Vec::with_capacity(8 + addresses.len() * 20);
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    alloy_primitives::keccak256(bytes)
}

/// A share-less node (verifier-join TEE full-node) that is about to participate in
/// a DKG reshare as a player must present the COMMITTEE's current output as the
/// ceremony `prev_output` - the DKG ceremony id binds the full previous output, so
/// a divergent prev_output yields a divergent `info_hash`, every dealer bundle is
/// dropped ("received DKG message for a different ceremony"), the ceremony times
/// out, and the joiner goes ACTIVE-but-voteless. Its in-memory `last_dkg_output`
/// may be the stale CLI `--consensus.dkg-output` bootstrap value (on a TEE chain
/// the runtime-derived genesis consensus output differs from the bootstrap file)
/// when it joins WITHOUT first following a reshare or restarting. Refresh it from
/// the chain's latest finalized DKG boundary (scanning back from `scan_height`)
/// before the ceremony. Signers already hold the correct output from their prior
/// ceremony, so this only runs for the share-less case. Best-effort: on any
/// recovery/decode failure it keeps the local value and warns.
pub(in crate::stack) fn refresh_verifier_join_prev_output(
    provider: &(impl HeaderProvider<Header = OutbeHeader> + BlockHashReader),
    scan_height: u64,
    dkg_rotation_params: DkgRotationParams,
    last_dkg_output: &mut Option<Output<MinSig, bls12381::PublicKey>>,
) {
    match recover_latest_boundary_artifact(provider, scan_height, dkg_rotation_params) {
        Ok(Some((commit_height, boundary))) => match decode_boundary_output(&boundary) {
            Ok(output) => {
                if last_dkg_output.as_ref() != Some(&output) {
                    info!(
                        commit_height,
                        "verifier-join: adopted chain canonical DKG output as reshare prev_output"
                    );
                }
                *last_dkg_output = Some(output);
            }
            Err(error) => warn!(
                %error,
                "verifier-join: failed to decode boundary output for reshare prev_output; using local"
            ),
        },
        Ok(None) => warn!(
            scan_height,
            "verifier-join: no boundary artifact for reshare prev_output; using local"
        ),
        Err(error) => warn!(
            %error,
            "verifier-join: failed to recover boundary for reshare prev_output; using local"
        ),
    }
}

pub(in crate::stack) fn startup_live_join_scan_height(
    execution_height: u64,
    consensus_finalized_height: u64,
    trust_el_head: bool,
) -> Result<u64> {
    if consensus_finalized_height == 0 {
        if !trust_el_head {
            ensure!(
                execution_height == 0,
                "startup live join found execution history at height {execution_height} but no durable consensus-finalized height; refusing to recover DKG artifacts from unfinalized execution head. Wait for consensus finalization evidence or use --testnet.trust-el-head for testnet consensus-archive recovery; it cannot bypass the permanent offer-key gate."
            );
        } else if execution_height > 0 {
            warn!(
                execution_height,
                "trusting EL head with no consensus-finalized height (--testnet.trust-el-head)"
            );
        }
        return Ok(0);
    }
    Ok(execution_height.min(consensus_finalized_height))
}

pub(in crate::stack) struct DkgCeremonyReplaySpec {
    pub(in crate::stack) epoch: Epoch,
    pub(in crate::stack) round: u64,
    pub(in crate::stack) previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
    pub(in crate::stack) participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    pub(in crate::stack) finalized_dealer_log_tx: Option<tokio::sync::mpsc::UnboundedSender<Bytes>>,
}

/// Recreate the manager's ceremony and replay the finalized DealerLog prefix
/// before a frozen-target DKG retry starts.
///
#[allow(clippy::too_many_arguments)]
pub(in crate::stack) fn restart_dkg_manager_from_finalized_history(
    provider: &(impl HeaderProvider<Header = OutbeHeader> + BlockHashReader),
    dkg_manager: &DkgManagerMailbox,
    spec: DkgCeremonyReplaySpec,
    freeze_height: u64,
    _scheduling_height: u64,
    verified_consensus_tip: impl FnOnce() -> crate::marshal_update_reporter::ConsensusTip,
) -> Result<()> {
    let replay_guard = dkg_manager.lock_finalized_replay();
    let verified_consensus_tip = verified_consensus_tip();
    ensure!(
        provider_matches_consensus_tip(
            provider,
            verified_consensus_tip,
            verified_consensus_tip.height.get(),
        )?,
        "execution provider does not contain the verified consensus tip at height {}",
        verified_consensus_tip.height.get(),
    );
    let finalized_logs = collect_finalized_dealer_logs(
        provider,
        freeze_height,
        verified_consensus_tip.height.get(),
    )?;
    replay_guard.restart_ceremony_with_finalized_logs(
        spec.epoch,
        spec.round,
        spec.previous_output,
        spec.participants,
        spec.finalized_dealer_log_tx,
        finalized_logs,
    )
}

fn collect_finalized_dealer_logs(
    provider: &impl HeaderProvider<Header = OutbeHeader>,
    start_height: u64,
    end_height: u64,
) -> Result<Vec<(u64, B256, Bytes)>> {
    let mut finalized_logs = Vec::new();
    for height in start_height..=end_height {
        let header = provider
            .sealed_header(height)
            .map_err(|error| eyre::eyre!("failed to read finalized header {height}: {error}"))?
            .ok_or_else(|| eyre::eyre!("missing finalized header at height {height}"))?;
        let artifacts = decode_outbe_block_artifacts(header.header().inner.extra_data.as_ref())
            .map_err(|error| {
                eyre::eyre!("failed to decode header artifacts at {height}: {error}")
            })?;
        if let Some(ConsensusHeaderArtifact::DealerLog(bytes)) = artifacts.consensus_header_artifact
        {
            finalized_logs.push((height, header.hash(), bytes));
        }
    }
    Ok(finalized_logs)
}

/// Refresh the target validator set from frozen EVM state.
///
/// Called at freeze_height so dynamically added/removed validators are applied
/// from the same historical state on every node.
pub(in crate::stack) fn refresh_validator_set_at_height(
    node: &OutbeFullNode,
    freeze_height: u64,
) -> Result<FrozenValidatorSetRefresh> {
    let Some(block_hash) = node.provider.block_hash(freeze_height).map_err(|e| {
        eyre::eyre!("failed to get block hash at freeze height {freeze_height}: {e}")
    })?
    else {
        return Ok(FrozenValidatorSetRefresh::PendingBlockHash);
    };
    let Some(header) = node
        .provider
        .sealed_header(freeze_height)
        .map_err(|error| {
            eyre::eyre!("failed to get canonical header at freeze height {freeze_height}: {error}")
        })?
    else {
        return Ok(FrozenValidatorSetRefresh::PendingBlockHash);
    };
    ensure!(
        header.hash() == block_hash,
        "canonical freeze header hash mismatch at height {freeze_height}: block-hash index {block_hash}, header {}",
        header.hash()
    );
    ensure!(
        header.header().inner.number == freeze_height,
        "canonical freeze header number mismatch: expected {freeze_height}, got {}",
        header.header().inner.number
    );

    let state = node
        .provider
        .state_by_block_hash(block_hash)
        .map_err(|e| eyre::eyre!("failed to get state at freeze height {freeze_height}: {e}"))?;
    // CycleTick has already moved every overdue ACTIVE validator into the jailed
    // lifecycle before this exact freeze state. The ordinary reshare target is
    // therefore authoritative; legacy boundary expiry fields remain empty.
    let filtered = validators::read_reshare_target_with_empty_tee_exclusions_from_state(&state)
        .wrap_err("failed to read frozen reshare target after TEE deadline enforcement")?;
    let new_set = filtered.validator_set;

    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> = new_set
        .public_keys
        .clone()
        .into_iter()
        .try_collect()
        .map_err(|e| eyre::eyre!("invalid participant set after refresh: {e}"))?;

    Ok(FrozenValidatorSetRefresh::Ready {
        validator_set: new_set,
        participants,
        tee_expired_target_exclusions: filtered.tee_expired_target_exclusions,
    })
}
