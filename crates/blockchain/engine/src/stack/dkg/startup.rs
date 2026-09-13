use super::super::*;

pub(in crate::stack) fn validate_recovered_vrf_material(
    polynomial: &Sharing<MinSig>,
    boundary: Option<&DkgBoundaryArtifact>,
) -> Result<()> {
    let Some(boundary) = boundary else {
        return Ok(());
    };
    let group_pk_bytes = commonware_codec::Encode::encode(polynomial.public());
    let local_vrf_group_public_key = alloy_primitives::keccak256(&group_pk_bytes);
    ensure!(
        local_vrf_group_public_key == boundary.vrf_group_public_key,
        "saved DKG material does not match finalized VRF group public key"
    );
    Ok(())
}

pub(in crate::stack) fn vrf_group_public_key_hash(polynomial: &Sharing<MinSig>) -> B256 {
    let group_pk_bytes = commonware_codec::Encode::encode(polynomial.public());
    alloy_primitives::keccak256(&group_pk_bytes)
}

/// Resolve the consensus participant set for restart/live-join recovery.
///
/// When the node recovers a finalized DKG boundary, the threshold material it
/// restores (`signing_share`, `polynomial`, `last_dkg_output`) belongs to the
/// committee the recovered ceremony ran for - recorded as the DKG output's
/// `players()`. The latest on-chain consensus set may have DRIFTED from that
/// committee (a join/exit/jail/slash after the recovered boundary activated but
/// before the next reshare), so the scheme must NOT be reconstructed against the
/// latest set: committee-dependent data (votes, VRF threshold partials) must be
/// decoded against the committee it was encoded for.
///
/// The recovered output's `players()` is already a sorted, deduplicated
/// `commonware_utils::ordered::Set`, so participant indices derive from it
/// canonically regardless of how the set was assembled - only the *membership*
/// matters, and the members ARE the share holders. In the common no-churn restart
/// this set is identical to the latest committed set, so recovery is unchanged;
/// it diverges only across a churn window, which is exactly the bug this closes.
///
/// **Drift guard.** The recovered boundary records the committee the ceremony ran
/// for in `reshare.new_active_set` (built 1:1 from the same `players()` list at
/// proposal time). A size mismatch between the recovered output's players and
/// that record means the restored consensus material does not correspond to the
/// recovered chain boundary (e.g. a stale or partial consensus-archive restore),
/// so recovery fails fast with an explicit drift error rather than reconstruct
/// the scheme against the wrong committee.
pub(in crate::stack) fn select_recovery_participants(
    recovered_output_players: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    boundary: &DkgBoundaryArtifact,
) -> Result<commonware_utils::ordered::Set<bls12381::PublicKey>> {
    let recorded = boundary.reshare.new_active_set.len();
    let recovered = recovered_output_players.len();
    ensure!(
        recovered > 0 && recovered == recorded,
        "validator set has drifted from saved DKG: recovered DKG output has {recovered} \
         player(s) but the recovered boundary (epoch {}, activation height {}) recorded an \
         active set of {recorded} validator(s) - the restored consensus material does not \
         match the chain's recovered DKG boundary",
        boundary.epoch,
        boundary.planned_activation_height,
    );
    Ok(recovered_output_players.clone())
}

pub(in crate::stack) fn recover_latest_boundary_artifact(
    provider: &(impl HeaderProvider<Header = OutbeHeader> + BlockHashReader),
    last_execution_height: u64,
    dkg_rotation_params: DkgRotationParams,
) -> Result<Option<(u64, DkgBoundaryArtifact)>> {
    let max_scan = dkg_rotation_params
        .epoch_length_blocks
        .saturating_add(dkg_rotation_params.prepare_window_blocks)
        .saturating_add(dkg_rotation_params.activation_grace_blocks)
        .saturating_mul(2)
        .max(10_000);
    let min_height = last_execution_height.saturating_sub(max_scan);
    let mut height = last_execution_height;
    while height > min_height {
        let Some(header) = provider
            .sealed_header(height)
            .map_err(|error| eyre::eyre!("failed to read header {height}: {error}"))?
        else {
            height = height.saturating_sub(1);
            continue;
        };
        let artifacts = decode_outbe_block_artifacts(header.header().inner.extra_data.as_ref())
            .map_err(|error| {
                eyre::eyre!("failed to decode header artifacts at {height}: {error}")
            })?;
        if let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) =
            artifacts.consensus_header_artifact
        {
            return Ok(Some((height, boundary)));
        }
        height = height.saturating_sub(1);
    }
    Ok(None)
}

#[derive(Clone, Debug)]
pub(in crate::stack) struct StartupDkgSnapshot {
    pub(in crate::stack) last_execution_height: u64,
    pub(in crate::stack) last_execution_hash: B256,
    pub(in crate::stack) recovered_boundary: Option<(u64, DkgBoundaryArtifact)>,
    pub(in crate::stack) pending_boundary: Option<PendingDkgBoundarySnapshot>,
    pub(in crate::stack) context: StartupDkgContext,
}

