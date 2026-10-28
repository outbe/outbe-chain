use super::super::*;

pub(super) fn startup_ancestry_readiness(
    last_execution_height: u64,
    ancestry_readiness_target: u64,
) -> AncestryReadiness {
    let ancestry_readiness =
        AncestryReadiness::new(last_execution_height, ancestry_readiness_target);
    if !ancestry_readiness.is_ready() {
        info!(
            last_execution_height,
            last_consensus_finalized = ancestry_readiness_target,
            "marshal ancestry gate closed until executor backfills durable consensus blocks"
        );
    }
    ancestry_readiness
}

pub(super) fn recovery_anchor_hash(
    provider: &impl BlockHashReader,
    recovery_anchor_height: u64,
    execution_head: (u64, B256),
    genesis_hash: B256,
) -> Result<B256> {
    if recovery_anchor_height == 0 {
        Ok(genesis_hash)
    } else if recovery_anchor_height == execution_head.0 {
        Ok(execution_head.1)
    } else {
        super::super::dkg::startup::required_block_hash(
            provider,
            recovery_anchor_height,
            |reason| {
                crate::errors::StackEpochError::ReadRecoveryAnchorBlockHash {
                    height: recovery_anchor_height,
                    reason,
                }
                .into()
            },
            crate::errors::StackEpochError::MissingRecoveryAnchorBlockHash {
                height: recovery_anchor_height,
            }
            .into(),
        )
    }
}

pub(super) fn seed_genesis_validation(
    bridge: &ConsensusExecutionBridge,
    validator_set: &validators::ValidatorSet,
    epoch_length_blocks: u32,
    genesis_dkg_boundary_artifact: &mut Option<DkgBoundaryArtifact>,
    vrf_material_version: u64,
) -> Result<DkgBoundaryArtifact> {
    use outbe_primitives::consensus::{GenesisValidator, GenesisValidators};

    let genesis_vals: Vec<GenesisValidator> = validator_set
        .addresses
        .iter()
        .zip(validator_set.public_keys.iter())
        .map(|(addr, pk)| {
            let pk_bytes = commonware_codec::Encode::encode(pk);
            let mut pubkey = [0u8; 48];
            let len = pk_bytes.len().min(48);
            pubkey[..len].copy_from_slice(&pk_bytes[..len]);
            GenesisValidator {
                address: *addr,
                consensus_pubkey: pubkey,
            }
        })
        .collect();

    bridge.set_genesis_validators(GenesisValidators {
        validators: genesis_vals,
        epoch_length_blocks,
    });

    let bootstrap_artifact = genesis_dkg_boundary_artifact
        .take()
        .ok_or(crate::errors::StackEpochError::FreshBootstrapLostGenesisBoundary)?;
    if bootstrap_artifact.vrf_material_version != vrf_material_version {
        return Err(
            crate::errors::StackEpochError::GenesisBoundaryVrfVersionDiffersFromActive.into(),
        );
    }
    info!(
        vrf_material_version,
        vrf_group_public_key = %bootstrap_artifact.vrf_group_public_key,
        target_set_hash = %bootstrap_artifact.target_set_hash,
        active_set_hash = %bootstrap_artifact.reshare.active_set_hash,
        "genesis DKG boundary artifact queued; VRF active from view 2"
    );
    Ok(bootstrap_artifact)
}

