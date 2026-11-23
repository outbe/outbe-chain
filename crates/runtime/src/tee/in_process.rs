//! Restore the local resident identity with the owner-only NodeHost key.
use super::*;
use outbe_primitives::tee_attestation_v1::{
    EnclaveInitializationManifestV1, NodeIdV1, TrustedNetworkDescriptorV1,
};
#[cfg(test)]
use outbe_tee::resident::ResidentTeeKeys;
use outbe_tee::{
    in_process::{InProcessTeeContext, ProofSourceV1},
    node_host::ResidentStateStore,
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
    let (store, restored) = ResidentStateStore::load_or_create(node_data_dir, &authorization)?;
    let keys = restored.keys;
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        attestation_mode: policy.attestation_mode,
        node_id: NodeIdV1 { reth_p2p_public },
        initialization_challenge: restored.initialization_challenge,
        node_host_noise_x25519: authorization.node_host_noise_x25519,
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let context = Arc::new(
        InProcessTeeContext::new(
            keys,
            identity,
            manifest,
            TrustedNetworkDescriptorV1 {
                network_binding: policy.network_binding(),
                genesis_consensus_keys: binding.genesis_consensus_keys,
            },
            proof,
        )?
        .with_restored_state(store, restored.material, restored.anchor),
    );
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

pub(super) fn new_supervisor(
    config: &LaunchConfig,
    node_data_dir: &Path,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    signer: &Arc<OutbeEvmSigner>,
    endpoint: &str,
) -> Result<crate::tee_supervisor::TeeLifecycleSupervisor, Box<AutonomousStartupError>> {
    let resident = resident_supervisor_parts(config, node_data_dir, policy)?;
    Ok(crate::tee_supervisor::TeeLifecycleSupervisor::new(
        endpoint,
        (signer.clone(), resident.node_signing),
        (resident.live, resident.node_data_dir, resident.descriptor),
    )
    .map_err(AutonomousStartupError::Supervisor)?)
}

pub(super) fn construct_in_process_supervisor(
    methods: jsonrpsee::Methods,
    signer: &Arc<OutbeEvmSigner>,
    resident: super::FoundingResident,
) -> Result<crate::tee_supervisor::TeeLifecycleSupervisor, Box<AutonomousStartupError>> {
    crate::tee_supervisor::TeeLifecycleSupervisor::new_in_process(
        methods,
        (signer.clone(), resident.node_signing),
        (resident.live, resident.node_data_dir, resident.descriptor),
    )
    .map_err(AutonomousStartupError::Supervisor)
    .map_err(Box::new)
}

pub(super) fn founding_resident(
    config: &LaunchConfig,
    node_data_dir: &Path,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) -> Result<super::FoundingResident, Box<AutonomousStartupError>> {
    let resident = resident_supervisor_parts(config, node_data_dir, policy)?;
    Ok(super::FoundingResident {
        node_signing: resident.node_signing,
        live: resident.live,
        node_data_dir: resident.node_data_dir,
        descriptor: resident.descriptor,
    })
}

pub(super) fn new_local_supervisor(
    config: &LaunchConfig,
    node_data_dir: &Path,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    signer: &Arc<OutbeEvmSigner>,
    methods: jsonrpsee::Methods,
) -> Result<crate::tee_supervisor::TeeLifecycleSupervisor, Box<AutonomousStartupError>> {
    let resident = founding_resident(config, node_data_dir, policy)?;
    construct_in_process_supervisor(methods, signer, resident)
}

struct ResidentParts {
    node_signing: k256::ecdsa::SigningKey,
    live: Arc<InProcessTeeContext>,
    node_data_dir: PathBuf,
    descriptor: TrustedNetworkDescriptorV1,
}

fn resident_supervisor_parts(
    config: &LaunchConfig,
    node_data_dir: &Path,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) -> Result<ResidentParts, Box<AutonomousStartupError>> {
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
    Ok(ResidentParts {
        node_signing,
        live,
        node_data_dir: node_data_dir.to_path_buf(),
        descriptor,
    })
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AutonomousStartupError {
    #[error("NodeHost signer loading failed")]
    Identity(#[source] eyre::Report),
    #[error(transparent)]
    Activation(#[from] outbe_tee::errors::TeeAttestationActivationError),
    #[error("resident context unavailable after initialization")]
    MissingContext,
    #[error("founding validator has no in-process lifecycle module")]
    MissingLocalLifecycleRpc,
    #[error(transparent)]
    Supervisor(#[from] crate::tee_supervisor::RuntimeSupervisorError),
    #[error(transparent)]
    Prepare(#[from] crate::tee_supervisor::PrepareSupervisorError),
}

#[derive(Debug, thiserror::Error)]
enum NodeHostStartupError {
    #[error("NodeHost directory must be a non-symlink directory with mode 0700")]
    InvalidDirectory,
    #[error("NodeHost identity key is missing while committed TEE state exists")]
    OrphanedNodeHostState,
}

const RESIDENT_STATE_FILE: &str = "resident-state.v1";
const LIVE_JOIN_ANCHOR_REL_PATH: &str = "tee-live-join-admission-v1/anchor.bin";
const SERVICE_TX_JOURNAL_REL_PATH: &str = "tee-service-tx-v1/journal.json";

fn load_node_host_key(
    node_data_dir: &Path,
) -> eyre::Result<outbe_tee::node_host::NodeHostIdentityKey> {
    use outbe_primitives::filesystem::entry_exists;
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    let directory = node_data_dir.join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
    match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {
            if let Some(parent) = directory.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::File::open(parent)?.sync_all()?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(NodeHostStartupError::InvalidDirectory.into());
    }
    let key_path = directory.join(outbe_tee::node_host::NODE_HOST_IDENTITY_KEY_V1);
    if entry_exists(&key_path)? {
        return Ok(outbe_tee::node_host::NodeHostIdentityKey::load(&key_path)?);
    }
    let resident_exists = entry_exists(&directory.join(RESIDENT_STATE_FILE))?;
    let anchor_exists = entry_exists(&node_data_dir.join(LIVE_JOIN_ANCHOR_REL_PATH))?;
    let journal_exists = entry_exists(&node_data_dir.join(SERVICE_TX_JOURNAL_REL_PATH))?;
    if resident_exists || anchor_exists || journal_exists {
        return Err(NodeHostStartupError::OrphanedNodeHostState.into());
    }
    Ok(outbe_tee::node_host::NodeHostIdentityKey::create_new(
        &key_path,
    )?)
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

    fn test_p2p_public(seed: u8) -> [u8; 33] {
        k256::ecdsa::SigningKey::from_slice(&[seed; 32])
            .unwrap()
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap()
    }

    fn test_authorization(
        node_host: &outbe_tee::node_host::NodeHostIdentityKey,
        p2p_seed: u8,
    ) -> outbe_primitives::tee_attestation_v1::NodeHostAuthorizationWitnessV1 {
        let identity = outbe_tee::NodeHostIdentityV1 {
            network_binding: outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
                chain_id: [0x11; 32],
                genesis_hash: alloy_primitives::B256::repeat_byte(0x22),
                attestation_mode:
                    outbe_primitives::tee_attestation_v1::AttestationMode::NoAttestationDev,
            },
            reth_p2p_public: test_p2p_public(p2p_seed),
        };
        identity.authorization_witness_v1(node_host).unwrap()
    }

    #[test]
    fn node_host_key_enforces_k_r_a_startup_matrix() {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

        // M1: K=no, R=no, A=no with pre-existing empty 0700 dir recovers first boot.
        {
            let root = tempfile::tempdir().unwrap();
            let host_dir = root
                .path()
                .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&host_dir)
                .unwrap();
            let first = load_node_host_key(root.path()).unwrap().public();
            assert_eq!(load_node_host_key(root.path()).unwrap().public(), first);
        }

        // M2: K=no, R=no, A=yes (anchor.bin or journal.json) -> OrphanedNodeHostState.
        for rel in [LIVE_JOIN_ANCHOR_REL_PATH, SERVICE_TX_JOURNAL_REL_PATH] {
            let root = tempfile::tempdir().unwrap();
            let artifact = root.path().join(rel);
            std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
            std::fs::write(&artifact, b"committed").unwrap();
            let error = load_node_host_key(root.path()).unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<NodeHostStartupError>(),
                    Some(NodeHostStartupError::OrphanedNodeHostState)
                ),
                "{rel}: {error:?}"
            );
            let key_path = root
                .path()
                .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
                .join(outbe_tee::node_host::NODE_HOST_IDENTITY_KEY_V1);
            assert!(!key_path.exists());
        }

        // M3: K=no, R=yes (with or without A) -> OrphanedNodeHostState without regenerating K.
        {
            let root = tempfile::tempdir().unwrap();
            let node_host = load_node_host_key(root.path()).unwrap();
            let auth = test_authorization(&node_host, 0x33);
            ResidentStateStore::load_or_create(root.path(), &auth).unwrap();
            let key_path = root
                .path()
                .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
                .join(outbe_tee::node_host::NODE_HOST_IDENTITY_KEY_V1);
            std::fs::remove_file(&key_path).unwrap();
            let error = load_node_host_key(root.path()).unwrap_err();
            assert!(matches!(
                error.downcast_ref::<NodeHostStartupError>(),
                Some(NodeHostStartupError::OrphanedNodeHostState)
            ));
            assert!(!key_path.exists());
        }

        // M4: K=yes, R=no, A=yes -> loads K and creates OBRESID2 tag 0 unkeyed (759n I5).
        {
            let root = tempfile::tempdir().unwrap();
            let node_host = load_node_host_key(root.path()).unwrap();
            let anchor = root.path().join(LIVE_JOIN_ANCHOR_REL_PATH);
            std::fs::create_dir_all(anchor.parent().unwrap()).unwrap();
            std::fs::write(&anchor, b"anchor").unwrap();
            let reloaded_key = load_node_host_key(root.path()).unwrap();
            assert_eq!(reloaded_key.public(), node_host.public());
            let auth = test_authorization(&reloaded_key, 0x33);
            let (_, loaded) = ResidentStateStore::load_or_create(root.path(), &auth).unwrap();
            assert!(loaded.material.is_none());
            assert!(loaded.anchor.is_none());
        }

        // M7: invalid K (bad mode) and invalid directory mode fail closed.
        {
            let root = tempfile::tempdir().unwrap();
            load_node_host_key(root.path()).unwrap();
            let host_dir = root
                .path()
                .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
            let key_path = host_dir.join(outbe_tee::node_host::NODE_HOST_IDENTITY_KEY_V1);
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(load_node_host_key(root.path()).is_err());
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::set_permissions(&host_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            let error = load_node_host_key(root.path()).unwrap_err();
            assert!(matches!(
                error.downcast_ref::<NodeHostStartupError>(),
                Some(NodeHostStartupError::InvalidDirectory)
            ));
        }
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
                gate: TeeLeaseGuardGateV1::Pending {
                    target: anchor,
                    floor: None,
                },
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

    #[test]
    fn p2p_key_rotation_rejected_at_startup_load_with_binding_mismatch() {
        let root = tempfile::tempdir().unwrap();
        let node_host = load_node_host_key(root.path()).unwrap();
        let auth_a = test_authorization(&node_host, 0x33);
        let chain_id = alloy_primitives::B256::from(auth_a.chain_id);
        let (store, first) = ResidentStateStore::load_or_create(root.path(), &auth_a).unwrap();
        let manifest_hash = EnclaveInitializationManifestV1 {
            chain_id: auth_a.chain_id,
            genesis_hash: auth_a.genesis_hash,
            attestation_mode: auth_a.attestation_mode,
            node_id: auth_a.node_id.clone(),
            initialization_challenge: first.initialization_challenge,
            node_host_noise_x25519: auth_a.node_host_noise_x25519,
            recipient_x25519: first.keys.tribute_offer_public(),
            attestation_ed25519: first.keys.attestation_pub(),
            noise_responder_x25519: first.keys.noise_public(),
        }
        .authorization_hash()
        .unwrap();
        let material =
            outbe_tee::resident::ResidentOfferMaterial::for_test(vec![7; 96], chain_id, 0, 0)
                .unwrap();
        store
            .persist_active(
                &first.keys,
                first.initialization_challenge,
                manifest_hash,
                &material,
                None,
            )
            .unwrap();

        let resident_path = root
            .path()
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .join(RESIDENT_STATE_FILE);
        let before = std::fs::read(&resident_path).unwrap();

        let auth_b = test_authorization(&node_host, 0x44);
        let reloaded = ResidentStateStore::load_or_create(root.path(), &auth_b);
        assert!(matches!(
            reloaded,
            Err(outbe_tee::node_host::ResidentStateError::BindingMismatch)
        ));
        assert_eq!(std::fs::read(&resident_path).unwrap(), before);
    }
}