fn startup_threshold_material_candidate_available(args: &ConsensusArgs) -> bool {
    if args.signing_share.is_some() && args.public_polynomial.is_some() {
        return true;
    }

    let Some(keys_dir) = &args.keys_dir else {
        return false;
    };
    keys_dir.join(DKG_SHARE_FILE).exists()
        && keys_dir.join(DKG_POLYNOMIAL_FILE).exists()
        && keys_dir.join(DKG_OUTPUT_FILE).exists()
}

fn read_startup_dkg_snapshot(
    node: &OutbeFullNode,
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    local_consensus_key: &bls12381::PublicKey,
    genesis_hash: B256,
    dkg_rotation_params: DkgRotationParams,
    last_consensus_finalized_height: u64,
) -> Result<StartupDkgSnapshot> {
    let last_execution_height = node
        .provider
        .last_block_number()
        .map_err(|e| eyre::eyre!("failed to get last block number: {e}"))?;
    let last_execution_hash = if last_execution_height > 0 {
        node.provider
            .block_hash(last_execution_height)
            .map_err(|e| {
                eyre::eyre!("failed to get block hash for height {last_execution_height}: {e}")
            })?
            .ok_or_else(|| {
                eyre::eyre!(
                    "missing block hash for execution height {last_execution_height}; refusing genesis fallback"
                )
            })?
    } else {
        genesis_hash
    };
    let boundary_recovery_height = startup_live_join_scan_height(
        last_execution_height,
        last_consensus_finalized_height,
        args.trust_el_head,
    )?;
    // NORMALIZE the tuple height to the ACTIVATION ANCHOR. The header scan
    // returns the height of the block CARRYING the boundary artifact, but that
    // artifact always rides the FIRST block of the new epoch - one block ABOVE
    // the activation height the committee anchored its rotation schedule on
    // (genesis: activation 0, committed in block 1; a reshare activated at H is
    // committed in block H+1). A restarted node that anchors on the commit
    // height runs its whole freeze/activation schedule one block LATE: it waits
    // for activation H+1 while the live committee restarts its engine at H. With
    // one such node the committee still has quorum and the laggard self-heals
    // one block later; with two of five (e.g. a restarted validator plus a
    // freshly promoted one) the new epoch is 3-of-5 < quorum and the chain
    // deadlocks at the boundary. Anchor = commit_height - 1, uniformly.
    let recovered_boundary = recover_latest_boundary_artifact(
        &node.provider,
        boundary_recovery_height,
        dkg_rotation_params,
    )
    .wrap_err("failed to recover latest DKG boundary artifact")?
    .map(|(commit_height, artifact)| (commit_height.saturating_sub(1), artifact));
    let recovered_boundary_finalized = recovered_boundary.is_some();
    let mut pending_boundary = None;

    if let Some(keys_dir) = args.keys_dir.as_ref() {
        if let Some(snapshot) = recover_pending_dkg_boundary_snapshot(
            keys_dir,
            key_backend,
            local_consensus_key,
            node,
            recovered_boundary.as_ref(),
        )
        .wrap_err("failed to recover pending DKG boundary snapshot")?
        {
            info!(
                keys_dir = %keys_dir.display(),
                completed_at_height = snapshot.completed_at_height,
                dkg_cycle = snapshot.artifact.dkg_cycle,
                epoch = snapshot.artifact.epoch,
                "recovered durable pending DKG boundary snapshot"
            );
            pending_boundary = Some(snapshot);
        }
    }

    let recovered_dkg_output_hash = recovered_boundary
        .as_ref()
        .map(|(_, artifact)| {
            decode_boundary_output(artifact).map(|output| dkg_manager::dkg_output_hash(&output))
        })
        .transpose()?;
    let context = StartupDkgContext {
        last_execution_height,
        last_consensus_finalized_height,
        recovered_boundary_finalized,
        recovered_vrf_group_public_key: recovered_boundary
            .as_ref()
            .map(|(_, artifact)| artifact.vrf_group_public_key),
        recovered_dkg_output_hash,
        genesis_formation_proven: false,
    };
    Ok(StartupDkgSnapshot {
        last_execution_height,
        last_execution_hash,
        recovered_boundary,
        pending_boundary,
        context,
    })
}

