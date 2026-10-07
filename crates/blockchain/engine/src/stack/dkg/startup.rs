use super::super::*;

mod genesis;
mod local;
mod manual;

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
/// When the node recovers a finalized DKG boundary, it restores threshold material
/// (`signing_share`, `polynomial`, `last_dkg_output`). That material belongs to the
/// committee the recovered ceremony ran for, recorded as the DKG output's `players()`.
/// The latest on-chain consensus set may have DRIFTED from that committee (a
/// join/exit/jail/slash after the recovered boundary activated but before the next
/// reshare). So the scheme must NOT be reconstructed against the latest set.
/// Committee-dependent data (votes, VRF threshold partials) must be decoded against the
/// committee it was encoded for.
///
/// The recovered output's `players()` is already a sorted, deduplicated
/// `commonware_utils::ordered::Set`. Participant indices therefore derive from it
/// canonically, regardless of how the set was assembled. Only the *membership* matters,
/// and the members ARE the share holders. In the common no-churn restart, this set is
/// identical to the latest committed set, so recovery is unchanged. It diverges only
/// across a churn window, which is exactly the bug this closes.
///
/// **Drift guard.** The recovered boundary records the committee the ceremony ran
/// for in `reshare.new_active_set` (built 1:1 from the same `players()` list at
/// proposal time). A size mismatch between the recovered output's players and that
/// record means that the restored consensus material does not correspond to the
/// recovered chain boundary (e.g. a stale or partial consensus-archive restore).
/// Recovery then fails fast with an explicit drift error, rather than reconstruct the
/// scheme against the wrong committee.
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

/// Pinned identity, membership and chain history for each startup snapshot probe.
#[derive(Clone, Copy)]
pub(in crate::stack) struct StartupDkgRequest<'a> {
    pub(in crate::stack) local_pk: &'a bls12381::PublicKey,
    pub(in crate::stack) validator_set: &'a validators::ValidatorSet,
    pub(in crate::stack) genesis_hash: B256,
    pub(in crate::stack) dkg_rotation_params: DkgRotationParams,
    pub(in crate::stack) last_consensus_finalized_height: u64,
}

/// Threshold selection inputs. Runtime, key backend and transport stay separate.
pub(in crate::stack) struct ThresholdMaterialRequest<'a> {
    pub(in crate::stack) args: &'a ConsensusArgs,
    pub(in crate::stack) signing_key: bls12381::PrivateKey,
    pub(in crate::stack) validator_set: &'a validators::ValidatorSet,
    pub(in crate::stack) context: StartupDkgContext,
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
    request: &StartupDkgRequest<'_>,
) -> Result<StartupDkgSnapshot> {
    let StartupDkgRequest {
        local_pk: local_consensus_key,
        genesis_hash,
        dkg_rotation_params,
        last_consensus_finalized_height,
        ..
    } = *request;
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
    // returns the height of the block CARRYING the boundary artifact. That
    // artifact always rides the FIRST block of the new epoch, one block ABOVE
    // the activation height the committee anchored its rotation schedule on.
    // Genesis: activation 0, committed in block 1. A reshare activated at H is
    // committed in block H+1. A restarted node that anchors on the commit
    // height runs its whole freeze/activation schedule one block LATE. It waits
    // for activation H+1 while the live committee restarts its engine at H. With
    // one such node, the committee still has quorum and the laggard self-heals
    // one block later. With two of five (e.g. a restarted validator plus a
    // freshly promoted one), the new epoch is 3-of-5 < quorum and the chain
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

pub(in crate::stack) async fn resolve_startup_dkg_snapshot<E>(
    ctx: E,
    node: &OutbeFullNode,
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    request: StartupDkgRequest<'_>,
) -> Result<StartupDkgSnapshot>
where
    E: Clock,
{
    let StartupDkgRequest {
        local_pk,
        validator_set,
        genesis_hash,
        last_consensus_finalized_height,
        ..
    } = request;
    let startup_participants: commonware_utils::ordered::Set<bls12381::PublicKey> = validator_set
        .public_keys
        .clone()
        .into_iter()
        .try_collect()
        .map_err(|e| eyre::eyre!("invalid participant set: {e}"))?;
    let local_key_in_current_consensus_set = startup_participants.position(local_pk).is_some();
    let expected_remote_peers = validator_set.public_keys.len().saturating_sub(1);
    let required_remote_peers =
        genesis_formation_required_remote_peers(validator_set.public_keys.len());
    let gate_required = !startup_threshold_material_candidate_available(args);
    let started_at = ctx.current();

    loop {
        let mut snapshot = read_startup_dkg_snapshot(node, args, key_backend, &request)?;
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
    /// threshold share. It runs the consensus engine as a VERIFIER. It follows and
    /// verifies finalized blocks (driving its execution layer to sync) but cannot
    /// propose/sign. It acquires a share at the next DKG reshare. After that, the
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
/// certified sync makes the canonical value locally available. Every other
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

    if evidence.peer_query_failed
        || evidence.connected_peers < required_remote_peers
        || evidence.peers.len() < required_remote_peers
    {
        return GenesisFormationGate::WaitForExecutionSync;
    }

    if evidence.peers.iter().any(|peer| {
        peer.genesis != genesis_hash
            || peer.blockhash != genesis_hash
            || peer.latest_block.unwrap_or(0) > 0
    }) {
        return GenesisFormationGate::ExistingChainJoin;
    }

    GenesisFormationGate::Proven
}

/// Direct Reth connections needed to prove a fresh genesis formation before
/// entering the all-member DKG. The execution P2P graph need not be a complete
/// mesh: one local validator plus `N-f-1` matching genesis peers is sufficient
/// evidence. Together they form an `N-f` BFT quorum. DKG itself still requires
/// every configured genesis dealer log, so lowering this transport gate cannot
/// let a partial committee complete network formation.
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
    let evm_signer = outbe_primitives::signer::load::from_file(&evm_key_path)
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

/// Recover threshold signer/verifier material before any interactive ceremony.
///
/// Saved local state takes precedence over matching pending recovery and manual
/// provisioning. Only proven empty genesis formation admits interactive DKG.
/// Existing-chain recovery and corrupt-material errors remain fail-fast.
pub(in crate::stack) async fn obtain_threshold_material<C>(
    clock: C,
    key_backend: &bls::KeyBackend,
    request: ThresholdMaterialRequest<'_>,
    dkg_sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    dkg_receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<ThresholdMaterial>
where
    C: Clock,
{
    if let Some(material) = local::load_local_material(request.args, key_backend, request.context)?
    {
        return Ok(material);
    }
    if let Some(material) =
        manual::load_manual_material(request.args, key_backend, request.context)?
    {
        return Ok(material);
    }
    genesis::run_genesis_dkg(clock, key_backend, request, dkg_sender, dkg_receiver).await
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
