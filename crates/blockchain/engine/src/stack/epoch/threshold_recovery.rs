//! Recover threshold authority against the exact committed DKG committee.
use super::super::*;
use super::transport::ChannelMux;

pub(super) struct RecoveredThreshold {
    pub(super) local_consensus_key: bls12381::PublicKey,
    pub(super) last_execution_height: u64,
    pub(super) last_execution_hash: B256,
    pub(super) recovered_boundary: Option<(u64, DkgBoundaryArtifact)>,
    pub(super) recovered_pending_boundary: Option<PendingDkgBoundarySnapshot>,
    pub(super) signing_share: Option<Share>,
    pub(super) polynomial: Sharing<MinSig>,
    pub(super) last_dkg_output: Option<Output<MinSig, bls12381::PublicKey>>,
    pub(super) coordinate_genesis_bootstrap: bool,
    pub(super) genesis_dkg_boundary_artifact: Option<DkgBoundaryArtifact>,
    pub(super) participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    pub(super) proposer_evm_address: Option<EthAddress>,
}
/// Pinned startup membership, finality evidence and local threshold inputs.
pub(super) struct ThresholdRecovery<'a, E: Clock> {
    pub(super) args: &'a ConsensusArgs,
    pub(super) node: &'a OutbeFullNode,
    pub(super) key_backend: &'a bls::KeyBackend,
    pub(super) signing_key: &'a bls12381::PrivateKey,
    pub(super) validator_set: &'a validators::ValidatorSet,
    pub(super) genesis_hash: B256,
    pub(super) dkg_rotation_params: DkgRotationParams,
    pub(super) last_consensus_finalized: Height,
    pub(super) dkg_mux: &'a mut ChannelMux<E>,
}

