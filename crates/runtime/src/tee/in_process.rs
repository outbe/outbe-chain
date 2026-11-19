//! Fresh RAM identity with the stable, owner-only NodeHost key kept on disk.
use super::*;
use outbe_primitives::tee_attestation_v1::{
    EnclaveInitializationManifestV1, NodeIdV1, TrustedNetworkDescriptorV1,
};
use outbe_tee::{
    in_process::{InProcessTeeContext, ProofSourceV1},
    resident::ResidentTeeKeys,
};

pub(super) fn initialize(
    config: &LaunchConfig,
    node_data_dir: &Path,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) -> eyre::Result<outbe_consensus::validators::LocalTeeRuntimeIdentityV1> {
    // Unsupported hardware fails before creating any persistent identity.
    let proof = ProofSourceV1::NoHardware;
    proof.ensure_available(policy.attestation_mode)?;
    let binding =
        outbe_tee::tee_attestation_activation::DcapSeededChainSpecBindingV1::from_chain_spec(
            &config.chain,
        )?;
    let (_, reth_p2p_public) =
        load_reth_p2p_node_host_signer(&config.network, config.datadir().p2p_secret())?;
    let node_host = load_node_host_key(node_data_dir)?;
    let identity = outbe_tee::NodeHostIdentityV1 {
        network_binding: policy.network_binding(),
        reth_p2p_public,
    };
    let authorization = identity.authorization_witness_v1(&node_host)?;
    let keys = ResidentTeeKeys::generate()?;
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        attestation_mode: policy.attestation_mode,
        node_id: NodeIdV1 { reth_p2p_public },
        initialization_challenge: alloy_primitives::B256::random().0,
        node_host_noise_x25519: authorization.node_host_noise_x25519,
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let context = Arc::new(InProcessTeeContext::new(
        keys,
        identity,
        manifest,
        TrustedNetworkDescriptorV1 {
            network_binding: policy.network_binding(),
            genesis_consensus_keys: binding.genesis_consensus_keys,
        },
        proof,
    )?);
    let expected_enclave_id = Some(context.manifest().enclave_id()?);
    outbe_tee::install_in_process_tee_context(context)?;
    Ok(outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
        reth_p2p_public,
        expected_enclave_id,
        validator: None,
    })
}