pub(super) fn reconcile_recovered_vrf_material(
    polynomial: &mut Sharing<MinSig>,
    last_dkg_output: &mut Option<Output<MinSig, bls12381::PublicKey>>,
    has_signing_share: bool,
    recovered_boundary_artifact: Option<&DkgBoundaryArtifact>,
) -> Result<()> {
    // These strict checks (saved polynomial / DKG output must equal the recovered
    // finalized boundary) only matter for a SIGNER, which signs with its polynomial +
    // share. A share-less VERIFIER follows finality via the certificate's PARTICIPANT
    // set (not its polynomial). So its local public material may be
    // off without affecting sync. Examples: a TEE chain's runtime-derived genesis
    // consensus polynomial differs from the bootstrap file, or the chain rotated past
    // it. Only its local VRF/leader view degrades (process-local, non-fatal, same as
    // the post-rotation verifier-follower case). Enforcing these checks on a restarted
    // verifier would fatally crash an otherwise-healthy follower. So gate them to
    // signers. The verifier syncs, and the running epoch loop advances it.
    if has_signing_share {
        validate_recovered_vrf_material(polynomial, recovered_boundary_artifact)?;
        if let (Some(output), Some(boundary)) =
            (last_dkg_output.as_ref(), recovered_boundary_artifact)
        {
            let canonical_output = decode_boundary_output(boundary)
                .wrap_err("failed to decode recovered DKG boundary output")?;
            dkg_manager::assert_canonical_output(output, &canonical_output, "restart recovery")?;
        }
    } else if let Some(boundary) = recovered_boundary_artifact {
        // Verifier-follower with a recovered on-chain DKG boundary (e.g. a restart, or
        // a TEE chain whose runtime genesis consensus output differs from the bootstrap
        // CLI files): adopt the chain's CURRENT canonical DKG output as both the
        // polynomial and the reshare prev_output. The DKG reshare ceremony binds the
        // FULL previous output into its `info_hash` (not just the group key). So if the
        // verifier later becomes a frozen-target player, it MUST present the committee's
        // current output as prev_output. Otherwise its stale local output
        // would cause this sequence:
        // 1. it yields a divergent `info_hash`.
        // 2. the dealers' bundles get dropped.
        // 3. the ceremony times out (the node never gets a share).
        // The genesis/boundary artifact carries the full `Output`, so
        // `decode_boundary_output` recovers exactly what the committee holds. Finality
        // still verifies via the participant set regardless.
        let canonical_output = decode_boundary_output(boundary)
            .wrap_err("failed to decode recovered DKG boundary output for verifier")?;
        *polynomial = canonical_output.public().clone();
        *last_dkg_output = Some(canonical_output);
    }
    Ok(())
}

pub(super) struct ValidatorBootstrapFollowInputs<'a> {
    pub(super) args: &'a ConsensusArgs,
    pub(super) node: &'a OutbeFullNode,
    pub(super) bridge: &'a ConsensusExecutionBridge,
    pub(super) signing_key: &'a bls12381::PrivateKey,
    pub(super) genesis_hash: B256,
    pub(super) dkg_rotation_params: DkgRotationParams,
    pub(super) tee_lease_armed_rx: tokio::sync::watch::Receiver<bool>,
    pub(super) finalized_execution_barrier: &'a Arc<dyn FinalizedExecutionBarrier>,
    pub(super) follower_shutdown: Option<crate::follower_shutdown::FollowerDrain>,
    pub(super) finalized_parent_cert_store:
        &'a outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
    pub(super) finalization_notify: &'a Arc<tokio::sync::Notify>,
}

pub(super) async fn maybe_run_validator_bootstrap_follow<E>(
    ctx: E,
    inputs: ValidatorBootstrapFollowInputs<'_>,
) -> Result<bool>
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
    use outbe_primitives::consensus::ConsensusOperatingMode;
    use reth_provider::BlockIdReader as _;

    let local_consensus_key = inputs.signing_key.public_key();
    let bootstrap = super::super::follower::ValidatorBootstrap {
        local_key_in_genesis_validator_set: validators::read_consensus_validators_at_block(
            &inputs.node.provider,
            inputs.genesis_hash,
        )?
        .public_keys
        .contains(&local_consensus_key),
        local_consensus_key,
        dkg_rotation_params: inputs.dkg_rotation_params,
        tee_lease_armed_rx: inputs.tee_lease_armed_rx,
    };
    let finalized_height = inputs
        .node
        .provider
        .finalized_block_num_hash()?
        .map_or(0, |head| head.number);
    if bootstrap.is_validator_bootstrap_ready(inputs.node, finalized_height)? {
        return Ok(false);
    }
    let upstream = inputs
        .args
        .upstream
        .clone()
        .ok_or(crate::errors::StackEpochError::ValidatorBootstrapRequiresUpstream)?;
    inputs
        .bridge
        .set_operating_mode(ConsensusOperatingMode::UpstreamBootstrap);
    let exit = run_follow_stack(
        ctx,
        inputs.args.clone(),
        services::FollowerConnection {
            node: inputs.node.clone(),
            bridge: inputs.bridge.clone(),
            upstream,
        },
        services::FollowerStackServices {
            finalized_execution_barrier: inputs.finalized_execution_barrier.clone(),
            follower_shutdown: inputs
                .follower_shutdown
                .ok_or(crate::errors::StackEpochError::FollowerPreStopHandshakeNotInstalled)?,
            parent_cert_store: inputs.finalized_parent_cert_store.clone(),
            bootstrap: Some(bootstrap),
            finalization_notify: inputs.finalization_notify.clone(),
        },
    )
    .await?;
    Ok(exit == crate::follower_shutdown::FollowStackExit::Shutdown)
}