pub(super) async fn recover_threshold<E>(
    ctx: &E,
    input: ThresholdRecovery<'_, E>,
) -> Result<RecoveredThreshold>
where
    E: BufferPooler
        + Clock
        + CryptoRng
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    let ThresholdRecovery {
        args,
        node,
        key_backend,
        signing_key,
        validator_set,
        genesis_hash,
        dkg_rotation_params,
        last_consensus_finalized,
        dkg_mux,
    } = input;
    let local_consensus_key = signing_key.public_key();
    let startup_snapshot = resolve_startup_dkg_snapshot(
        ctx.child("startup_dkg_snapshot"),
        node,
        args,
        key_backend,
        local_consensus_key.clone(),
        validator_set,
        genesis_hash,
        dkg_rotation_params,
        last_consensus_finalized.get(),
    )
    .await?;
    let last_execution_height = startup_snapshot.last_execution_height;
    let last_execution_hash = startup_snapshot.last_execution_hash;
    let recovered_boundary = startup_snapshot.recovered_boundary;
    let recovered_pending_boundary = startup_snapshot.pending_boundary;
    let startup_dkg_context = startup_snapshot.context;

    // Determine founding versus existing identity before any DKG/live-join path.
    // The mandatory enclave client was installed by the node entrypoint before
    // Reth launch; this gate prevents threshold work and consensus startup from
    // treating the pre-DKG onboarding recipient as a permanent offer key.
    let verifier_join = args.signing_share.is_none()
        && args.public_polynomial.is_some()
        && args.dkg_output.is_some();
    let local_key_in_current_consensus_set = validator_set
        .public_keys
        .iter()
        .any(|key| key == &local_consensus_key);
    let on_chain_offer = validators::read_tee_offer_public_at_latest(&node.provider)
        .wrap_err("failed to read canonical offer key before threshold work")?;
    let resident_offer = outbe_tee::resident_offer_public_key_state_v1()
        .wrap_err("failed to read enclave offer-key readiness before threshold work")?;
    validate_offer_key_before_threshold_work(
        startup_dkg_context,
        local_key_in_current_consensus_set,
        verifier_join,
        on_chain_offer,
        resident_offer,
    )?;
    info!(
        founding = startup_dkg_mode(startup_dkg_context, local_key_in_current_consensus_set)
            == StartupDkgMode::InitialGenesisDkg
            && !verifier_join,
        offer_key_ready = resident_offer.is_some(),
        canonical_offer_key_present = !on_chain_offer.is_zero(),
        "permanent offer-key gate passed before threshold work"
    );

    // -- 6. Obtain threshold material ------------------------------------
    // For initial DKG, use subchannel 0 of the DKG mux.
    let (dkg_init_tx, dkg_init_rx) = dkg_mux
        .register(0)
        .await
        .map_err(|e| eyre::eyre!("failed to register initial DKG subchannel: {e}"))?;

    let recovered_shareless_output = recovered_boundary
        .as_ref()
        .map(|(_height, boundary)| decode_boundary_output(boundary))
        .transpose()?
        .filter(|output| output.players().position(&local_consensus_key).is_none());
    let threshold_material = if let Some(output) = recovered_shareless_output {
        info!(
        target: "outbe_engine::stack",
                   dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                   "local validator is absent from the finalized DKG boundary; restoring shareless verifier mode"
               );
        ThresholdMaterial::VerifierOnly {
            polynomial: output.public().clone(),
            last_dkg_output: Some(output),
        }
    } else {
        obtain_threshold_material(
            ctx.child("initial_dkg_material"),
            args,
            key_backend,
            signing_key.clone(),
            validator_set,
            startup_dkg_context,
            dkg_init_tx,
            dkg_init_rx,
        )
        .await?
    };
    let (signing_share, polynomial, last_dkg_output, bootstrap_from_live_dkg) =
        match threshold_material {
            ThresholdMaterial::Ready {
                signing_share,
                polynomial,
                last_dkg_output,
                bootstrap_from_live_dkg,
            } => (
                Some(signing_share),
                polynomial,
                last_dkg_output,
                bootstrap_from_live_dkg,
            ),
            ThresholdMaterial::VerifierOnly {
                polynomial,
                last_dkg_output,
            } => (None, polynomial, last_dkg_output, false),
        };
    // Threshold material, not CLI file presence, owns consensus authority. A
    // recovered validator excluded from the current boundary is VerifierOnly even
    // when its original signing-share path is still configured on disk.
    let shareless_verifier = signing_share.is_none();

    // Verifier-join supplies public threshold material without a signing share.
    // Its local database can still be at height zero while it joins an already
    // running chain, so local height alone must never reproduce genesis DKG/OST3.
    let coordinate_genesis_bootstrap = should_coordinate_genesis_tee_bootstrap(
        startup_dkg_context,
        local_key_in_current_consensus_set,
        shareless_verifier,
    );

    // Block 1 carries `BoundaryOutcome` before `TeeBootstrap`. Only a proven
    // founding member builds that canonical boundary from the completed
    // consensus DKG and reuses its committee hash for OST3. The corresponding
    // snapshot does not and must not exist in provider state until block 1
    // executes.
    let genesis_dkg_boundary_artifact = if coordinate_genesis_bootstrap {
        let bootstrap_output = last_dkg_output.as_ref().ok_or_else(|| {
            eyre::eyre!(
                "fresh bootstrap requires full DKG output; public polynomial alone cannot build canonical boundary"
            )
        })?;
        Some(build_genesis_dkg_boundary_artifact(
            validator_set,
            bootstrap_output,
            bootstrap_from_live_dkg,
        )?)
    } else {
        None
    };

    // -- 7. Build participant set (updated after each DKG reshare) -------
    // when recovering a finalized DKG boundary, reconstruct the scheme
    // against the committee the recovered threshold material belongs to (the DKG
    // output's players), NOT the latest on-chain set, which may have drifted
    // across a churn window. `select_recovery_participants` also fails fast if the
    // restored material does not match the recovered boundary. On a fresh chain or
    // when no boundary/output is recovered, fall back to the latest committed set
    // (the genesis committee on first start).
    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        match (recovered_boundary.as_ref(), last_dkg_output.as_ref()) {
            (Some((_, boundary)), Some(output)) => {
                select_recovery_participants(output.players(), boundary)?
            }
            _ => validator_set
                .public_keys
                .clone()
                .into_iter()
                .try_collect()
                .map_err(|e| eyre::eyre!("invalid participant set: {e}"))?,
        };

    let reshare_target_validator_set = {
        let state = node
            .provider
            .latest()
            .wrap_err("failed to load latest state for EVM signer validation")?;
        validators::read_reshare_target_from_state(&state)
            .wrap_err("failed to load current reshare target for EVM signer validation")?
    };
    let recovered_committee_for_signer = recovered_boundary
        .as_ref()
        .map(|(_, boundary)| (&participants, boundary));
    let proposer_evm_address = validate_validator_evm_signer(
        args,
        signing_key,
        validator_set,
        &reshare_target_validator_set,
        recovered_committee_for_signer,
        shareless_verifier,
    )?;

    Ok(RecoveredThreshold {
        local_consensus_key,
        last_execution_height,
        last_execution_hash,
        recovered_boundary,
        recovered_pending_boundary,
        signing_share,
        polynomial,
        last_dkg_output,
        coordinate_genesis_bootstrap,
        genesis_dkg_boundary_artifact,
        participants,
        proposer_evm_address,
    })
}