pub(super) async fn prepare_supervisor(
    (config, args): (&LaunchConfig, &ConsensusArgs),
    (node_data_dir, policy): (&Path, &outbe_primitives::tee_attestation_v1::TeePolicyV1),
    signer: Option<&Arc<OutbeEvmSigner>>,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<
    (
        Option<crate::admission::LocalTeeAdmissionAnchorV1>,
        Option<crate::tee_supervisor::TeeLifecycleSupervisor>,
    ),
    AutonomousStartupError,
> {
    let (Some(upstream), Some(signer)) = (args.upstream.as_deref(), signer) else {
        return Ok((None, None));
    };
    let mut supervisor =
        new_supervisor(config, node_data_dir, policy, signer, upstream).map_err(|error| *error)?;
    let anchor = supervisor.prepare(cancellation).await?;
    Ok((anchor, Some(supervisor)))
}

pub(super) fn local_lifecycle_endpoint(
    args: &ConsensusArgs,
    has_signer: bool,
    local: Option<std::net::SocketAddr>,
) -> Option<String> {
    if args.upstream.is_some() || !has_signer {
        return None;
    }
    local.map(|addr| format!("http://{addr}"))
}

pub(super) fn new_supervisor(
    config: &LaunchConfig,
    node_data_dir: &Path,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    signer: &Arc<OutbeEvmSigner>,
    endpoint: &str,
) -> Result<crate::tee_supervisor::TeeLifecycleSupervisor, Box<AutonomousStartupError>> {
    let (node_signing, _) =
        load_reth_p2p_node_host_signer(&config.network, config.datadir().p2p_secret())
            .map_err(AutonomousStartupError::Identity)?;
    let binding =
        outbe_tee::tee_attestation_activation::DcapSeededChainSpecBindingV1::from_chain_spec(
            &config.chain,
        )
        .map_err(AutonomousStartupError::Activation)?;
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: policy.network_binding(),
        genesis_consensus_keys: binding.genesis_consensus_keys,
    };
    let live = outbe_tee::in_process_tee_context().ok_or(AutonomousStartupError::MissingContext)?;
    Ok(crate::tee_supervisor::TeeLifecycleSupervisor::new(
        endpoint,
        (signer.clone(), node_signing),
        (live, node_data_dir.to_path_buf(), descriptor),
    )
    .map_err(AutonomousStartupError::Supervisor)?)
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AutonomousStartupError {
    #[error("NodeHost signer loading failed")]
    Identity(#[source] eyre::Report),
    #[error(transparent)]
    Activation(#[from] outbe_tee::errors::TeeAttestationActivationError),
    #[error("resident context unavailable after initialization")]
    MissingContext,
    #[error(transparent)]
    Supervisor(#[from] crate::tee_supervisor::RuntimeSupervisorError),
    #[error(transparent)]
    Prepare(#[from] crate::tee_supervisor::PrepareSupervisorError),
}

#[derive(Debug, thiserror::Error)]
enum NodeHostStartupError {
    #[error("NodeHost directory must be a non-symlink directory with mode 0700")]
    InvalidDirectory,
}

fn load_node_host_key(
    node_data_dir: &Path,
) -> eyre::Result<outbe_tee::node_host::NodeHostIdentityKey> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    let directory = node_data_dir.join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
    let fresh = match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error.into()),
    };
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(NodeHostStartupError::InvalidDirectory.into());
    }
    let path = directory.join(outbe_tee::node_host::NODE_HOST_IDENTITY_KEY_V1);
    // A previously initialized directory never regenerates a lost identity.
    Ok(if fresh {
        outbe_tee::node_host::NodeHostIdentityKey::create_new(&path)?
    } else {
        outbe_tee::node_host::NodeHostIdentityKey::load(&path)?
    })
}

/// Before block one, only the genesis consensus set may bootstrap authority.
pub(super) fn founder_lease_gate(
    protocol: &dyn outbe_consensus::protocol_state::EngineProtocolState,
    node: &OutbeFullNode,
    args: &ConsensusArgs,
    genesis_hash: alloy_primitives::B256,
) -> eyre::Result<TeeLeaseGuardGateV1> {
    let signing_key_path = args
        .signing_key
        .as_ref()
        .ok_or(outbe_consensus::errors::StackEpochError::SigningKeyRequired)?;
    let key_backend = args.key_backend().wrap_err("invalid BLS key backend")?;
    let local_consensus_key =
        outbe_consensus::validators::load_signing_public_key(signing_key_path, &key_backend)
            .wrap_err("failed to load consensus signing public key for TEE lease guard")?;
    let genesis_validators = outbe_consensus::validators::read_consensus_validators_at_block(
        protocol,
        &node.provider,
        genesis_hash,
    )
    .wrap_err("failed to read genesis consensus validator set for TEE lease guard")?;
    Ok(
        if genesis_validators
            .public_keys
            .contains(&local_consensus_key)
        {
            TeeLeaseGuardGateV1::Armed
        } else {
            TeeLeaseGuardGateV1::AwaitingAnchor
        },
    )
}

pub(super) fn bind_activation_refresh<P>(
    protocol: &Arc<crate::OutbeRuntime>,
    provider: P,
    config: crate::admission::TeeLeaseGuardConfigV1,
    owner: Arc<crate::admission_authority::live::LiveTeeRuntimeV1>,
) -> eyre::Result<()>
where
    P: BlockIdReader
        + HeaderProvider<Header = OutbeHeader>
        + StateProviderFactory
        + Send
        + Sync
        + 'static,
{
    owner.bind_armed(config.armed)?;
    let weak_protocol = Arc::downgrade(protocol);
    protocol.install_tee_refresh(Arc::new(move || {
        use outbe_consensus::protocol_state::EngineProtocolError;
        let protocol = weak_protocol
            .upgrade()
            .ok_or(EngineProtocolError::NotInstalled)?;
        owner
            .refresh(&provider, protocol.as_ref(), &config.chain)
            .map_err(EngineProtocolError::TeeRefresh)?;
        Ok(())
    }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{TeeLeaseGuardConfigV1, TeeLeaseGuardGateV1};
    use crate::admission_authority::{live::*, test_support as support};
    use outbe_consensus::protocol_state::EngineProtocolState as _;
    use outbe_tee::TeeAuthorityState;

    #[test]
    fn node_host_key_is_stable_and_loss_never_regenerates_identity() {
        let root = tempfile::tempdir().unwrap();
        let first = load_node_host_key(root.path()).unwrap().public();
        assert_eq!(load_node_host_key(root.path()).unwrap().public(), first);
        let path = root
            .path()
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .join(outbe_tee::node_host::NODE_HOST_IDENTITY_KEY_V1);
        std::fs::remove_file(&path).unwrap();
        assert!(load_node_host_key(root.path()).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn activation_callback_opens_bootstrap_and_bool_watch_before_any_worker_tick() {
        let _port = outbe_tee::authority::tee_authority_test_guard();
        let original = support::context(9);
        let (provider, chain, identity) =
            support::provider(&original, 0, 1_700_000_000, 1_800_001_000, false);
        let context = Arc::new(
            InProcessTeeContext::new(
                ResidentTeeKeys::from_seed([9; 32]).unwrap(),
                *original.identity(),
                original.manifest().clone(),
                TrustedNetworkDescriptorV1 {
                    network_binding: original.manifest().network_binding(),
                    genesis_consensus_keys: vec![],
                },
                ProofSourceV1::NoHardware,
            )
            .unwrap(),
        );
        provider.inner.accounts.lock().clear();
        let authority = outbe_tee::TeeAuthorityChannel::default();
        outbe_tee::install_tee_authority_watch(authority.subscribe()).unwrap();
        let gate = TeeLeaseGuardGateV1::Armed;
        let owner = Arc::new(
            LiveTeeRuntimeV1::new(
                LiveTeeSnapshotV1 {
                    identity,
                    gate,
                    context: context.clone(),
                },
                authority.clone(),
            )
            .unwrap(),
        );
        let runtime = Arc::new(crate::OutbeRuntime::new());
        let (armed, armed_rx) = tokio::sync::watch::channel(false);
        bind_activation_refresh(
            &runtime,
            provider,
            TeeLeaseGuardConfigV1 {
                chain,
                finalization_notify: Arc::new(tokio::sync::Notify::new()),
                armed,
            },
            owner,
        )
        .unwrap();
        runtime.refresh_tee_authority().unwrap();
        assert_eq!(
            authority.snapshot(),
            TeeAuthorityState::UnkeyedAwaitingAdmission
        );
        assert!(!*armed_rx.borrow());
        context
            .activate_for_test(
                outbe_tee::resident::ResidentOfferMaterial::for_test(
                    vec![5; 96],
                    alloy_primitives::B256::from(context.manifest().chain_id),
                    0,
                    0,
                )
                .unwrap(),
            )
            .unwrap();
        runtime.refresh_tee_authority().unwrap();
        assert_eq!(
            authority.snapshot(),
            TeeAuthorityState::ArmedBootstrap {
                enclave_id: context.manifest().enclave_id().unwrap()
            }
        );
        assert!(*armed_rx.borrow());
        assert_eq!(runtime.tee_authority_state(), authority.snapshot());
    }

    #[test]
    fn durable_checkpoint_cannot_arm_a_fresh_process_without_the_resident_offer_key() {
        let old = support::context(9);
        let (provider, chain, identity) =
            support::provider(&old, 7, 1_800_000_000, 1_800_001_000, false);
        let fresh = Arc::new(
            InProcessTeeContext::new(
                ResidentTeeKeys::from_seed([9; 32]).unwrap(),
                *old.identity(),
                old.manifest().clone(),
                TrustedNetworkDescriptorV1 {
                    network_binding: old.manifest().network_binding(),
                    genesis_consensus_keys: vec![],
                },
                ProofSourceV1::NoHardware,
            )
            .unwrap(),
        );
        let finalized = provider.finalized_block_num_hash().unwrap().unwrap();
        let anchor = crate::admission::LocalTeeAdmissionAnchorV1 {
            finalized_height: finalized.number,
            finalized_hash: finalized.hash,
        };
        let owner = LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                identity,
                context: fresh.clone(),
                gate: TeeLeaseGuardGateV1::Pending(anchor),
            },
            outbe_tee::TeeAuthorityChannel::default(),
        )
        .unwrap();
        assert_eq!(
            owner
                .refresh(&provider, &crate::OutbeRuntime::new(), &chain)
                .unwrap(),
            TeeAuthorityState::UnkeyedAwaitingAdmission
        );
        assert!(!owner.authority.snapshot().is_authority_open());
        assert!(!fresh.offer_key_ready().unwrap());
    }
}