async fn collect_reth_genesis_peer_evidence(node: &OutbeFullNode) -> RethGenesisPeerEvidence {
    let peers_result = node.network.get_all_peers().await;
    let (peer_query_failed, peers) = match peers_result {
        Ok(peers) => {
            let statuses = peers
                .into_iter()
                .map(|peer| RethGenesisPeerStatus {
                    genesis: peer.status.genesis,
                    blockhash: peer.status.blockhash,
                    latest_block: peer.status.latest_block,
                })
                .collect();
            (false, statuses)
        }
        Err(error) => {
            warn!(
                ?error,
                "failed to query Reth peers during genesis formation gate"
            );
            (true, Vec::new())
        }
    };

    RethGenesisPeerEvidence {
        connected_peers: node.network.num_connected_peers(),
        is_syncing: node.network.is_syncing(),
        is_initially_syncing: node.network.is_initially_syncing(),
        peer_query_failed,
        peers,
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::stack) async fn resolve_startup_dkg_snapshot<E>(
    ctx: E,
    node: &OutbeFullNode,
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    local_pk: bls12381::PublicKey,
    validator_set: &validators::ValidatorSet,
    genesis_hash: B256,
    dkg_rotation_params: DkgRotationParams,
    last_consensus_finalized_height: u64,
) -> Result<StartupDkgSnapshot>
where
    E: Clock,
{
    let startup_participants: commonware_utils::ordered::Set<bls12381::PublicKey> = validator_set
        .public_keys
        .clone()
        .into_iter()
        .try_collect()
        .map_err(|e| eyre::eyre!("invalid participant set: {e}"))?;
    let local_key_in_current_consensus_set = startup_participants.position(&local_pk).is_some();
    let expected_remote_peers = validator_set.public_keys.len().saturating_sub(1);
    let required_remote_peers =
        genesis_formation_required_remote_peers(validator_set.public_keys.len());
    let gate_required = !startup_threshold_material_candidate_available(args);
    let started_at = ctx.current();

    loop {
        let mut snapshot = read_startup_dkg_snapshot(
            node,
            args,
            key_backend,
            &local_pk,
            genesis_hash,
            dkg_rotation_params,
            last_consensus_finalized_height,
        )?;
        let evidence = collect_reth_genesis_peer_evidence(node).await;
        let gate = genesis_formation_gate_decision(
            snapshot.context,
            genesis_hash,
            required_remote_peers,
            &evidence,
        );

        snapshot.context.genesis_formation_proven = gate == GenesisFormationGate::Proven;

        if !gate_required
            || startup_dkg_mode(snapshot.context, local_key_in_current_consensus_set)
                == StartupDkgMode::InitialGenesisDkg
            || gate == GenesisFormationGate::ExistingChainJoin
            || !local_key_in_current_consensus_set
        {
            info!(
                last_execution_height = snapshot.last_execution_height,
                %snapshot.last_execution_hash,
                last_consensus_finalized_height,
                recovered_dkg_boundary = snapshot.context.has_chain_finalized_dkg_boundary(),
                genesis_formation_gate = ?gate,
                local_key_in_current_consensus_set,
                gate_required,
                "resolved startup DKG state"
            );
            return Ok(snapshot);
        }

        let elapsed = elapsed_since(ctx.current(), started_at);
        if elapsed >= config::STARTUP_GENESIS_FORMATION_PROBE_TIMEOUT {
            return Err(eyre::eyre!(
                "could not prove genesis formation before DKG round 0: connected_reth_peers={} required_remote_peers={} configured_remote_peers={} reth_syncing={} reth_initial_syncing={} peer_query_failed={}",
                evidence.connected_peers,
                required_remote_peers,
                expected_remote_peers,
                evidence.is_syncing,
                evidence.is_initially_syncing,
                evidence.peer_query_failed,
            ));
        }

        info!(
            connected_reth_peers = evidence.connected_peers,
            required_remote_peers,
            expected_remote_peers,
            reth_syncing = evidence.is_syncing,
            reth_initial_syncing = evidence.is_initially_syncing,
            peer_query_failed = evidence.peer_query_failed,
            "waiting for Reth peer/sync evidence before allowing DKG round 0"
        );
        ctx.sleep(config::STARTUP_GENESIS_FORMATION_PROBE_INTERVAL)
            .await;
    }
}

pub(in crate::stack) enum ThresholdMaterial {
    Ready {
        signing_share: Share,
        polynomial: Sharing<MinSig>,
        last_dkg_output: Option<Output<MinSig, bls12381::PublicKey>>,
        bootstrap_from_live_dkg: bool,
    },
    /// Verifier-join: the node has the public group polynomial + DKG output but NO
    /// threshold share. It runs the consensus engine as a VERIFIER - it follows and
    /// verifies finalized blocks (driving its execution layer to sync) but cannot
    /// propose/sign - and acquires a share at the next DKG reshare, after which the
    /// epoch loop rebuilds its scheme as a signer (Stage 4).
    VerifierOnly {
        polynomial: Sharing<MinSig>,
        last_dkg_output: Option<Output<MinSig, bls12381::PublicKey>>,
    },
}

pub(in crate::stack) fn missing_current_threshold_material_error(
    reason: impl std::fmt::Display,
) -> eyre::Report {
    eyre::eyre!(
        "startup cannot recover threshold material before sync starts: {reason}. Restart with the current --consensus.public-polynomial and --consensus.dkg-output without --consensus.signing-share so the node can sync as VerifierOnly and acquire a share through the running reshare path"
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::stack) enum StartupDkgMode {
    InitialGenesisDkg,
    LiveJoinRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::stack) enum GenesisFormationGate {
    Proven,
    WaitForExecutionSync,
    ExistingChainJoin,
}

#[derive(Clone, Copy, Debug)]
pub(in crate::stack) struct StartupDkgContext {
    pub(in crate::stack) last_execution_height: u64,
    pub(in crate::stack) last_consensus_finalized_height: u64,
    pub(in crate::stack) recovered_boundary_finalized: bool,
    pub(in crate::stack) recovered_vrf_group_public_key: Option<B256>,
    pub(in crate::stack) recovered_dkg_output_hash: Option<B256>,
    pub(in crate::stack) genesis_formation_proven: bool,
}

impl StartupDkgContext {
    fn has_chain_finalized_dkg_boundary(self) -> bool {
        self.recovered_vrf_group_public_key.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::stack) struct RethGenesisPeerStatus {
    pub(in crate::stack) genesis: B256,
    pub(in crate::stack) blockhash: B256,
    pub(in crate::stack) latest_block: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::stack) struct RethGenesisPeerEvidence {
    pub(in crate::stack) connected_peers: usize,
    pub(in crate::stack) is_syncing: bool,
    pub(in crate::stack) is_initially_syncing: bool,
    pub(in crate::stack) peer_query_failed: bool,
    pub(in crate::stack) peers: Vec<RethGenesisPeerStatus>,
}

pub(in crate::stack) fn startup_dkg_mode(
    context: StartupDkgContext,
    local_key_in_current_consensus_set: bool,
) -> StartupDkgMode {
    if !local_key_in_current_consensus_set {
        return StartupDkgMode::LiveJoinRequired;
    }

    if context.last_execution_height == 0
        && context.last_consensus_finalized_height == 0
        && !context.has_chain_finalized_dkg_boundary()
        && context.genesis_formation_proven
    {
        StartupDkgMode::InitialGenesisDkg
    } else {
        StartupDkgMode::LiveJoinRequired
    }
}

/// Enforce the permanent offer-key invariant before any threshold ceremony,
/// live join, reshare, or consensus actor can start.
///
/// Only proven block-1 founders may be keyless while canonical state is still
/// zero. An empty-DB verifier join may carry an already-installed key until
/// certified sync makes the canonical value locally available; every other
/// existing identity must match canonical state exactly at this gate.
pub(in crate::stack) fn validate_offer_key_before_threshold_work(
    context: StartupDkgContext,
    local_key_in_current_consensus_set: bool,
    verifier_join: bool,
    on_chain_offer: B256,
    resident_offer: Option<B256>,
) -> Result<()> {
    let founding = !verifier_join
        && startup_dkg_mode(context, local_key_in_current_consensus_set)
            == StartupDkgMode::InitialGenesisDkg;
    if founding {
        ensure!(
            on_chain_offer.is_zero(),
            "fresh block-1 founding startup found a pre-existing canonical offer key"
        );
        return Ok(());
    }

    let resident_offer = resident_offer.ok_or_else(|| {
        eyre::eyre!(
            "existing identity has no permanent resident offer key before threshold work; no recovery or fallback exists"
        )
    })?;
    ensure!(
        !resident_offer.is_zero(),
        "existing identity has a zero permanent resident offer key before threshold work; no recovery or fallback exists"
    );

    let has_existing_local_state = context.last_execution_height > 0
        || context.last_consensus_finalized_height > 0
        || context.has_chain_finalized_dkg_boundary();
    if on_chain_offer.is_zero() {
        ensure!(
            !has_existing_local_state,
            "existing canonical state has no mandatory OST3 offer key; no recovery or fallback exists"
        );
        ensure!(
            verifier_join,
            "only an empty-DB verifier join with an installed permanent key may defer exact offer-key comparison until certified sync"
        );
        return Ok(());
    }

    ensure!(
        resident_offer == on_chain_offer,
        "local enclave does not hold the canonical permanent offer key before threshold work; no recovery or fallback exists"
    );
    Ok(())
}

pub(in crate::stack) fn should_coordinate_genesis_tee_bootstrap(
    context: StartupDkgContext,
    local_key_in_current_consensus_set: bool,
    shareless_verifier: bool,
) -> bool {
    !shareless_verifier
        && startup_dkg_mode(context, local_key_in_current_consensus_set)
            == StartupDkgMode::InitialGenesisDkg
}

pub(in crate::stack) fn genesis_formation_gate_decision(
    context: StartupDkgContext,
    genesis_hash: B256,
    required_remote_peers: usize,
    evidence: &RethGenesisPeerEvidence,
) -> GenesisFormationGate {
    if context.last_execution_height > 0
        || context.last_consensus_finalized_height > 0
        || context.has_chain_finalized_dkg_boundary()
    {
        return GenesisFormationGate::ExistingChainJoin;
    }

    if evidence.peer_query_failed {
        return GenesisFormationGate::WaitForExecutionSync;
    }

    if evidence.connected_peers < required_remote_peers {
        return GenesisFormationGate::WaitForExecutionSync;
    }

    if evidence.peers.len() < required_remote_peers {
        return GenesisFormationGate::WaitForExecutionSync;
    }

    for peer in &evidence.peers {
        if peer.genesis != genesis_hash {
            return GenesisFormationGate::ExistingChainJoin;
        }
        if peer.blockhash != genesis_hash || peer.latest_block.unwrap_or(0) > 0 {
            return GenesisFormationGate::ExistingChainJoin;
        }
    }

    GenesisFormationGate::Proven
}

/// Direct Reth connections needed to prove a fresh genesis formation before
/// entering the all-member DKG. The execution P2P graph need not be a complete
/// mesh: one local validator plus a `N-f` BFT quorum of matching genesis peers
/// is sufficient evidence. DKG itself still requires every configured genesis
/// dealer log, so lowering this transport gate cannot let a partial committee
/// complete network formation.
pub(in crate::stack) fn genesis_formation_required_remote_peers(validator_count: usize) -> usize {
    let max_byzantine = validator_count.saturating_sub(1) / 3;
    validator_count
        .saturating_sub(max_byzantine)
        .saturating_sub(1)
}

pub(in crate::stack) fn vrf_material_matches_recovered_boundary(
    polynomial: &Sharing<MinSig>,
    context: StartupDkgContext,
) -> bool {
    let local = vrf_group_public_key_hash(polynomial);
    match context.recovered_vrf_group_public_key {
        Some(expected) => local == expected,
        None => true,
    }
}

fn dkg_output_matches_recovered_boundary(
    output: &Output<MinSig, bls12381::PublicKey>,
    context: StartupDkgContext,
) -> bool {
    match context.recovered_dkg_output_hash {
        Some(expected) => dkg_manager::dkg_output_hash(output) == expected,
        None => true,
    }
}

/// Recover the participant-index-aligned EVM address vector from a finalized DKG
/// boundary instead of provider-latest validator state.
///
/// This is the recovery/live-join counterpart of [`ordered_validator_addresses`].
/// `build_boundary_artifact` constructs `reshare.new_active_set` by iterating
/// `output.players()` in Commonware participant order, so the boundary itself is
/// the canonical source of the old epoch's address mapping. That matters on
/// restart when Reth head may have executed an unfinalized membership-changing
/// `BoundaryOutcome` while marshal-finalized consensus still needs the old
/// committee.
pub(in crate::stack) fn ordered_addresses_from_recovered_boundary(
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    boundary: &DkgBoundaryArtifact,
) -> Result<Vec<EthAddress>> {
    let boundary_output = decode_boundary_output(boundary)
        .wrap_err("failed to decode recovered DKG boundary output for address mapping")?;
    ensure!(
        boundary_output.players() == participants,
        "recovered DKG boundary output players do not match active participant set"
    );

    let ordered_addresses = boundary.reshare.new_active_set.clone();
    ensure!(
        ordered_addresses.len() == participants.len(),
        "recovered DKG boundary active-set length {} does not match participant count {}",
        ordered_addresses.len(),
        participants.len(),
    );
    ensure!(
        active_set_hash_from_addresses(&ordered_addresses) == boundary.reshare.active_set_hash,
        "recovered DKG boundary active-set hash does not match active-set addresses"
    );
    ensure!(
        alloy_primitives::keccak256(boundary.vrf_group_public_key_bytes.as_ref())
            == boundary.vrf_group_public_key,
        "recovered DKG boundary VRF group public key bytes do not match hash"
    );

    let mut committee = Vec::with_capacity(participants.len());
    for (address, bls_pk) in ordered_addresses.iter().zip(participants.iter()) {
        let encoded = commonware_codec::Encode::encode(bls_pk).to_vec();
        let consensus_pubkey: [u8; 48] = encoded.as_slice().try_into().map_err(|_| {
            eyre::eyre!(
                "encoded MinPk consensus pubkey has unexpected length: expected 48, got {}",
                encoded.len()
            )
        })?;
        committee.push(outbe_consensus::proof::CommitteeEntry {
            address: *address,
            consensus_pubkey,
        });
    }
    let snapshot = outbe_consensus::proof::CommitteeSnapshot {
        committee,
        vrf_material_version: boundary.vrf_material_version,
        vrf_group_public_key_bytes: boundary.vrf_group_public_key_bytes.to_vec(),
        vrf_public_polynomial_hash: dkg_manager::public_polynomial_hash(boundary_output.public()),
    };
    ensure!(
        outbe_consensus::proof::committee_set_hash_v2(boundary.epoch, &snapshot)
            == boundary.committee_set_hash,
        "recovered DKG boundary committee_set_hash does not match boundary committee/address mapping"
    );

    Ok(ordered_addresses)
}

/// Load the validator EVM signer and the committee address set for the one-time
/// TEE bootstrap coordination.
pub(in crate::stack) fn tee_bootstrap_setup(
    args: &ConsensusArgs,
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    validator_set: &validators::ValidatorSet,
) -> Result<(
    outbe_primitives::signer::OutbeEvmSigner,
    std::collections::BTreeSet<alloy_primitives::Address>,
)> {
    let evm_key_path = args
        .effective_validator_evm_key()?
        .ok_or_else(|| eyre::eyre!("TEE bootstrap requires a validator EVM key"))?;
    let evm_signer = outbe_primitives::signer::OutbeEvmSigner::from_file(&evm_key_path)
        .map_err(|e| eyre::eyre!("failed to load validator EVM signer for TEE bootstrap: {e}"))?;
    let committee: std::collections::BTreeSet<alloy_primitives::Address> =
        ordered_validator_addresses(participants, validator_set)?
            .into_iter()
            .collect();
    Ok((evm_signer, committee))
}

/// Build the one canonical epoch-0 DKG boundary before block 1 exists.
///
/// OST3 must bind the exact committee snapshot that the preceding block-1
/// `BoundaryOutcome` transaction will commit. Reading that snapshot from the
/// provider before block 1 creates an impossible dependency cycle: the state is
/// intentionally absent until `BoundaryOutcome` executes. Deriving both system
/// transactions from this single artifact preserves proposer/executor parity and
/// introduces no second committee authority.
pub(in crate::stack) fn build_genesis_dkg_boundary_artifact(
    validator_set: &validators::ValidatorSet,
    output: &Output<MinSig, bls12381::PublicKey>,
    is_full_dkg: bool,
) -> Result<DkgBoundaryArtifact> {
    validate_dkg_output_players_exact(output, validator_set)
        .wrap_err("fresh bootstrap DKG output does not cover the genesis validator set")?;
    dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(0),
        validator_set,
        output,
        is_full_dkg,
        dkg_cycle: 0,
        freeze_height: 0,
        planned_activation_height: 0,
        vrf_material_version: 0,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
}

/// Obtain threshold material (signing share + public polynomial).
///
/// Three paths (tried in order):
/// 1. **Saved DKG state** in `keys_dir` - restart precedence, wins over CLI
/// 2. **CLI args provided** - fallback for fresh bootstrap / manual provisioning
/// 3. **No material and no chain DKG history** - run the one-time interactive
///    genesis DKG ceremony over P2P (BLOCKING, no blocks)
/// 4. **No material or stale material on an existing chain** - fail startup with
///    the explicit `VerifierOnly` recovery contract; startup cannot wait for sync
///    before Marshal and Executor are running
///
/// Returns `(share, polynomial, previous_output, bootstrap_from_live_dkg)`.
///
/// `previous_output` is restored from persisted state when available so the
/// next live reshare can continue from the correct prior DKG output.
/// `bootstrap_from_live_dkg` is `true` only when this startup actually ran the
/// interactive initial DKG ceremony (path 3).
#[allow(clippy::too_many_arguments)]
pub(in crate::stack) async fn obtain_threshold_material<C>(
    clock: C,
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    signing_key: bls12381::PrivateKey,
    validator_set: &validators::ValidatorSet,
    startup_dkg_context: StartupDkgContext,
    dkg_sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    dkg_receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<ThresholdMaterial>
where
    C: Clock,
{
    // Path 1: Try loading saved DKG state from keys_dir (restart precedence).
    // On ordinary restart, saved local DKG state wins over CLI bootstrap material.
    if let Some(ref keys_dir) = args.keys_dir {
        let mut saved_state_error: Option<eyre::Report> = None;
        let saved_state = match load_saved_dkg_state(keys_dir, key_backend) {
            Ok(state) => state,
            Err(error) => {
                warn!(
                    %error,
                    keys_dir = %keys_dir.display(),
                    "saved DKG state is incomplete or corrupt; checking pending DKG state before failing"
                );
                saved_state_error = Some(error);
                None
            }
        };
        if let Some((signing_share, polynomial, output)) = saved_state {
            if vrf_material_matches_recovered_boundary(&polynomial, startup_dkg_context)
                && dkg_output_matches_recovered_boundary(&output, startup_dkg_context)
            {
                info!(
                    keys_dir = %keys_dir.display(),
                    vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                    "threshold material ready from saved DKG state"
                );
                return Ok(ThresholdMaterial::Ready {
                    signing_share,
                    polynomial,
                    last_dkg_output: Some(output),
                    bootstrap_from_live_dkg: false,
                });
            }
            warn!(
                keys_dir = %keys_dir.display(),
                local_vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                local_dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                recovered_vrf_group_public_key = ?startup_dkg_context.recovered_vrf_group_public_key,
                recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
                "saved DKG material is stale for the latest finalized boundary; checking pending DKG state"
            );
        }

        let pending_state = match load_pending_dkg_state(keys_dir, key_backend) {
            Ok(state) => state,
            Err(error) => {
                warn!(
                    %error,
                    keys_dir = %keys_dir.display(),
                    "pending DKG state is incomplete or corrupt; ignoring pending material"
                );
                None
            }
        };
        if let Some((signing_share, polynomial, output)) = pending_state {
            if startup_dkg_context.recovered_dkg_output_hash.is_some()
                && vrf_material_matches_recovered_boundary(&polynomial, startup_dkg_context)
                && dkg_output_matches_recovered_boundary(&output, startup_dkg_context)
            {
                if startup_dkg_context.recovered_boundary_finalized {
                    save_dkg_state(keys_dir, &signing_share, &polynomial, &output, key_backend)
                        .wrap_err(
                            "failed to promote pending DKG state after boundary finalization",
                        )?;
                    remove_pending_dkg_state(keys_dir);
                    clear_pending_dkg_boundary(keys_dir);
                    dkg_actor::DkgRetryStore::in_keys_dir(keys_dir, key_backend.clone())
                        .clear()
                        .wrap_err("failed to retire recovered DKG retry state")?;
                    info!(
                        keys_dir = %keys_dir.display(),
                        vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                        dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                        "threshold material ready from promoted pending DKG state"
                    );
                } else {
                    info!(
                        keys_dir = %keys_dir.display(),
                        vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                        dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                        "threshold material ready from durable pending DKG state and boundary snapshot"
                    );
                }
                return Ok(ThresholdMaterial::Ready {
                    signing_share,
                    polynomial,
                    last_dkg_output: Some(output),
                    bootstrap_from_live_dkg: false,
                });
            }
            warn!(
                keys_dir = %keys_dir.display(),
                local_vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                local_dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                recovered_vrf_group_public_key = ?startup_dkg_context.recovered_vrf_group_public_key,
                recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
                "pending DKG material is not finalized for the latest boundary"
            );
        }

        if let Some(error) = saved_state_error {
            return Err(error).wrap_err(
                "saved DKG state failed to load and pending state could not be promoted",
            );
        }

        if startup_dkg_context.has_chain_finalized_dkg_boundary()
            && !startup_dkg_context.recovered_boundary_finalized
        {
            return Err(eyre::eyre!(
                "pending DKG boundary snapshot was recovered but matching DKG material is unavailable"
            ));
        }

        if startup_dkg_context.has_chain_finalized_dkg_boundary()
            && !(args.signing_share.is_none()
                && args.public_polynomial.is_some()
                && args.dkg_output.is_some())
        {
            return Err(missing_current_threshold_material_error(
                "saved and pending DKG material do not match the latest finalized boundary",
            ));
        }
    }

    // Path 2: Load from CLI args (fresh bootstrap / manual provisioning).
    if let (Some(share_path), Some(poly_path)) = (&args.signing_share, &args.public_polynomial) {
        let signing_share = bls::load_signing_share(share_path, key_backend)
            .wrap_err("failed to load BLS signing share")?;
        let polynomial = bls::load_public_polynomial(poly_path, key_backend)
            .wrap_err("failed to load BLS public polynomial")?;
        let cli_dkg_output = if let Some(output_path) = &args.dkg_output {
            let output = bls::load_dkg_output(output_path, key_backend)
                .wrap_err("failed to load BLS DKG output")?;
            bls::validate_dkg_triplet(&signing_share, &polynomial, &output)
                .wrap_err("CLI DKG material triplet is inconsistent")?;
            Some(output)
        } else {
            None
        };

        if startup_dkg_context.recovered_dkg_output_hash.is_some() && cli_dkg_output.is_none() {
            warn!(
                share_path = %share_path.display(),
                poly_path = %poly_path.display(),
                recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
                "CLI DKG material lacks required output for recovered chain boundary"
            );
            return Err(missing_current_threshold_material_error(
                "CLI DKG material lacks the output required by the latest finalized boundary",
            ));
        }

        if !vrf_material_matches_recovered_boundary(&polynomial, startup_dkg_context)
            || cli_dkg_output.as_ref().is_some_and(|output| {
                !dkg_output_matches_recovered_boundary(output, startup_dkg_context)
            })
        {
            warn!(
                share_path = %share_path.display(),
                poly_path = %poly_path.display(),
                local_vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                local_dkg_output_hash = ?cli_dkg_output.as_ref().map(dkg_manager::dkg_output_hash),
                recovered_vrf_group_public_key = ?startup_dkg_context.recovered_vrf_group_public_key,
                recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
                "CLI DKG material is stale for the latest finalized boundary"
            );
            return Err(missing_current_threshold_material_error(
                "CLI DKG material is stale for the latest finalized boundary",
            ));
        }

        info!(
            vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
            "threshold material ready from CLI args"
        );
        return Ok(ThresholdMaterial::Ready {
            signing_share,
            polynomial,
            last_dkg_output: cli_dkg_output,
            bootstrap_from_live_dkg: false,
        });
    }

    // Path 2b: Verifier-join - public group material (--consensus.public-polynomial
    // + --consensus.dkg-output) WITHOUT a signing share. The node runs the consensus
    // engine in verifier mode (follow/verify finalized blocks -> sync its execution
    // layer) and acquires a share at the next reshare. See ThresholdMaterial::VerifierOnly.
    if args.signing_share.is_none() {
        if let (Some(poly_path), Some(output_path)) = (&args.public_polynomial, &args.dkg_output) {
            let polynomial = bls::load_public_polynomial(poly_path, key_backend)
                .wrap_err("failed to load BLS public polynomial for verifier-join")?;
            let output = bls::load_dkg_output(output_path, key_backend)
                .wrap_err("failed to load BLS DKG output for verifier-join")?;
            info!(
                vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                "verifier-join: no threshold share; running consensus in VERIFIER mode \
                 (follow + verify) until the next reshare grants a share"
            );
            return Ok(ThresholdMaterial::VerifierOnly {
                polynomial,
                last_dkg_output: Some(output),
            });
        }
    }

    // Path 3: Run interactive DKG ceremony.
    let local_pk = signing_key.public_key();
    let startup_participants: commonware_utils::ordered::Set<bls12381::PublicKey> = validator_set
        .public_keys
        .clone()
        .into_iter()
        .try_collect()
        .map_err(|e| eyre::eyre!("invalid participant set: {e}"))?;
    let local_key_in_current_consensus_set = startup_participants.position(&local_pk).is_some();
    match startup_dkg_mode(startup_dkg_context, local_key_in_current_consensus_set) {
        StartupDkgMode::LiveJoinRequired => {
            warn!(
                last_execution_height = startup_dkg_context.last_execution_height,
                has_finalized_dkg_boundary = startup_dkg_context.has_chain_finalized_dkg_boundary(),
                local_key_in_current_consensus_set,
                "no current threshold material is available for existing-chain startup"
            );
            return Err(missing_current_threshold_material_error(
                "no current threshold material is available for existing-chain startup",
            ));
        }
        StartupDkgMode::InitialGenesisDkg => {}
    }

    info!("no threshold material available - running DKG ceremony (NO BLOCKS until complete)");

    let dkg_result = dkg_actor::run_initial_dkg_durable(
        &clock,
        signing_key,
        startup_participants,
        None, // initial: no previous output
        None, // initial: no previous share
        0,    // initial: round 0
        None,
        None,
        dkg_retry_store(args, key_backend)?,
        dkg_sender,
        dkg_receiver,
    )
    .await
    .wrap_err("DKG ceremony failed")?;

    let polynomial = dkg_result.output.public().clone();
    let signing_share = dkg_result.share;
    info!(
        vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
        "initial DKG ceremony completed; threshold material ready"
    );

    // Save DKG state to keys_dir for future restarts.
    if let Some(ref keys_dir) = args.keys_dir {
        let save_result = save_dkg_state(
            keys_dir,
            &signing_share,
            &polynomial,
            &dkg_result.output,
            key_backend,
        );
        if let Err(e) = save_result {
            warn!(
                ?e,
                "failed to save DKG state to disk (node will need to re-run DKG on restart)"
            );
        } else {
            info!(keys_dir = %keys_dir.display(), "saved DKG state to disk");
        }
    } else {
        warn!("no --consensus.keys-dir set, DKG state will not be persisted");
    }

    info!("DKG ceremony complete - threshold material obtained via P2P");

    Ok(ThresholdMaterial::Ready {
        signing_share,
        polynomial,
        last_dkg_output: Some(dkg_result.output),
        bootstrap_from_live_dkg: true,
    })
}

pub(in crate::stack) fn validator_set_for_dkg_output_players(
    output: &Output<MinSig, bls12381::PublicKey>,
    source: &validators::ValidatorSet,
) -> Result<validators::ValidatorSet> {
    let players = output.players();
    let mut public_keys = Vec::with_capacity(players.len());
    let mut addresses = Vec::with_capacity(players.len());
    let mut p2p_addresses = Vec::with_capacity(players.len());
    for player in players.iter() {
        let Some(idx) = source.public_keys.iter().position(|pk| pk == player) else {
            return Err(eyre::eyre!(
                "DKG output contains a player absent from the frozen validator set"
            ));
        };
        public_keys.push(source.public_keys[idx].clone());
        addresses.push(source.addresses[idx]);
        p2p_addresses.push(source.p2p_addresses[idx].clone());
    }
    Ok(validators::ValidatorSet {
        public_keys,
        addresses,
        p2p_addresses,
    })
}

pub(in crate::stack) fn participants_from_validator_set(
    validator_set: &validators::ValidatorSet,
) -> Result<commonware_utils::ordered::Set<bls12381::PublicKey>> {
    validator_set
        .public_keys
        .clone()
        .into_iter()
        .try_collect()
        .map_err(|e| eyre::eyre!("invalid DKG output participant set: {e}"))
}

fn validate_dkg_output_players_exact(
    output: &Output<MinSig, bls12381::PublicKey>,
    validator_set: &validators::ValidatorSet,
) -> Result<()> {
    let players = output.players();
    ensure!(
        players.len() == validator_set.public_keys.len(),
        "DKG output player count {} does not match validator set size {}",
        players.len(),
        validator_set.public_keys.len()
    );
    for public_key in &validator_set.public_keys {
        ensure!(
            players.position(public_key).is_some(),
            "validator set public key is missing from DKG output players"
        );
    }
    Ok(())
}
